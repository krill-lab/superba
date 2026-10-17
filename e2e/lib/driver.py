"""Superba end-to-end driver (see ../README.md).

    driver.py all                 full run: setup, every scenario, teardown
    driver.py setup               start a run (namespace, isolated home, CA, forwarders, Xvfb)
    driver.py s1 .. s23           run one scenario against the current run
    driver.py spb ARGS...         run spb inside the current run's environment
    driver.py cdp ARGS...         cdp.py against the current run's app
    driver.py exec CMD...         any command in the run's namespace and environment
    driver.py env                 print the current run's environment (no secrets)
    driver.py teardown            stop everything this run started
    driver.py report              write report.md from report.json

Private settings come from the environment (run.sh loads SUPERBA_E2E_ENV):
SUPERBA_E2E_UPSTREAM, SUPERBA_E2E_KEY, SUPERBA_E2E_APP_DIR.

Everything runs inside an unprivileged user + PID namespace (nsinit.py): `all`
re-executes itself there; `setup` starts a holder namespace whose init stays up,
and every later step enters it with nsenter, so the run's processes (app, GUI,
router, forwarders) survive between steps and only they are visible.
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import infra  # noqa: E402
import legacy  # noqa: E402
import scenarios  # noqa: E402
from infra import Run  # noqa: E402


NS_ENV = "SUPERBA_E2E_NS"
NSINIT = Path(__file__).resolve().parent / "nsinit.py"
UNSHARE = ["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc"]
HOLDER = infra.E2E_DIR / ".work" / "ns-holder.json"


def in_namespace():
    return os.environ.get(NS_ENV) == "1"


def run_in_namespace(argv):
    """Re-execute `driver.py argv...` inside the run's namespace: a fresh one for
    `all` (dies with the run), the step-mode holder for anything else."""
    me = [sys.executable, "-B", str(Path(__file__).resolve()), *argv]
    env = {**os.environ, NS_ENV: "1"}
    if argv[0] == "all":
        os.execvpe("unshare", [*UNSHARE, sys.executable, "-B", str(NSINIT), "--", *me], env)
    if argv[0] == "setup":
        start_holder()
    holder = json.loads(HOLDER.read_text()) if HOLDER.exists() else None
    if not holder or not infra.pid_alive(holder["init"]):
        sys.exit("no e2e namespace is running; start one with `run.sh step setup`")
    os.execvpe("nsenter", ["nsenter", "-t", str(holder["init"]), "-U", "-p", "-m", "--preserve-credentials",
                           *me], env)


def start_holder():
    """Step mode: a namespace whose init (nsinit --hold) stays up until teardown."""
    if HOLDER.exists():
        old = json.loads(HOLDER.read_text())
        if infra.pid_alive(old.get("init", 0)):
            sys.exit("an e2e namespace is already running; `run.sh step teardown` first")
    HOLDER.parent.mkdir(parents=True, exist_ok=True)
    ready = HOLDER.with_suffix(".ready")
    ready.unlink(missing_ok=True)
    p = subprocess.Popen([*UNSHARE, sys.executable, "-B", str(NSINIT), "--hold", str(ready)],
                         stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                         start_new_session=True)
    for _ in range(100):
        kids = Path(f"/proc/{p.pid}/task/{p.pid}/children")
        child = kids.read_text().split() if kids.exists() else []
        if ready.exists() and child:
            HOLDER.write_text(json.dumps({"unshare": p.pid, "init": int(child[0])}))
            return
        time.sleep(0.1)
    raise SystemExit("the e2e namespace did not start")


def stop_holder():
    """Called outside the namespace after teardown: ending its init kills every
    process left in it."""
    if not HOLDER.exists():
        return
    h = json.loads(HOLDER.read_text())
    for pid in (h.get("init"), h.get("unshare")):
        if pid and infra.pid_alive(pid):
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
    HOLDER.unlink(missing_ok=True)


def setup():
    for k in ("SUPERBA_E2E_UPSTREAM", "SUPERBA_E2E_KEY", "SUPERBA_E2E_APP_DIR"):
        if not os.environ.get(k):
            sys.exit(f"{k} is not set (run through e2e/run.sh with SUPERBA_E2E_ENV)")
    if not in_namespace():
        sys.exit("setup runs inside the e2e namespace (driver.py re-executes itself there)")
    run = Run.create()
    run.d.update({"app_dir": os.environ["SUPERBA_E2E_APP_DIR"]})
    infra.install_bins(run, Path(os.environ.get("SPB_BIN") or infra.REPO / "target-v2-shared/release/spb").parent)
    (run.work / "xdg-runtime").mkdir(mode=0o700, exist_ok=True)
    infra.make_ca(run)
    # a, b, c: Krill-kind test lines; d: the custom OpenAI-compatible site (s12);
    # e: the Krill-kind site with tier-suffixed model names (s19).
    for line in ("a", "b", "c", "d", "e"):
        infra.start_forwarder(run, line)
    infra.start_mockrelay(run)
    # Feed mirrors + app download fixture (s17, s23) and the Codex package source
    # (s21, s22): local https servers, so no scenario needs GitHub or OpenAI.
    infra.start_feedmirror(run)
    infra.build_feed_trees(run)
    infra.start_pkgcache(run)
    infra.write_sites(run)
    infra.start_xvfb(run)
    run.save()
    print(
        json.dumps(
            {
                "run": run.d["run_id"],
                "home": run.home,
                "display": run.d["display"],
                "lines": {k: run.line_url(k) for k in run.d["ports"]},
                "out": str(run.out),
            },
            indent=1,
        )
    )
    return run


def teardown(run):
    killed = []
    for app in run.d.get("apps", []):
        killed += infra.kill_pids(infra.descendants(app) + [app])
    killed += infra.kill_pids(infra.pids_with_home(run.home))
    for line in list(run.d.get("ports", {})):
        p = infra.stop_forwarder(run, line)
        if p:
            killed.append(p)
    for name in [k for k in run.d.get("pids", {}) if k != "xvfb"]:
        killed += infra.kill_pids([run.d["pids"].pop(name)])
    infra.stop_xvfb(run)
    run.d["apps"] = []
    run.save()
    # Caches of the app and Codex (~2 GB per run) are not evidence.
    shutil.rmtree(run.work / "home" / ".cache", ignore_errors=True)
    # The short /tmp alias of the isolated home; the home itself stays in .work/.
    if os.path.islink(run.home):
        os.remove(run.home)
    print("killed", killed)


def main(argv):
    if len(argv) < 2:
        sys.exit(__doc__)
    cmd = argv[1]
    if cmd == "teardown" and not in_namespace():
        if HOLDER.exists():
            subprocess.run(["nsenter", "-t", str(json.loads(HOLDER.read_text())["init"]), "-U", "-p", "-m",
                            "--preserve-credentials", sys.executable, "-B", str(Path(__file__).resolve()),
                            "teardown"], env={**os.environ, NS_ENV: "1"})
        stop_holder()
        return
    if not in_namespace():
        run_in_namespace(argv[1:])
    if cmd == "setup":
        setup()
        return
    if cmd == "all":
        run = setup()
        try:
            scenarios.run_all(run)
        finally:
            teardown(run)
            scenarios.write_report(run)
        print(scenarios.summary_line(run))
        return
    run = Run.load()
    if cmd == "teardown":
        teardown(run)
    elif cmd == "spb":
        r = run.spb(*argv[2:], timeout=900, label="manual")
        print(r["out"], end="")
        print(r["err"], end="", file=sys.stderr)
        print(f"[rc={r['rc']} {r['secs']}s]", file=sys.stderr)
    elif cmd == "cdp":
        import cdp

        port = scenarios.cdp_port(run)
        cdp.run_cli(["cdp", str(port), *argv[2:]])
    elif cmd == "env":
        env = run.base_env()
        print(json.dumps({k: v for k, v in env.items()}, indent=1))
    elif cmd == "exec":
        # Any command inside the run's namespace and environment (debugging).
        sys.exit(subprocess.run(argv[2:], env=run.base_env(), cwd=run.home).returncode)
    elif cmd == "legacy":
        print(json.dumps(legacy.build(run), indent=1))
    elif cmd == "report":
        scenarios.write_report(run)
        print(scenarios.summary_line(run))
    elif cmd in scenarios.SCENARIOS:
        scenarios.run_one(run, cmd)
        print(json.dumps(run.d["report"].get(cmd), indent=1, ensure_ascii=False)[:6000])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
