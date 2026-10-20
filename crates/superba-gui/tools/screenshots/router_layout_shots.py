#!/usr/bin/env python3
"""Capture the Router page in two sizes, themes and languages from real binaries.

Uses shoot.py's isolated HOME, PID namespace, mock relay and XTEST driver. No
real accounts are used. Pass --out and --bin-dir explicitly; each run writes a
source/binary manifest.

Two states (--states):

- `off`: what shoot.py provisions — a Krill profile and a keyless custom site,
  router mode off and no member, so the page has to guide the user to add one.
- `on`: a second fictional provider, router mode on with both members and the
  real `superba-router` serving them (`superba-router run` in the namespace; no
  service is installed), so the live line health and events are real.

For every state, size, theme and language one window is opened and shot at the
top (`router-<state>-<size>-<theme>-<lang>.png`). With --details the expanders
of the member section are opened by their visible text and shot again
(`…-details.png`). With --advanced the whole page is shot page by page from
the top (`…-adv1.png`, `…-adv2.png`, …) until the end (「高级」 and 「协议转换」
are plain sections since user bug 116, nothing is opened); with
--edit-price the first price ratio's 编辑 is clicked on the way and an invalid
value typed. Expanders and buttons are found on the screen with tesseract
(eng + chi_sim), so the driver follows the layout instead of fixed coordinates.

    python3 -I crates/superba-gui/tools/screenshots/router_layout_shots.py \\
        --bin-dir target/debug --out /tmp/router-shots --display :203 \\
        --states off,on --advanced --details --edit-price
"""

import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
SIZES = [(1280, 800), (960, 640)]
spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)

# The conversion matrix, opened on the way through Advanced when --details did
# not reach it (it is below the fold at 960×640).
CONVERSION = {"zh-CN": "协议转换", "en": "Protocol conversion"}
# 编辑 of the first price ratio: the shot shows the editor with an invalid value.
EDIT = {"zh-CN": "编辑", "en": "Edit"}
# The router's event log at the end of Advanced, opened when it is reached.
EVENTS = {"zh-CN": "路由事件", "en": "routing events"}
# Expanders of the member section, opened for the details shot.
DETAILS = {
    "zh-CN": ["线路详情", "转移与恢复规则", "协议转换"],
    "en": ["line details", "recovery rules", "Protocol conversion"],
}


def ocr_words(png, lang):
    """Words tesseract reads on `png` (scaled 2x): (text, x0, y0, x1, y1, line key)."""
    from PIL import Image  # only needed for --details / --advanced

    with tempfile.TemporaryDirectory() as tmp:
        big = Path(tmp) / "big.png"
        img = Image.open(png).convert("L")
        img = img.resize((img.width * 2, img.height * 2))
        # Dark theme: light text on dark; tesseract reads dark on light best.
        if img.resize((1, 1)).getpixel((0, 0)) < 128:
            img = img.point(lambda v: 255 - v)
        img.save(big)
        tsv = subprocess.run(
            ["tesseract", str(big), "stdout", "-l", "eng+chi_sim" if lang == "zh-CN" else "eng",
             "--psm", "11", "tsv"], capture_output=True, text=True, check=True).stdout
    words = []
    for row in tsv.splitlines()[1:]:
        cols = row.split("\t")
        if len(cols) < 12 or not cols[11].strip():
            continue
        x, y, w, h = (int(v) // 2 for v in cols[6:10])
        words.append((cols[11].strip(), x, y, x + w, y + h, tuple(cols[1:5])))
    return words


def find_texts(png, needle, lang):
    """Centers of every place `needle` is read on `png` (consecutive words of one
    OCR line whose concatenation, spaces ignored, contains it)."""
    words = ocr_words(png, lang)
    want = needle.replace(" ", "").lower()
    found = []
    i = 0
    while i < len(words):
        joined = ""
        end = None
        for j in range(i, min(i + 8, len(words))):
            if words[j][5] != words[i][5]:
                break
            joined += words[j][0].lower()
            if want in joined:
                end = j
                break
        if end is None:
            i += 1
            continue
        part = words[i:end + 1]
        y0 = min(w[2] for w in part)
        y1 = max(w[4] for w in part)
        found.append((min(w[1] for w in part) + 4, (y0 + y1) // 2))
        # The words of this hit cannot start another one.
        i = end + 1
    # A shorter run starting later on the same line repeats a hit: keep one per line.
    unique = []
    for p in found:
        if all(abs(p[1] - q[1]) > 8 for q in unique):
            unique.append(p)
    return unique


def conversion_open(png, x, y):
    """The conversion disclosure at (x, y) is open: its matrix header (a row of
    table captions) follows within two lines, else the next line is the next
    section. Read from the text under it."""
    from PIL import Image

    img = Image.open(png).convert("L")
    # The hint paragraph of an open disclosure starts right under its header:
    # dark (or, in the dark theme, light) text pixels 20–40 pt below it.
    band = img.crop((max(0, x - 10), y + 14, min(img.width, x + 400), y + 34))
    bg = img.getpixel((max(0, x - 30), y))
    ink = sum(1 for v in band.tobytes() if abs(v - bg) > 60)
    return ink > 40


def grab(s, path, size):
    w, h = size
    subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}", str(path)], env=s.env(), check=True)


def same_image(a, b):
    """True when the page did not scroll: at most 1 % of the pixels differ (live
    counters such as the router's uptime keep changing)."""
    from PIL import Image, ImageChops

    diff = ImageChops.difference(Image.open(a).convert("L"), Image.open(b).convert("L"))
    changed = sum(diff.point(lambda v: 255 if v > 24 else 0).histogram()[255:])
    return changed <= diff.width * diff.height // 100


def capture(s, args, name, theme, lang, size):
    """One window: the top, optionally the details and the Advanced pages."""
    shoot.guard(s.home)
    gui = subprocess.Popen(
        [str(args.bin_dir / "superba"), "--size", f"{size[0]}x{size[1]}",
         "--theme", theme, "--lang", lang, "--page", "router"],
        env=s.env(), cwd=s.home, stdout=subprocess.DEVNULL,
        stderr=open(s.work / "gui.err", "a"), start_new_session=True)
    shots = []
    try:
        time.sleep(7.0)
        s.input.park()
        time.sleep(0.6)
        top = args.out / f"{name}.png"
        grab(s, top, size)
        shots.append(top)
        content_x = 220 + (size[0] - 220) // 2
        if args.details:
            hits = [p for needle in DETAILS[lang] for p in find_texts(top, needle, lang)]
            # From the bottom up, so opening one does not move the next.
            for x, y in sorted(hits, key=lambda p: -p[1]):
                s.input.click(x, y)
                time.sleep(0.3)
            s.input.park()
            time.sleep(0.6)
            path = args.out / f"{name}-details.png"
            grab(s, path, size)
            shots.append(path)
        if args.advanced:
            # 「高级」 is a plain section since user bug 116: nothing to open,
            # the page is shot from the top down.
            # Pages of the expanded section: a notch (about 80 pt) less than the
            # column under the header.
            notches = int(os.environ.get("ROUTER_SHOTS_NOTCHES", "0")) or max(2, (size[1] - 200) // 80)
            prev = None
            events_opened = False
            edited = not args.edit_price
            converted = False
            for i in range(1, 16):
                path = args.out / f"{name}-adv{i}.png"
                grab(s, path, size)
                if prev is not None and same_image(prev, path):
                    path.unlink()
                    hits = [] if events_opened else find_texts(prev, EVENTS[lang], lang)
                    if not hits:
                        break
                    # The end of the page: open the event log and go on.
                    events_opened = True
                    x, y = max(hits, key=lambda p: p[1])
                    s.input.click(x, y)
                    s.input.park()
                    time.sleep(0.8)
                    s.input.scroll(content_x, size[1] - 80, 60)
                    s.input.park()
                    time.sleep(0.6)
                    path = args.out / f"{name}-adv{i}.png"
                    grab(s, path, size)
                if args.details and not converted:
                    hits = [p for p in find_texts(path, CONVERSION[lang], lang)
                            if p[1] < size[1] - 40]
                    if hits:
                        converted = True
                        # Closed (a chevron right) only when the details pass
                        # did not open it: look at the line before clicking.
                        x, y = hits[0]
                        if not conversion_open(path, x, y):
                            s.input.click(x, y)
                            s.input.park()
                            time.sleep(0.8)
                            grab(s, path, size)
                if not edited:
                    # The price editor, on the page that shows the ratios'
                    # actions column (the right quarter of the window).
                    hits = [p for p in find_texts(path, EDIT[lang], lang)
                            if p[0] > size[0] * 0.75]
                    if hits:
                        edited = True
                        x, y = min(hits, key=lambda p: p[1])
                        s.input.click(x, y)
                        time.sleep(0.5)
                        s.input.key("ctrl+a")
                        s.input.type("abc")
                        s.input.park()
                        time.sleep(0.6)
                        grab(s, path, size)
                shots.append(path)
                prev = path
                s.input.scroll(content_x, size[1] - 80, notches)
                s.input.park()
                time.sleep(0.6)
    finally:
        shoot.kill(gui.pid)
        try:
            gui.wait(timeout=5)
        except subprocess.TimeoutExpired:
            shoot.kill(gui.pid, shoot.signal.SIGKILL)
    for p in shots:
        shoot.log(str(p))
    return shots


def turn_on(s, args):
    """Second member, router mode on, the real router serving both."""
    spb = str(s.bin / "spb")
    s.run([spb, "profile", "add", "backup", "--name", "Example API", "--site", "example-api",
           "--auth", "apikey", "--key-stdin"], stdin=shoot.KEY + "\n")
    s.run([spb, "router", "enable", "--member", "krill", "--member", "backup",
           "--yes", "--write-only"])
    data = s.home / ".local/share/superba"
    router = subprocess.Popen(
        [str(s.bin / "superba-router"), "run", "--data-dir", str(data)],
        env=s.env(), cwd=s.home, stdout=subprocess.DEVNULL,
        stderr=open(s.work / "router.err", "a"), start_new_session=True)
    s.pids.append(router.pid)
    time.sleep(3.0)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--display", default=":178")
    ap.add_argument("--states", default="on", help="comma-separated: off, on")
    ap.add_argument("--sizes", default=",".join(f"{w}x{h}" for w, h in SIZES))
    ap.add_argument("--variants", default="",
                    help="comma-separated theme-lang pairs, e.g. light-zh,dark-en (default all)")
    ap.add_argument("--advanced", action="store_true", help="also shoot the expanded Advanced")
    ap.add_argument("--details", action="store_true", help="also shoot the opened expanders")
    ap.add_argument("--edit-price", action="store_true",
                    help="with --advanced: open the first price ratio's editor and type an invalid value")
    args = ap.parse_args()
    if os.environ.get("SUPERBA_ROUTER_LAYOUT_NS") != "1":
        os.execvpe("unshare", ["unshare", "--user", "--map-current-user", "--pid",
                   "--fork", "--mount-proc", sys.executable, "-I", str(Path(__file__).resolve()),
                   *sys.argv[1:]], {**os.environ, "SUPERBA_ROUTER_LAYOUT_NS": "1"})
    args.bin_dir = args.bin_dir.resolve()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    sizes = [tuple(int(x) for x in v.lower().split("x", 1)) for v in args.sizes.split(",") if v]
    states = [v for v in args.states.split(",") if v]
    wanted = [v for v in args.variants.split(",") if v]
    variants = [(t, l) for t, l in shoot.VARIANTS
                if not wanted or f"{t}-{'zh' if l == 'zh-CN' else 'en'}" in wanted]
    manifest = {
        "source": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
        "diff_sha256": hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD"], cwd=REPO)).hexdigest(),
        "binaries": {name: hashlib.sha256((args.bin_dir / name).read_bytes()).hexdigest()
                     for name in ("superba", "spb", "superba-router")},
        "states": {"off": "Krill profile and a keyless site; router off, no member",
                   "on": "Two mock providers; router on and running; three Krill lines"},
        "screenshots": [],
    }
    opts = SimpleNamespace(display=args.display, sizes=sizes, app_dir=None,
                           no_router_log=False, rollouts=False, router_on=False,
                           bin_dir=args.bin_dir, out=args.out, keep=False)
    session = shoot.Session(opts)
    try:
        for name in manifest["binaries"]:
            shutil.copy2(args.bin_dir / name, session.bin / name)
        opts.bin_dir = session.bin
        args.bin_dir = session.bin
        shoot.guard(session.home)
        session.start_xvfb()
        session.start_relay()
        session.write_sites()
        session.provision()
        session.check_paths()
        for state in ("off", "on"):
            if state not in states:
                continue
            if state == "on":
                turn_on(session, args)
            for size in sizes:
                for theme, lang in variants:
                    name = (f"router-{state}-{size[0]}x{size[1]}-{theme}-"
                            f"{'zh' if lang == 'zh-CN' else 'en'}")
                    for path in capture(session, args, name, theme, lang, size):
                        manifest["screenshots"].append({
                            "file": path.name, "state": state, "size": size, "theme": theme,
                            "locale": lang, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
    finally:
        for filename in ("gui.err", "relay.err", "router.err"):
            path = session.work / filename
            if path.exists():
                shutil.copy2(path, args.out / filename)
        session.close()
        (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
