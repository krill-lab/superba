#!/usr/bin/env python3
"""Recover only sessions owned by this hosted job; never upload raw HOMEs."""

import argparse
import json
import shutil
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from e2e.macos.lib.local import configured_local  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    results = []
    if args.root.exists():
        vm, lease = configured_local(str(args.root))
        with lease:
            for session in sorted((args.root / "runs").glob("*")):
                try:
                    result = vm.rpc("cleanup", session=session.name, rpc_timeout=240)
                except Exception as error:
                    # Do not propagate command lines, files or environment values.
                    result = {"ok": False, "error": type(error).__name__}
                results.append({"session": session.name, **result})
        if all(result["ok"] for result in results):
            shutil.rmtree(args.root)
    (args.out / "recovery.json").write_text(json.dumps(results, indent=2) + "\n")
    return 0 if all(result["ok"] for result in results) else 1


if __name__ == "__main__":
    sys.exit(main())
