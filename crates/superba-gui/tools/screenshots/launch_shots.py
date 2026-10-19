#!/usr/bin/env python3
"""Screenshots and a real-binary check of GH#7 (user bug 66): opening ChatGPT
through Superba from the Overview, the command palette, a second start
(`superba --launch-chatgpt`, what the Windows "ChatGPT (Superba)" shortcut and
the Linux desktop action run) and the start-up preference.

shoot.py's Session provisions an isolated HOME (throwaway CA, mock relay, the
Krill profile applied with a PAT) and opens the desktop app given with
--app-dir through `spb launch`. Then, with the real `superba` and `spb`:

1. ChatGPT not running: `launch-open-overview-*` (the hero's "Open ChatGPT with
   Superba"), `launch-palette-*` (the palette, "chatgpt" typed). Check: the
   hero's button opens ChatGPT through Superba (`spb status`: launched by
   Superba, hot switch ready) and goes away (`launch-opened-overview-light-en`).
2. ChatGPT started directly (no debugging port): `launch-reopen-overview-*` and
   `launch-reopen-confirm-*` (the button's confirmation). Check: with a Superba
   window open, `superba --launch-chatgpt` exits at once, opens no second
   window, and the open window asks to confirm the reopen
   (`launch-forwarded-confirm-light-en`); confirming reopens ChatGPT through
   Superba.
3. No Superba window: `superba --launch-chatgpt` opens ChatGPT through
   Superba and shows no window.
4. `launch-prefs-*`: Preferences with "Open ChatGPT through Superba when
   Superba starts".

Light and dark, English and Chinese, 1280x800. Like shoot.py it re-runs itself
in an unprivileged user + PID namespace and refuses a display in use.

    python3 -I crates/superba-gui/tools/screenshots/launch_shots.py \\
        --bin-dir <target>/debug --app-dir <extracted desktop app> \\
        --display :168 --out <dir> --work-dir <scratch dir>
"""

import argparse
import ctypes
import importlib.util
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
NS_ENV = "SUPERBA_LAUNCH_SHOTS_NS"
SIZE = (1280, 800)
VARIANTS = [("light", "en"), ("dark", "en"), ("light", "zh-CN"), ("dark", "zh-CN")]
# The hero's action at 1280x800: right-aligned at x = 1228, centred on y = 108;
# 70 px left of the right edge is inside it in both languages.
ACTION = (1158, 108)
FAILURES = []

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)

spec2 = importlib.util.spec_from_file_location("hot", HERE / "hot_switch_shots.py")
hot = importlib.util.module_from_spec(spec2)
spec2.loader.exec_module(hot)


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    if shutil.which("unshare") is None:
        raise SystemExit("unshare (util-linux) not found")
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def check(ok, what):
    shoot.log(("PASS " if ok else "FAIL ") + what)
    if not ok:
        FAILURES.append(what)


def status(s):
    p = s.run([str(s.args.bin_dir / "spb"), "status"], check=False)
    return p.stdout


def launched_by_superba(s):
    out = status(s)
    line = next((l for l in out.splitlines() if "Launched by Superba" in l), "")
    return "yes" in line.lower() or "true" in line.lower(), out


def quit_app(s):
    for pid in s.app_pids():
        shoot.kill(pid)
    time.sleep(3)
    for pid in s.app_pids():
        shoot.kill(pid, signal.SIGKILL)
    time.sleep(1)
    hot.reap()


def superba_windows(s):
    p = subprocess.run(["xdotool", "search", "--onlyvisible", "--name", "^Superba$"],
                       env=s.env(), capture_output=True, text=True)
    return [w for w in p.stdout.split() if w]


def gui(s, page="overview", theme="light", lang="en"):
    w, h = SIZE
    return subprocess.Popen(
        [str(s.args.bin_dir / "superba"), "--size", f"{w}x{h}", "--theme", theme,
         "--lang", lang, "--page", page],
        env=s.env(), cwd=s.home, stdout=subprocess.DEVNULL,
        stderr=open(s.work / "gui.err", "a"), start_new_session=True)


def stop(p):
    shoot.kill(p.pid)
    try:
        p.wait(timeout=5)
    except subprocess.TimeoutExpired:
        shoot.kill(p.pid, signal.SIGKILL)


def grab(s, name):
    w, h = SIZE
    out = s.args.out / f"{name}.png"
    subprocess.run(["scrot", "-o", "-a", f"0,0,{w},{h}", str(out)], env=s.env(), check=True)
    shoot.log(str(out))


def click_action(s):
    x, y = ACTION
    s.input.click(x, y)
    time.sleep(3.0)
    s.input.park()


def palette(s):
    s.input.focus()
    s.input.key("ctrl+k")
    time.sleep(0.6)
    s.input.type("chatgpt")
    time.sleep(0.8)


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, required=True)
    ap.add_argument("--app-dir", type=Path, required=True)
    ap.add_argument("--display", default=":168")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--work-dir", type=Path, help="scratch space (default: the system temp directory)")
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    a.bin_dir = a.bin_dir.resolve()
    a.out = a.out.resolve()
    work = (a.work_dir or Path(tempfile.gettempdir())).resolve()
    work.mkdir(parents=True, exist_ok=True)
    a.out.mkdir(parents=True, exist_ok=True)
    for tool in ("Xvfb", "xdotool", "scrot", "openssl"):
        if shutil.which(tool) is None:
            raise SystemExit(f"{tool} not found")
    enter_namespace()
    hot.become_subreaper()

    real_mkdtemp = tempfile.mkdtemp
    shoot.tempfile = SimpleNamespace(
        mkdtemp=lambda prefix="", dir=None: real_mkdtemp(prefix=prefix, dir=str(work)))
    args = SimpleNamespace(display=a.display, sizes=[SIZE], app_dir=a.app_dir.resolve(),
                           no_router_log=False, rollouts=False, router_on=False,
                           bin_dir=a.bin_dir, out=a.out, keep=a.keep)
    s = shoot.Session(args)
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.copy_app()
        s.provision()
        s.check_paths()

        # 1. Not running.
        quit_app(s)
        shoot.log("status with the app quit:\n" + status(s).strip())
        for theme, lang in VARIANTS:
            short = "zh" if lang == "zh-CN" else "en"
            s.shot(f"launch-open-overview-{theme}-{short}", "overview", theme, lang, 8.0, None, SIZE)
            s.shot(f"launch-palette-{theme}-{short}", "overview", theme, lang, 8.0, palette, SIZE)
        g = gui(s)
        try:
            time.sleep(8)
            s.input.park()
            click_action(s)
            time.sleep(15)
            ok, out = launched_by_superba(s)
            check(ok, "the hero's button opened ChatGPT through Superba")
            check("Hot switch ready" in out or "hot switch: ready" in out.lower()
                  or "ready" in out.lower(), "hot switching is available afterwards")
            time.sleep(6)
            s.input.park()
            grab(s, "launch-opened-overview-light-en")
        finally:
            stop(g)
        shoot.log("status after the hero's button:\n" + status(s).strip())

        # 2. Started directly.
        hot.reopen_outside_superba(s)
        ok, _ = launched_by_superba(s)
        check(not ok, "the directly started ChatGPT is not Superba's")
        for theme, lang in VARIANTS:
            short = "zh" if lang == "zh-CN" else "en"
            s.shot(f"launch-reopen-overview-{theme}-{short}", "overview", theme, lang, 8.0, None, SIZE)
            s.shot(f"launch-reopen-confirm-{theme}-{short}", "overview", theme, lang, 8.0,
                   click_action, SIZE)
        g = gui(s)
        try:
            time.sleep(8)
            s.input.park()
            before = superba_windows(s)
            t0 = time.monotonic()
            p = subprocess.run([str(s.args.bin_dir / "superba"), "--launch-chatgpt"],
                               env=s.env(), cwd=s.home, capture_output=True, text=True, timeout=60)
            took = time.monotonic() - t0
            check(p.returncode == 0 and took < 5,
                  f"a second start hands the request over and exits (exit {p.returncode}, {took:.1f} s)")
            time.sleep(4)
            after = superba_windows(s)
            check(len(after) == len(before) == 1, f"no second Superba window ({before} -> {after})")
            s.input.park()
            grab(s, "launch-forwarded-confirm-light-en")
            # Confirm: the dialog's primary button sits left in its button row;
            # Enter is not bound to it, so press it through the keyboard focus.
            s.input.focus()
            s.input.key("Tab")
            time.sleep(0.3)
            s.input.key("space")
            time.sleep(25)
            ok, out = launched_by_superba(s)
            check(ok, "confirming the reopen opened ChatGPT through Superba")
            s.input.park()
            grab(s, "launch-forwarded-reopened-light-en")
        finally:
            stop(g)

        # 3. No Superba window: no window opens.
        quit_app(s)
        before = superba_windows(s)
        p = subprocess.Popen([str(s.args.bin_dir / "superba"), "--launch-chatgpt"],
                             env=s.env(), cwd=s.home, stdout=subprocess.DEVNULL,
                             stderr=open(s.work / "launch.err", "w"), start_new_session=True)
        seen = []
        for _ in range(60):
            seen.extend(w for w in superba_windows(s) if w not in seen)
            if p.poll() is not None:
                break
            time.sleep(0.5)
        check(p.poll() == 0, f"superba --launch-chatgpt without a window exits 0 ({p.poll()})")
        check(not seen and not before, f"and shows no Superba window ({seen})")
        time.sleep(3)
        ok, out = launched_by_superba(s)
        check(ok, "ChatGPT was opened through Superba")
        shoot.log("status after superba --launch-chatgpt:\n" + out.strip())

        # 4. Preferences.
        for theme, lang in VARIANTS:
            short = "zh" if lang == "zh-CN" else "en"
            s.shot(f"launch-prefs-{theme}-{short}", "general", theme, lang, 6.0, None, SIZE)
    finally:
        s.close()
    if FAILURES:
        shoot.log("FAILED: " + "; ".join(FAILURES))
        raise SystemExit(1)
    shoot.log("all checks passed")


if __name__ == "__main__":
    main()
