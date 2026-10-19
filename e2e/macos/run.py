#!/usr/bin/env python3
"""Run real macOS scenarios through a caller-provided SSH helper."""

import argparse
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))

from e2e.macos.lib import scenarios  # noqa: E402,F401
from e2e.macos.lib.agent import Agent  # noqa: E402
from e2e.macos.lib.runner import REGISTRY, Runner  # noqa: E402
from e2e.macos.lib.vm import Blocked, VmError, configured_vm  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("list", "prepare", "permissions", "run", "cleanup"))
    parser.add_argument("ids", nargs="*")
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--dmg", help="local native arm64 or universal DMG")
    parser.add_argument("--upgrade-dmg")
    parser.add_argument("--out", default="e2e/out/macos-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ"))
    parser.add_argument("--net", choices=("mock", "real"), default="mock")
    parser.add_argument("--env", default=os.environ.get("SUPERBA_E2E_ENV"))
    parser.add_argument("--responses-path", default="/codex/v1")
    parser.add_argument("--allow-reboot", action="store_true")
    parser.add_argument("--wait-lock", type=int, default=0, help="seconds to wait for another VM user")
    parser.add_argument("--session", help="owned session id for cleanup after an interrupted run")
    opts = parser.parse_args()
    if opts.command == "list":
        for case in REGISTRY.values():
            print(f"{case.id}\t{case.title}\t{case.counterpart}")
        return 0
    ids = list(REGISTRY) if opts.all else opts.ids
    if opts.command == "run":
        if not ids or any(id not in REGISTRY for id in ids) or len(set(ids)) != len(ids):
            parser.error("choose unique scenario IDs (list) or --all")
        if not opts.dmg or not Path(opts.dmg).is_file():
            parser.error("--dmg must name an existing local DMG")
    vm, lease = configured_vm()
    lease.wait = opts.wait_lock
    with lease:
        if opts.command == "prepare":
            vm.prepare()
            print(Agent(vm).preflight())
        elif opts.command == "permissions":
            print(Agent(vm).preflight())
        elif opts.command == "cleanup":
            if not opts.session:
                parser.error("cleanup requires --session from the saved session.json")
            result = vm.rpc("cleanup", session=opts.session, rpc_timeout=240)
            print(result)
            return 0 if result["ok"] else 1
        else:
            return Runner(vm, opts).run(ids)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Blocked as error:
        print("BLOCKED: " + str(error), file=sys.stderr)
        sys.exit(2)
    except (VmError, ValueError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
