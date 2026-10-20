"""Scenarios 1-9 of the Superba e2e run (see ../README.md for what each proves);
scenarios 10-15 live in scenarios_v2.py, 16-23 in
scenarios_v3.py.

Each scenario records PASS/FAIL with concrete evidence (pids, forwarder log
lines, file diffs, exit codes) into out/<run>/report.json; `write_report`
renders report.md. Scenarios run in the order of ORDER because later ones build
on the state earlier ones leave (takeover -> launch -> hot switches -> ...).
"""

import asyncio
import difflib
import glob
import hashlib
import json
import os
import re
import subprocess
import threading
import time
import tomllib
import traceback
from pathlib import Path

import cdp
import infra
import legacy

ORDER = ["s1", "s2", "s3", "s4", "s5", "s8", "s6", "s11", "s12", "s13", "s14",
         "s17", "s18", "s19", "s20", "s21", "s22", "s23", "s10", "s16", "s7", "s9", "s15"]
NAMES = {
    "s1": "First takeover of a legacy Codex home (profile: Automatic auth, lines A>B, recommended settings; sessions migrated)",
    "s2": "spb launch: app with CDP, PAT login, turn 1 on line A, migrated threads in recents",
    "s3": "Hot switch to line B: same main process, new app-server, turn 2 in the same thread on B",
    "s4": "Hot switch PAT -> API key (no chatgpt-account-id) and back to PAT",
    "s5": "Busy turn: switch is held (cancel / --wait-turn), --force proceeds",
    "s6": "Unhealthy launch-time auth origin (plan 7.1 rule 7): full restart with new env",
    "s7": "spb restore original (reverse-migrate): files, env block and sessions back",
    "s8": "codex exec (bundled CLI) after a switch hits the new line",
    "s9": "Remote hosts: superba-remote docker sshd integration test",
    "s10": "GUI (real runtime): every page in dark/light x en/zh-CN, live router page, switch overlay with settings rows, GUI hot switch",
    "s11": "Codex settings: set 2 stable keys + 1 feature, diff, hot apply, codex features list, unknown key skipped, legacy migration",
    "s12": "Custom OpenAI-compatible site: site add with model map + key, probe, switch, turn through a local forwarder",
    "s13": "Local router (real binaries): install, enable, stickiness, failover between turns and mid-turn, 401/403/429 not failed over, disable, watchdog fallback",
    "s14": "Account data from standard endpoints (mock): balance + usage, each endpoint 404 hidden, 429 cached with note",
    "s15": "Tracked tree: no demo mode artifacts, no user-facing 'context management' wording, no account-summary endpoint, no 'for Krill' brand string, no internal hosts, no key bytes",
    "s16": "Setup wizard with the Krill preset (fresh home, GUI): first start, key typed, lines, finish = profile + switch; re-run from Preferences edits the profile; Skip writes nothing",
    "s17": "Model lists from the provider (Codex-native and /v1/models) and the signed feeds through raced mirrors: 404 / HTML / stale / valid, winner remembered, cache, built-in snapshot",
    "s18": "Per-model context override: catalog file at <CODEX_HOME>/superba/models-<sha8>.json, preflight, picker list, rollout context window, reset and pruning, a refused preflight writes nothing",
    "s19": "Fast / Ultrafast in API-key direct mode: service_tier for the CLI, the app runs Standard, Ultrafast through the catalog, tier-suffixed model names for the app, Standard = no tier",
    "s20": "Request log and stats line (router on, real app): line under the reply, stripped from the next request, final answer only in a tool loop, codex exec untouched, log rows, GUI, no request content recorded, no key bytes",
    "s21": "CODEX_CLI_PATH pin: verified install from the local package source (corrupted copy rejected), pin = full restart with the store binary, rollout cli_version, unpin, inherited CODEX_CLI_PATH ignored",
    "s22": "Remote host (Docker sshd): app-known hosts only, probe, Codex install over SSH stdin, settings sync filtered by the host's Codex version, follow off/on, sessions per host, rollback",
    "s23": "Desktop app update check from the feed and a local APT index: installed vs latest with bundled Codex, verified download, command shown, never a privileged command",
}
SCENARIOS = {}


def scenario(sid):
    def wrap(fn):
        SCENARIOS[sid] = fn
        return fn

    return wrap


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------


LAST_CHECK = {}


class Check:
    """Collects assertions as evidence instead of raising on the first one."""

    def __init__(self):
        self.items = []
        self.failed = []
        LAST_CHECK["c"] = self

    def ok(self, cond, what):
        self.items.append(("PASS" if cond else "FAIL") + ": " + what)
        if not cond:
            self.failed.append(what)
        return cond

    def info(self, what):
        self.items.append("INFO: " + what)

    @property
    def status(self):
        return "pass" if not self.failed else "fail"


def cdp_port(run):
    files = glob.glob(f"{run.home}/.config/*/DevToolsActivePort")
    files.sort(key=lambda f: Path(f).stat().st_mtime, reverse=True)
    for f in files:
        try:
            return int(Path(f).read_text().split()[0])
        except (OSError, ValueError, IndexError):
            continue
    raise RuntimeError("no DevToolsActivePort under the isolated home")


def acall(coro):
    return asyncio.run(coro)


class _InFn(Exception):
    """An error raised by the callable itself (not by the connection)."""


async def _with_cdp(run, fn, retries=20, fn_retries=20):
    """Run `fn(cdp)` on the main renderer. Connecting is retried `retries`
    times (renderer not up yet); an error of `fn` itself `fn_retries` times
    (the onboarding loop, which waits 90 s itself, passes 1)."""
    last = None
    failed_in_fn = 0
    for _ in range(retries):
        try:
            async with cdp.Cdp(cdp_port(run)) as c:
                try:
                    return await fn(c)
                except Exception as e:
                    raise _InFn(e) from e
        except _InFn as e:
            last = e.args[0]
            failed_in_fn += 1
            if failed_in_fn > fn_retries:
                raise last
        except Exception as e:  # renderer not ready yet
            last = e
        await asyncio.sleep(1)
    raise RuntimeError(f"CDP unavailable: {last!r}; app processes: {app_process_states(run)}")


def app_process_states(run):
    """(pid, state, process type, CPU seconds) of the run's desktop app
    processes: what a hung renderer looks like when CDP stops answering."""
    out = []
    tick = os.sysconf("SC_CLK_TCK")
    for pid in infra.pids_with_home(run.home):
        cl = infra.cmdline(pid)
        if "/ChatGPT" not in cl.split(" ", 1)[0]:
            continue
        kind = next((a.split("=", 1)[1] for a in cl.split() if a.startswith("--type=")), "main")
        try:
            with open(f"/proc/{pid}/stat") as f:
                st = f.read().split(") ", 1)[1].split()
            out.append((pid, st[0], kind, round((int(st[11]) + int(st[12])) / tick, 1)))
        except (OSError, IndexError, ValueError):
            continue
    return out


def probe(run):
    return acall(_with_cdp(run, lambda c: c.probe()))


def shot(run, name):
    path = run.out / "screens" / f"{name}.png"
    path.parent.mkdir(exist_ok=True)
    acall(_with_cdp(run, lambda c: c.shot(str(path))))
    return str(path.relative_to(run.out))


def app_procs(run):
    """Main process and app-servers of the desktop app this run launched."""
    mains, servers = [], []
    for pid in infra.pids_with_home(run.home):
        cl = infra.cmdline(pid)
        if cl.startswith(run.d["app_dir"] + "/ChatGPT") and "--type=" not in cl:
            mains.append(pid)
    for pid in infra.pids_with_home(run.home):
        cl = infra.cmdline(pid)
        if "/resources/codex" in cl and " app-server" in cl:
            ppid = parent(pid)
            if ppid in mains:
                servers.append(pid)
    for m in mains:
        if m not in run.d.setdefault("apps", []):
            run.d["apps"].append(m)
    run.save()
    return (mains[0] if mains else None), sorted(servers)


def parent(pid):
    try:
        with open(f"/proc/{pid}/stat") as f:
            return int(f.read().split(") ", 1)[1].split()[1])
    except (OSError, IndexError, ValueError):
        return None


def proc_started_wall(pid):
    """Wall-clock start time of a process (boot-time based; ~1 s precision)."""
    ticks = infra.start_time_ticks(pid)
    if ticks is None:
        return None
    with open("/proc/stat") as f:
        btime = next(int(x.split()[1]) for x in f if x.startswith("btime"))
    return btime + ticks / os.sysconf("SC_CLK_TCK")


def server_env(pid, var="CODEX_AUTHAPI_BASE_URL"):
    return infra.environ(pid).get(var)


def sha(path):
    p = Path(path)
    return hashlib.sha256(p.read_bytes()).hexdigest()[:16] if p.exists() else None


def spb_stream(run, *args, label, timeout=600, stdin=None):
    """Run spb, timestamping every stdout/stderr line as it arrives."""
    argv = [run.d["spb"], *args]
    t0 = time.time()
    p = subprocess.Popen(
        argv,
        env=run.base_env(),
        cwd=run.home,
        stdin=subprocess.PIPE if stdin else subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    lines = []
    lock = threading.Lock()

    def pump(stream, tag):
        for raw in stream:
            with lock:
                lines.append((round(time.time() - t0, 2), tag, raw.rstrip("\n")))

    ts = [threading.Thread(target=pump, args=(p.stdout, "out")), threading.Thread(target=pump, args=(p.stderr, "err"))]
    for t in ts:
        t.start()
    if stdin:
        p.stdin.write(stdin)
        p.stdin.close()
    try:
        rc = p.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        p.kill()
        rc = "timeout"
    for t in ts:
        t.join(5)
    secs = round(time.time() - t0, 2)
    transcript = "\n".join(f"[{t:6.2f}] {tag} {line}" for t, tag, line in lines)
    log = run.out / "transcripts" / f"{label}.log"
    log.parent.mkdir(exist_ok=True)
    log.write_text(f"$ spb {' '.join(args)}\nrc={rc} secs={secs}\n{transcript}\n")
    with open(run.out / "cmds.log", "a", encoding="utf-8") as f:
        f.write(f"\n### [{time.strftime('%H:%M:%S')}] {label} $ spb {' '.join(args)}\n### rc={rc} {secs}s\n{transcript}\n")
    out = "\n".join(line for _, tag, line in lines if tag == "out")
    err = "\n".join(line for _, tag, line in lines if tag == "err")
    return {"rc": rc, "secs": secs, "t0": t0, "lines": lines, "out": out, "err": err, "all": out + "\n" + err,
            "log": str(log.relative_to(run.out))}


def phase_at(res, name):
    for t, _, line in res["lines"]:
        if line.strip().startswith("→") and name in line:
            return t
    return None


def elapsed_of(res):
    # Older spb printed "Elapsed: X s"; it now ends with "Switched to ... in X s".
    m = re.search(r"Elapsed: ([0-9.]+) s", res["all"]) or re.search(r"Switched to .* in ([0-9.]+) s", res["all"])
    return float(m.group(1)) if m else None


def config(run):
    return tomllib.loads((run.codex_home / "config.toml").read_text())


def auth_shape(run):
    try:
        j = json.loads((run.codex_home / "auth.json").read_text())
    except (OSError, json.JSONDecodeError):
        return None
    return sorted(k for k in j)


def fmt_entry(e):
    keep = {k: e.get(k) for k in ("t", "line", "method", "path", "status", "auth_kind", "originator", "model", "n_input", "last_user") if e.get(k) is not None}
    if "last_user" in keep:
        keep["last_user"] = keep["last_user"][:50]
    return json.dumps(keep, ensure_ascii=False)


def wait_turn(run, marker, since, timeout=180):
    """Wait until a /responses POST whose last user text starts with `marker`
    went through a forwarder and the renderer shows the turn finished.
    Returns (line, entry)."""
    def find():
        for line in run.d["ports"]:
            for e in infra.responses_posts(run, line, since):
                if (e.get("last_user") or "").startswith(marker):
                    return line, e
        return None

    hit = infra.wait_for(f"turn {marker} on a line", find, timeout=timeout)

    def idle():
        p = probe(run)
        return p if (not p["stop"]) else None

    infra.wait_for(f"turn {marker} finished in the renderer", idle, timeout=timeout)
    time.sleep(1.0)
    return hit


def send_turn(run, prompt):
    acall(_with_cdp(run, lambda c: c.send_turn(prompt)))




def onboarding(run, check):
    """Answer the first-run onboarding (Engineering -> Continue -> Skip -> Go to
    ChatGPT) and dismiss announcement modals."""
    async def go(c):
        steps = []
        end = time.time() + 90
        while time.time() < end:
            p = await c.probe()
            text = await c.body_text()
            if p["composer"] and not any(w in text for w in ("Which best describes your work", "Skip setup?")):
                clicked = await c.click_text(["Continue with current model", "Not now", "Maybe later", "Got it"])
                if clicked:
                    steps.append(clicked)
                    await asyncio.sleep(1)
                    continue
                return steps, p
            # "Try again": the app's own error page (ChatGPT hit a snag).
            for labels in (["Go to ChatGPT"], ["Skip"], ["Continue"], ["Engineering"], ["Try again"]):
                clicked = await c.click_text(labels)
                if clicked:
                    steps.append(clicked)
                    break
            await asyncio.sleep(1.5)
        raise TimeoutError(f"onboarding did not finish: {steps}")

    steps, p = acall(_with_cdp(run, go, fn_retries=1))
    check.info(f"onboarding/modals answered via CDP: {steps}")
    return p


def rc_blocks(run):
    out = {}
    home = Path(run.home)
    for rel in (".bashrc", ".profile", ".zshenv", ".bash_profile", ".config/environment.d/60-superba.conf"):
        f = home / rel
        if f.exists():
            m = re.search(r"CODEX_AUTHAPI_BASE_URL=\"?([^\"\n]+)", f.read_text())
            out[rel] = m.group(1) if m else None
    return out


def flatten(d, prefix=""):
    out = {}
    for k, v in d.items():
        key = f"{prefix}.{k}" if prefix else k
        if isinstance(v, dict):
            out.update(flatten(v, key))
        else:
            out[key] = v
    return out


def text_diff(a, b, name):
    return "".join(difflib.unified_diff(a.splitlines(True), b.splitlines(True), f"{name} (before)", f"{name} (after)"))


def save(run, name, text):
    p = run.out / "evidence" / name
    p.parent.mkdir(exist_ok=True)
    p.write_text(text)
    return str(p.relative_to(run.out))


def finish(run, sid, check, **details):
    run.record(sid, NAMES[sid], check.status, check.items, **details)


# ---------------------------------------------------------------------------
# scenarios
# ---------------------------------------------------------------------------


@scenario("s1")
def s1(run):
    c = Check()
    built = legacy.build(run)
    c.ok(built["as_rc"] == 0 and len(built["threads"]) == 2 and built["exec_rc"] == 0,
         f"legacy threads created by real Codex with provider 'krill': {[t['thread_id'] for t in built['threads']]} + 1 exec thread")
    pre = legacy.snapshot(run, "pre-takeover")
    run.d["pre"] = {"config": pre["files"][".codex/config.toml"].decode(),
                    "auth_sha": sha(run.codex_home / "auth.json"),
                    "rc": {k: (v.decode() if v else None) for k, v in pre["files"].items() if not k.startswith(".codex")},
                    "threads": sorted(pre["sqlite"])}
    run.save()
    c.ok(set(v["provider"] for v in pre["sqlite"].values()) == {"krill"} and len(pre["sqlite"]) == 3,
         f"before: state_5.sqlite has 3 threads, all provider 'krill' ({len(pre['rollout'])} rollouts)")

    r = run.run([run.d["spb"], "profile", "add", "e2e", "--name", "E2E", "--site", "krill-e2e", "--line", "a",
                 "--line", "b", "--key-stdin"], stdin=run.key + "\n", label="s1 profile add")
    c.ok(r["rc"] == 0 and "file" in r["out"], "spb profile add e2e --site krill-e2e --line a --line b --key-stdin: Automatic auth (key in the 0600 file backend)")
    prof = next((p for p in json.loads((run.data_dir / "profiles.json").read_text()).get("profiles", []) if p.get("id") == "e2e"), {})
    c.ok(prof.get("line_id") == "a" and prof.get("backup_lines") == ["b"], f"profile lines in priority order: {prof.get('line_id')} > {prof.get('backup_lines')}")
    c.info(f"profile auth: {json.dumps({k: prof.get(k) for k in ('auth', 'auth_choice', 'auth_mode') if k in prof})}; settings: {sorted((prof.get('codex_settings') or {}))}")
    dry = spb_stream(run, "profile", "use", "e2e", "--dry-run", label="s1-dry-run")
    c.ok(dry["rc"] == 4 and "--migrate-legacy-sessions" in dry["all"] and "3 sessions" in dry["all"],
         f"dry run without an answer exits 4 (needs answers) and asks about 3 legacy sessions [{dry['log']}]")
    res = spb_stream(run, "profile", "use", "e2e", "--migrate-legacy-sessions", "--yes", label="s1-takeover")
    c.ok(res["rc"] == 0 and "Switched" in res["all"], f"takeover exit 0 in {res['secs']} s [{res['log']}]")
    c.ok("not running" in res["all"], "app not running: config written, nothing restarted ('applies the next time it opens')")

    after = (run.codex_home / "config.toml").read_text()
    diff = text_diff(run.d["pre"]["config"], after, "config.toml")
    c.info("config.toml diff: " + save(run, "s1-config.diff", diff))
    before_t, after_t = tomllib.loads(run.d["pre"]["config"]), tomllib.loads(after)
    fa = flatten(after_t)
    c.ok(fa.get("model_provider") == "openai", "model_provider = \"openai\"")
    c.ok(fa.get("openai_base_url") == run.line_url("a") + "/backend-api/codex", "openai_base_url = <line A>/backend-api/codex")
    c.ok(fa.get("chatgpt_base_url") == run.line_url("a") + "/backend-api/", "chatgpt_base_url = <line A>/backend-api/")
    untouched = {k: v for k, v in flatten(before_t).items() if k != "model_provider"}
    changed = [k for k, v in untouched.items() if fa.get(k) != v]
    c.ok(not changed, f"every other original key unchanged ({len(untouched)} keys incl. [model_providers.krill], [mcp_servers.e2e-docs], [projects], approval_policy, file_opener); changed: {changed}")
    comments = [l for l in run.d["pre"]["config"].splitlines() if "# keep:" in l]
    c.ok(all(l in after.splitlines() for l in comments), f"all {len(comments)} original comment lines kept verbatim")
    removed = [l for l in diff.splitlines() if l.startswith("-") and not l.startswith("---")]
    c.ok(removed == ['-model_provider = "krill"'], f"the only removed line is the provider line: {removed}")
    c.ok(auth_shape(run) == ["OPENAI_API_KEY", "personal_access_token"], f"Automatic resolved to PAT on the https line: auth.json is PAT shaped {auth_shape(run)}")
    rec = {"web_search": "live", "features.fast_mode": True, "analytics.enabled": False}
    c.ok(all(fa.get(k) == v for k, v in rec.items()), f"Krill recommended settings written as profile settings: { {k: fa.get(k) for k in rec} }")
    c.ok(fa.get("features.collab") is False, "legacy [features] collab left as it was (migration is only offered, s11)")
    blocks = rc_blocks(run)
    c.ok(blocks.get(".bashrc") == run.line_url("a") and blocks.get(".profile") == run.line_url("a"),
         f"rc marker blocks export CODEX_AUTHAPI_BASE_URL=<line A>: {blocks}")
    home = Path(run.home)
    c.ok(all(legacy.USER_RC in (home / f).read_text() for f in (".bashrc", ".profile")), "user's own rc content preserved")
    sq = legacy.sqlite_providers(run.codex_home)
    ro = legacy.rollout_providers(run.codex_home)
    c.ok(set(v["provider"] for v in sq.values()) == {"openai"} and len(sq) == 3, f"state_5.sqlite: all 3 threads now 'openai'")
    c.ok(set(v["provider"] for v in ro.values()) == {"openai"} and len(ro) == 3, f"rollout session_meta: all 3 now 'openai'")
    save(run, "s1-providers-after.json", json.dumps({"sqlite": sq, "rollout": ro}, indent=1))
    st = run.spb("status", json_out=True, label="s1 status")
    j = st.get("json") or {}
    ov = j.get("overview", j)
    c.ok(bool(ov.get("baseline_captured")), "status: baseline (Original) captured")
    pl = run.spb("profile", "list", label="s1 profile list")
    c.ok("original" in pl["out"] and "Original configuration" in pl["out"], "profile list shows the built-in 'Original configuration'")
    fw = list((run.data_dir / "backups" / "first-write").rglob("*.enc"))
    c.ok(len(fw) >= 2, f"first-write baseline backups (encrypted): {[p.name for p in fw]}")
    se = run.spb("sessions", label="s1 sessions")
    c.ok(re.search(r"krill: 3 \| 3 \| 3", se["out"]) is not None, "spb sessions: migration ledger krill: 3 -> openai")
    finish(run, "s1", c)


@scenario("s2")
def s2(run):
    c = Check()
    t0 = time.time()
    res = spb_stream(run, "launch", label="s2-launch")
    c.ok(res["rc"] == 0, f"spb launch exit 0 in {res['secs']} s [{res['log']}]")
    m = re.search(r"PID: (\d+)", res["all"])
    port = re.search(r"CDP port: (\d+)", res["all"])
    auth = re.search(r"Auth environment: (\S+)", res["all"])
    c.ok(bool(m and port), f"launched pid {m and m.group(1)}, CDP port {port and port.group(1)} (random, loopback)")
    c.ok(auth and auth.group(1) == run.line_url("a"), f"launch env CODEX_AUTHAPI_BASE_URL = line A ({auth and auth.group(1)})")
    main, servers = infra.wait_for("app-server child", lambda: (lambda r: r if r[0] and r[1] else None)(app_procs(run)), 60)
    c.ok(str(main) == (m and m.group(1)), f"app main pid {main} (ours), app-server {servers}")
    c.ok(all(server_env(s) == run.line_url("a") for s in servers), f"app-server env CODEX_AUTHAPI_BASE_URL = {[server_env(s) for s in servers]}")
    argv = infra.cmdline(main)
    c.ok("--remote-debugging-port=0" in argv or "--remote-debugging-port=" in argv, "app started with --remote-debugging-port (Chromium picks a free port; DevToolsActivePort)")
    c.ok(cdp_port(run) == int(port.group(1)), f"DevToolsActivePort = {cdp_port(run)}")
    p = onboarding(run, c)
    c.ok(p["composer"] and not p["login"], "renderer shows the composer, not the login page")
    whoami = [e for e in infra.fwd_entries(run, "a", t0) if e.get("path", "").startswith("/v1/user-auth-credential/whoami") and e.get("originator") == "Codex Desktop"]
    check = [e for e in infra.fwd_entries(run, "a", t0) if e.get("path", "").startswith("/backend-api/wham/accounts/check")]
    c.ok(whoami and all(e["status"] == 200 for e in whoami), f"PAT whoami on line A from the app: {fmt_entry(whoami[0]) if whoami else None}")
    c.ok(check and check[-1]["status"] == 200 and check[-1]["auth_kind"] == "pat", f"wham/accounts/check on line A: {fmt_entry(check[-1]) if check else None}")
    titles = [r["title"] for r in p["rows"]]
    c.ok(any(t.startswith("LEGACY-ALPHA") for t in titles) and any(t.startswith("LEGACY-BETA") for t in titles),
         f"migrated legacy threads listed in recents: {titles}")
    c.info("the legacy 'exec' thread is migrated too but the app never lists exec threads (bundle filters source kind exec)")
    since = time.time()
    send_turn(run, "E2E-T1: reply with exactly the text PONG-1")
    line, e = wait_turn(run, "E2E-T1", since)
    c.ok(line == "a" and e["auth_kind"] == "pat", f"turn 1 -> {fmt_entry(e)}")
    run.d["thread"] = probe(run)["conversation"]
    run.d["t1_inputs"] = e["n_input"]
    run.save()
    c.ok(bool(run.d["thread"]), f"turn 1 opened thread {run.d['thread']}")
    c.info("screenshot: " + shot(run, "s2-after-turn1"))
    finish(run, "s2", c, main_pid=main, app_servers=servers)


@scenario("s3")
def s3(run):
    c = Check()
    main0, servers0 = app_procs(run)
    thread0 = probe(run)["conversation"]
    res = spb_stream(run, "switch", "--line", "b", "--countdown", "0", "--yes", label="s3-switch-b")
    c.ok(res["rc"] == 0 and "Hot restart of the Codex core" in res["all"], f"plan: hot restart; exit 0 [{res['log']}]")
    c.ok("still goes through the line" in res["all"], "plan notes that auth stays on the launch-time line A until a full restart")
    main1, servers1 = app_procs(run)
    c.ok(main1 == main0, f"main pid unchanged: {main0} -> {main1}")
    c.ok(set(servers1).isdisjoint(servers0) and servers1, f"app-server replaced: {servers0} -> {servers1}")
    c.ok(all(server_env(s) == run.line_url("a") for s in servers1), "new app-server inherits CODEX_AUTHAPI_BASE_URL = line A (main process env)")
    t_restart = phase_at(res, "Restarting")
    started = [proc_started_wall(s) for s in servers1]
    first_b = [e for e in infra.fwd_entries(run, "b", res["t0"]) if e.get("originator") == "Codex Desktop"]
    timing = {
        "spb_total_s": res["secs"],
        "execute_elapsed_s": elapsed_of(res),
        "restarting_phase_at_s": t_restart,
        "new_app_server_started_at_s": round(min(started) - res["t0"], 2) if started and started[0] else None,
        "first_app_request_on_B_at_s": round(first_b[0]["ts"] - res["t0"], 2) if first_b else None,
        "verified_done_at_s": phase_at(res, "Done"),
    }
    c.info("timing: " + json.dumps(timing))
    gui = (timing["execute_elapsed_s"] or 0) + 5
    c.info(f"GUI-equivalent: default 5 s countdown + {timing['execute_elapsed_s']} s execution = {gui:.1f} s until the toast; the window stays usable throughout (the core is down ~1 s)")
    cfg = flatten(config(run))
    c.ok(cfg.get("openai_base_url") == run.line_url("b") + "/backend-api/codex" and cfg.get("chatgpt_base_url") == run.line_url("b") + "/backend-api/",
         "config now points at line B")
    prof = next((x for x in json.loads((run.data_dir / "profiles.json").read_text()).get("profiles", []) if x.get("id") == "e2e"), {})
    lines = [prof.get("line_id"), *(prof.get("backup_lines") or [])]
    # By design (executor after_success): the line chosen in a switch becomes the
    # profile's primary line; s11 restores a > b before it relies on the order.
    c.ok(lines == ["b", "a"], f"switching to line B made it the profile's primary line: {lines[0]} > {lines[1:]}")
    p = probe(run)
    c.ok(p["conversation"] == thread0 and not p["login"], f"window kept the open thread {thread0}")
    since = time.time()
    send_turn(run, "E2E-T2: reply with exactly the text PONG-2")
    line, e = wait_turn(run, "E2E-T2", since)
    c.ok(line == "b" and e["auth_kind"] == "pat", f"turn 2 -> {fmt_entry(e)}")
    c.ok(e["n_input"] > run.d["t1_inputs"], f"turn 2 carries the history: {e['n_input']} input items vs {run.d['t1_inputs']} in turn 1")
    c.ok(probe(run)["conversation"] == thread0, "turn 2 stayed in the same thread")
    run.d["t2_inputs"] = e["n_input"]
    run.save()
    c.info("screenshot: " + shot(run, "s3-after-turn2"))
    finish(run, "s3", c, timing=timing)


@scenario("s4")
def s4(run):
    c = Check()
    main0, servers0 = app_procs(run)
    res = spb_stream(run, "switch", "--auth", "apikey", "--countdown", "0", "--yes", label="s4-apikey")
    main1, servers1 = app_procs(run)
    c.ok(res["rc"] == 0 and "Hot restart" in res["all"], f"PAT -> API key planned and run as a hot restart, exit 0 in {res['secs']} s [{res['log']}]")
    c.ok(main1 == main0 and set(servers1).isdisjoint(servers0), f"main {main0} kept, app-server {servers0} -> {servers1}")
    c.ok(auth_shape(run) == ["OPENAI_API_KEY", "auth_mode"], f"auth.json API-key shaped: {auth_shape(run)}")
    cfg = flatten(config(run))
    c.ok("chatgpt_base_url" not in cfg, "chatgpt_base_url removed in API-key mode")
    c.ok(cfg.get("web_search") == "live" and cfg.get("features.fast_mode") is True,
         "the profile's Codex settings stay in API-key mode (Krill's recommended keys are profile settings; open product question)")
    since = time.time()
    send_turn(run, "E2E-T3: reply with exactly the text PONG-3")
    line, e = wait_turn(run, "E2E-T3", since)
    c.ok(line == "b" and e["auth_kind"] == "bearer", f"turn 3 has no chatgpt-account-id header: {fmt_entry(e)}")
    res2 = spb_stream(run, "switch", "--auth", "pat", "--countdown", "0", "--yes", label="s4-pat")
    main2, servers2 = app_procs(run)
    c.ok(res2["rc"] == 0 and "Hot restart" in res2["all"] and main2 == main0, f"back to PAT hot (main {main2}), exit 0 [{res2['log']}]")
    c.ok(auth_shape(run) == ["OPENAI_API_KEY", "personal_access_token"], "auth.json PAT shaped again")
    since = time.time()
    send_turn(run, "E2E-T4: reply with exactly the text PONG-4")
    line, e = wait_turn(run, "E2E-T4", since)
    c.ok(line == "b" and e["auth_kind"] == "pat", f"turn 4 PAT again: {fmt_entry(e)}")
    finish(run, "s4", c)


LONG = ("Without using any tools, write a long essay of at least 2000 words about the history of {topic}, "
        "with headings. Start writing immediately; do not ask questions and do not answer in JSON.")
LONG_TOPICS = ["the printing press", "the telescope", "the steam engine", "cartography", "the telegraph", "paper making"]


def rollout_turns(run, thread_id):
    """Turns of a thread from its rollout: {turn_id: {"text", "started", "end", "end_at"}}."""
    files = glob.glob(f"{run.codex_home}/sessions/**/rollout-*{thread_id}.jsonl", recursive=True)
    turns = {}
    if not files:
        return turns
    for raw in open(files[0], encoding="utf-8"):
        try:
            e = json.loads(raw)
        except json.JSONDecodeError:
            continue
        p = e.get("payload", {})
        if e.get("type") != "event_msg":
            continue
        kind = p.get("type")
        tid = p.get("turn_id")
        if kind == "task_started":
            tid = (p.get("turn_attribution") or {}).get("turn_id") or tid
            turns.setdefault(tid, {})["started"] = e["timestamp"]
        elif kind == "item_completed" and (p.get("item") or {}).get("type") == "UserMessage":
            text = " ".join(c.get("text", "") for c in p["item"].get("content", []) if isinstance(c, dict))
            turns.setdefault(tid, {}).setdefault("text", text)
        elif kind in ("task_complete", "turn_aborted"):
            t = turns.setdefault(tid, {})
            t["end"], t["end_at"] = kind, e["timestamp"]
    return turns


def turn_outcome(run, thread_id, marker):
    for tid, t in rollout_turns(run, thread_id).items():
        if (t.get("text") or "").startswith(marker):
            return t.get("end"), t.get("end_at")
    return None, None


def iso_to_epoch(s):
    from datetime import datetime
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()


def start_long_turn(run, c, tag, used):
    """Send a long turn and make sure it is still running a few seconds later.
    The forwarders hold the end of an "E2E-LONG" turn's stream for 40 s
    (linefwd.py), so this does not depend on how long the model's answer is;
    retries with another topic if a turn still ends early."""
    for _ in range(3):
        topic = next(t for t in LONG_TOPICS if t not in used)
        used.append(topic)
        marker = f"{tag}-{len(used)}"
        send_turn(run, f"{marker}: " + LONG.format(topic=topic))
        infra.wait_for(f"{marker} streaming", lambda: probe(run)["stop"], 60)
        # Planning plus a 5 s countdown take ~10 s: the turn must outlive that.
        time.sleep(8)
        if probe(run)["stop"]:
            return marker
        c.info(f"{marker} ({topic}) ended within seconds (short model reply); sending another long turn")
    raise RuntimeError(f"no long-running turn after 3 attempts ({tag})")


@scenario("s5")
def s5(run):
    c = Check()
    thread = probe(run)["conversation"]
    cfg_sha = sha(run.codex_home / "config.toml")
    main0, servers0 = app_procs(run)
    used = []
    since = time.time()
    long1 = start_long_turn(run, c, "E2E-LONG", used)
    res = spb_stream(run, "switch", "--line", "a", "--countdown", "5", label="s5-no-force")
    c.ok(res["rc"] == 5 and "Cancelled; nothing was changed" in res["all"], f"no --force/--wait-turn: exit 5 (cancelled) [{res['log']}]")
    busy = [l for _, _, l in res["lines"] if "is running" in l]
    c.ok(bool(busy), f"busy turn detected: {busy[:2]}")
    c.ok(bool(busy) and not any("untitled" in b for b in busy), "busy message names the thread by its sidebar title")
    c.ok(sha(run.codex_home / "config.toml") == cfg_sha and app_procs(run)[1] == servers0, "zero writes, app-server untouched")
    still = turn_outcome(run, thread, long1)[0] is None
    c.info(f"{long1} still running after the cancelled switch: {still}")
    # --wait-turn: held until the long turn ends, then switched.
    res = spb_stream(run, "switch", "--line", "a", "--countdown", "0", "--wait-turn", label="s5-wait-turn", timeout=900)
    main1, servers1 = app_procs(run)
    end, end_at = turn_outcome(run, thread, long1)
    new_start = min(proc_started_wall(s) for s in servers1) if servers1 else None
    c.ok(res["rc"] == 0 and main1 == main0 and set(servers1).isdisjoint(servers0), f"--wait-turn switched after waiting, exit 0 in {res['secs']} s [{res['log']}]")
    waited = [l for _, _, l in res["lines"] if "is running" in l or "Waiting" in l]
    c.ok(bool(waited), f"--wait-turn reported the running turn while waiting: {waited[:2]}")
    c.ok(end == "task_complete" and new_start and iso_to_epoch(end_at) <= new_start + 1.5,
         f"{long1} completed ({end} at {end_at}) before the new app-server started ({time.strftime('%H:%M:%S', time.localtime(new_start)) if new_start else None})")
    posts = [e for e in infra.responses_posts(run, "b", since) if (e.get("last_user") or "").startswith(long1)]
    c.ok(bool(posts), f"{long1} ran on line B: {fmt_entry(posts[0]) if posts else None}")
    # --force: proceeds and the in-flight turn is aborted.
    servers1 = app_procs(run)[1]
    long2 = start_long_turn(run, c, "E2E-LONG", used)
    res = spb_stream(run, "switch", "--line", "b", "--countdown", "5", "--force", label="s5-force")
    main2, servers2 = app_procs(run)
    c.ok(res["rc"] == 0 and main2 == main0 and set(servers2).isdisjoint(servers1), f"--force: switched to B while busy, exit 0 [{res['log']}]")
    end = infra.wait_for(f"{long2} outcome", lambda: turn_outcome(run, thread, long2)[0], 30)
    c.ok(end == "turn_aborted", f"{long2} aborted by the restart (rollout: {turn_outcome(run, thread, long2)})")
    p = probe(run)
    body = acall(_with_cdp(run, lambda cc: cc.body_text()))
    i = body.rfind(long2)
    c.info(f"UI right after the forced switch: conversation={p['conversation']} (same={p['conversation'] == thread}), stop button={p['stop']}, login={p['login']}, "
           f"user message kept={i >= 0}, partial assistant text shown={len(body) - i > 400 if i >= 0 else False}")
    c.info("screenshot: " + shot(run, "s5-after-force"))
    t_force = time.time()
    infra.wait_for("the aborted turn leaves the UI", lambda: not probe(run)["stop"], 60)
    c.info(f"the stop button of the aborted turn disappeared {time.time() - t_force:.1f} s later (UI reconnected to the new core)")
    since = time.time()
    send_turn(run, "E2E-T5: reply with exactly the text PONG-5")
    line, e = wait_turn(run, "E2E-T5", since)
    c.ok(line == "b", f"thread still usable after the forced switch, next turn on B: {fmt_entry(e)}")
    finish(run, "s5", c)


@scenario("s8")
def s8(run):
    c = Check()
    codex = run.d["app_dir"] + "/resources/codex"
    t0 = time.time()
    r = run.run(["bash", "-lc", f'echo "authapi=$CODEX_AUTHAPI_BASE_URL"; exec "{codex}" exec --skip-git-repo-check "E2E-CLI: reply with exactly the text CLI-PONG"'],
                cwd=run.home + "/proj", stdin="", timeout=240, label="s8 codex exec")
    c.ok(r["rc"] == 0 and "CLI-PONG" in r["out"], "bundled codex exec (login shell, isolated HOME/CODEX_HOME) completed")
    c.ok(f"authapi={run.line_url('b')}" in r["out"], "login shell picked up CODEX_AUTHAPI_BASE_URL=<line B> from Superba's rc block")
    ents = infra.fwd_entries(run, "b", t0)
    who = [e for e in ents if e.get("originator") == "codex_exec" and "whoami" in e.get("path", "")]
    post = [e for e in infra.responses_posts(run, "b", t0) if e.get("originator") == "codex_exec"]
    c.ok(bool(who) and who[0]["status"] == 200, f"PAT whoami on line B: {fmt_entry(who[0]) if who else None}")
    c.ok(bool(post) and post[0]["auth_kind"] == "pat", f"turn on line B: {fmt_entry(post[0]) if post else None}")
    other = [e for l in run.d["ports"] if l != "b" for e in infra.fwd_entries(run, l, t0) if e.get("originator") == "codex_exec"]
    c.ok(not other, f"no codex_exec request on other lines ({len(other)})")
    side = [fmt_entry(e) for e in ents if e.get("status") not in (200, None) and e.get("originator") != "Codex Desktop"]
    if side:
        c.info(f"non-200 side requests from Codex (relay endpoints Codex probes): {side}")
    finish(run, "s8", c)


@scenario("s6")
def s6(run):
    c = Check()
    main0, servers0 = app_procs(run)
    c.ok(all(server_env(s) == run.line_url("a") for s in servers0), f"precondition: app launched with auth origin A ({[server_env(s) for s in servers0]})")
    thread0 = probe(run)["conversation"]
    pid = infra.stop_forwarder(run, "a")
    c.info(f"stopped forwarder A (pid {pid}): the launch-time auth origin is down")
    try:
        res = spb_stream(run, "switch", "--line", "c", "--countdown", "0", label="s6-plan")
        c.ok(res["rc"] == 4 and "Full restart of ChatGPT" in res["all"], f"plan: full restart, needs confirmation (exit 4) [{res['log']}]")
        c.ok("auth address of the line ChatGPT was started with is unavailable" in res["all"], "reason: launch-time auth address unavailable (rule 7)")
        c.ok("Hot-switch with a plain API key instead" in res["all"], "alternative offered: hot switch with a plain API key")
        pj = run.spb("switch", "--line", "c", "--countdown", "0", "--dry-run", json_out=True, label="s6 plan json")
        prim = ((pj.get("json") or {}).get("plan") or {}).get("primary")
        c.ok(isinstance(prim, dict) and "auth_api_unhealthy" in json.dumps(prim), f"plan.primary = {json.dumps(prim)}")
        res = spb_stream(run, "switch", "--line", "c", "--countdown", "0", "--allow-full-restart", label="s6-full-restart")
        main1, servers1 = infra.wait_for("relaunched app", lambda: (lambda r: r if r[0] and r[1] else None)(app_procs(run)), 60)
        c.ok(res["rc"] == 0 and "Switched" in res["all"], f"full restart executed, exit 0 in {res['secs']} s [{res['log']}]")
        c.ok(main1 != main0 and not infra.pid_alive(main0), f"app relaunched: main {main0} -> {main1}")
        c.ok(all(server_env(s) == run.line_url("c") for s in servers1), f"new env CODEX_AUTHAPI_BASE_URL = line C ({[server_env(s) for s in servers1]})")
        port = cdp_port(run)
        c.ok("--remote-debugging-port" in infra.cmdline(main1), f"relaunched with CDP (port {port})")
        p = infra.wait_for("renderer ready", lambda: (lambda x: x if x["composer"] else None)(probe(run)), 60)
        c.ok(not p["login"], "no login page after the relaunch")
        who = [e for e in infra.fwd_entries(run, "c", res["t0"]) if "whoami" in e.get("path", "") and e.get("originator") == "Codex Desktop"]
        c.ok(bool(who) and who[0]["status"] == 200, f"PAT whoami now on line C: {fmt_entry(who[0]) if who else None}")
        c.info(f"thread before: {thread0}; after relaunch: {p['conversation']} (returned_to: {'yes' if p['conversation'] == thread0 else 'no'})")
        since = time.time()
        send_turn(run, "E2E-T6: reply with exactly the text PONG-6")
        line, e = wait_turn(run, "E2E-T6", since)
        c.ok(line == "c" and e["auth_kind"] == "pat", f"turn after the full restart: {fmt_entry(e)}")
        c.info("screenshot: " + shot(run, "s6-after-full-restart"))
    finally:
        infra.start_forwarder(run, "a")
        c.info("forwarder A restarted on the same port")
    # Back to line B (hot: the app now runs with auth origin C, which is healthy).
    res = spb_stream(run, "switch", "--line", "b", "--countdown", "0", "--yes", label="s6-back-to-b")
    c.ok(res["rc"] == 0 and "Hot restart" in res["all"], f"back to line B as a hot switch (auth origin C healthy) [{res['log']}]")
    c.info("rule 6 (PAT with no CODEX_AUTHAPI_BASE_URL in the app) cannot occur for an app Superba launched; it is covered by rules_tests.rs")
    finish(run, "s6", c)


def xdo(run, *args):
    return subprocess.run(["xdotool", *args], env=run.base_env(), capture_output=True, text=True)


@scenario("s7")
def s7(run):
    c = Check()
    if f"fwd-a" not in run.d["pids"]:
        infra.start_forwarder(run, "a")
    main0, _ = app_procs(run)
    dry = spb_stream(run, "restore", "original", "--dry-run", label="s7-dry-run")
    c.ok(dry["rc"] == 4 and "--provider keep-openai|reverse-migrate|no-migrate" in dry["all"], f"restore asks how to treat the provider [{dry['log']}]")
    res = spb_stream(run, "restore", "original", "--provider", "reverse-migrate", "--yes", label="s7-restore")
    c.ok(res["rc"] == 0 and "Restored Original configuration" in res["all"], f"restore original --provider reverse-migrate exit 0 in {res['secs']} s [{res['log']}]")
    c.ok("Full restart of ChatGPT" in res["all"], "reverse migration ran as a full restart (all Codex processes closed first)")
    after = (run.codex_home / "config.toml").read_text()
    before = run.d["pre"]["config"]
    diff = text_diff(before, after, "config.toml")
    c.info("config.toml diff vs pre-takeover: " + save(run, "s7-config.diff", diff))
    removed = [l for l in diff.splitlines() if l.startswith("-") and not l.startswith("---")]
    c.ok(not removed, f"every pre-takeover line is back verbatim (no removed/changed lines): {removed}")
    fb, fa = flatten(tomllib.loads(before)), flatten(tomllib.loads(after))
    c.ok(all(fa.get(k) == v for k, v in fb.items()), "every pre-takeover key has its original value (model_provider = \"krill\")")
    extra = sorted(k for k in fa if k not in fb)
    owned = [k for k in extra if k.split(".")[0] in ("openai_base_url", "chatgpt_base_url", "web_search", "suppress_unstable_features_warning", "features", "analytics",
                                                    "hide_agent_reasoning", "project_doc_max_bytes", "tool_output_token_limit", "model_verbosity")]
    c.ok(not owned, f"no Superba-owned key left behind: {owned}")
    c.info(f"keys added by the desktop app itself during the run (not owned by Superba, kept): {sorted({k.rsplit('.', 1)[0] if k.startswith('mcp_servers') else k.split('.')[0] for k in extra})}")
    c.ok(sha(run.codex_home / "auth.json") == run.d["pre"]["auth_sha"], "auth.json byte-identical to the pre-takeover file")
    for rel, content in run.d["pre"]["rc"].items():
        f = Path(run.home) / rel
        now = f.read_text() if f.exists() else None
        c.ok(now == content, f"{rel} {'byte-identical' if content is not None else 'absent'} as before")
    sq = legacy.sqlite_providers(run.codex_home)
    ro = legacy.rollout_providers(run.codex_home)
    # Plan 10.4 option 2: the reverse migration takes the takeover ledger plus the
    # sessions created while a Krill site was active; a session created on another
    # site (s12's custom compatible site) is not Krill's and stays on "openai".
    # A thread belongs to the site whose line carried its first request (the
    # forwarders log Codex's conversation key, the thread id): line D is the
    # custom site (s12, s18, s19); A, B, C and E are Krill-kind.
    first = {}
    for line in run.d["ports"]:
        for e in infra.fwd_entries(run, line):
            t = e.get("session")
            if t and (t not in first or e["ts"] < first[t][0]):
                first[t] = (e["ts"], line)
    other_site = {t for t in sq if first.get(t, (0, "a"))[1] == "d"}
    if run.d.get("compat_thread"):
        other_site.add(run.d["compat_thread"])
    want = {tid: ("openai" if tid in other_site else "krill") for tid in sq}
    c.ok(sq and other_site <= set(sq) and all(sq[t]["provider"] == w for t, w in want.items()),
         f"state_5.sqlite: all {len(sq) - len(other_site)} Krill-era threads back on 'krill' (incl. the ones created during the run), "
         f"the custom site's thread {sorted(other_site)} still 'openai': {sorted({t: v['provider'] for t, v in sq.items()}.items())}")
    c.ok(ro and all(ro.get(t, {}).get("provider") == w for t, w in want.items() if t in ro) and set(ro) == set(sq),
         f"rollout session_meta: the same {len(ro)} providers as the database")
    save(run, "s7-providers-after.json", json.dumps({"sqlite": sq, "rollout": ro}, indent=1))
    se = run.spb("sessions", label="s7 sessions")
    c.ok(re.search(r"\| krill \| openai: \d+ \|", se["out"]) is not None, "migration ledger records the reverse migration")
    main1, servers1 = app_procs(run)
    if main1:
        c.info(f"app relaunched by the restore: main {main0} -> {main1}")
        p = infra.wait_for("renderer", lambda: (lambda x: x if x["composer"] else None)(probe(run)), 60)
        listed = {r["id"].split(":")[-1] for r in p["rows"] if r.get("id")}
        expected = {tid for tid, v in sq.items() if v["source"] != "exec" and v["provider"] == "krill"}
        c.ok(expected <= listed, f"the app (provider 'krill' again) lists every reverse-migrated non-exec thread ({len(expected)}): {[r['title'] for r in p['rows']]}")
        if other_site:
            c.info(f"the custom site's thread (provider 'openai') listed by the app: {bool(other_site & listed)}")
        since = time.time()
        send_turn(run, "E2E-T8: reply with exactly the text PONG-8")
        line, e = wait_turn(run, "E2E-T8", since)
        c.ok(line == "a" and e["auth_kind"] == "bearer", f"turn on the restored legacy config ([model_providers.krill] on line A, API key): {fmt_entry(e)}")
    finish(run, "s7", c)


@scenario("s9")
def s9(run):
    c = Check()
    env = dict(os.environ)
    # The shared build directory and settings of the repository's disk policy.
    env.update({"SUPERBA_REMOTE_IT": "1", "CARGO_TARGET_DIR": os.environ.get("CARGO_TARGET_DIR") or str(infra.REPO / "target-v2-shared"),
                "CARGO_INCREMENTAL": "0", "CARGO_PROFILE_DEV_DEBUG": "line-tables-only", "CARGO_PROFILE_TEST_DEBUG": "line-tables-only"})
    for k in ("CODEX_AUTHAPI_BASE_URL", "CODEX_HOME"):
        env.pop(k, None)
    t0 = time.time()
    p = subprocess.run(["cargo", "test", "-p", "superba-remote", "--test", "docker_it", "--", "--ignored", "--nocapture"],
                       cwd=str(infra.REPO), env=env, capture_output=True, text=True, timeout=1800)
    out = p.stdout + p.stderr
    save(run, "s9-docker-it.log", out)
    m = re.search(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored", out)
    c.ok(p.returncode == 0 and m and m.group(1) == "ok", f"SUPERBA_REMOTE_IT=1 cargo test -p superba-remote --test docker_it -- --ignored: {m.group(0) if m else 'no result'} in {time.time() - t0:.0f} s")
    names = re.findall(r"^test (\S+) \.\.\. (\w+)", out, re.M)
    c.info(f"tests: {names}")
    c.info("SKIPPED: app-level remote hot restart (hostId + killCodexProcess through the desktop app) is out of scope: it needs the app connected to an SSH host, which the isolated app has no account/remote-connection state for")
    finish(run, "s9", c)


import scenarios_v2  # noqa: E402,F401  (registers s10-s15)
import scenarios_v3  # noqa: E402,F401  (registers s16-s23)


# ---------------------------------------------------------------------------
# running and reporting
# ---------------------------------------------------------------------------


def run_one(run, sid):
    LAST_CHECK.clear()
    try:
        SCENARIOS[sid](run)
    except Exception as e:
        tb = traceback.format_exc()
        so_far = LAST_CHECK["c"].items if "c" in LAST_CHECK else []
        run.record(sid, NAMES[sid], "fail", so_far + [f"ERROR: {e!r}"], traceback=tb[-3000:])
        try:
            shot(run, f"{sid}-error")
        except Exception:
            pass
    if run.d["report"].get(sid, {}).get("status") == "fail":
        notes = recover_after_failure(run)
        if notes:
            run.note(sid, "after the failure the harness restored: " + "; ".join(notes))


def recover_after_failure(run):
    """Put back what a scenario that stopped half-way may have left, so one
    failure does not fail the scenarios after it: every forwarder running and
    forwarding, Codex not routed and no router service, the package source in
    its normal mode."""
    notes = []
    for line in list(run.d.get("ports", {})):
        infra.set_fault(run, line, None)
        if f"fwd-{line}" not in run.d["pids"]:
            infra.start_forwarder(run, line)
            notes.append(f"forwarder {line} restarted")
    if run.d.get("pkg_port"):
        infra.set_pkg_mode(run, "normal")
    try:
        st = (run.spb("router", "status", json_out=True, label="recover router status").get("json")) or {}
    except Exception:
        st = {}
    if st.get("codex_routed") or (st.get("settings") or {}).get("enabled"):
        r = run.spb("router", "disable", "--countdown", "0", "--yes", "--force", label="recover router disable", timeout=300)
        notes.append(f"router mode off (exit {r['rc']})")
    if st.get("installed"):
        r = run.spb("router", "uninstall", label="recover router uninstall")
        notes.append(f"router service removed (exit {r['rc']})")
    return notes


def run_all(run):
    meta(run)
    for sid in ORDER:
        print(f"== {sid}: {NAMES[sid]}", flush=True)
        t = time.time()
        run_one(run, sid)
        r = run.d["report"][sid]
        print(f"   {r['status'].upper()} ({time.time() - t:.0f} s)", flush=True)
        for line in r["evidence"]:
            if line.startswith("FAIL") or line.startswith("ERROR"):
                print("   " + line[:300], flush=True)


def meta(run):
    head = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=str(infra.REPO), capture_output=True, text=True).stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], cwd=str(infra.REPO), capture_output=True, text=True).stdout.strip()
    meta = json.loads((Path(run.d["app_dir"]) / "resources" / "linux-package-metadata.json").read_text()) if (Path(run.d["app_dir"]) / "resources" / "linux-package-metadata.json").exists() else {}
    codex_v = subprocess.run([run.d["app_dir"] + "/resources/codex", "--version"], capture_output=True, text=True).stdout.strip()
    run.d["meta"] = {"superba_head": head + ("+dirty" if dirty else ""), "app": meta.get("version") or meta.get("appVersion"),
                     "codex": codex_v, "display": run.d.get("display"), "home": run.home, "started": time.strftime("%Y-%m-%d %H:%M:%S")}
    run.save()


def write_report(run):
    rep = run.d.get("report", {})
    lines = [f"# Superba e2e run {run.d['run_id']}", ""]
    m = run.d.get("meta", {})
    lines.append(f"Superba {m.get('superba_head')} · desktop app {m.get('app')} · {m.get('codex')} · display {m.get('display')} · HOME {m.get('home')}")
    lines.append("")
    lines.append("| # | Scenario | Result |")
    lines.append("|---|---|---|")
    for sid in ORDER:
        r = rep.get(sid)
        lines.append(f"| {sid[1:]} | {NAMES[sid]} | {(r or {}).get('status', 'not run').upper()} |")
    for sid in ORDER:
        r = rep.get(sid)
        if not r:
            continue
        lines += ["", f"## {sid[1:]}. {NAMES[sid]} — {r['status'].upper()}", ""]
        lines += [f"- {e}" for e in r["evidence"]]
        lines += [f"- NOTE: {n}" for n in r.get("notes", [])]
        if r.get("details", {}).get("timing"):
            lines.append(f"- timing: `{json.dumps(r['details']['timing'])}`")
        if r.get("details", {}).get("traceback"):
            lines += ["", "```", r["details"]["traceback"], "```"]
    (run.out / "report.md").write_text("\n".join(lines) + "\n")


def summary_line(run):
    rep = run.d.get("report", {})
    return " ".join(f"{sid}={rep.get(sid, {}).get('status', '-')}" for sid in ORDER) + f"  report: {run.out / 'report.md'}"
