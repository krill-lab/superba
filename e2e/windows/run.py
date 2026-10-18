#!/usr/bin/env python3
"""Superba Windows full-flow harness: drives the local Windows VM from the host.

    e2e/windows/run.py list [--area AREA]                 scenarios of MATRIX.md with their ids
    e2e/windows/run.py run ID [ID ...] [options]          run scenarios (prerequisites are run for you)
    e2e/windows/run.py run --all [options]                every automated scenario, in matrix order
    e2e/windows/run.py prep-snapshot                      (re)build the `wintest` base snapshot from `clean`

Options of `run`:
    --msi PATH        Superba MSI under test (default: newest superba-<workspace version>-windows-x86_64.msi
                      in .private/winvm/out/)
    --old-msi PATH    older MSI for the self-update / upgrade scenarios
    --net mock|real   mock relay on the host (default) or line forwarders to the test relay of
                      SUPERBA_E2E_ENV (needed by scenarios marked real)
    --base wintest|uacoff|wintest-wsl1  UAC on (default), the UAC-off variant (snapshot `clean`, old MSI
                      removed) or wintest with WSL 1 (snapshot `wintest-wsl1`, the WSL scenarios' base)
    --vm ID           use this VM of the pool (default: the first free one, queueing for it)
    --held            the caller already holds --vm via `pool.sh acquire`: no acquire/release
    --model NAME      set the Krill profile's model after the wizard (when the relay's default is busy)
    --keep-vm         do not give the VM back at the end (debugging only; then: pool.sh release ID)

The VM comes from the pool (.private/winvm/pool.sh): `run` acquires one (restored to `wintest`),
restores snapshots per scenario as MATRIX.md says, and releases it at the end (`clean`, stopped).

Debugging an open session (state in e2e/.work/windows/session-vm<id>.json):
    run.py session up [--snapshot wintest] [--install]    acquire, restore, boot, agent, network; stays open
    run.py session status | down                          down = close and restore `clean`
    run.py ui dump | shot NAME | click NAME [--type T] | find NAME   UIA on the Superba window
    run.py spb ARGS...                                    spb in the user's desktop session
    run.py guest 'POWERSHELL'                             PowerShell over SSH (admin token)

Reports: e2e/out/windows-<timestamp>/report.{md,json}, run.log, evidence/<id>/, transcripts/, net/.
Requires python3 (stdlib), ssh/scp, docker access for snapshots, the VM helper scripts in
.private/winvm/ (SUPERBA_WINVM_HELPERS); `--net real` also needs aiohttp for e2e/lib/linefwd.py.
"""

import argparse
import json
import os
import re
import signal
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "lib"))

import runner  # noqa: E402
from runner import REGISTRY, SKIP, NOTRUN, FAIL, T, SkipScenario, StopScenario, status_of, step_json, write_report  # noqa: E402
from session import BASE_SNAPSHOT, Run  # noqa: E402
from vm import REPO, Vm, VmError  # noqa: E402

import scenarios  # noqa: E402,F401  (registers every scenario)
import flows  # noqa: E402


# `run --all` order: chains share one restore where the state allows (a failed scenario makes
# the next one start from the base snapshot again); scenarios that need a pristine wizard or
# their own install follow at the end.
ALL_ORDER = ["W01", "W03", "W04", "W05", "W10", "W11", "W12", "W13", "W14", "W15", "W20", "W21", "W22",
             "W23", "W08", "W60", "W61", "W62", "W63", "W36", "W30", "W31", "W32", "W33", "W34", "W35",
             "W37", "W50", "W51", "W40", "W42", "W02", "W06", "W07", "W41", "W43", "W53", "W52", "W74", "W73", "W71", "W70",
             "W72", "W64", "W80", "W82", "W83", "W84", "W85", "W89", "W88", "W81", "W90", "W86", "W87"]


def default_msi():
    m = re.search(r'^version = "([^"]+)"', (REPO / "Cargo.toml").read_text(), re.M)
    version = m.group(1) if m else "0.0.0"
    out = Path(REPO / ".private" / "winvm" / "out")
    cands = sorted(out.glob(f"superba-{version}*-windows-x86_64.msi"), key=lambda p: p.stat().st_mtime)
    return str(cands[-1]) if cands else None


def cmd_list(opts):
    rows = sorted(REGISTRY.values(), key=lambda s: s.id)
    for s in rows:
        if opts.area and s.area != opts.area:
            continue
        flags = []
        if s.snapshot:
            flags.append(f"fresh:{s.snapshot}")
        if s.net not in ("any",):
            flags.append(f"net:{s.net}")
        if s.manual_only:
            flags.append("manual")
        if s.bugs:
            flags.append("bugs:" + ",".join(map(str, s.bugs)))
        print(f"{s.id:6} {s.area:10} {s.title}  [{' '.join(flags)}]")


def run_one(run, sc):
    t = T(run, sc)
    t0 = time.time()
    rec = {"id": sc.id, "title": sc.title, "area": sc.area, "bugs": list(sc.bugs)}
    run.log(f"=== {sc.id} {sc.title}")
    run.vm.renew()
    try:
        if sc.manual_only and not run.opts.include_manual:
            raise SkipScenario("manual scenario: follow MATRIX.md (run with --include-manual to do its automated part)")
        if sc.net == "real" and run.net_mode != "real":
            raise SkipScenario("needs --net real (the test relay of SUPERBA_E2E_ENV)")
        if sc.net == "mock" and run.net_mode != "mock":
            raise SkipScenario("needs --net mock")
        if any(run.state.get(c) for c in sc.conflicts):
            run.log(f"state has {[c for c in sc.conflicts if run.state.get(c)]}: starting from the base snapshot")
            run.dirty = True
        if sc.on and run.session_snapshot != sc.on:
            run.dirty = True
        if (set(sc.needs) & {"configured", "app_ready", "turn", "router_on"} and not run.state.get("configured")
                and run.state.get("wizard_seen")):
            run.log("the setup wizard was used up in this VM state (e.g. by a restore): starting from the snapshot again")
            run.dirty = True
        run.ensure_session(sc.snapshot, on=sc.on)
        for need in sc.needs:
            flows.ensure(run, t, need)
        sc.fn(t)
    except SkipScenario as e:
        rec.update(status=SKIP, reason=str(e))
    except StopScenario as e:
        rec["reason"] = f"stopped after the failed step: {e}"
        run.dirty = True
    except Exception as e:  # a bug in the scenario itself
        import traceback
        rec["reason"] = f"scenario error: {type(e).__name__}: {e}"
        run.log(traceback.format_exc())
        run.dirty = True
    rec.setdefault("status", status_of(t.steps) if t.steps else (FAIL if rec.get("reason") else SKIP))
    if rec["status"] == FAIL:
        run.dirty = True
    rec["secs"] = round(time.time() - t0, 1)
    rec["steps"] = [step_json(s) for s in t.steps]
    run.log(f"=== {sc.id}: {rec['status']} ({rec['secs']} s)")
    return rec


def cmd_run(opts):
    if opts.all:
        ids = [i for i in ALL_ORDER if i in REGISTRY] + sorted(set(REGISTRY) - set(ALL_ORDER))
    else:
        ids = [i.upper() for i in opts.ids]
    unknown = [i for i in ids if i not in REGISTRY]
    if unknown:
        raise SystemExit(f"unknown scenario ids: {unknown} (see `run.py list`)")
    if not opts.msi:
        opts.msi = default_msi()
    if not opts.msi or not Path(opts.msi).is_file():
        raise SystemExit("no MSI under test: pass --msi PATH (a superba-<version>-windows-x86_64.msi)")
    run = Run(opts)
    # A private copy: the build pipeline may replace the file in .private/winvm/out meanwhile.
    import shutil
    (run.root / "msi").mkdir(exist_ok=True)
    for attr in ("msi", "old_msi"):
        src = getattr(run, attr)
        if src:
            dst = run.root / "msi" / src.name
            shutil.copy2(src, dst)
            setattr(run, attr, dst)
    run.log(f"run {run.root.name}: {', '.join(ids)}; MSI {opts.msi}; net {opts.net}")
    results = []
    try:
        first = REGISTRY[ids[0]]
        run.acquire(snapshot=first.snapshot or first.on)  # the first scenario's snapshot: one restore less
        if not run.vm.snapshot_exists(BASE_SNAPSHOT):
            run.log(f"base snapshot {BASE_SNAPSHOT} missing: building it from clean")
            flows.prep_snapshot(run)
        for i in ids:
            results.append(run_one(run, REGISTRY[i]))
            write_report(run, results)
    except KeyboardInterrupt:
        run.log("interrupted")
    except VmError as e:  # no VM (pool) or the VM broke: report what ran, the rest as not run
        run.log(f"VM error: {e}")
        done = {r["id"] for r in results}
        results += [{"id": i, "title": REGISTRY[i].title, "area": REGISTRY[i].area, "bugs": list(REGISTRY[i].bugs),
                     "status": NOTRUN, "reason": f"VM error: {str(e)[-300:]}", "secs": 0, "steps": []}
                    for i in ids if i not in done]
    finally:
        run.teardown(final=True)
        data = write_report(run, results)
    print(f"\nreport: {run.root / 'report.md'}")
    print("summary: " + ", ".join(f"{k} {v}" for k, v in data["summary"].items() if v))
    return 1 if any(r["status"] == FAIL for r in results) else 0


def cmd_prep(opts):
    run = Run(opts)
    flows.prep_snapshot(run, rebuild=True)


def cmd_session(opts):
    if opts.action == "up":
        if not opts.msi:
            opts.msi = default_msi()
        run = Run(opts)
        # The lock must outlive this command: owned by the calling agent / terminal process,
        # the pool's default 2 h lease (`pool.sh renew ID` extends it; `session down` releases).
        from vm import caller_owner
        run.acquire(owner=caller_owner() or "none", ttl="2h")
        run.fresh(opts.snapshot, reuse=opts.reuse)
        if opts.install:
            t = T(run, runner.Scenario(id="SESSION", title="session", fn=None))
            flows.ensure(run, t, "installed")
        run.save_session()
        print(f"session open on VM {run.vm.id}: {run.root}; `run.py --vm {run.vm.id} session down` closes it")
    elif opts.action == "status":
        run = Run.attach(opts)
        print(json.dumps({"root": str(run.root), "agent": run.agent.alive() if run.agent else None,
                          "net": run.net.alive() if run.net else None, "guest": run.guest, "state": run.state}, indent=1))
    elif opts.action == "down":
        try:
            run = Run.attach(opts)
        except SystemExit:
            run = Run(opts)
        run.teardown(final=not opts.keep_vm)
        run.close_session_file()


def cmd_ui(opts):
    run = Run.attach(opts)
    from ui import Ui
    ui = Ui(run, lang=opts.lang)
    if opts.action == "dump":
        p = run.ui_dump(run.root / "ui-dump.txt")
        print(Path(p).read_text())
    elif opts.action == "shot":
        p = run.screenshot(run.root / f"{opts.name or 'shot'}.png", full=opts.full)
        print(p)
    elif opts.action == "click":
        print(json.dumps(ui.click(name=opts.name, type=opts.type, index=opts.index), ensure_ascii=False))
    elif opts.action == "find":
        sel = {"name_re": opts.name} if opts.regex else {"name": opts.name}
        if opts.type:
            sel["type"] = opts.type
        print(json.dumps(ui.find(**sel), ensure_ascii=False, indent=1))
    run.save_session()


def cmd_spb(opts):
    run = Run.attach(opts)
    r = run.spb(*opts.args, timeout=opts.timeout)
    sys.stdout.write(r["stdout"])
    sys.stderr.write(r["stderr"])
    run.save_session()
    return r["code"] or 0


def cmd_guest(opts):
    vm = Vm(opts.vm or "1")
    r = vm.ps(opts.script, check=False, timeout=opts.timeout)
    sys.stdout.write(r.stdout)
    sys.stderr.write(r.stderr)
    return r.returncode


def _interrupt(signum, frame):
    raise KeyboardInterrupt(f"signal {signum}")


def main(argv=None):
    # SIGINT is ignored in background jobs of non-interactive shells: take it back, and treat
    # SIGTERM the same, so an interrupted run still stops its helpers and gives the VM back.
    signal.signal(signal.SIGINT, _interrupt)
    signal.signal(signal.SIGTERM, _interrupt)
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--vm", default="", help="pool VM id (default: the first free one, queueing for it)")
    ap.add_argument("--held", action="store_true", help="the caller holds --vm already (pool.sh acquire)")
    ap.add_argument("--quiet", action="store_true")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("list")
    p.add_argument("--area")
    p.set_defaults(fn=cmd_list)

    def run_opts(p):
        p.add_argument("--msi")
        p.add_argument("--old-msi")
        p.add_argument("--net", default="mock", choices=["mock", "real", "none"])
        p.add_argument("--base", default="wintest", choices=["wintest", "uacoff", "wintest-wsl1"],
                       help="machine variant: wintest = UAC on (default), uacoff = the clean image (UAC off), "
                            "wintest-wsl1 = wintest with WSL 1 (the WSL scenarios' snapshot)")
        p.add_argument("--keep-vm", action="store_true")
        p.add_argument("--include-manual", action="store_true")
        p.add_argument("--model", default=os.environ.get("SUPERBA_WINTEST_MODEL"),
                       help="model for the Krill profile (e.g. one the test relay serves; env SUPERBA_WINTEST_MODEL)")

    p = sub.add_parser("run")
    p.add_argument("ids", nargs="*")
    p.add_argument("--all", action="store_true")
    run_opts(p)
    p.set_defaults(fn=cmd_run)

    p = sub.add_parser("prep-snapshot")
    run_opts(p)
    p.set_defaults(fn=cmd_prep)

    p = sub.add_parser("session")
    p.add_argument("action", choices=["up", "status", "down"])
    p.add_argument("--snapshot", default=None, help="default: the base (--base)")
    p.add_argument("--install", action="store_true")
    p.add_argument("--reuse", action="store_true", help="keep the VM state (no restore; debugging)")
    run_opts(p)
    p.set_defaults(fn=cmd_session)

    p = sub.add_parser("ui")
    p.add_argument("action", choices=["dump", "shot", "click", "find"])
    p.add_argument("name", nargs="?")
    p.add_argument("--type", default=None)
    p.add_argument("--index", type=int, default=0)
    p.add_argument("--regex", action="store_true")
    p.add_argument("--full", action="store_true")
    p.add_argument("--lang", default="en")
    p.set_defaults(fn=cmd_ui, net="mock")

    p = sub.add_parser("spb")
    p.add_argument("args", nargs=argparse.REMAINDER)
    p.add_argument("--timeout", type=int, default=300)
    p.set_defaults(fn=cmd_spb, net="mock")

    p = sub.add_parser("guest")
    p.add_argument("script")
    p.add_argument("--timeout", type=int, default=300)
    p.set_defaults(fn=cmd_guest)

    opts = ap.parse_args(argv)
    for k, v in (("msi", None), ("old_msi", None), ("keep_vm", False), ("include_manual", False), ("keep_going", False)):
        if not hasattr(opts, k):
            setattr(opts, k, v)
    if opts.cmd == "ui" and opts.action == "click" and not opts.type:
        opts.type = "Button"
    return opts.fn(opts) or 0


if __name__ == "__main__":
    sys.exit(main())
