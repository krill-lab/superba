#!/usr/bin/env python3
"""Screenshots of the router-off confirmation (user bug 103) from the
`dialog_shots` example (crates/superba-gui/examples/dialog_shots.rs).

The alpha.7 user on Windows turned the router off and got the full-restart
confirmation with one line per model the relay lists that the app's Codex has
no built-in entry for, plus the catalog file, a stopped WSL distribution and an
unprobed compaction capability: the dialog grew taller than the window and its
buttons were off screen. The example draws the shell's real `confirm_full`
dialog for a plan with those notices (real catalog keys, real localization)
over the window background, the sidebar and the Router page header. It runs no
service and reads no files besides the system fonts.

Shots, at every --sizes entry, dark and light, English and Chinese:

- router-off-<size>-<theme>-<lang>: the dialog as it opens;
- router-off-scrolled-...: after scrolling its body to the end (--scroll);
- router-off-expanded-...: with the notes and the model group expanded
  (--expand; only builds that have the grouped notices accept it), and
  router-off-expanded-scrolled-... scrolled to the end (--expand --scroll);
- preview-...: the hot-switch preview overlay with the same notices
  (--preview; only builds that have the grouped notices).

    cargo build -p superba-gui --example dialog_shots
    python3 -I crates/superba-gui/tools/screenshots/dialog_shots.py \\
        --bin target/debug/examples/dialog_shots --display :202 \\
        --out /tmp/dialog-shots --prefix after- \\
        [--sizes 1280x800,960x640] [--scroll] [--expand] [--preview]
"""

import argparse
import importlib.util
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
VARIANTS = [("dark", "zh-CN"), ("light", "zh-CN"), ("dark", "en"), ("light", "en")]

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)


def start_xvfb(display, sizes):
    num = display.lstrip(":")
    if Path(f"/tmp/.X11-unix/X{num}").exists():
        raise SystemExit(f"display {display} is in use; pick another with --display")
    w = max(w for w, _ in sizes) + shoot.PARK_MARGIN
    h = max(h for _, h in sizes) + shoot.PARK_MARGIN
    xvfb = subprocess.Popen(["Xvfb", display, "-screen", "0", f"{w}x{h}x24", "-nolisten", "tcp"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            start_new_session=True)
    for _ in range(50):
        if Path(f"/tmp/.X11-unix/X{num}").exists():
            break
        time.sleep(0.1)
    else:
        raise SystemExit("Xvfb did not start")
    for _ in range(50):
        try:
            return xvfb, shoot.Input(display, window="^Superba$")
        except SystemExit:
            time.sleep(0.1)
    return xvfb, shoot.Input(display, window="^Superba$")


def env(display, home):
    return {"PATH": "/usr/local/bin:/usr/bin:/bin", "HOME": str(home), "LANG": "C.UTF-8",
            "DISPLAY": display, "XDG_RUNTIME_DIR": str(home), "NO_COLOR": "1"}


def shot(a, inp, home, name, size, theme, lang, extra=(), after=None):
    out = a.out / f"{a.prefix}{name}.png"
    argv = [str(a.bin), "--size", f"{size[0]}x{size[1]}", "--theme", theme, "--lang", lang,
            *extra]
    gui = subprocess.Popen(argv, env=env(a.display, home), cwd=home,
                           stdout=subprocess.DEVNULL, stderr=open(home / "gui.err", "a"),
                           start_new_session=True)
    try:
        time.sleep(a.wait)
        inp.park()
        if after:
            after(inp, size)
            inp.park()
        time.sleep(0.8)
        w, h = size
        subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}", str(out)],
                       env=env(a.display, home), check=True)
        shoot.log(str(out))
    finally:
        os.killpg(gui.pid, signal.SIGTERM)
        try:
            gui.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(gui.pid, signal.SIGKILL)


def scroll_body(inp, size):
    """Wheel down over the middle of the dialog until its body ends."""
    w, h = size
    inp.scroll(w // 2, h // 2, 40)
    time.sleep(0.5)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", type=Path, required=True, help="the dialog_shots example binary")
    ap.add_argument("--display", default=":202")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--prefix", default="")
    ap.add_argument("--sizes", default="1280x800,960x640")
    ap.add_argument("--wait", type=float, default=3.0)
    ap.add_argument("--scroll", action="store_true")
    ap.add_argument("--expand", action="store_true")
    ap.add_argument("--preview", action="store_true")
    a = ap.parse_args()
    a.bin = a.bin.resolve()
    a.out = a.out.resolve()
    a.out.mkdir(parents=True, exist_ok=True)
    sizes = [tuple(int(v) for v in s.split("x")) for s in a.sizes.split(",")]
    for tool in ("Xvfb", "xdotool", "scrot"):
        if shutil.which(tool) is None:
            raise SystemExit(f"{tool} not found")
    home = Path(os.environ.get("TMPDIR", "/tmp")) / f"dialog-shots-{os.getpid()}"
    home.mkdir(parents=True)
    xvfb, inp = start_xvfb(a.display, sizes)
    try:
        inp.park()
        for size in sizes:
            for theme, lang in VARIANTS:
                tail = f"{size[0]}x{size[1]}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                shot(a, inp, home, f"router-off-{tail}", size, theme, lang)
                if a.scroll:
                    shot(a, inp, home, f"router-off-scrolled-{tail}", size, theme, lang,
                         after=scroll_body)
                if a.expand:
                    shot(a, inp, home, f"router-off-expanded-{tail}", size, theme, lang,
                         extra=["--expand", "1"])
                if a.expand and a.scroll:
                    shot(a, inp, home, f"router-off-expanded-scrolled-{tail}", size, theme, lang,
                         extra=["--expand", "1"], after=scroll_body)
                if a.preview:
                    shot(a, inp, home, f"preview-{tail}", size, theme, lang,
                         extra=["--scenario", "preview"])
    finally:
        inp.close()
        os.killpg(xvfb.pid, signal.SIGTERM)
        time.sleep(0.3)
        shutil.rmtree(home, ignore_errors=True)


if __name__ == "__main__":
    main()
