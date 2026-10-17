#!/usr/bin/env python3
"""Screenshots of the Router page once Krill is a member (user bug 39), from the
real `superba` binary.

The 0.1.1-alpha.1 user on Windows had Krill (API key, Auto auth) as the router's
only member and the router answering, yet the page kept 开启 disabled under
"开启路由需要至少一个用 API key 的成员。Krill：可以作为成员（使用 API key）" with
"添加 Krill 为成员": the gate read the window's copy of the profile set, loaded
before Krill became a member. The shots:

- router-gate-added-live-*: the window is open without members (the panel
  rightly offers 添加 Krill 为成员), then `spb router install --member krill`
  makes Krill the member and starts the router while the window stays open;
  within a poll the page lists Krill, the panel is gone and both 开启 are
  enabled.
- router-gate-member-*: a fresh window with that state.

Krill is provisioned as the user had it: Auto auth (applied direct with a PAT),
its key stored. Like router_shots.py it re-runs itself in an unprivileged user +
PID namespace with an isolated HOME under --work-dir; the router started by
`spb router install` lives in that namespace and ends with it.

    python3 -I crates/superba-gui/tools/screenshots/router_gate_shots.py \\
        --bin-dir target/debug --display :169 --out .private/v3/fix-router-gate \\
        --work-dir /var/tmp
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
NS_ENV = "SUPERBA_ROUTER_GATE_SHOTS_NS"
SIZE = (1280, 800)

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


def provision(s):
    """Krill with its key and Auto auth, applied direct (Auto picks the PAT)."""
    spb = str(s.args.bin_dir / "spb")
    s.run([spb, "profile", "add", "krill", "--name", "Krill", "--site", "krill-local",
           "--line", "overseas", "--auth", "auto", "--key-stdin"], stdin=shoot.KEY + "\n")
    s.run([spb, "profile", "use", "krill", "--yes", "--countdown", "0"], check=False)
    shoot.log("status after setup:\n" + s.run([spb, "status"], check=False).stdout.strip())


def make_member(s):
    """Krill becomes the router's member and the router starts, from outside
    the open window (as `spb router enable --member` or an earlier session)."""
    p = s.run([str(s.args.bin_dir / "spb"), "router", "install", "--member", "krill"],
              check=False, timeout=120)
    shoot.log(f"spb router install rc={p.returncode}\n{p.stdout.strip()}\n{p.stderr.strip()}")
    if p.returncode != 0:
        raise SystemExit("spb router install failed")


def added_while_open(s):
    make_member(s)
    # The page polls its snapshot every 2 s; the router needs a moment to bind.
    time.sleep(6.0)
    s.input.park()


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/debug")
    ap.add_argument("--display", default=":169")
    ap.add_argument("--out", type=Path, default=REPO / ".private/v3/fix-router-gate")
    ap.add_argument("--work-dir", type=Path, default=Path("/var/tmp"),
                    help="scratch space for the isolated HOME (default /var/tmp)")
    ap.add_argument("--keep", action="store_true")
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
    # The router service starts the program next to spb: copy the binaries into
    # the scratch space so no build path appears anywhere.
    for name in ("superba", "spb", "superba-router"):
        shutil.copy2(a.bin_dir / name, s.bin / name)
    args.bin_dir = s.bin
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        provision(s)
        s.check_paths()
        s.shot("router-gate-added-live-light-zh", "router", "light", "zh-CN", 8.0,
               added_while_open, SIZE)
        s.shot("router-gate-member-light-zh", "router", "light", "zh-CN", 8.0, None, SIZE)
        spb = str(s.bin / "spb")
        shoot.log(s.run([spb, "router", "status"], check=False).stdout.strip())
        p = s.run([spb, "--locale", "zh-CN", "router", "doctor"], check=False)
        (a.out / "router-gate-doctor.txt").write_text(p.stdout.replace(str(s.work), "<work>"))
    finally:
        s.close()


if __name__ == "__main__":
    main()
