#!/usr/bin/env python3
"""Screenshots of user bug 61 from the real `superba` and `spb` binaries: the
desktop app is running but was not opened by Superba (started like from the
Start menu or the Dock, without the debugging port), so a switch cannot be hot.

shoot.py's Session provisions an isolated HOME (throwaway CA, mock relay, the
Krill profile applied with a PAT) and opens the desktop app given with
--app-dir through `spb launch`. This script then quits that app and starts the
same copy directly, the way a user does, so `spb status` says "ChatGPT was not
opened from Superba".

Shots, at 1280x800 in light and dark, English and Chinese:

- `hot-off-overview-*`: the Overview: the To-do names it with the reopen, the
  "This computer" node has a warning marker;
- `hot-off-popover-*`: the provider and line popover (where a switch is
  chosen): why the switch will be a full restart, and the reopen.

Like shoot.py it re-runs itself in an unprivileged user + PID namespace and
refuses a display in use.

    python3 -I crates/superba-gui/tools/screenshots/hot_switch_shots.py \\
        --bin-dir <target>/debug --app-dir <extracted desktop app> \\
        --display :167 --out <dir> --work-dir <scratch dir>
"""

import argparse
import ctypes
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
NS_ENV = "SUPERBA_HOT_SHOTS_NS"
SIZE = (1280, 800)
VARIANTS = [("light", "en"), ("dark", "en"), ("light", "zh-CN"), ("dark", "zh-CN")]
# The provider node's chevron at 1280x800 (its title "Krill · Overseas" /
# "Krill · 海外" decides where it sits).
CHEVRON = {"en": (928, 185), "zh-CN": (923, 185)}

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    if shutil.which("unshare") is None:
        raise SystemExit("unshare (util-linux) not found")
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def become_subreaper():
    """Adopt the processes our children leave behind (the app `spb launch`
    detaches): in the PID namespace `unshare` is PID 1 and never reaps them, so
    a quit app would stay as zombies that look like a running app."""
    if sys.platform == "linux":
        ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0)  # PR_SET_CHILD_SUBREAPER


def reap():
    while True:
        try:
            pid, _ = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return


def reopen_outside_superba(s):
    """Quit the app `spb launch` opened and start the same copy directly: no
    debugging port, none of Superba's environment."""
    for pid in s.app_pids():
        shoot.kill(pid)
    time.sleep(3)
    for pid in s.app_pids():
        shoot.kill(pid, 9)
    time.sleep(1)
    reap()
    env = s.env()
    for k in ("SUPERBA_APP_PATH", "SUPERBA_APP_ARGS"):
        env.pop(k, None)
    exe = s.app_dir() / "ChatGPT"
    subprocess.Popen([str(exe), "--no-sandbox", "--disable-gpu", "--password-store=basic"],
                     env=env, cwd=s.home, stdout=subprocess.DEVNULL,
                     stderr=open(s.work / "app-direct.err", "w"), start_new_session=True)
    time.sleep(12)
    p = s.run([str(s.args.bin_dir / "spb"), "status"], check=False)
    shoot.log("status with the app opened directly:\n" + p.stdout.strip())


def open_popover(lang):
    def act(s):
        x, y = CHEVRON[lang]
        s.input.click(x, y)
        time.sleep(1.0)
        s.input.park()
    return act


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, required=True)
    ap.add_argument("--app-dir", type=Path, required=True)
    ap.add_argument("--display", default=":167")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--work-dir", type=Path, help="scratch space (default: the system temp directory)")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--only", help="comma-separated shot name prefixes")
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
    become_subreaper()

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
        reopen_outside_superba(s)
        s.check_paths()
        only = [p for p in (a.only or "").split(",") if p]
        for theme, lang in VARIANTS:
            short = "zh" if lang == "zh-CN" else "en"
            for stem, act in (("hot-off-overview", None), ("hot-off-popover", open_popover(lang))):
                name = f"{stem}-{theme}-{short}"
                if only and not any(name.startswith(p) for p in only):
                    continue
                s.shot(name, "overview", theme, lang, 8.0, act, SIZE)
    finally:
        s.close()


if __name__ == "__main__":
    main()
