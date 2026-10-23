#!/usr/bin/env python3
"""Screenshots of "Import from other tools" (cc-switch, AI Toolbox) from the
real `superba` binary.

The isolated HOME of shoot.py's Session (throwaway CA, mock relay, a Krill
profile on the mock lines) gets the other tools' stores where they keep them:
`~/.cc-switch/cc-switch.db` + `settings.json` and
`~/.local/share/com.ai-toolbox/ai-toolbox.db`, copied from a fixture home made by

    SUPERBA_IMPORT_FIXTURE_HOME=<dir> \\
        cargo test -p superba-import --test fixture_homes -- --ignored

Shots, at 1280x800 in light and dark, English and Chinese:

- `import-add-*`: Providers > Add, step 1, with "Import from other tools";
- `import-dialog-*`: the import dialog (opened with `--command import`);
- `import-scrolled-*`: the same dialog scrolled to AI Toolbox's providers;
- `import-result-*`: after Enter imported the selection (Superba's data is
  restored before each of these, so every variant imports the same set).

Like shoot.py it re-runs itself in an unprivileged user + PID namespace, so no
desktop app or Codex process of the real user is visible, and it refuses a
display that is in use.

    python3 -I crates/superba-gui/tools/screenshots/import_shots.py \\
        --bin-dir target/debug --display :170 --fixture-home <dir> \\
        --out .private/import-research/shots --work-dir <scratch dir>
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
NS_ENV = "SUPERBA_IMPORT_SHOTS_NS"
SIZE = (1280, 800)
VARIANTS = [("light", "en"), ("dark", "en"), ("light", "zh-CN"), ("dark", "zh-CN")]
STORES = [".cc-switch", ".local/share/com.ai-toolbox"]

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


def settle(wait=2.5):
    """Let a page opened with `--command` settle (the command palette the
    older version typed into is gone, user request 118)."""
    def act(s):
        time.sleep(wait)
        s.input.park()
    return act


def scrolled(s):
    settle()(s)
    # Over the dialog's list; the wheel scrolls it down to AI Toolbox.
    s.input.scroll(640, 420, 9)
    s.input.park()
    time.sleep(0.8)


def result(s):
    settle()(s)
    s.input.key("Return")
    time.sleep(3.0)


def add_dialog(_lang):
    """Providers > Add: the page header's button (top right of the window)."""
    def act(s):
        shoot.focus(s)
        s.input.click(1186, 76)
        s.input.park()
        time.sleep(1.0)
    return act


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/debug")
    ap.add_argument("--display", default=":170")
    ap.add_argument("--fixture-home", type=Path, required=True,
                    help="home with the tools' stores (see the module docs)")
    ap.add_argument("--out", type=Path, default=REPO / ".private/import-research/shots")
    ap.add_argument("--work-dir", type=Path, help="scratch space (default: the system temp directory)")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--only", help="comma-separated shot name prefixes")
    a = ap.parse_args()
    a.bin_dir = a.bin_dir.resolve()
    a.out = a.out.resolve()
    a.fixture_home = a.fixture_home.resolve()
    for store in STORES:
        if not (a.fixture_home / store).is_dir():
            raise SystemExit(f"{a.fixture_home / store} missing; materialise the fixtures first")
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
    s = shoot.Session(args)
    try:
        shoot.guard(s.home)
        s.start_xvfb()
        s.start_relay()
        s.write_sites()
        s.provision()
        s.check_paths()
        for store in STORES:
            dest = s.home / store
            if dest.exists():
                shutil.rmtree(dest)
            shutil.copytree(a.fixture_home / store, dest)
        data = s.home / ".local/share/superba"
        snapshot = s.work / "superba-data-snapshot"
        shutil.copytree(data, snapshot)
        only = [p for p in (a.only or "").split(",") if p]
        plan = [
            ("import-add", "providers", add_dialog),
            ("import-dialog", "providers#import", lambda _lang: settle()),
            ("import-scrolled", "providers#import", lambda _lang: scrolled),
            ("import-result", "providers#import", lambda _lang: result),
        ]
        for stem, page, make_actions in plan:
            for theme, lang in VARIANTS:
                actions = make_actions(lang)
                name = f"{stem}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                if only and not any(name.startswith(p) for p in only):
                    continue
                # Every shot starts from the same Superba data (an import of an
                # earlier shot would turn into conflicts).
                shutil.rmtree(data)
                shutil.copytree(snapshot, data)
                s.shot(name, page, theme, lang, 7.0, actions, SIZE)
    finally:
        s.close()


if __name__ == "__main__":
    main()
