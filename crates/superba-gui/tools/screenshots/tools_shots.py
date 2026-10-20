#!/usr/bin/env python3
"""Capture the Tools page with different rows selected, from the real binaries.

Uses shoot.py's isolated HOME, PID namespace, mock relay and XTEST driver. Two
stand-in programs on the session's PATH make opencode and Gemini CLI look
installed (they only answer `--version`); pi is applied through `spb harness
apply` so one row shows an applied tool with its files; WorkBuddy stays not
installed. Nothing outside the throwaway HOME is read or written.

    python3 -I crates/superba-gui/tools/screenshots/tools_shots.py \\
        --bin-dir target/debug --out /tmp/tools-shots [--display :205] \\
        [--sizes 1280x800,960x640] [--only codex,opencode] [--variants light-zh,dark-en]

Shots are named tools-<row>-<theme>-<lang>-<W>x<H>.png; `models` is opencode
with its model chooser open.
"""

import argparse
import importlib.util
import os
import sys
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)

# Rows of the list, top to bottom (Up/Down move the selection).
ROWS = ["codex", "claude", "dsh", "opencode", "pi", "zcode", "workbuddy", "gemini"]
# Stand-ins for installed programs: name -> what `--version` prints.
INSTALLED = {"opencode": "2.0.32", "gemini": "0.9.0"}


def select(row, chooser=False):
    """Click the Codex row (it takes keyboard focus), then walk down to `row`."""
    def act(s):
        w, _ = s.size
        # The first row of the list, right of the sidebar (shoot.py's Linux window).
        x = 300 if w < 1100 else 320
        # Window focus first: focusing the window again would clear the row's focus.
        s.input.focus()
        s.input.click(x, 179)
        time.sleep(0.4)
        steps = ROWS.index(row)
        if steps:
            s.input.xdo("key", "--delay", "150", *["Down"] * steps)
            # A pointer press hides the keyboard focus ring (the page title is inert).
            time.sleep(0.4)
            s.input.click(x - 40, 66)
        if chooser:
            # "Choose…" of "Models written": the last row of the Models section.
            s.input.click(*s.chooser)
            time.sleep(0.6)
        s.input.park()
        time.sleep(0.8)
    return act


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--display", default=":205")
    ap.add_argument("--sizes", default="1280x800,960x640")
    ap.add_argument("--only", help="comma-separated rows (codex, opencode, gemini, workbuddy, pi, models)")
    ap.add_argument("--variants", default="light-zh,dark-en,light-en,dark-zh")
    args = ap.parse_args()
    if os.environ.get("SUPERBA_TOOLS_SHOTS_NS") != "1":
        os.execvpe("unshare", ["unshare", "--user", "--map-current-user", "--pid",
                   "--fork", "--mount-proc", sys.executable, "-I", str(Path(__file__).resolve()),
                   *sys.argv[1:]], {**os.environ, "SUPERBA_TOOLS_SHOTS_NS": "1"})
    sizes = [tuple(int(x) for x in v.split("x", 1)) for v in args.sizes.split(",") if v]
    variants = []
    for v in args.variants.split(","):
        theme, lang = v.split("-", 1)
        variants.append((theme, "zh-CN" if lang == "zh" else "en"))
    rows = (args.only or "codex,opencode,gemini,workbuddy,pi,models").split(",")
    args.bin_dir = args.bin_dir.resolve()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    opts = SimpleNamespace(display=args.display, sizes=sizes, app_dir=None,
                           no_router_log=False, rollouts=False, router_on=False,
                           bin_dir=args.bin_dir, out=args.out, keep=False)
    s = shoot.Session(opts)
    try:
        for name, version in INSTALLED.items():
            stub = s.bin / name
            stub.write_text(f"#!/bin/sh\necho {version}\n")
            stub.chmod(0o755)
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        p = s.run([str(args.bin_dir / "spb"), "harness", "apply", "pi", "--follow-codex",
                   "--yes"], check=False)
        shoot.log(f"apply pi: rc={p.returncode}")
        s.check_paths()
        for size in sizes:
            s.size = size
            for theme, lang in variants:
                # "Choose…" of "Models written" (opencode), measured on these sizes.
                s.chooser = (size[0] - (66 if size[0] >= 1100 else 56),
                             517 if size[0] >= 1100 else 534)
                tag = f"{theme}-{'zh' if lang == 'zh-CN' else 'en'}-{size[0]}x{size[1]}"
                for row in rows:
                    target = "opencode" if row == "models" else row
                    s.shot(f"tools-{row}-{tag}", "tools", theme, lang, 7.0,
                           select(target, chooser=row == "models"), size)
    finally:
        s.close()


if __name__ == "__main__":
    main()
