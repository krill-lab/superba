#!/usr/bin/env python3
"""Screenshots of the Router page when no member can carry traffic (user bug 28)
and of its 诊断 / Diagnostics dialog, from the real `superba` binary.

shoot.py's Session provisions what the 0.1.0 user had: a Krill profile set to
PAT only (`--auth pat`) and a custom site without a key, no router member. The
Router page then shows 开启 disabled, the reason under the banner and the fixes
("为 Krill 填写 API key", ...). The diagnostics dialog is opened through the
command palette ("doctor") and shows the real `router_doctor` report of that
HOME. Nothing is installed and no button is clicked.

Like shoot.py it re-runs itself in an unprivileged user + PID namespace, with an
isolated HOME under --work-dir (default /var/tmp, never the real home or /data).

    python3 -I crates/superba-gui/tools/screenshots/router_shots.py \\
        --bin-dir target/debug --display :175 --out .private/v3/router-bug28
"""

import argparse
import importlib.util
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
NS_ENV = "SUPERBA_ROUTER_SHOTS_NS"
SIZE = (1280, 800)
VARIANTS = [("light", "zh-CN"), ("dark", "zh-CN"), ("light", "en"), ("dark", "en")]

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)


def doctor_action(s):
    """Open 诊断 from the palette and wait for the report."""
    shoot.palette_action("doctor")(s)
    s.input.key("Return")
    time.sleep(4.0)
    s.input.park()


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    if shutil.which("unshare") is None:
        raise SystemExit("unshare (util-linux) not found")
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/debug")
    ap.add_argument("--display", default=":175")
    ap.add_argument("--out", type=Path, default=REPO / ".private/v3/router-bug28")
    ap.add_argument("--work-dir", type=Path, default=Path("/var/tmp"),
                    help="scratch space for the isolated HOME (default /var/tmp)")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--only", help="comma-separated shot name prefixes")
    a = ap.parse_args()
    a.bin_dir = a.bin_dir.resolve()
    a.out = a.out.resolve()
    work = a.work_dir.resolve()
    work.mkdir(parents=True, exist_ok=True)
    a.out.mkdir(parents=True, exist_ok=True)
    for tool in ("Xvfb", "xdotool", "scrot", "openssl"):
        if shutil.which(tool) is None:
            raise SystemExit(f"{tool} not found")
    enter_namespace()

    real_mkdtemp = tempfile.mkdtemp
    shoot.tempfile = SimpleNamespace(
        mkdtemp=lambda prefix="", dir=None: real_mkdtemp(prefix=prefix, dir=str(work)))
    args = SimpleNamespace(display=a.display, sizes=[SIZE], app_dir=None, no_router_log=False,
                           rollouts=False, router_on=False, bin_dir=a.bin_dir, out=a.out,
                           keep=a.keep)
    s = shoot.Session(args)
    # The doctor names the router program: copy the binaries next to each other
    # in the scratch space so no build path appears in the pictures.
    for name in ("superba", "spb", "superba-router"):
        shutil.copy2(a.bin_dir / name, s.bin / name)
    args.bin_dir = s.bin
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        s.check_paths()
        only = [p for p in (a.only or "").split(",") if p]
        for theme, lang in VARIANTS:
            tail = f"{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
            for name, action in ((f"router-no-member-{tail}", None),
                                 (f"router-doctor-{tail}", doctor_action)):
                if only and not any(name.startswith(p) for p in only):
                    continue
                s.shot(name, "router", theme, lang, 7.0, action, SIZE)
    finally:
        s.close()


if __name__ == "__main__":
    main()
