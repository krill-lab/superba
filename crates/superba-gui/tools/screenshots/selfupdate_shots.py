#!/usr/bin/env python3
"""Screenshots of Superba's own update UI, from the real `superba` binary.

Shows Superba → Updates in three states (up to date, update
available, the release page cannot be read) and the Overview to-do item, at
1280x800 in light and dark, English and Chinese.

It reuses shoot.py's Session (isolated HOME, throwaway CA, mock relay, Xvfb,
XTEST pointer) and adds a tiny loopback server that plays the GitHub release
API for `SUPERBA_SELF_UPDATE_API` (http on loopback is accepted by the same
rule as the other overrides). Nothing is installed: the buttons are not
clicked. Like shoot.py it re-runs itself in an unprivileged user + PID
namespace so no desktop app or Codex process of the real user is visible, and
the scratch HOME lives under --work-dir (default: the system temp directory, like
shoot.py: Superba's pictures must not show a path under /data/ or the real home).

    python3 -I crates/superba-gui/tools/screenshots/selfupdate_shots.py \\
        --bin-dir target/debug --display :176 --out .private/selfupdate
"""

import argparse
import http.server
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import threading
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
NS_ENV = "SUPERBA_SELFUPDATE_SHOTS_NS"
SIZE = (1280, 800)
VARIANTS = [("light", "en"), ("dark", "en"), ("light", "zh-CN"), ("dark", "zh-CN")]
STATES = ["uptodate", "available", "unavailable"]

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)

NOTES = """## What's new

- Superba can check for its own updates and install them after you confirm.
- **Faster hot switch** when the local router is on.
- Fixes a crash when `~/.codex` is a symlink ([#41](https://example.invalid/41)).

Thanks to everyone who reported problems!
"""


def release(version, notes):
    assets = [{"name": "SHA256SUMS.txt", "size": 400}]
    for name in (f"superba-{version}-macos-universal.zip",
                 f"superba-{version}-windows-x86_64.msi",
                 f"superba-{version}-linux-x86_64.tar.gz",
                 f"superba-{version}-linux-aarch64.tar.gz",
                 f"spb-{version}-linux-x86_64-musl.tar.gz"):
        assets.append({"name": name, "size": 21_000_000})
    return {"tag_name": f"v{version}", "draft": False, "prerelease": False, "body": notes,
            "published_at": "2026-10-17T08:00:00Z", "assets": assets}


class Api(http.server.BaseHTTPRequestHandler):
    """The GitHub release API for one state."""
    state = "uptodate"

    def do_GET(self):  # noqa: N802
        path = self.path.split("?")[0]
        if self.state == "unavailable":
            body, code = b'{"message":"Not Found"}', 404
        elif path == "/releases/latest":
            version = "0.2.0" if self.state == "available" else "0.1.0"
            body, code = json.dumps(release(version, NOTES)).encode(), 200
        else:
            body, code = b"[]", 200
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def start_api(state):
    handler = type(f"Api_{state}", (Api,), {"state": state})
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


class UpdateSession(shoot.Session):
    """shoot.Session with the self-update API of the current state in its env."""
    api_port = None

    def env(self, **extra):
        if self.api_port:
            extra.setdefault("SUPERBA_SELF_UPDATE_API", f"http://127.0.0.1:{self.api_port}")
            extra.setdefault("SUPERBA_SELF_UPDATE_DOWNLOADS", f"http://127.0.0.1:{self.api_port}/dl")
        return super().env(**extra)


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
    ap.add_argument("--display", default=":176")
    ap.add_argument("--out", type=Path, default=REPO / ".private/selfupdate")
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

    real_mkdtemp = tempfile.mkdtemp
    shoot.tempfile = SimpleNamespace(
        mkdtemp=lambda prefix="", dir=None: real_mkdtemp(prefix=prefix, dir=str(work)))
    args = SimpleNamespace(display=a.display, sizes=[SIZE], app_dir=None, no_router_log=False,
                           rollouts=False, router_on=False, bin_dir=a.bin_dir, out=a.out,
                           keep=a.keep)
    servers = {state: start_api(state) for state in STATES}
    s = UpdateSession(args)
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        s.check_paths()
        only = [p for p in (a.only or "").split(",") if p]
        plan = [(f"updates-{state}", "updates", state) for state in STATES]
        plan.append(("overview-todo", "overview", "available"))
        for stem, page, state in plan:
            s.api_port = servers[state].server_address[1]
            for theme, lang in VARIANTS:
                name = f"{stem}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                if only and not any(name.startswith(p) for p in only):
                    continue
                s.shot(name, page, theme, lang, 9.0, None, SIZE)
    finally:
        s.close()
        for server in servers.values():
            server.shutdown()


if __name__ == "__main__":
    main()
