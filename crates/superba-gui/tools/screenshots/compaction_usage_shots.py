#!/usr/bin/env python3
"""Screenshots of remote compaction (GitHub issue 3) and the usage history
import (GitHub issue 1) from the real `superba` binary.

The isolated HOME of shoot.py's Session (throwaway CA, mock relay, a Krill
profile on the mock lines, a router request log and Codex rollouts) gets the
other tools' stores, including their usage history, copied from a fixture home
made by

    SUPERBA_IMPORT_FIXTURE_HOME=<dir> \\
        cargo test -p superba-import --test fixture_homes -- --ignored

Shots, at 1280x800 in light and dark, English and Chinese:

- `usage-import-dialog-*`: the import dialog (command palette) with each tool's
  usage history next to its providers;
- `usage-imported-*`: the Usage page after `spb import usage ccs` and
  `spb import usage ai-toolbox`, imported rows next to the router's and
  Codex's, with the source filter;
- `compaction-native-*`: Providers > Krill > Connection: the Compaction row
  (Krill compacts natively, no check sent);
- `compaction-unsupported-*`: the same provider set to "Not supported"
  (`spb profile edit --compaction unsupported`): the row and its note.

Like shoot.py it re-runs itself in an unprivileged user + PID namespace, so no
desktop app or Codex process of the real user is visible, and it refuses a
display that is in use.

    python3 -I crates/superba-gui/tools/screenshots/compaction_usage_shots.py \\
        --bin-dir target/debug --display :163 --fixture-home <dir> \\
        --out <dir> --work-dir <scratch dir>
"""

import argparse
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
NS_ENV = "SUPERBA_COMPACTION_SHOTS_NS"
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


def palette(query, settle=3.5):
    """Open the command palette, run the first entry matching `query`."""
    def act(s):
        shoot.palette_action(query)(s)
        s.input.key("Return")
        time.sleep(settle)
        s.input.park()
    return act


def spb(s, *argv):
    p = s.run([str(s.args.bin_dir / "spb"), *argv], check=False)
    return p.stdout + p.stderr


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin-dir", type=Path, default=REPO / "target/debug")
    ap.add_argument("--display", default=":163")
    ap.add_argument("--fixture-home", type=Path, required=True,
                    help="home with the tools' stores (see the module docs)")
    ap.add_argument("--out", type=Path, required=True)
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
                           rollouts=True, router_on=False, bin_dir=a.bin_dir, out=a.out,
                           keep=a.keep)
    s = shoot.Session(args)
    transcript = []
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
        only = [p for p in (a.only or "").split(",") if p]

        def wanted(stem):
            return not only or any(stem.startswith(p) for p in only)

        def shots(stem, page, actions, wait=7.0):
            for theme, lang in VARIANTS:
                name = f"{stem}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                if only and not any(name.startswith(p) for p in only):
                    continue
                s.shot(name, page, theme, lang, wait, actions, SIZE)

        if wanted("usage-import-dialog"):
            shots("usage-import-dialog", "providers", palette("import"))
        for cmd in (["import", "usage", "ccs", "--dry-run"], ["import", "usage", "ccs"],
                    ["import", "usage", "ai-toolbox"], ["import", "usage", "ccs"],
                    ["usage", "log", "--since", "1d", "--source", "ccs", "--limit", "5"],
                    ["usage", "stats", "--by", "provider", "--since", "120d", "--source", "ccs"],
                    ["profile", "compaction", "krill"]):
            transcript.append(f"$ spb {' '.join(cmd)}\n{spb(s, *cmd)}")
        if wanted("usage-imported"):
            shots("usage-imported", "usage", None, wait=8.0)
        if wanted("compaction-native"):
            shots("compaction-native", "providers", None)
        transcript.append("$ spb profile edit krill --compaction unsupported\n"
                          + spb(s, "profile", "edit", "krill", "--compaction", "unsupported"))
        transcript.append("$ spb profile compaction krill\n" + spb(s, "profile", "compaction", "krill"))
        if wanted("compaction-unsupported"):
            shots("compaction-unsupported", "providers", None)
        transcript.append("$ spb --locale zh-CN profile compaction krill\n"
                          + spb(s, "--locale", "zh-CN", "profile", "compaction", "krill"))
        transcript.append("$ spb switch --profile krill --dry-run\n"
                          + spb(s, "switch", "--profile", "krill", "--dry-run"))
    finally:
        (a.out / "transcript.txt").write_text("\n".join(transcript))
        s.close()


if __name__ == "__main__":
    main()
