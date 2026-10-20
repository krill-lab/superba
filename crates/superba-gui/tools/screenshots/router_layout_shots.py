#!/usr/bin/env python3
"""Capture the router layout in two sizes, themes and languages from real binaries.

Uses shoot.py's isolated HOME, PID namespace, mock relay and XTEST driver. Two
fictional providers exercise member and line priority. No real accounts are used.
Pass --out and --bin-dir explicitly; each run writes a source/binary manifest.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
SIZES = [(1280, 800), (960, 640)]
spec = importlib.util.spec_from_file_location("shoot", HERE / "shoot.py")
shoot = importlib.util.module_from_spec(spec)
spec.loader.exec_module(shoot)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--bin-dir", type=Path, required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--display", default=":178")
    args = ap.parse_args()
    if os.environ.get("SUPERBA_ROUTER_LAYOUT_NS") != "1":
        os.execvpe("unshare", ["unshare", "--user", "--map-current-user", "--pid",
                   "--fork", "--mount-proc", sys.executable, "-I", str(Path(__file__).resolve()),
                   *sys.argv[1:]], {**os.environ, "SUPERBA_ROUTER_LAYOUT_NS": "1"})
    args.bin_dir = args.bin_dir.resolve()
    args.out = args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    manifest = {
        "source": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
        "diff_sha256": hashlib.sha256(subprocess.check_output(["git", "diff", "HEAD"], cwd=REPO)).hexdigest(),
        "binaries": {name: hashlib.sha256((args.bin_dir / name).read_bytes()).hexdigest()
                     for name in ("superba", "spb", "superba-router")},
        "fixture": "Two mock providers; router enabled; three preferred/failover Krill lines",
        "screenshots": [],
    }
    opts = SimpleNamespace(display=args.display, sizes=SIZES, app_dir=None,
                           no_router_log=False, rollouts=False, router_on=False,
                           bin_dir=args.bin_dir, out=args.out, keep=False)
    session = shoot.Session(opts)
    try:
        for name in manifest["binaries"]:
            shutil.copy2(args.bin_dir / name, session.bin / name)
        opts.bin_dir = session.bin
        shoot.guard(session.home)
        session.start_xvfb()
        session.start_relay()
        session.write_sites()
        session.provision()
        spb = str(session.bin / "spb")
        session.run([spb, "profile", "add", "backup", "--name", "Example API", "--site", "example-api",
                     "--auth", "apikey", "--key-stdin"], stdin=shoot.KEY + "\n")
        session.run([spb, "router", "enable", "--member", "krill", "--member", "backup",
                     "--yes", "--write-only"])
        session.check_paths()
        for size in SIZES:
            for theme, lang in shoot.VARIANTS:
                name = f"router-{size[0]}x{size[1]}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                session.shot(name, "router", theme, lang, size=size)
                path = args.out / (name + ".png")
                manifest["screenshots"].append({"file": path.name, "size": size,
                    "theme": theme, "locale": lang, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
    finally:
        for filename in ("gui.err", "relay.err"):
            path = session.work / filename
            if path.exists():
                shutil.copy2(path, args.out / filename)
        session.close()
        (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    main()
