"""Scenarios of the Superba e2e run: Codex settings (s11), a custom compatible
site (s12), the local router (s13), account data (s14), the GUI in every theme and
language with the live router and the switch overlay (s10) and a tracked-tree check
(s15). Registered into scenarios.SCENARIOS; the order is scenarios.ORDER.

Same rules as scenarios.py: real binaries copied into the run, the real desktop app
and Codex, an isolated HOME, every process inside the run's namespace, and every
assertion recorded as evidence.
"""

import json
import os
import re
import signal
import stat
import subprocess
import time
from pathlib import Path

import infra
from scenarios import (
    LONG,
    Check,
    _with_cdp,
    acall,
    app_procs,
    auth_shape,
    config,
    finish,
    flatten,
    fmt_entry,
    probe,
    proc_started_wall,
    save,
    scenario,
    send_turn,
    shot,
    spb_stream,
    turn_outcome,
    wait_turn,
    xdo,
)

CATALOG_VERSION = "0.162.0-alpha.17.2"
# Keys only a newer Codex knows (catalog 0.163.0), skipped for 0.162.0-alpha.17.2.
NEWER_KEY = "include_environment_context_time"
STABLE_KEYS = [("hide_agent_reasoning", "true", True), ("project_doc_max_bytes", "65536", 65536)]
FEATURE = ("features.memories", "true", True)


def codex_bin(run):
    return run.d["app_dir"] + "/resources/codex"


def features_list(run):
    """`codex features list` of the bundled Codex in the isolated home:
    {name: (stage, enabled)} plus the raw text."""
    r = run.run([codex_bin(run), "features", "list"], label="codex features list", timeout=60)
    rows = {}
    for line in r["out"].splitlines():
        parts = line.split()
        if len(parts) >= 3 and parts[-1] in ("true", "false"):
            rows[parts[0]] = (" ".join(parts[1:-1]), parts[-1] == "true")
    return rows, r


def json_of(r):
    return r.get("json") or {}


def changes_of(diff_json):
    out = {}
    for ch in (diff_json.get("settings") or {}).get("changes", []):
        reason = ch.get("reason")
        out[ch["path"]] = (ch.get("from"), ch.get("to"), reason if isinstance(reason, str) else json.dumps(reason))
    return out


def plan_targets(diff_json):
    return (diff_json.get("settings") or {}).get("targets", [])


def hot_switch_ok(c, res, before, after, what):
    (main0, servers0), (main1, servers1) = before, after
    c.ok(res["rc"] == 0 and "Hot restart" in res["all"] and main1 == main0 and servers1 and set(servers1).isdisjoint(servers0),
         f"{what}: hot restart, exit 0 in {res['secs']} s, main {main0} kept, app-server {servers0} -> {servers1} [{res['log']}]")


# ---------------------------------------------------------------------------
# s11 settings
# ---------------------------------------------------------------------------


def profile_of(run, pid):
    data = json.loads((run.data_dir / "profiles.json").read_text())
    return next((p for p in data.get("profiles", []) if p.get("id") == pid), {})


def reset_e2e_lines(run):
    """Put profile e2e's line order back to A, B: `spb switch --line X` makes X the primary line
    (executor after_success), so a scenario that follows s3-s6 or runs alone must not rely on it."""
    return run.spb("profile", "edit", "e2e", "--line", "a", "--line", "b", label="reset e2e line order")


def profile_lines(p):
    return [p.get("line_id"), *(p.get("backup_lines") or [])]


@scenario("s11")
def s11(run):
    c = Check()
    p = profile_of(run, "e2e")
    c.info(f"profile e2e after the v1 scenarios: lines {profile_lines(p)}, auth {p.get('auth')} "
           "(`spb switch --line X` makes X the primary line and `--auth` stores a manual choice: executor after_success)")
    r = run.spb("profile", "edit", "e2e", "--line", "a", "--line", "b", "--auth", "auto", label="s11 profile edit")
    p = profile_of(run, "e2e")
    c.ok(r["rc"] == 0 and profile_lines(p) == ["a", "b"] and p.get("auth") == "auto",
         f"spb profile edit e2e --line a --line b --auth auto: lines {profile_lines(p)}, auth {p.get('auth')}")
    # This computer's config.toml lists every catalog row; `--profile` only the
    # profile's own settings (user bug 72).
    view = json_of(run.spb("settings", "list", json_out=True, label="s11 settings list"))
    c.ok(view.get("catalog_version") == CATALOG_VERSION,
         f"settings catalog of the installed Codex: {view.get('catalog_version')} (match {json.dumps(view.get('catalog_match'))})")
    c.info("consumers of this home: " + json.dumps([(x.get("label"), x.get("version")) for x in view.get("consumers", [])]))
    rows = {r["path"]: r for r in view.get("rows", [])}
    c.ok("features.memories" in rows and "hide_agent_reasoning" in rows and NEWER_KEY not in rows,
         f"default view lists stable keys and features ({len(rows)} rows); {NEWER_KEY} (Codex 0.163+) is not offered")
    text_list = run.spb("settings", "list", label="s11 settings list text")
    save(run, "s11-settings-list.txt", text_list["out"])
    c.info("spb settings list (text): evidence/s11-settings-list.txt")

    d0 = json_of(run.spb("settings", "diff", "--profile", "e2e", json_out=True, label="s11 diff before"))
    offered = [m for t in plan_targets(d0) for m in t.get("migrations", [])]
    c.ok(any(m.get("legacy_path") == "features.collab" and m.get("canonical") == "features.multi_agent"
             and m.get("source") == "config" for m in offered),
         f"legacy key in the fixture config.toml offered for migration: {json.dumps(offered)[:300]}")

    for path, lit, _ in [*STABLE_KEYS, FEATURE]:
        r = run.spb("settings", "set", path, lit, "--profile", "e2e", label=f"s11 set {path}")
        c.ok(r["rc"] == 0 and "Saved the settings of profile e2e" in r["out"], f"spb settings set {path} {lit}: exit {r['rc']}")
    g = run.spb("settings", "get", "features.memories", "--profile", "e2e", label="s11 get memories")
    c.ok(g["rc"] == 0 and "Stage" in g["out"] and "Profile" in g["out"], "spb settings get features.memories shows stage, default, current and the managed value")
    gz = run.spb("--locale", "zh-CN", "settings", "get", "hide_agent_reasoning", "--profile", "e2e", label="s11 get zh")
    c.info("zh-CN get (official English + Chinese status): " + " | ".join(l.strip() for l in gz["out"].splitlines()[:4]))

    r = run.spb("settings", "set", NEWER_KEY, "true", "--profile", "e2e", label="s11 set newer key")
    c.ok(r["rc"] != 0 and "not a Codex setting of this Codex version" in r["out"] + r["err"],
         f"{NEWER_KEY} refused without --force (exit {r['rc']}): {(r['err'] or r['out']).strip()[:160]}")
    r = run.spb("settings", "set", NEWER_KEY, "true", "--profile", "e2e", "--force", label="s11 set newer key --force")
    c.ok(r["rc"] == 0, f"stored with --force (exit {r['rc']}): {r['out'].strip()[:200]}")

    r = run.spb("settings", "migrate", "features.collab", "--profile", "e2e", label="s11 migrate")
    c.ok(r["rc"] == 0, f"spb settings migrate features.collab: exit {r['rc']}")

    text_diff = spb_stream(run, "settings", "diff", "--profile", "e2e", label="s11-diff")
    d1 = json_of(run.spb("settings", "diff", "--profile", "e2e", json_out=True, label="s11 diff json"))
    save(run, "s11-diff.json", json.dumps(d1, indent=1))
    ch = changes_of(d1)
    c.info("diff rows: " + json.dumps(ch))
    c.ok(ch.get("hide_agent_reasoning", (None, None))[1] == "true" and ch.get("project_doc_max_bytes", (None, None))[1] == "65536"
         and ch.get("features.memories", (None, None))[1] == "true", "diff preview: the two stable keys and the feature flag are added")
    c.ok(ch.get("features.collab", ("", "x"))[:2] == ("false", None) and "migration" in ch["features.collab"][2]
         and ch.get("features.multi_agent", (None, None))[1] == "false" and "migration" in ch["features.multi_agent"][2],
         "diff preview: features.collab removed and features.multi_agent = false, both as the legacy migration")
    skipped = [s for t in plan_targets(d1) for s in t.get("skipped", [])]
    sk = next((s for s in skipped if s.get("path") == NEWER_KEY), None)
    c.ok(sk is not None and "not_supported" in json.dumps(sk.get("reason")),
         f"{NEWER_KEY} skipped for Codex {CATALOG_VERSION}: {json.dumps(sk)}")
    c.ok(f"Not written: {NEWER_KEY}" in text_diff["all"], f"text preview names the skipped key and its reason [{text_diff['log']}]")
    other_skips = [s.get("path") for s in skipped if s.get("path") != NEWER_KEY]
    c.info(f"other skipped settings of the profile: {other_skips}")

    before = app_procs(run)
    res = spb_stream(run, "switch", "--profile", "e2e", "--countdown", "0", "--yes", label="s11-apply")
    after = app_procs(run)
    hot_switch_ok(c, res, before, after, "settings applied through a hot switch")
    c.ok("hide_agent_reasoning" in res["all"] or "settings" in res["all"].lower(), "the switch preview lists the settings changes")
    cfg = flatten(config(run))
    for path, _, want in [*STABLE_KEYS, FEATURE]:
        c.ok(cfg.get(path) == want, f"config.toml {path} = {cfg.get(path)!r}")
    c.ok(NEWER_KEY not in cfg, f"{NEWER_KEY} not written")
    c.ok("features.collab" not in cfg and cfg.get("features.multi_agent") is False, "legacy features.collab replaced by features.multi_agent = false")
    raw = (run.codex_home / "config.toml").read_text()
    save(run, "s11-config.toml", raw)
    c.ok("# keep: legacy feature name" in raw, "comments next to the migrated key kept")

    feats, r = features_list(run)
    save(run, "s11-features-list.txt", r["out"])
    c.ok(r["rc"] == 0 and feats, f"`codex features list` (bundled {CATALOG_VERSION}, isolated CODEX_HOME): {len(feats)} features")
    c.ok(feats.get("memories") == ("stable", True), f"memories: {feats.get('memories')} (catalog default false; set by the profile)")
    c.ok(feats.get("multi_agent") == ("stable", False), f"multi_agent: {feats.get('multi_agent')} (migrated from collab = false)")
    c.ok(feats.get("fast_mode", ("", False))[1] is True, f"fast_mode (Krill recommended): {feats.get('fast_mode')}")
    gen = Path(run.d["spb"]).parent / "superba-catalog-gen"
    if gen.exists():
        lst = run.work / "features-list.txt"
        lst.write_text(r["out"])
        x = run.run([str(gen), "--catalog", str(infra.REPO / "catalog"), "features-check", "--version", CATALOG_VERSION,
                     "--list", str(lst), "--config", str(run.codex_home / "config.toml")], label="s11 features-check", timeout=60)
        save(run, "s11-features-check.txt", x["out"] + x["err"])
        c.ok(x["rc"] == 0 and " 0 differences" in x["out"],
             f"cross-check `codex features list` vs catalog {CATALOG_VERSION} with this config.toml: {x['out'].strip().splitlines()[-1] if x['out'].strip() else x['err'][:200]}")
    else:
        c.info("superba-catalog-gen not built: cross-check skipped")

    since = time.time()
    send_turn(run, "E2E-S11: reply with exactly the text PONG-S11")
    line, e = wait_turn(run, "E2E-S11", since)
    c.ok(line == "a" and e["status"] == 200, f"turn after the settings switch (profile e2e, primary line A): {fmt_entry(e)}")
    finish(run, "s11", c)


# ---------------------------------------------------------------------------
# s12 custom compatible site
# ---------------------------------------------------------------------------


@scenario("s12")
def s12(run):
    c = Check()
    reset_e2e_lines(run)
    base = f"{run.line_url('d')}/codex/v1"
    r = run.spb("site", "add", "e2e-compat", "--name", "E2E compatible", "--base-url", base,
                "--map", "gpt-6-luna=gpt-5.6-luna", label="s12 site add")
    c.ok(r["rc"] == 0 and "Site e2e-compat added" in r["out"], f"spb site add e2e-compat --base-url <forwarder D>/codex/v1 --map gpt-6-luna=gpt-5.6-luna: exit {r['rc']}")
    sl = run.spb("site", "list", label="s12 site list")
    c.ok("e2e-compat" in sl["out"] and "gpt-6-luna" in sl["out"] + json.dumps(json_of(run.spb("site", "list", json_out=True))),
         "site list shows the custom site with its model map")
    r = run.run([run.d["spb"], "profile", "add", "compat", "--name", "Compat", "--site", "e2e-compat", "--key-stdin"],
                stdin=run.key + "\n", label="s12 profile add")
    c.ok(r["rc"] == 0, f"spb profile add compat --site e2e-compat --key-stdin: exit {r['rc']}")
    t0 = time.time()
    p = spb_stream(run, "site", "probe", "e2e-compat", "--profile", "compat", "--responses", "--model", "gpt-6-luna", label="s12-probe")
    c.ok(p["rc"] == 0 and "Reachable" in p["all"], f"site probe: reachable [{p['log']}]")
    c.ok("Responses: OK with gpt-5.6-luna" in p["all"], "probe's minimal Responses request used the mapped model gpt-5.6-luna")
    c.ok("is not in the site's model list" not in p["all"], "mapped model found in the site's model list")
    pe = [e for e in infra.fwd_entries(run, "d", t0)]
    c.info("probe requests on forwarder D: " + json.dumps(sorted({(e.get("method"), e.get("path", "").split("?")[0], e.get("status"), e.get("model")) for e in pe}, key=str)))
    dry = json_of(run.spb("switch", "--profile", "compat", "--dry-run", json_out=True, label="s12 dry run"))
    plan = dry.get("plan", dry)
    to = plan.get("to") or {}
    c.ok(to.get("auth_mode") == "apikey", f"Automatic resolves to an API key on a compatible site (to: {json.dumps({k: to.get(k) for k in ('site_id', 'line_id', 'auth_mode')})})")
    c.info("plan warnings: " + json.dumps(plan.get("warnings", []))[:600])
    before = app_procs(run)
    res = spb_stream(run, "switch", "--profile", "compat", "--countdown", "0", "--yes", label="s12-switch")
    after = app_procs(run)
    hot_switch_ok(c, res, before, after, "switch to the custom site")
    cfg = flatten(config(run))
    c.ok(cfg.get("openai_base_url") == base and cfg.get("model_provider") == "openai" and "chatgpt_base_url" not in cfg,
         f"config: model_provider = openai, openai_base_url = {cfg.get('openai_base_url')}")
    c.ok(cfg.get("model") == "gpt-5.6-luna", f"direct mode writes the mapped model: model = {cfg.get('model')!r}")
    c.ok(auth_shape(run) == ["OPENAI_API_KEY", "auth_mode"], f"auth.json API-key shaped: {auth_shape(run)}")
    released = {k: cfg.get(k) for k in ("web_search", "features.memories", "features.collab", "features.multi_agent", "hide_agent_reasoning")}
    c.info(f"keys of profile e2e released (pre-takeover values back): {released}")
    since = time.time()
    send_turn(run, "E2E-CS1: reply with exactly the text PONG-CS1")
    line, e = wait_turn(run, "E2E-CS1", since)
    c.ok(line == "d" and e.get("path", "").startswith("/codex/v1/responses") and e["auth_kind"] == "bearer" and e["status"] == 200,
         f"turn in the open thread went through forwarder D: {fmt_entry(e)}")
    c.info(f"model of that request: {e.get('model')} (the open thread keeps its own model; the config default is the mapped name)")
    acall(_with_cdp(run, lambda cc: cc.click_text(["New chat"])))
    time.sleep(2)
    since = time.time()
    send_turn(run, "E2E-CS2: reply with exactly the text PONG-CS2")
    line, e = wait_turn(run, "E2E-CS2", since)
    c.ok(line == "d" and e.get("model") == "gpt-5.6-luna" and e["status"] == 200,
         f"turn in a new thread uses the configured (mapped) model: {fmt_entry(e)}")
    run.d["compat_thread"] = probe(run)["conversation"]
    run.save()
    back = spb_stream(run, "profile", "use", "e2e", "--countdown", "0", "--yes", label="s12-back-to-e2e")
    cfg = flatten(config(run))
    c.ok(back["rc"] == 0 and cfg.get("openai_base_url") == run.line_url("a") + "/backend-api/codex" and cfg.get("model") == "gpt-6-luna",
         f"back to profile e2e (line A, model gpt-6-luna again) [{back['log']}]")
    finish(run, "s12", c)


# ---------------------------------------------------------------------------
# s13 router
# ---------------------------------------------------------------------------


def router_status(run, label):
    return json_of(run.spb("router", "status", json_out=True, label=label))


def admin_lines(run):
    st = infra.router_admin(run) or {}
    out = {}
    for m in st.get("members", []):
        for line in m.get("lines", []):
            out[line["id"]] = {k: line.get(k) for k in ("state", "requests", "failovers_away", "broken_streams", "consecutive_failures", "last_error")}
    return out, st


def events(st, since_ms, kinds=None):
    return [e for e in (st or {}).get("recent", []) if e.get("at_ms", 0) >= since_ms and (not kinds or e.get("kind") in kinds)]


def router_token(run):
    try:
        return json.loads((run.codex_home / "auth.json").read_text()).get("OPENAI_API_KEY")
    except (OSError, json.JSONDecodeError):
        return None


def turn_on(run, c, marker, want_line, what):
    since = time.time()
    send_turn(run, f"{marker}: reply with exactly the text PONG-{marker[4:]}")
    line, e = wait_turn(run, marker, since)
    c.ok(line == want_line and e["status"] == 200 and e["auth_kind"] == "bearer",
         f"{what}: {marker} reached line {line.upper()} through the router (member key, bearer): {fmt_entry(e)}")
    return line, e


def supervisor_pid(run):
    try:
        return int((run.router_dir / "supervisor.pid").read_text().split()[0])
    except (OSError, ValueError, IndexError):
        return None


@scenario("s13")
def s13(run):
    c = Check()
    st = router_status(run, "s13 status before")
    c.ok(not st.get("installed") and not st.get("running") and not (st.get("settings") or {}).get("enabled"),
         "router not installed, router mode off (opt-in, off by default)")
    res = spb_stream(run, "router", "install", "--member", "e2e", label="s13-install")
    st = router_status(run, "s13 status installed")
    port = st.get("port")
    c.ok(res["rc"] == 0 and st.get("installed") and st.get("running") and port,
         f"spb router install --member e2e: installed, running on 127.0.0.1:{port} [{res['log']}]")
    autostart = list((Path(run.home) / ".config" / "autostart").glob("*.desktop"))
    c.ok(bool(autostart), f"service: XDG autostart entry + supervisor in the isolated home ({[p.name for p in autostart]}; no systemd user manager here)")
    sec = run.router_dir / "secrets.json"
    c.ok(sec.exists() and stat.S_IMODE(sec.stat().st_mode) == 0o600, "router key file secrets.json is 0600")
    lines, adm = admin_lines(run)
    c.ok(list(lines) == ["a", "b", "c"], f"member e2e: lines in priority order {list(lines)} (profile a > b, then the site's other lines)")
    c.ok(not st.get("codex_routed"), "installing changed nothing in Codex")
    with open(run.out / "router-status-installed.json", "w") as f:
        json.dump(adm, f, indent=1)

    before = app_procs(run)
    res = spb_stream(run, "router", "enable", "--countdown", "0", "--yes", label="s13-enable")
    after = app_procs(run)
    hot_switch_ok(c, res, before, after, "spb router enable")
    c.ok("forwards them with each site's own key" in res["all"] and "127.0.0.1" in res["all"],
         "the CLI states that traffic goes through a local Superba process")
    cfg = flatten(config(run))
    st = router_status(run, "s13 status enabled")
    c.ok(cfg.get("openai_base_url") == f"http://127.0.0.1:{port}/backend-api/codex" and "chatgpt_base_url" not in cfg and st.get("codex_routed"),
         f"Codex routed: openai_base_url = {cfg.get('openai_base_url')}, no chatgpt_base_url")
    token = router_token(run)
    c.ok(auth_shape(run) == ["OPENAI_API_KEY", "auth_mode"] and token and token != run.key,
         "auth.json: API-key mode with the router token (not the member's key)")
    r = run.spb("router", "uninstall", label="s13 uninstall refused")
    st = router_status(run, "s13 status after refused uninstall")
    c.ok(r["rc"] != 0 and st.get("running") and st.get("codex_routed"),
         f"spb router uninstall refused while Codex is routed (exit {r['rc']}): {(r['err'] or r['out']).strip()[:200]}")

    status, route, _ = infra.router_request(run, "GET", "/backend-api/codex/models?client_version=0.162.0", token=token)
    c.ok(status == 200 and route and route.endswith("/a"), f"direct request through the router: HTTP {status}, x-superba-route: {route}")
    status, route, _ = infra.router_request(run, "GET", "/backend-api/codex/models?client_version=0.162.0")
    c.ok(status == 401, f"request without the router token refused: HTTP {status}")

    main_r, servers_r = app_procs(run)
    t_ms = int(time.time() * 1000)
    _, e1 = turn_on(run, c, "E2E-R1", "a", "routed turn")
    _, e2 = turn_on(run, c, "E2E-R2", "a", "stickiness while healthy")
    c.ok(e1.get("session") and e1.get("session") == e2.get("session"), f"same conversation key on both turns ({(e1.get('session') or '')[:13]}…)")
    lines, adm = admin_lines(run)
    c.ok((lines.get("a") or {}).get("requests", 0) >= 2 and (lines.get("b") or {}).get("requests", 0) == 0,
         f"router status: requests per line a={lines.get('a', {}).get('requests')} b={lines.get('b', {}).get('requests')}")

    # Failover between turns.
    pid = infra.stop_forwarder(run, "a")
    c.info(f"stopped forwarder A (pid {pid}) between turns")
    t_kill = int(time.time() * 1000)
    _, e3 = turn_on(run, c, "E2E-R3", "b", "failover between turns")
    lines, adm = admin_lines(run)
    fo = events(adm, t_kill, ("failover", "line_down"))
    c.ok(any(e.get("line") == "a" for e in fo), f"router events: {json.dumps(fo)[:400]}")
    c.ok(app_procs(run) == (main_r, servers_r), f"no app restart: main {main_r}, app-server {servers_r} unchanged")
    infra.start_forwarder(run, "a")
    c.info("forwarder A back on the same port")
    time.sleep(2)
    _, e4 = turn_on(run, c, "E2E-R4", "b", "after failover the conversation stays on B (no flapping back)")

    # Mid-turn: a new conversation is pinned to A; A dies while the turn streams.
    # A is the first healthy line only once the router's health probes closed its
    # breaker again (they back off while A was down; the run of 2026-10-17 13:19
    # started R5 15 s after A came back, still open, and R5 went to B).
    t_up = time.time()
    infra.wait_for("line A healthy in the router again", lambda: (admin_lines(run)[0].get("a") or {}).get("state") == "healthy", 120, 1)
    c.info(f"line A healthy in the router {time.time() - t_up:.1f} s after the turn on B")
    acall(_with_cdp(run, lambda cc: cc.click_text(["New chat"])))
    time.sleep(2)
    _, e5 = turn_on(run, c, "E2E-R5", "a", "a new conversation starts on the first healthy line")
    thread2 = probe(run)["conversation"]
    marker = "E2E-LONG-R6"
    since = time.time()
    send_turn(run, f"{marker}: " + LONG.format(topic="the lighthouse"))
    hit = infra.wait_for("long turn on A", lambda: [e for e in infra.responses_posts(run, "a", since) if (e.get("last_user") or "").startswith(marker)], 60)
    c.ok(bool(hit), f"{marker} streaming on line A: {fmt_entry(hit[0])}")
    time.sleep(6)
    t_kill = int(time.time() * 1000)
    pid = infra.stop_forwarder(run, "a")
    c.info(f"stopped forwarder A (pid {pid}) mid-turn, {time.time() - since:.1f} s after the turn started")
    on_b = infra.wait_for(f"{marker} retried on B", lambda: [e for e in infra.responses_posts(run, "b", since) if (e.get("last_user") or "").startswith(marker)], 120)
    c.ok(bool(on_b), f"Codex re-sent the broken turn and the router sent it to B: {fmt_entry(on_b[0])}")
    infra.wait_for(f"{marker} finished", lambda: not probe(run)["stop"], 180)
    end = infra.wait_for(f"{marker} outcome", lambda: turn_outcome(run, thread2, marker)[0], 30)
    c.ok(end == "task_complete", f"{marker} completed after the mid-turn failover (rollout: {end})")
    lines, adm = admin_lines(run)
    ev = events(adm, t_kill)
    c.ok(any(e.get("kind") in ("stream_broken", "line_down", "failover") and e.get("line") == "a" for e in ev),
         f"router events after the mid-turn kill: {json.dumps(ev)[:500]}")
    c.ok(app_procs(run) == (main_r, servers_r), "still no app restart")
    infra.start_forwarder(run, "a")
    time.sleep(2)

    # 401/403/429 from a line are passed back, never failed over. First let A's
    # breaker leave the open state (it opened when A was killed and the health
    # probes kept it open with a doubling backoff, up to 60 s, while A was down):
    # an open line is skipped whatever it would answer, which is what the run of
    # 2026-10-17 01:32 measured instead (A got no request, B answered 200).
    t_up = time.time()
    infra.wait_for("line A not open in the router", lambda: (admin_lines(run)[0].get("a") or {}).get("state") in ("healthy", "half_open"), 120, 1)
    c.info(f"line A back to {(admin_lines(run)[0].get('a') or {}).get('state')} after {time.time() - t_up:.1f} s (breaker backoff)")
    for code in (401, 403, 429):
        infra.set_fault(run, "a", code)
        t_fault = time.time()
        try:
            # A marker in the query tells this request apart from the app's own
            # background requests in the forwarder logs.
            marker = f"e2e-fault-{code}"
            status, route, body = infra.router_request(run, "GET", f"/backend-api/codex/models?client_version=0.162.0&{marker}=1", token=token)
            a_hits = [e for e in infra.fwd_entries(run, "a", t_fault) if marker in (e.get("path") or "")]
            b_hits = [e for e in infra.fwd_entries(run, "b", t_fault) if marker in (e.get("path") or "")]
        finally:
            infra.set_fault(run, "a", None)
        c.ok(status == code and route and route.endswith("/a") and "injected by the e2e forwarder" in body,
             f"line A answers {code}: the router passes it back (HTTP {status}, x-superba-route {route})")
        c.ok(a_hits and not b_hits, f"no failover on {code}: line A got {len(a_hits)} request(s), line B {len(b_hits)}")
    lines, _ = admin_lines(run)
    c.ok((lines.get("a") or {}).get("state") == "healthy", f"line A healthy after its account errors (they are answers, not failures): {lines.get('a')}")

    # Router off: direct mode through a hot restart.
    before = app_procs(run)
    res = spb_stream(run, "router", "disable", "--countdown", "0", "--yes", label="s13-disable")
    after = app_procs(run)
    hot_switch_ok(c, res, before, after, "spb router disable")
    cfg = flatten(config(run))
    st = router_status(run, "s13 status disabled")
    c.ok(cfg.get("openai_base_url") == run.line_url("a") + "/backend-api/codex" and auth_shape(run) == ["OPENAI_API_KEY", "personal_access_token"]
         and not st.get("codex_routed") and not (st.get("settings") or {}).get("enabled") and st.get("running"),
         f"direct mode again (line A, PAT); router still running, mode off: {cfg.get('openai_base_url')}")
    since = time.time()
    send_turn(run, "E2E-R7: reply with exactly the text PONG-R7")
    line, e = wait_turn(run, "E2E-R7", since)
    c.ok(line == "a" and e["auth_kind"] == "pat", f"direct turn after disable: {fmt_entry(e)}")

    # Watchdog: the router dies while Codex is routed.
    res = spb_stream(run, "router", "enable", "--countdown", "0", "--yes", label="s13-enable-again")
    c.ok(res["rc"] == 0 and router_status(run, "s13 status re-enabled").get("codex_routed"), f"router mode on again [{res['log']}]")
    rt = infra.router_runtime(run) or {}
    sup = supervisor_pid(run)
    c.info(f"router pid {rt.get('pid')}, supervisor pid {sup}")
    # The supervisor alone restarts a crashed router (same port).
    infra.kill_pids([rt.get("pid")])
    back = infra.wait_for("router restarted by its supervisor",
                          lambda: (lambda x: x if x and x.get("pid") != rt.get("pid") and infra.router_admin(run, "health") else None)(infra.router_runtime(run)), 30)
    c.ok(back and back.get("port") == rt.get("port"), f"supervisor restarted the killed router: pid {rt.get('pid')} -> {back.get('pid')}, same port {back.get('port')}")
    before = app_procs(run)
    log = open(run.out / "transcripts" / "s13-watch.log", "w")
    watch = subprocess.Popen([run.d["spb"], "router", "watch", "--interval", "2"], env=run.base_env(), cwd=run.home,
                             stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    run.d["pids"]["router-watch"] = watch.pid
    run.save()
    try:
        time.sleep(3)
        rt = infra.router_runtime(run) or {}
        sup = supervisor_pid(run)
        t_dead = time.time()
        infra.kill_pids([p for p in (sup, rt.get("pid")) if p])
        c.info(f"killed the supervisor ({sup}) and the router ({rt.get('pid')}) at +0 s; `spb router watch --interval 2` is running")

        def fell_back():
            cfg = flatten(config(run))
            return cfg if cfg.get("openai_base_url", "").startswith("https://") else None

        infra.wait_for("watchdog fallback to direct mode", fell_back, 90)
        t_cfg = time.time() - t_dead
        infra.wait_for("new app-server after the fallback", lambda: (lambda m, s: s and set(s).isdisjoint(before[1]))(*app_procs(run)), 30)
        t_srv = time.time() - t_dead
        c.ok(t_cfg <= 20 + 2 * 2 + 10, f"fell back to direct mode {t_cfg:.1f} s after the router died (grace 20 s + 2 s ticks); new app-server at +{t_srv:.1f} s")
        after = app_procs(run)
        c.ok(after[0] == before[0], f"hot restart (main {before[0]} kept, app-server {before[1]} -> {after[1]})")
    finally:
        watch.send_signal(signal.SIGINT)
        try:
            watch.wait(10)
        except subprocess.TimeoutExpired:
            infra.kill_pids([watch.pid])
        run.d["pids"].pop("router-watch", None)
        run.save()
        log.close()
    wl = (run.out / "transcripts" / "s13-watch.log").read_text()
    c.ok("switched back to direct mode" in wl, "watch output: " + " | ".join(l.strip() for l in wl.splitlines() if l.strip())[:400])
    st = router_status(run, "s13 status after fallback")
    cfg = flatten(config(run))
    c.ok(not (st.get("settings") or {}).get("enabled") and not st.get("codex_routed") and cfg.get("openai_base_url") == run.line_url("a") + "/backend-api/codex",
         f"router mode turned off; Codex direct on line A ({cfg.get('openai_base_url')})")
    since = time.time()
    send_turn(run, "E2E-R8: reply with exactly the text PONG-R8")
    line, e = wait_turn(run, "E2E-R8", since)
    c.ok(line == "a", f"turn after the watchdog fallback: {fmt_entry(e)}")
    log_text = (run.router_dir / "router.log").read_text() if (run.router_dir / "router.log").exists() else ""
    c.ok(log_text and run.key not in log_text and (token or "x") not in log_text, f"router.log ({len(log_text)} bytes) holds neither the member key nor the router token")
    save(run, "s13-router.log", log_text)
    finish(run, "s13", c)


# ---------------------------------------------------------------------------
# s14 account data
# ---------------------------------------------------------------------------

USAGE = "/v1/organization/usage/completions"
COSTS = "/v1/organization/costs"
SUB = "/v1/dashboard/billing/subscription"
BUSAGE = "/v1/dashboard/billing/usage"


def account(run, label, *extra, json_mode=False):
    args = ["account", "--profile", "acct", *extra]
    r = run.spb(*args, json_out=json_mode, label=label)
    if not json_mode:
        (run.out / "transcripts").mkdir(exist_ok=True)
        (run.out / "transcripts" / f"{slug(label)}.log").write_text(f"$ spb {' '.join(args)}\nrc={r['rc']}\n{r['out']}{r['err']}")
    return r


def slug(text):
    return re.sub(r"[^A-Za-z0-9.]+", "-", text).strip("-")


def mock_hits(run, since):
    out = []
    p = run.out / "mockrelay.jsonl"
    if p.exists():
        for raw in p.read_text().splitlines():
            e = json.loads(raw)
            if e.get("ts", 0) >= since:
                out.append((e["path"], e["status"]))
    return out


@scenario("s14")
def s14(run):
    c = Check()
    r = run.run([run.d["spb"], "profile", "add", "acct", "--name", "Account mock", "--site", "krill-mock", "--key-stdin", "--replace"],
                stdin=infra.MOCK_KEY + "\n", label="s14 profile add")
    c.ok(r["rc"] == 0, f"profile acct on the mock Krill site (fake key known only to the mock): exit {r['rc']}")
    infra.set_mock_faults(run, {})
    t0 = time.time()
    a = account(run, "s14 account")
    out = a["out"]
    c.ok(a["rc"] == 0 and "dev@example.com" in out and "Pro" in out and "300" in out and "Remaining" in out and "Expires" in out,
         "spb account: e-mail (whoami), plan, limit, used, remaining, expiry from /v1/dashboard/billing/subscription + /usage")
    c.info("requests: " + json.dumps(sorted(set(mock_hits(run, t0)))))
    # Usage reports are cached per range for 10 min and explicit refreshes are
    # debounced to one a minute (Krill allows 10 usage requests an hour), so each
    # case below asks for its own range (--days) and the 429 case refreshes a
    # report saved more than a minute earlier.
    t0 = time.time()
    t_fill = time.time()
    account(run, "s14 fill 429 range", "--days", "9")
    u = account(run, "s14 account usage", "--usage")
    c.ok(u["rc"] == 0 and "Daily" in u["out"] and "Models" in u["out"] and "gpt-5.5" in u["out"] and "Total cost" in u["out"],
         "spb account --usage: daily cost, totals and per-model rows")
    hits = mock_hits(run, t0)
    c.ok(any(p.startswith(COSTS) and s == 200 for p, s in hits) and any(p.startswith(USAGE) and s == 200 for p, s in hits),
         f"usage from /v1/organization/costs and /v1/organization/usage/completions: {sorted(set(hits))}")
    t0 = time.time()
    j = json_of(account(run, "s14 account json", "--usage", json_mode=True))
    usage = j.get("usage") or {}
    c.ok(usage.get("from_cache") is True and not [h for h in mock_hits(run, t0) if h[0].startswith(("/v1/organization",))],
         f"a second --usage within 10 min is served from the disk cache without a request (from_cache={usage.get('from_cache')}, cost_source={usage.get('cost_source')})")

    cases = [
        ("subscription 404", {SUB: 404}, 2, lambda o, j: "Balance: not available from this site" in o and "dev@example.com" in o and "Daily" in o),
        ("billing usage 404", {BUSAGE: 404}, 2, lambda o, j: "Limit" in o and "Used" not in o and "Remaining" not in o),
        ("usage-completions 404", {USAGE: 404}, 3, lambda o, j: "Per-model usage: not available from this site" in o and "Total cost" in o),
        ("costs 404", {COSTS: 404}, 4, lambda o, j: "Total cost" in o and "Models" in o and (j.get("usage") or {}).get("cost_source") not in (None, "costs")),
        ("usage + costs 404", {USAGE: 404, COSTS: 404}, 5, lambda o, j: "Usage data is not available on this site." in o and "Limit" in o),
    ]
    for name, faults, days, ok in cases:
        infra.set_mock_faults(run, faults)
        t0 = time.time()
        r = account(run, f"s14 {name}", "--days", str(days))
        jj = json_of(account(run, f"s14 {name} json", "--days", str(days), json_mode=True))
        c.ok(ok(r["out"], jj) and r["rc"] == 0, f"{name}: that part hidden, the rest shown (exit {r['rc']}) [transcripts/{slug('s14 ' + name)}.log]")
        c.info(f"{name}: requests {sorted(set(mock_hits(run, t0)))}; cost_source={json.dumps((jj.get('usage') or {}).get('cost_source'))}")

    infra.set_mock_faults(run, {USAGE: 429, COSTS: 429})
    wait = 61 - (time.time() - t_fill)
    if wait > 0:
        time.sleep(wait)
    t0 = time.time()
    r = account(run, "s14 rate limited", "--days", "9", "--refresh")
    jj = json_of(account(run, "s14 rate limited json", "--days", "9", json_mode=True))
    c.ok(any(s == 429 for _, s in mock_hits(run, t0)), f"the mock answered 429 to the usage requests: {sorted(set(mock_hits(run, t0)))}")
    both = r["out"] + r["err"]
    # The product's wording (superba-core usage.rate_limited*, cli.account.saved).
    c.ok(r["rc"] == 0 and "Daily" in r["out"] and "Saved data from" in both and "limits usage queries" in both,
         "429: the saved report is shown with a rate-limit note and its time [transcripts/s14-rate-limited.log]")
    c.info("429 notice: " + " | ".join(l.strip() for l in both.splitlines() if "limits usage" in l or "Saved" in l))
    m = re.search(r"Saved data from (\S+)", both)
    saved_at = None
    if m:
        try:
            from datetime import datetime

            saved_at = datetime.fromisoformat(m.group(1).replace("Z", "+00:00")).timestamp()
        except ValueError:
            pass
    cached = jj.get("usage") or {}
    c.ok(saved_at is not None and saved_at < t0 and abs(saved_at * 1000 - (cached.get("fetched_at") or 0)) < 1000,
         f"the time shown is when the saved report was fetched (before the 429; cached fetched_at {cached.get('fetched_at')}), "
         f"not the time of the failed refresh: {m.group(1) if m else None}")
    c.ok("Try again in 30 min" in both, "the note carries the site's Retry-After (1800 s -> 30 min)")
    infra.set_mock_faults(run, {})
    finish(run, "s14", c)


# ---------------------------------------------------------------------------
# s10 GUI
# ---------------------------------------------------------------------------

GUI_SIZE = (1280, 800)
# Taller window for the switch overlay, so its expanded settings rows fit.
OVERLAY_SIZE = (1280, 940)
# The pages in navigation order when `superba --gui-help` cannot be read
# (superba-gui args::PAGE_NAMES); s10 takes the list from the binary.
PAGES = ["overview", "providers", "models", "router", "usage", "settings", "sessions", "hosts", "claude-code",
         "claude-settings", "updates", "diagnostics", "preferences"]


def gui_pages(run):
    """The page names the binary accepts (`superba --gui-help`, `--page NAME`)."""
    r = run.run([run.d["superba"], "--gui-help"], label="superba --gui-help", timeout=30)
    text = r["out"] + r["err"]
    m = re.search(r"--page NAME\s+open a page first:(.*?)(?:\n\s*\n|$)", text, re.S)
    names = m.group(1).split() if m else []
    return names or PAGES
VARIANTS = [("dark", "en"), ("light", "en"), ("dark", "zh-CN"), ("light", "zh-CN")]
# Window sizes every page is shot at (UX §8): the default and a larger one.
SIZES = [(1280, 800), (1440, 900)]
# Pages that load more before they settle (lists, request log, catalog).
SLOW_PAGES = ("providers", "models", "router", "settings", "sessions", "hosts", "updates")


def gui(run, page, theme, lang, log, size=GUI_SIZE):
    return subprocess.Popen([run.d["superba"], "--size", f"{size[0]}x{size[1]}", "--theme", theme, "--lang", lang, "--page", page],
                            env=run.base_env(), cwd=run.home, stdout=log, stderr=log, start_new_session=True)


def xshot(run, name, size=GUI_SIZE):
    """Screenshot of the Superba window (top-left GUI_SIZE of the screen; there is
    no window manager, the desktop app's window lies behind it)."""
    path = run.out / "screens" / f"{name}.png"
    path.parent.mkdir(exist_ok=True)
    subprocess.run(["scrot", "-o", str(path)], env=run.base_env(), check=False)
    try:
        from PIL import Image

        with Image.open(path) as img:
            img.crop((0, 0, *size)).save(path)
    except (ImportError, OSError):
        pass
    return str(path.relative_to(run.out))


def stop(p):
    infra.kill_pids([p.pid])
    try:
        p.wait(5)
    except subprocess.TimeoutExpired:
        pass


def set_countdown(run, secs):
    f = run.data_dir / "profiles.json"
    data = json.loads(f.read_text())
    data.setdefault("settings", {})["countdown_secs"] = secs
    f.write_text(json.dumps(data, indent=2))


def switch_finished(run):
    """True when no switch is running: every switch journal is finished and
    nobody holds the state lock (the executor holds it from its first write until
    after it stored the profile overrides). The harness must not kill the GUI
    before that: a GUI killed while verifying leaves a "committed" journal that
    blocks every later plan until it is recovered."""
    import fcntl

    for f in (run.data_dir / "journal" / "switch").glob("*.json"):
        try:
            status = json.loads(f.read_text()).get("status")
        except (OSError, json.JSONDecodeError):
            return False
        if status in ("applying", "committed", "rolling_back"):
            return False
    lock = run.data_dir / "state.lock"
    if not lock.exists():
        return True
    with open(lock, "a") as fh:
        try:
            fcntl.flock(fh, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        fcntl.flock(fh, fcntl.LOCK_UN)
    return True


def changes_link(png):
    """Centre of the switch overlay's "Show N changes" link: the lowest line of
    link-blue text below the overview's own links (y > 730) and right of the
    sidebar (the overlay is anchored under the line tiles), or None."""
    from PIL import Image

    with Image.open(png) as img:
        im = img.convert("RGB")
    w, h = im.size
    px = im.load()
    rows = {}
    for y in range(730, h):
        xs = [x for x in range(400, min(1150, w)) if px[x, y][2] > 200 and px[x, y][0] < 120 and 90 < px[x, y][1] < 190]
        if xs:
            rows[y] = xs
    lines, cur = [], []
    for y in sorted(rows):
        if cur and y - cur[-1] > 3:
            lines.append(cur)
            cur = []
        cur.append(y)
    if cur:
        lines.append(cur)
    boxes = [(min(x for y in ys for x in rows[y]), max(x for y in ys for x in rows[y]), ys[0], ys[-1]) for ys in lines]
    boxes = [b for b in boxes if b[1] - b[0] >= 30]
    if not boxes:
        return None
    x0, x1, y0, y1 = max(boxes, key=lambda b: b[2])
    return ((x0 + x1) // 2, (y0 + y1) // 2)


def overlay_top(png):
    """Top edge of the bottom-anchored switch overlay: walk up its left border
    (x = 410, constant border colour) from inside the card, minus the corner."""
    from PIL import Image

    with Image.open(png) as img:
        im = img.convert("RGB")
    px = im.load()
    x, y = 410, 880
    if im.size[1] <= y:
        return None
    ref = px[x, y]

    def same(a, b):
        return all(abs(i - j) <= 6 for i, j in zip(a, b))

    # A border: the same colour 20 px higher, a different one just left of it.
    if not same(ref, px[x, y - 20]) or same(ref, px[x - 5, y]):
        return None
    while y > 300 and same(px[x, y - 1], ref):
        y -= 1
    return y - 11


def disclosure_pos(png, top):
    """Centre of the compact overlay's "What restarts?" line below the overlay's
    top edge `top`: the text line whose first ink is the disclosure chevron
    (x 440-455), not a line of the reason text or an icon (x about 430). The
    reason may wrap, so its offset from the top is not fixed."""
    from PIL import Image

    with Image.open(png) as img:
        im = img.convert("RGB")
    px = im.load()
    w, h = im.size
    bg = px[420, top + 12]

    def ink(p):
        return sum(abs(a - b) for a, b in zip(p, bg)) > 150

    lines, cur = [], []
    for y in range(top + 8, min(h - 5, top + 300)):
        xs = [x for x in range(425, min(w, 1080)) if ink(px[x, y])]
        if xs:
            cur.append((y, xs[0]))
        elif cur:
            lines.append(cur)
            cur = []
    for ln in lines:
        first = min(x for _, x in ln)
        if 440 <= first <= 455 and len(ln) <= 22:
            return 500, (ln[0][0] + ln[-1][0]) // 2
    return None


def palette_switch(run, query):
    xdo(run, "search", "--sync", "--name", "^Superba$", "windowfocus", "--sync")
    xdo(run, "mousemove", "640", "700", "click", "1")
    time.sleep(0.3)
    xdo(run, "key", "ctrl+k")
    time.sleep(0.8)
    xdo(run, "type", "--delay", "60", query)
    time.sleep(0.6)


@scenario("s10")
def s10(run):
    c = Check()
    log = open(run.out / "gui.log", "a")
    r = spb_stream(run, "router", "install", "--member", "e2e", "--member", "compat", label="s10-router-install")
    st = router_status(run, "s10 router status")
    c.ok(r["rc"] == 0 and st.get("running"), f"live router for the Router page: members e2e + compat, port {st.get('port')} [{r['log']}]")
    shots = {}
    early = []
    pages = gui_pages(run)
    c.info(f"pages of this build: {pages}")
    for size in SIZES:
        tag = "" if size == GUI_SIZE else f"-{size[0]}x{size[1]}"
        for theme, lang in VARIANTS:
            suffix = f"{theme}-{'zh' if lang == 'zh-CN' else 'en'}{tag}"
            for page in pages:
                p = gui(run, page, theme, lang, log, size=size)
                try:
                    time.sleep(8 if page in SLOW_PAGES else 6)
                    xdo(run, "mousemove", str(size[0] - 1), str(size[1] - 1))
                    time.sleep(0.5)
                    alive = p.poll() is None
                    shots[f"{page}-{suffix}"] = xshot(run, f"s10-{page}-{suffix}", size)
                    if not alive:
                        early.append(f"{page} {suffix}")
                finally:
                    stop(p)
    c.ok(not early, f"the GUI stayed up on every page (exited early: {early})")
    c.ok(len(shots) == len(pages) * len(VARIANTS) * len(SIZES),
         f"{len(shots)} page screenshots ({len(pages)} pages x dark/light x en/zh-CN x 1280x800 / 1440x900) from the real binary in the namespace")

    # The switch overlay: a pending settings change on profile e2e makes the
    # plan's settings rows visible; the GUI then runs the hot switch itself.
    set_countdown(run, 10)
    overlays = [("dark", "en", "tool_output_token_limit", "12000", 12000, "line B", "b"),
                ("light", "zh-CN", "model_verbosity", "low", "low", "line A", "a")]
    for theme, lang, key, lit, want, query, target in overlays:
        r = run.spb("settings", "set", key, lit, "--profile", "e2e", label=f"s10 set {key}")
        c.ok(r["rc"] == 0, f"pending setting for the overlay: {key} = {lit}")
        main0, servers0 = app_procs(run)
        p = gui(run, "overview", theme, lang, log, size=OVERLAY_SIZE)
        suffix = f"{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
        try:
            time.sleep(7)
            palette_switch(run, query)
            shots[f"palette-{suffix}"] = xshot(run, f"s10-palette-{suffix}", OVERLAY_SIZE)
            t_enter = time.time()
            xdo(run, "key", "Return")
            time.sleep(3.5)
            shots[f"overlay-{suffix}"] = xshot(run, f"s10-overlay-{suffix}", OVERLAY_SIZE)
            # The settings rows are behind the overlay's "Show N changes" link.
            # After a completed switch that showed the restart risks, the overlay
            # is compact (BannerState::risks_seen) and that link sits inside its
            # "What restarts?" disclosure, which is opened first.
            link = changes_link(run.out / shots[f"overlay-{suffix}"])
            compact = link is None
            if compact:
                top = overlay_top(run.out / shots[f"overlay-{suffix}"])
                if top:
                    pos = disclosure_pos(run.out / shots[f"overlay-{suffix}"], top) or (500, top + 64)
                    xdo(run, "mousemove", str(pos[0]), str(pos[1]), "click", "1")
                    time.sleep(0.8)
                    shots[f"overlay-risks-{suffix}"] = xshot(run, f"s10-overlay-risks-{suffix}", OVERLAY_SIZE)
                    link = changes_link(run.out / shots[f"overlay-risks-{suffix}"])
            if link:
                xdo(run, "mousemove", str(link[0]), str(link[1]), "click", "1")
                time.sleep(1.2)
            shots[f"overlay-rows-{suffix}"] = xshot(run, f"s10-overlay-rows-{suffix}", OVERLAY_SIZE)
            mode = ', compact: opened "What restarts?" first' if compact else ""
            c.ok(link is not None and time.time() - t_enter < 9,
                 f"overlay ({theme}/{lang}{mode}): settings rows opened through \"Show N changes\" at {link} "
                 f"during the countdown [{shots[f'overlay-rows-{suffix}']}]")

            def switched():
                m, s = app_procs(run)
                return (m, s) if s and set(s).isdisjoint(servers0) else None

            main1, servers1 = infra.wait_for("GUI hot switch", switched, 60)
            cfg = infra.wait_for("config written", lambda: (lambda f: f if f.get(key) == want else None)(flatten(config(run))), 30)
            c.ok(main1 == main0 and cfg.get("openai_base_url") == run.line_url(target) + "/backend-api/codex",
                 f"GUI palette switch to {query} ({theme}/{lang}): hot restart {time.time() - t_enter:.1f} s after Enter (10 s countdown), main {main0} kept, app-server {servers0} -> {servers1}; {key} = {cfg.get(key)!r}")
            done = infra.wait_for("the GUI's switch finished (journal done, state lock free)", lambda: switch_finished(run), 60)
            c.ok(done, f"the GUI finished its switch (verification, profile overrides) {time.time() - t_enter:.1f} s after Enter")
            time.sleep(1)
            shots[f"after-{suffix}"] = xshot(run, f"s10-after-switch-{suffix}", OVERLAY_SIZE)
        finally:
            stop(p)
    set_countdown(run, 5)
    since = time.time()
    send_turn(run, "E2E-T7: reply with exactly the text PONG-7")
    line, e = wait_turn(run, "E2E-T7", since)
    c.ok(line == "a", f"next turn after the GUI switches: {fmt_entry(e)}")
    r = run.spb("router", "uninstall", label="s10 router uninstall")
    st = router_status(run, "s10 router status after uninstall")
    c.ok(r["rc"] == 0 and not st.get("running") and not st.get("installed"), f"router uninstalled while not routed (exit {r['rc']})")
    c.info("screenshots must be reviewed by eye: " + ", ".join(sorted(shots.values())))
    finish(run, "s10", c, screenshots=shots)


# ---------------------------------------------------------------------------
# s15 tracked tree and release binaries
# ---------------------------------------------------------------------------


# Lines that state or test the absence of demo mode / context-management wording.
ABSENCE = re.compile(r"no demo mode|ships no demo|no demo backend|no demo$|demo_flags_are_gone|\[\"--demo\"|contains\(\"demo\"\)|"
                     r"\"Demo data\"|\"demo mode\"|no_longer_mention_demo|No user-facing \"context management\"|"
                     r"contains\(\"context management\"\)|contains\(\"上下文管理\"\)|"
                     r"_tests\.rs:\d+:\s+\"(Context management|上下文管理)\",$", re.I)


@scenario("s15")
def s15(run):
    c = Check()
    repo = str(infra.REPO)

    def grep(*args):
        p = subprocess.run(["git", "grep", "-n", "-I", *args, "--", ".", ":!CHANGELOG.md", ":!e2e/lib/"],
                           cwd=repo, capture_output=True, text=True)
        return [l for l in p.stdout.splitlines() if l]

    # "demo" as a word or name part, not the prefix of "demonstrate" (the verbatim
    # upstream Codex prompts in catalog/feeds/prompts/ use that word).
    demo = grep("-i", "-E", r"\bdemo([^a-z]|$)")
    bad = [l for l in demo if not ABSENCE.search(l)]
    save(run, "s15-demo-grep.txt", "\n".join(demo))
    c.ok(not bad, f"no demo mode, demo data or demo wording in the tracked tree: {len(demo)} hits, all of them statements or tests "
         f"that demo mode is gone; others: {bad[:5]} (evidence/s15-demo-grep.txt)")
    cm = grep("-i", "-E", r"context[ -]management|上下文管理")
    save(run, "s15-context-management-grep.txt", "\n".join(cm))
    upstream = [l for l in cm if l.startswith("catalog/dist/") or l.startswith("catalog/overlay/")]
    other = [l for l in cm if l not in upstream and not ABSENCE.search(l)]
    c.ok(not other, f"no user-facing 'context management' wording of Superba's own: {len(cm)} hits = {len(upstream)} official upstream "
         f"catalog descriptions of the Codex feature `features.context_management` + tests of its absence; others: {other[:5]}")
    keys = grep("-n", "-E", r"context_management")
    c.info(f"the Codex feature key itself appears {len(keys)} times (Krill's recommended setting, catalog, tests)")

    # Rules (patterns assembled at run time so this file never matches).
    everything = ["git", "grep", "-n", "-I", "-F"]

    def tracked(*args):
        p = subprocess.run([*everything, *args, "--", "."], cwd=repo, capture_output=True, text=True)
        return [l for l in p.stdout.splitlines() if l]

    endpoint = tracked("/api/client/" + "account-summary")
    c.ok(not endpoint, f"no reference to the account summary endpoint: {endpoint[:3]}")
    hosts = tracked("114.80." + "150.200") + tracked("192." + "168.")
    c.ok(not hosts, f"no internal hosts or IPs in the tracked tree: {len(hosts)} hits")
    brand = [l for l in subprocess.run(["git", "grep", "-n", "-I", "-i", "-E", "superba for " + "krill|brand\\.for_" + "krill", "--", "."],
                                       cwd=repo, capture_output=True, text=True).stdout.splitlines()
             if l and "assert_eq!(tr.t(" not in l]
    ui_brand = subprocess.run(["git", "grep", "-n", "-I", "-i", "for krill", "--", "crates/superba-gui/locales", "crates/superba-cli/locales"],
                              cwd=repo, capture_output=True, text=True).stdout.splitlines()
    c.ok(not brand and not ui_brand, f"no \"for Krill\" brand string (UI locales: {ui_brand[:3]}; elsewhere: {brand[:3]}); "
         "Krill appears through the preset and capabilities only")
    secrets = {"e2e key": run.key, "test relay host": run.upstream.split("://", 1)[-1].split("/")[0]}
    leaked = {name: len(tracked(value)) for name, value in secrets.items() if value}
    c.ok(not any(leaked.values()), f"key bytes and the test relay host in tracked files: {leaked} hits (values never printed)")
    bins = [Path(run.d[k]) for k in ("spb", "superba", "router_bin")]
    in_bins = {b.name: [n for n, v in secrets.items() if v and v.encode() in b.read_bytes()] for b in bins}
    c.ok(not any(in_bins.values()), f"release binaries {sorted(in_bins)}: no key or relay host bytes ({in_bins})")
    finish(run, "s15", c)
