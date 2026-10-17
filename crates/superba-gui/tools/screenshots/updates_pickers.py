#!/usr/bin/env python3
"""Screenshots of the Updates page's version pickers (pre-release, bundled and
current tags; the "Include pre-releases" box in front of its dropdown).

Reuses shoot.py's Session (isolated HOME under /tmp, throwaway CA, local mock
relay, provisioning) and adds, inside that HOME only:

- a mock ChatGPT desktop app directory found through SUPERBA_APP_PATH. It is
  never started; it bundles Codex 0.163.0-alpha.10.2 (codex-package.json);
- a standalone Codex CLI 0.163.0 (~/.codex/packages/standalone, linked from
  ~/.local/bin, first on PATH) and an npm one (0.162.0);
- two remote hosts the app knows (~/.codex/.codex-global-state.json) with a
  cached probe: devbox on 0.162.0, gpu-01 on 0.165.0-alpha.3;
- a mock Codex release feed with pre-releases (alpha, beta, rc, numeric parts)
  written as Superba's fresh release cache, with online checks off so nothing
  real is fetched.

Every shot is 1280x800, light and dark, English and Chinese. Clicks come from
updates_pickers.json (window coordinates per language, found with --probe).
The script re-runs itself in an unprivileged user + PID namespace
(unshare --user --map-current-user --pid --fork --mount-proc) and uses display
:153 unless --display names another; processes are stopped by PID.

    python3 -I updates_pickers.py --bin-dir DIR --out DIR [--only a,b] [--probe]
"""

import argparse
import importlib.util
import json
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
NS_ENV = "SUPERBA_PICKERS_NS"
DISPLAY = ":153"
SIZE = (1280, 800)
VARIANTS = [("light", "zh-CN"), ("dark", "zh-CN"), ("light", "en"), ("dark", "en")]

# The mock release feed, newest first is not required (Superba sorts).
RELEASES = [
    ("0.150.1", False),
    ("0.160.0", False),
    ("0.162.0-alpha.17.2", True),
    ("0.162.0", False),
    ("0.163.0-alpha.10.2", True),
    ("0.163.0", False),
    ("0.164.0", False),
    ("0.165.0-alpha.3", True),
    ("0.165.0-alpha.12", True),
    ("0.165.0-beta.2", True),
    ("0.165.0-rc.1", True),
    ("0.165.0", False),
    ("0.166.0-alpha.1", True),
    ("0.166.0-alpha.2", True),
]
BUNDLED = "0.163.0-alpha.10.2"
STANDALONE = "0.163.0"
NPM = "0.162.0"
HOSTS = [
    ("devbox-7c1e", "devbox", "devbox.invalid", "0.162.0"),
    ("gpu-01-3f2a", "gpu-01", "gpu-01.invalid", "0.165.0-alpha.3"),
]


def load_shoot():
    spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def enter_namespace():
    if os.environ.get(NS_ENV) == "1":
        return
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def script(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/bin/sh\n{text}\n")
    path.chmod(0o755)


def mock_world(s):
    """The mock app, CLIs, hosts and release feed in the session's HOME."""
    home = s.home
    data = home / ".local/share/superba"
    # Desktop app (never started).
    app = s.work / "ChatGPT"
    res = app / "resources"
    script(app / "ChatGPT", "exit 0")
    res.mkdir(parents=True, exist_ok=True)
    (res / "app.asar").write_bytes(b"mock")
    (res / "linux-package-metadata.json").write_text(json.dumps({"version": "26.1013.2"}))
    (res / "codex-cli").mkdir(parents=True, exist_ok=True)
    (res / "codex-cli/codex-package.json").write_text(
        json.dumps({"version": BUNDLED, "entrypoint": "bin/codex"}))
    script(res / "codex-cli/bin/codex", f'echo "codex-cli {BUNDLED}"')
    # Standalone CLI, first on PATH.
    std = home / ".codex/packages/standalone"
    rel = std / f"releases/{STANDALONE}-x86_64-unknown-linux-musl"
    script(rel / "bin/codex", f'echo "codex-cli {STANDALONE}"')
    (std / "current").symlink_to(rel.relative_to(std))
    (home / ".local/bin").mkdir(parents=True, exist_ok=True)
    (home / ".local/bin/codex").symlink_to(std / "current/bin/codex")
    # npm CLI.
    js = home / ".npm-global/lib/node_modules/@openai/codex/bin/codex.js"
    script(js, f'echo "codex-cli {NPM}"')
    (home / ".npm-global/bin").mkdir(parents=True, exist_ok=True)
    (home / ".npm-global/bin/codex").symlink_to(js)
    # Remote hosts the app knows, with a cached probe each.
    state = {"codex-managed-remote-connections": [
        {"hostId": hid, "hostname": host, "displayName": label, "sshUser": "dev"}
        for hid, label, host, _ in HOSTS]}
    (home / ".codex").mkdir(parents=True, exist_ok=True)
    (home / ".codex/.codex-global-state.json").write_text(json.dumps(state, indent=1))
    now = int(time.time() * 1000)
    cache = {f"ssh:{hid}": {"state": {
        "target_id": f"ssh:{hid}", "probed_at": now - 600_000, "os": "linux",
        "shell": "bash", "codex_home": "/home/dev/.codex",
        "codex_path": "/home/dev/.local/bin/codex", "codex_version": ver,
        "auth": {"kind": "missing"}, "arch": "x86_64", "codex_install": "standalone",
    }} for hid, _, _, ver in HOSTS}
    (data / "cache").mkdir(parents=True, exist_ok=True)
    (data / "cache/hosts.json").write_text(json.dumps(cache, indent=1))
    # Release feed as a fresh cache; online checks off.
    releases = [{"version": v, "tag": f"rust-v{v}", "prerelease": pre} for v, pre in RELEASES]
    (data / "codex").mkdir(parents=True, exist_ok=True)
    (data / "codex/releases.json").write_text(json.dumps({"checked_at": now, "data": releases}))
    profiles = data / "profiles.json"
    doc = json.loads(profiles.read_text())
    doc.setdefault("settings", {}).setdefault("network", {})["online_checks"] = False
    profiles.write_text(json.dumps(doc, indent=2))


def act(s, actions):
    for a in actions:
        kind = a[0]
        # Pointer input goes through shoot.py's XTEST helper (xdotool's
        # mousemove does not move the pointer on Xvfb).
        if kind == "click":
            s.input.click(a[1], a[2])
            time.sleep(0.6)
        elif kind == "move":
            s.input.move(a[1], a[2])
            time.sleep(0.3)
        elif kind == "park":
            s.input.park()
            time.sleep(0.3)
        elif kind == "scroll":
            # Mouse wheel down `a[3]` notches at (a[1], a[2]).
            s.input.scroll(a[1], a[2], a[3])
            time.sleep(0.6)
        elif kind == "sleep":
            time.sleep(a[1])


def shots(coords):
    """(name, theme, lang, actions) of every shot."""
    out = []
    for theme, lang in VARIANTS:
        c = coords[lang]
        tag = f"{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
        park = ["park"]
        out += [
            (f"pickers-closed-hidden-{tag}", theme, lang, []),
            (f"pin-open-hidden-{tag}", theme, lang, [["click", *c["pin_field"]]]),
            (f"pin-open-shown-{tag}", theme, lang,
             [["click", *c["pin_box"]], ["click", *c["pin_field"]]]),
            # Further down the open menu: the bundled version ("内置"), which the
            # app uses now ("当前").
            (f"pin-open-shown-scrolled-{tag}", theme, lang,
             [["click", *c["pin_box"]], ["click", *c["pin_field"]],
              ["scroll", *c["pin_menu"], 3]]),
            (f"pin-closed-shown-{tag}", theme, lang,
             [["click", *c["pin_box"]], ["click", *c["pin_field"]],
              ["click", *c["pin_first_row"]], park]),
            # The CLI and remote pickers are shot with the page scrolled to its end.
            (f"cli-open-hidden-{tag}", theme, lang,
             [c["scroll"], ["click", *c["cli_field"]]]),
            (f"cli-open-shown-{tag}", theme, lang,
             [c["scroll"], ["click", *c["cli_box"]], ["click", *c["cli_field_shown"]]]),
            (f"cli-open-shown-scrolled-{tag}", theme, lang,
             [c["scroll"], ["click", *c["cli_box"]], ["click", *c["cli_field_shown"]],
              ["scroll", *c["cli_menu"], 3]]),
            (f"cli-closed-shown-{tag}", theme, lang,
             [c["scroll"], ["click", *c["cli_box"]], park]),
            (f"remote-closed-hidden-{tag}", theme, lang, [c["scroll"], park]),
            (f"remote-open-hidden-{tag}", theme, lang,
             [c["scroll"], ["click", *c["remote_field"]]]),
            (f"remote-open-shown-{tag}", theme, lang,
             [c["scroll"], ["click", *c["remote_box"]], ["click", *c["remote_field_shown"]]]),
            (f"remote-closed-shown-{tag}", theme, lang,
             [c["scroll"], ["click", *c["remote_box"]], park]),
        ]
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--coords", type=Path, default=HERE / "updates_pickers.json")
    ap.add_argument("--only", default="", help="comma-separated shot name prefixes")
    ap.add_argument("--probe", action="store_true",
                    help="only the page as opened and scrolled (to find coordinates)")
    ap.add_argument("--wait", type=float, default=7.0)
    ap.add_argument("--display", default=DISPLAY)
    args = ap.parse_args()
    args.bin_dir = args.bin_dir.resolve()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    args.app_dir = None
    args.keep = False
    args.sizes = [SIZE]
    # shoot.py options this script leaves at their defaults.
    for name in ("no_router_log", "rollouts", "router_on"):
        vars(args).setdefault(name, False)
    enter_namespace()
    shoot = load_shoot()

    class Session(shoot.Session):
        def env(self, **extra):
            env = super().env(**extra)
            env["PATH"] = f"{self.home / '.local/bin'}:{env['PATH']}"
            env["SUPERBA_APP_PATH"] = str(self.work / "ChatGPT")
            env.update(extra)
            return env

    if args.probe:
        plan = []
        for lang in ("zh-CN", "en"):
            short = "zh" if lang == "zh-CN" else "en"
            plan.append((f"probe-top-{short}", "light", lang, []))
            plan.append((f"probe-scrolled-{short}", "light", lang,
                         [["scroll", 760, 500, 10], ["park"]]))
    else:
        plan = shots(json.loads(args.coords.read_text()))
    only = [p for p in args.only.split(",") if p]
    s = Session(args)
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        mock_world(s)
        s.check_paths()
        for name, theme, lang, actions in plan:
            if only and not any(name.startswith(p) for p in only):
                continue
            s.shot(name, "updates", theme, lang, args.wait,
                   (lambda acts: (lambda sess: act(sess, acts)))(actions), SIZE)
    finally:
        s.close()


if __name__ == "__main__":
    main()
