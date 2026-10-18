#!/usr/bin/env python3
"""Screenshots of the coexistence with cc-switch (GH#5) from the real `superba`
and `spb` binaries.

shoot.py's Session provisions an isolated HOME (throwaway CA, mock relay, the
Krill profile applied with a PAT), the other tools' stores are copied in from a
fixture home made by

    SUPERBA_IMPORT_FIXTURE_HOME=<dir> \\
        cargo test -p superba-import --test fixture_homes -- --ignored

and then cc-switch's write is reproduced on `~/.codex` (its provider "Kimi",
current in the fixture store): the key fields it manages are cleared,
`model_provider = "custom"` and `[model_providers.custom]` with the key as
`experimental_bearer_token`, `auth.json` deleted (cc-switch 4.0.15
`codex_direct.rs` / `live/project/codex.rs`; the same write as
`superba-config/tests/common/tools.rs`).

Shots, at 1280x800 in light and dark, English and Chinese:

- `coexist-dialog-*`: the dialog the Overview opens by itself (who, what,
  take over again / let cc-switch manage it / not now);
- `coexist-banner-*`: a switch to another line planned from the command
  palette asks first (the dialog is dismissed with Esc);
- `coexist-handed-off-*`: the Overview after `spb hand-off --yes`.

Every shot starts from the same Superba data and Codex files. Like shoot.py it
re-runs itself in an unprivileged user + PID namespace and refuses a display in
use.

    python3 -I crates/superba-gui/tools/screenshots/coexist_shots.py \\
        --bin-dir <target>/debug --display :163 --fixture-home <dir> \\
        --out <dir> --work-dir <scratch dir>
"""

import argparse
import importlib.util
import os
import re
import shutil
import sys
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
NS_ENV = "SUPERBA_COEXIST_SHOTS_NS"
SIZE = (1280, 800)
VARIANTS = [("light", "en"), ("dark", "en"), ("light", "zh-CN"), ("dark", "zh-CN")]
STORES = [".cc-switch", ".local/share/com.ai-toolbox"]
KIMI = {
    "name": "kimi",
    "base_url": "https://api.moonshot.example/v1",
    "key": "sk-test-v20-kimi-000000000000000000",
    "model": "kimi-k3",
}
# cc-switch's top-level key fields (live/floor.rs) and nested model keys.
CCS_CLEARS = ["model_provider", "openai_base_url", "model", "review_model",
              "model_reasoning_effort", "plan_mode_reasoning_effort",
              "disable_response_storage", "model_catalog_json",
              "experimental_bearer_token", "base_url", "wire_api"]

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


def cc_switch_switches(codex):
    """cc-switch's write over the live files (top level: managed keys
    cleared, its values appended; its provider table appended; auth.json
    deleted)."""
    cfg = codex / "config.toml"
    lines = cfg.read_text().splitlines() if cfg.exists() else []
    first_table = next((i for i, l in enumerate(lines) if l.startswith("[")), len(lines))
    key = re.compile(r"^\s*([A-Za-z0-9_]+)\s*=")
    top = [l for l in lines[:first_table]
           if not (key.match(l) and key.match(l).group(1) in CCS_CLEARS)]
    while top and not top[-1].strip():
        top.pop()
    top += [f'model = "{KIMI["model"]}"', 'model_provider = "custom"',
            'model_reasoning_effort = "high"', "disable_response_storage = true", ""]
    rest = lines[first_table:]
    while rest and not rest[-1].strip():
        rest.pop()
    table = ["", "[model_providers.custom]", f'name = "{KIMI["name"]}"',
             f'base_url = "{KIMI["base_url"]}"', 'wire_api = "responses"',
             "requires_openai_auth = false", f'experimental_bearer_token = "{KIMI["key"]}"']
    cfg.write_text("\n".join(top + rest + table) + "\n")
    (codex / "auth.json").unlink(missing_ok=True)


def dismiss_and_switch(s):
    """Esc closes the dialog ("Not now"); the palette plans a line switch."""
    shoot.focus(s)
    s.input.key("Escape")
    time.sleep(0.6)
    shoot.palette_action("cdn")(s)
    s.input.key("Return")
    time.sleep(3.0)
    s.input.park()


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
        codex = s.home / ".codex"
        cc_switch_switches(codex)
        p = s.run([str(a.bin_dir / "spb"), "status"], check=False)
        shoot.log("status after cc-switch:\n" + p.stdout.strip())
        data = s.home / ".local/share/superba"
        snapshots = {d: s.work / f"snapshot-{d.name}" for d in (data, codex)}
        for src, dst in snapshots.items():
            shutil.copytree(src, dst)

        def restored():
            for src, dst in snapshots.items():
                shutil.rmtree(src)
                shutil.copytree(dst, src)

        def handed_off(lang):
            def prep():
                p = s.run([str(a.bin_dir / "spb"), "hand-off", "--yes"], check=False)
                shoot.log(f"spb hand-off rc={p.returncode}\n{p.stdout.strip()}\n{p.stderr.strip()}")
            return prep

        only = [p for p in (a.only or "").split(",") if p]
        plan = [
            ("coexist-dialog", None, None),
            ("coexist-banner", None, dismiss_and_switch),
            ("coexist-handed-off", handed_off, None),
        ]
        for stem, prep, actions in plan:
            for theme, lang in VARIANTS:
                name = f"{stem}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                if only and not any(name.startswith(p) for p in only):
                    continue
                restored()
                if prep:
                    prep(lang)()
                s.shot(name, "overview", theme, lang, 8.0, actions, SIZE)
    finally:
        s.close()


if __name__ == "__main__":
    main()
