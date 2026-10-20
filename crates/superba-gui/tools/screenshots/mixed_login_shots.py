#!/usr/bin/env python3
"""Screenshots of mixed login (Providers > official ChatGPT login > "Use the
official account for remote features") from the real `superba` binary.

shoot.py's Session (isolated HOME, throwaway CA, mock relay, a Krill profile)
with router mode on. The official login is a fixture: an unsigned JWT with a
made-up account, written to the isolated `~/.codex/auth.json`; no real
ChatGPT account is used and nothing leaves the machine.

Shots (1280x800 and 960x640, light and dark, English and Chinese):

- `mixed-off-*`: the setting off;
- `mixed-nologin-*`: on, but no official login is saved;
- `mixed-pending-*`: on, a saved login, Codex still on the router token;
- `mixed-active-*`: after the switch: mixed login in effect.

    python3 -I crates/superba-gui/tools/screenshots/mixed_login_shots.py \\
        --bin-dir <target>/debug --display :171 --out <dir> --work-dir <scratch>
"""

import argparse
import base64
import importlib.util
import json
import os
import shutil
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
NS_ENV = "SUPERBA_MIXED_SHOTS_NS"
VARIANTS = [("light", "en"), ("dark", "en"), ("light", "zh-CN"), ("dark", "zh-CN")]

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def fake_login():
    """A ChatGPT login as Codex writes it, with fixture tokens only."""
    enc = lambda v: base64.urlsafe_b64encode(json.dumps(v).encode()).rstrip(b"=").decode()
    jwt = "{}.{}.c2ln".format(
        enc({"alg": "none"}),
        enc({"email": "me@example.com", "exp": 4102444800,
             "https://api.openai.com/auth": {"chatgpt_account_id": "acct-fixture",
                                             "chatgpt_user_id": "user-fixture",
                                             "chatgpt_plan_type": "pro"}}))
    return json.dumps({
        "OPENAI_API_KEY": None,
        "tokens": {"id_token": jwt, "access_token": jwt,
                   "refresh_token": "rt-fixture-not-real", "account_id": "acct-fixture"},
        "last_refresh": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }, indent=2)


def select_official(y):
    def act(s):
        shoot.focus(s)
        s.input.click(160, y)
        time.sleep(1.5)
        s.input.park()
    return act


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/debug")
    ap.add_argument("--display", default=":171")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--work-dir", type=Path)
    ap.add_argument("--sizes", default="1280x800,960x640")
    ap.add_argument("--official-y", type=int, default=0,
                    help="y of the official entry in the list (0: from the window height)")
    ap.add_argument("--only")
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    a.bin_dir = a.bin_dir.resolve()
    a.out = a.out.resolve()
    sizes = [tuple(int(n) for n in s.split("x")) for s in a.sizes.split(",")]
    work = (a.work_dir or Path(tempfile.gettempdir())).resolve()
    work.mkdir(parents=True, exist_ok=True)
    a.out.mkdir(parents=True, exist_ok=True)
    enter_namespace()
    real_mkdtemp = tempfile.mkdtemp
    shoot.tempfile = SimpleNamespace(
        mkdtemp=lambda prefix="", dir=None: real_mkdtemp(prefix=prefix, dir=str(work)))
    args = SimpleNamespace(display=a.display, sizes=sizes, app_dir=None, no_router_log=False,
                           rollouts=False, router_on=True, bin_dir=a.bin_dir, out=a.out,
                           keep=a.keep)
    s = shoot.Session(args)
    spb = str(a.bin_dir / "spb")
    transcript = []
    profiles = None

    def set_mixed(on):
        data = json.loads(profiles.read_text())
        data.setdefault("settings", {}).setdefault("router", {})["mixed_login"] = on
        profiles.write_text(json.dumps(data, indent=2))

    def use_krill():
        p = s.run([spb, "profile", "use", "krill", "--yes", "--countdown", "0"], check=False)
        transcript.append(f"$ spb profile use krill\n{p.stdout}{p.stderr}")

    def shots(stem):
        for w, h in sizes:
            for theme, lang in VARIANTS:
                name = f"{stem}-{w}x{h}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                if a.only and not name.startswith(a.only):
                    continue
                y = a.official_y or 0
                s.shot(name, "providers", theme, lang, 7.0,
                       select_official(y) if y else None, (w, h))

    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        s.check_paths()
        profiles = s.home / ".local/share/superba/profiles.json"
        shots("mixed-off")
        set_mixed(True)
        shots("mixed-nologin")
        # A fixture login, stashed by a switch to the router token.
        set_mixed(False)
        (s.home / ".codex" / "auth.json").write_text(fake_login())
        use_krill()
        set_mixed(True)
        shots("mixed-pending")
        use_krill()
        p = s.run([spb, "status"], check=False)
        transcript.append(f"$ spb status\n{p.stdout}{p.stderr}")
        cfg = (s.home / ".codex" / "config.toml").read_text()
        transcript.append("config.toml routing keys:\n" + "\n".join(
            l for l in cfg.splitlines() if "base_url" in l or l.startswith("model_provider")))
        auth = json.loads((s.home / ".codex" / "auth.json").read_text())
        transcript.append(f"auth.json kind: {'chatgpt login' if 'tokens' in auth else sorted(auth)}")
        router_cfg = json.loads((s.home / ".local/share/superba/router/config.json").read_text())
        transcript.append(f"router official_client: {router_cfg.get('official_client')}")
        shots("mixed-active")
    finally:
        (a.out / "transcript.txt").write_text("\n\n".join(transcript))
        s.close()


if __name__ == "__main__":
    main()
