#!/usr/bin/env python3
"""Superba → Notifications (GitHub issue 12) from the real `superba` binary,
English and Chinese, light (and dark for the overview shot).

The isolated home gets three channels (Telegram, Feishu with a signing secret,
WeChat through ServerChan) added with `spb notify channels add` (fake tokens,
the file secret backend; nothing is sent) and Codex's `notify` hook installed
in a throwaway CODEX_HOME. Each shot opens the section through the command
palette (Ctrl+K "notify" / the add-channel entry), so no coordinates of the
page are needed:

1. `notify-<lang>.png`: the top of the section (switch, channels).
2. `notify-more-<lang>.png`: scrolled further (events, content, quiet hours,
   rate limit, hooks).
3. `notify-add-<lang>.png`: the add-channel dialog.

Like the other shot scripts it re-runs itself in an unprivileged user + PID
namespace with an isolated HOME. Build first: `cargo build -p superba-gui --bin
superba -p superba-cli --bin spb`.

    python3 -I crates/superba-gui/tools/screenshots/notify_shots.py \\
        --bin-dir target/debug --display :172 --out /path/to/shots
"""

import argparse
import importlib.util
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
NS_ENV = "SUPERBA_NOTIFY_SHOTS_NS"
SIZE = (1280, 900)

spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)


def enter_namespace():
    if sys.platform != "linux" or os.environ.get(NS_ENV) == "1":
        return
    argv = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
            sys.executable, "-I", str(Path(__file__).resolve()), *sys.argv[1:]]
    os.execvpe("unshare", argv, {**os.environ, NS_ENV: "1"})


def seed(s, spb):
    codex = s.home / ".codex"
    codex.mkdir(parents=True, exist_ok=True)
    (codex / "config.toml").write_text('model = "gpt-6.1-sol"\n')
    env = s.env(TG="123456789:AAF-not-a-real-bot-token-x9Qz",
                FS="https://open.feishu.cn/open-apis/bot/v2/hook/00000000-demo-hook",
                FSSEC="demo-sign-secret", SC="SCT000000Tdemokey")

    def run(*argv):
        p = subprocess.run([str(spb), *argv], env=env, cwd=s.home, capture_output=True, text=True)
        if p.returncode != 0:
            raise SystemExit(f"spb {' '.join(argv[:3])} failed:\n{p.stdout}\n{p.stderr}")
        return p.stdout

    run("notify", "channels", "add", "telegram", "--name", "My phone", "--param",
        "chat_id=518302047", "--secret-env", "bot_token=TG")
    run("notify", "channels", "add", "feishu", "--name", "Team group",
        "--secret-env", "webhook=FS", "--secret-env", "sign_secret=FSSEC")
    run("notify", "channels", "add", "serverchan", "--name", "WeChat",
        "--secret-env", "send_key=SC")
    run("notify", "set", "--enabled", "on", "--quiet-hours", "23:00-08:00",
        "--urgent-in-quiet", "on", "--min-turn-secs", "30")
    run("notify", "install", "codex")
    # The approvals hook too: no Codex is installed here, so it waits for
    # the user's trust in Codex ("Trust in Codex").
    run("notify", "install", "codex", "--approvals")


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, required=True)
    ap.add_argument("--display", default=":172")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--keep", action="store_true")
    a = ap.parse_args()
    a.bin_dir = a.bin_dir.resolve()
    a.out = a.out.resolve()
    a.out.mkdir(parents=True, exist_ok=True)
    enter_namespace()
    args = SimpleNamespace(display=a.display, sizes=[SIZE], app_dir=None, keep=a.keep,
                           no_router_log=False, rollouts=False, router_on=False,
                           bin_dir=a.bin_dir, out=a.out)
    s = shoot.Session(args)
    try:
        for name in ("superba", "spb"):
            shutil.copy2(a.bin_dir / name, s.bin / name)
        args.bin_dir = s.bin
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        seed(s, s.bin / "spb")
        s.check_paths()

        def open_with(query, scroll=0):
            def act(s):
                shoot.palette_action(query)(s)
                s.input.key("Return")
                time.sleep(1.2)
                if scroll:
                    s.input.scroll(760, 500, scroll)
                    time.sleep(0.8)
                s.input.park()
            return act

        for lang in ("en", "zh-CN"):
            suffix = "en" if lang == "en" else "zh"
            s.shot(f"notify-{suffix}", "notify", "light", lang,
                   actions=open_with("notification settings"), size=SIZE)
            s.shot(f"notify-more-{suffix}", "notify", "light", lang,
                   actions=open_with("notification settings", scroll=9), size=SIZE)
            s.shot(f"notify-add-{suffix}", "notify", "light", lang,
                   actions=open_with("add notification channel"), size=SIZE)

            wechat = open_with("add serverchan channel")

            s.shot(f"notify-add-wechat-{suffix}", "notify", "light", lang,
                   actions=wechat, size=SIZE)
        s.shot("notify-dark-zh", "notify", "dark", "zh-CN",
               actions=open_with("notification settings"), size=SIZE)
    finally:
        s.close()


if __name__ == "__main__":
    main()
