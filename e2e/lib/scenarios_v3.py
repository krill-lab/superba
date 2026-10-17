"""Scenarios of the Superba e2e run (s16-s23): the setup wizard with the Krill
preset in a fresh home (s16), model lists and the signed feeds through raced local
mirrors (s17), per-model context overrides through a model catalog file (s18),
Fast / Ultrafast in API-key direct mode (s19), the request log and the stats line
under replies (s20), pinning the desktop app's Codex (s21), a remote host in
Docker: settings sync filtered by its Codex version, Codex install and sessions
per host (s22), and the desktop-app update check (s23). Registered into
scenarios.SCENARIOS; the order is scenarios.ORDER.

Same rules as scenarios.py: real binaries copied into the run, the real desktop app
and Codex, an isolated HOME, every process inside the run's namespace, every
assertion recorded as evidence, no key or token printed.
"""

import glob
import hashlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import time
import tomllib
from pathlib import Path

import infra
from scenarios import (
    Check,
    _with_cdp,
    acall,
    app_procs,
    auth_shape,
    config,
    finish,
    flatten,
    onboarding,
    parent,
    probe,
    save,
    scenario,
    send_turn,
    shot,
    spb_stream,
    wait_turn,
    xdo,
)
from scenarios import cdp_port as scenarios_cdp_port
from scenarios_v2 import GUI_SIZE, reset_e2e_lines, gui, hot_switch_ok, json_of, profile_of, router_status, router_token, stop, xshot

# The Superba stats-line marker (bolt + WORD JOINER, superba-core codex::STATS_LINE_MARKER).
MARKER = "⚡⁠"
STORE_TARGET = infra.CODEX_PKG_TARGET
PKG_VERSION = infra.CODEX_PKG_VERSION


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------


def codex(run):
    """The bundled codex on the run's PATH (bin/codex)."""
    return str(run.bin_dir / "codex")


def exec_turn(run, prompt, *extra, out_file=None, label=None):
    """`codex exec` of the run's PATH in the project dir; returns the result."""
    argv = [codex(run), "exec", "--skip-git-repo-check", *extra]
    if out_file:
        argv += ["-o", str(out_file)]
    return run.run([*argv, prompt], cwd=str(Path(run.home) / "proj"), timeout=240, label=label or f"codex exec {prompt[:12]}")


def posts(run, line, marker, since):
    return [e for e in infra.responses_posts(run, line, since) if (e.get("last_user") or "").startswith(marker)]


def wait_post(run, line, marker, since, timeout=180):
    return infra.wait_for(f"{marker} on line {line}", lambda: posts(run, line, marker, since), timeout)[-1]


def fmt(e):
    keep = ("t", "line", "path", "status", "auth_kind", "originator", "model", "service_tier", "tier_sent",
            "has_cache_key", "markers", "n_input")
    return json.dumps({k: e.get(k) for k in keep if e.get(k) is not None}, ensure_ascii=False)


def new_chat(run):
    acall(_with_cdp(run, lambda c: c.click_text(["New chat"])))
    time.sleep(2)


def app_turn(run, marker, text=None):
    """A user turn in a new chat of the desktop app; returns (line, entry)."""
    new_chat(run)
    since = time.time()
    send_turn(run, f"{marker}: {text or 'reply with exactly the text PONG-' + marker[4:]}")
    return wait_turn(run, marker, since)


def app_servers(run):
    """(main pid, app-server pids) of the run's desktop app, whatever Codex binary
    the app-server runs (the pinned store binary has another path than the
    bundled one, which scenarios.app_procs looks for)."""
    main, _ = app_procs(run)
    if not main:
        return None, []
    kids = [p for p in infra.pids_with_home(run.home) if parent(p) == main and " app-server" in infra.cmdline(p)]
    return main, sorted(kids)


def exe_of(pid):
    try:
        return os.readlink(f"/proc/{pid}/exe").removesuffix(" (deleted)")
    except OSError:
        return None


def same_file(a, b):
    return bool(a and b) and os.path.realpath(a) == os.path.realpath(b)


def superba_files(run):
    d = run.codex_home / "superba"
    return sorted(p.name for p in d.glob("models-*.json")) if d.exists() else []


def catalog_entries(path):
    data = json.loads(Path(path).read_text())
    return {m["slug"]: m for m in data.get("models", [])}


def file_sha(path):
    p = Path(path)
    return hashlib.sha256(p.read_bytes()).hexdigest() if p.exists() else None


def newest_rollout(run, since):
    files = [f for f in glob.glob(f"{run.codex_home}/sessions/**/rollout-*.jsonl", recursive=True)
             if os.path.getmtime(f) >= since]
    return max(files, key=os.path.getmtime) if files else None


def rollout_events(path):
    out = []
    for raw in open(path, encoding="utf-8"):
        try:
            out.append(json.loads(raw))
        except json.JSONDecodeError:
            pass
    return out


def rollout_of_marker(run, marker, since):
    """The rollout file whose user message starts with `marker` (newest first)."""
    files = sorted((f for f in glob.glob(f"{run.codex_home}/sessions/**/rollout-*.jsonl", recursive=True)
                    if os.path.getmtime(f) >= since), key=os.path.getmtime, reverse=True)
    for f in files:
        if marker in Path(f).read_text(encoding="utf-8", errors="replace"):
            return f
    return None


def context_windows(path):
    return [e["payload"]["info"].get("model_context_window") for e in rollout_events(path)
            if e.get("type") == "event_msg" and (e.get("payload") or {}).get("type") == "token_count"
            and (e.get("payload") or {}).get("info")]


def codex_debug_models(run):
    """`codex debug models` in the isolated home: the catalog Codex itself uses."""
    r = run.run([codex(run), "debug", "models"], timeout=60, label="codex debug models")
    try:
        data = json.loads(r["out"])
    except json.JSONDecodeError:
        return r, {}
    models = data["models"] if isinstance(data, dict) else data
    return r, {m["slug"]: m for m in models}


def model_list(run):
    """The app-server's `model/list` (what the app's picker is built from), from
    the same Codex binary and home as the app."""
    r = run.run(["python3", "-B", str(infra.LIB / "codex_as.py"), "--model-list", codex(run), str(Path(run.home) / "proj")],
                timeout=90, label="app-server model/list")
    try:
        return [m.get("id") or m.get("model") for m in json.loads(r["out"].strip().splitlines()[-1])["models"]]
    except (IndexError, KeyError, json.JSONDecodeError):
        return []


def switch_to(run, c, profile, label):
    before = app_servers(run)
    res = spb_stream(run, "profile", "use", profile, "--countdown", "0", "--yes", label=label)
    after = app_servers(run)
    hot_switch_ok(c, res, before, after, f"switch to profile {profile}")
    return res


def edit_and_switch(run, c, label, *args):
    """A models/speed edit of the applied profile: stored, then a hot switch."""
    before = app_servers(run)
    res = spb_stream(run, *args, "--countdown", "0", "--yes", label=label)
    after = app_servers(run)
    hot_switch_ok(c, res, before, after, " ".join(["spb", *args]))
    return res


def add_key_profile(run, pid, site, *extra, label):
    return run.run([run.d["spb"], "profile", "add", pid, "--name", pid, "--site", site, "--auth", "apikey",
                    *extra, "--key-stdin"], stdin=run.key + "\n", label=label)


EFFORTS = ("None", "Minimal", "Light", "Medium", "High", "Extra High", "Max", "Ultra", "Persistent")


def model_picker(run, name):
    """The composer's model label (e.g. "6 Astra Ultrafast" before the effort)
    and, when the picker opens (label -> popover -> label -> list), the model
    names it lists; with a screenshot. Menus are closed again."""
    import asyncio

    label_js = ("[...document.querySelectorAll('button')].map((b) => (b.innerText || '').split('\\n')"
                ".map((s) => s.trim()).filter(Boolean)).filter((l) => l.length >= 2 && %s.includes(l[1]))"
                ".map((l) => l[0])" % json.dumps(list(EFFORTS)))

    async def go(c):
        labels = await c.eval(label_js) or []
        label = labels[0] if labels else None
        names = []
        if label and await c.click_on_text(label):
            await asyncio.sleep(1.0)
            if await c.click_on_text(label):
                await asyncio.sleep(1.0)
                names = await c.eval(
                    "[...document.querySelectorAll('[data-radix-popper-content-wrapper]')]"
                    ".map((e) => e.innerText).join('\\n').split('\\n').map((s) => s.trim()).filter(Boolean)")
        return label, names or []

    label, names = acall(_with_cdp(run, go))
    png = shot(run, name)
    for _ in range(3):
        acall(_with_cdp(run, lambda c: c.key("Escape")))
    return label, names, png


# ---------------------------------------------------------------------------
# s17 model lists and feeds
# ---------------------------------------------------------------------------


def feed_log(run, since):
    path = run.out / "feedmirror.jsonl"
    out = []
    if path.exists():
        for raw in path.read_text().splitlines():
            e = json.loads(raw)
            if e.get("ts", 0) >= since and e.get("listener") != "all":
                out.append(e)
    return out


def mirror_name(run, m):
    return f"127.0.0.1:{run.d['feed_ports'][m]}"


@scenario("s17")
def s17(run):
    c = Check()
    t0 = time.time()
    r = run.spb("models", "--profile", "e2e", "--refresh", json_out=True, label="s17 models e2e")
    view = json_of(r)
    src = (view.get("list") or {}).get("source") or {}
    models = (view.get("list") or {}).get("models") or []
    c.ok(r["rc"] == 0 and src.get("kind") == "upstream" and (src.get("path") or "").startswith("/backend-api/codex/models?client_version=")
         and len(models) >= 5,
         f"spb models --profile e2e: source = upstream {src.get('path')} with {len(models)} models")
    hits = [e for e in infra.fwd_entries(run, "a", t0) if (e.get("path") or "").startswith("/backend-api/codex/models?client_version=")]
    c.ok(any(e.get("status") in (200, 304) and e.get("auth_kind") == "bearer" for e in hits),
         f"the list came from line A with the profile's key: {[(e.get('path'), e.get('status')) for e in hits][-2:]}")
    tiers = {m["slug"]: [t.get("id") for t in (m.get("service_tiers") or [])] for m in models}
    c.info(f"tiers per model (Codex-native list): {tiers}")
    t1 = time.time()
    r = run.spb("models", "--profile", "compat", "--refresh", json_out=True, label="s17 models compat")
    view = json_of(r)
    src = (view.get("list") or {}).get("source") or {}
    ids = [m["slug"] for m in (view.get("list") or {}).get("models") or []]
    c.ok(r["rc"] == 0 and src.get("kind") == "upstream" and src.get("path") == "/codex/v1/models" and len(ids) >= 5,
         f"spb models --profile compat (custom site D): source = upstream {src.get('path')} (OpenAI list), {len(ids)} ids, e.g. {ids[:4]}")
    d_hits = [e for e in infra.fwd_entries(run, "d", t1) if (e.get("path") or "").split("?")[0] == "/codex/v1/models"]
    c.ok(any(e.get("status") == 200 for e in d_hits), f"forwarder D served GET /codex/v1/models: {[(e.get('path'), e.get('status')) for e in d_hits][-1:]}")

    # Feeds: built-in until the mirrors are configured.
    st = json_of(run.spb("feeds", "status", json_out=True, label="s17 feeds status builtin"))
    fi = run.d["feeds"]
    c.ok(st.get("serial") == fi["stale_serial"] and (st.get("source") or {}).get("kind") == "builtin",
         f"before any mirror: the built-in feed, serial {st.get('serial')}")
    run.d["feed_mirrors_on"] = True
    run.save()
    c.info("SUPERBA_FEED_MIRRORS = the four local mirrors m1..m4 (https, the run's CA) for every later command")
    infra.set_feed_modes(run, m1="404", m2="html", m3="stale", m4="valid")
    t2 = time.time()
    r = run.spb("feeds", "refresh", json_out=True, label="s17 feeds refresh race")
    st = json_of(r)
    log = feed_log(run, t2)
    asked = sorted({e["listener"] for e in log if e["path"] == "/feeds/index.json"})
    c.ok(asked == ["m1", "m2", "m3", "m4"], f"the race asked every mirror for index.json: {asked}")
    c.ok(r["rc"] == 0 and st.get("serial") == fi["valid_serial"] and st.get("mirror") == mirror_name(run, "m4"),
         f"m1 404, m2 HTML landing page, m3 stale serial {fi['stale_serial']}, m4 valid: serial {st.get('serial')} from {st.get('mirror')} (= m4) won")
    files = sorted({e["path"] for e in log if e["listener"] == "m4" and e["path"] not in ("/feeds/index.json", "/feeds/index.json.sig")})
    c.info(f"files fetched from m4 (the others reused by digest): {files}")
    state = json.loads((run.data_dir / "feeds" / "state.json").read_text())
    c.ok(state.get("mirror") == mirror_name(run, "m4"), f"winner remembered in feeds/state.json: {state.get('mirror')}")
    t3 = time.time()
    r = run.spb("feeds", "refresh", json_out=True, label="s17 feeds refresh again")
    first = next((e for e in feed_log(run, t3) if e["path"] == "/feeds/index.json"), {})
    c.ok(r["rc"] == 0 and first.get("listener") == "m4" and json_of(r).get("serial") == fi["valid_serial"],
         f"next check starts with the remembered mirror: first index request to {first.get('listener')}; still serial {json_of(r).get('serial')}")
    infra.set_feed_modes(run, m1="down", m2="down", m3="down", m4="down")
    r = run.spb("feeds", "refresh", json_out=True, label="s17 feeds refresh all down")
    st = json_of(run.spb("feeds", "status", json_out=True, label="s17 feeds status cached"))
    c.ok(st.get("serial") == fi["valid_serial"] and (st.get("source") or {}).get("kind") == "cache"
         and (st.get("last_error") or {}).get("code") == "network",
         f"all mirrors down: refresh fails (exit {r['rc']}, {(st.get('last_error') or {}).get('code')}), the cached serial {st.get('serial')} stays in use")
    feeds_dir = run.data_dir / "feeds"
    aside = run.data_dir / "feeds.e2e-aside"
    feeds_dir.rename(aside)
    try:
        st = json_of(run.spb("feeds", "status", json_out=True, label="s17 feeds status no cache"))
        c.ok(st.get("serial") == fi["stale_serial"] and (st.get("source") or {}).get("kind") == "builtin",
             f"no cache: the compiled-in snapshot, serial {st.get('serial')}")
        r = run.spb("models", "--profile", "e2e", json_out=True, label="s17 models offline feed")
        c.ok(r["rc"] == 0, "the model list still works without a cache")
    finally:
        shutil.rmtree(feeds_dir, ignore_errors=True)
        aside.rename(feeds_dir)
    infra.set_feed_modes(run, m1="404", m2="html", m3="stale", m4="valid")
    st = json_of(run.spb("feeds", "status", json_out=True, label="s17 feeds status restored"))
    c.ok(st.get("serial") == fi["valid_serial"], f"cache back: serial {st.get('serial')} for the rest of the run (m4 stays valid)")
    # The Models page with its source strip (applied profile e2e: the provider's list).
    log_f = open(run.out / "gui.log", "a")
    shots = {}
    for theme, lang in (("light", "en"), ("dark", "zh-CN")):
        p = gui(run, "models", theme, lang, log_f)
        try:
            time.sleep(8)
            xdo(run, "mousemove", str(GUI_SIZE[0] - 1), str(GUI_SIZE[1] - 1))
            shots[lang] = xshot(run, f"s17-models-{theme}-{'zh' if lang == 'zh-CN' else 'en'}")
        finally:
            stop(p)
    c.info(f"Models page with the source strip (look at them): {list(shots.values())}")
    finish(run, "s17", c, screenshots=shots)


# ---------------------------------------------------------------------------
# s18 per-model context override
# ---------------------------------------------------------------------------

FAKE_CLI = """#!/bin/sh
# e2e s18: a terminal Codex of another version that refuses every model catalog
# file (a broken template for this consumer); everything else is the real codex.
case "$*" in
  *--version*) echo "codex-cli 0.163.0"; exit 0 ;;
  *model_catalog_json*) echo "Error: failed to parse model_catalog_json: missing field \\`e2e_required_field\\`" >&2; exit 1 ;;
esac
# Not `exec`: Superba follows a launcher's exec target to the Codex it starts;
# this stand-in is its own program.
"{real}" "$@"
"""


@scenario("s18")
def s18(run):
    c = Check()
    switch_to(run, c, "compat", "s18-use-compat")
    cfg = flatten(config(run))
    slug = cfg.get("model")
    c.ok(slug == "gpt-5.6-luna", f"compat applied (API key, direct, line D); model = {slug!r} (the site's name for gpt-6-luna)")
    files_before = superba_files(run)
    res = edit_and_switch(run, c, "s18-set-override", "models", "set", slug, "--context", "123456", "--compact", "100k", "--profile", "compat")
    cfg = flatten(config(run))
    path = cfg.get("model_catalog_json") or ""
    m = re.fullmatch(re.escape(str(run.codex_home / "superba")) + r"/models-([0-9a-f]{8})\.json", path)
    c.ok(bool(m) and Path(path).exists(), f"config.toml model_catalog_json = <CODEX_HOME>/superba/models-<sha8>.json ({Path(path).name if path else None})")
    entries = catalog_entries(path) if m else {}
    e = entries.get(slug) or {}
    c.ok(e.get("context_window") == 123456 and e.get("auto_compact_token_limit") == 100000 and e.get("effective_context_window_percent") == 95,
         f"catalog entry {slug}: context_window {e.get('context_window')}, auto_compact_token_limit {e.get('auto_compact_token_limit')}, "
         f"effective {e.get('effective_context_window_percent')} % ({len(entries)} models in the file)")
    c.ok(hashlib.sha256(Path(path).read_bytes()).hexdigest()[:8] == (m.group(1) if m else ""), "the file name is its content address (sha256 prefix)")
    c.ok("model_catalog_json" in res["all"] and "Model catalog file" in res["all"], "the plan showed the catalog file and the new key")
    rec = json.loads((run.data_dir / "catalog-files.json").read_text())
    cur = ((rec.get("homes") or {}).get(str(run.codex_home)) or {}).get("current") or {}
    c.info(f"catalog-files.json: current {cur.get('file')} for consumer {cur.get('consumer')} (cli {cur.get('cli')}), files before {files_before}")
    dr, dm = codex_debug_models(run)
    dme = dm.get(slug) or {}
    c.ok(dr["rc"] == 0 and dme.get("context_window") == 123456 and dme.get("auto_compact_token_limit") == 100000,
         f"the bundled codex accepts the file: `codex debug models` (config) lists {len(dm)} models, {slug} window {dme.get('context_window')}, compaction {dme.get('auto_compact_token_limit')}")
    picker = model_list(run)
    c.ok(slug in picker and set(picker) <= set(entries), f"app-server model/list (the app's picker source) = the file's models: {len(picker)} listed, all from the file, incl. {slug}")
    since = time.time()
    line, e = app_turn(run, "E2E-MC1")
    c.ok(line == "d" and e.get("model") == slug and e["status"] == 200, f"a turn in a new chat: {fmt(e)}")
    ro = rollout_of_marker(run, "E2E-MC1", since)
    wins = context_windows(ro) if ro else []
    c.ok(wins and wins[-1] == 117283, f"rollout token_count model_context_window = {wins[-1:]} (123456 x 95 % = 117283)")
    acall(_with_cdp(run, lambda cc: cc.click_text(["New chat"])))
    c.info("screenshot (app after the turn): " + shot(run, "s18-app-after-turn"))

    # Krill API key: an override stored for a profile that is not applied.
    r = add_key_profile(run, "e2e-key", "krill-e2e", "--line", "a", label="s18 profile add e2e-key")
    c.ok(r["rc"] == 0, f"spb profile add e2e-key --site krill-e2e --auth apikey: exit {r['rc']}")
    r = run.spb("models", "set", "gpt-6-luna", "--context", "400k", "--compact", "80%", "--profile", "e2e-key", label="s18 set e2e-key")
    p = profile_of(run, "e2e-key")
    o = (p.get("model_overrides") or {}).get("gpt-6-luna") or {}
    c.ok(r["rc"] == 0 and o.get("context_window") == 400000, f"profile not applied: override stored for the next switch ({json.dumps(o)})")
    res = switch_to(run, c, "e2e-key", "s18-use-e2e-key")
    cfg = flatten(config(run))
    path2 = cfg.get("model_catalog_json") or ""
    e2 = catalog_entries(path2).get("gpt-6-luna", {}) if path2 else {}
    c.ok(path2 and path2 != path and e2.get("context_window") == 400000 and e2.get("auto_compact_token_limit") == 320000,
         f"Krill API key profile: own catalog file {Path(path2).name if path2 else None}, gpt-6-luna window {e2.get('context_window')}, compaction {e2.get('auto_compact_token_limit')} (80 %)")
    c.ok("no longer look like stock Codex" in res["all"], "the plan warns that a catalog file makes Krill requests differ from stock Codex")
    c.ok(Path(path).exists() and len(superba_files(run)) == 2, f"previous file kept: {superba_files(run)}")
    since = time.time()
    r = exec_turn(run, "E2E-MC2: reply with exactly the text PONG-MC2", "-m", "gpt-6-luna", label="s18 exec e2e-key")
    e = wait_post(run, "a", "E2E-MC2", since - 1)
    ro = rollout_of_marker(run, "E2E-MC2", since)
    wins = context_windows(ro) if ro else []
    c.ok(r["rc"] == 0 and e.get("auth_kind") == "bearer" and wins and wins[-1] == 380000,
         f"codex exec on line A with the catalog: {fmt(e)}; rollout model_context_window {wins[-1:]} (400000 x 95 %)")

    # Reset: the key goes, the file stays one generation, then it is pruned.
    res = edit_and_switch(run, c, "s18-reset-e2e-key", "models", "reset", "--all", "--profile", "e2e-key")
    cfg = flatten(config(run))
    c.ok("model_catalog_json" not in cfg, f"spb models reset --all: model_catalog_json removed from config.toml; files {superba_files(run)}")
    switch_to(run, c, "compat", "s18-back-to-compat")
    left = superba_files(run)
    c.ok(Path(path2).name not in left and Path(path).name in left, f"after the next switch the e2e-key file is pruned; compat's current file stays: {left}")

    # A terminal Codex that refuses the file. First the start-up check (run before
    # every command that executes) repairs the configuration so no Codex is left
    # with a file it cannot load; then a switch that would write the file again is
    # blocked by the preflight and writes nothing.
    fake = run.bin_dir / "codex"
    target = os.readlink(fake)
    try:
        fake.unlink()
        fake.write_text(FAKE_CLI.format(real=target))
        fake.chmod(0o755)
        c.info("the `codex` first on PATH is now a stand-in reporting 0.163.0 that refuses every model catalog file")
        rec = spb_stream(run, "recover", label="s18-recover-refused")
        cfg = flatten(config(run))
        c.ok(rec["rc"] == 0 and "terminal Codex" in rec["all"] and "model_catalog_json" not in cfg,
             f"start-up check: the file is refused by the changed terminal Codex, the configuration is rewritten without it "
             f"(write-only): {' | '.join(l.strip() for l in rec['all'].splitlines() if 'catalog' in l.lower())[:300]}")
        cfg_sha, files0 = file_sha(run.codex_home / "config.toml"), superba_files(run)
        res = spb_stream(run, "profile", "use", "compat", "--countdown", "0", "--yes", label="s18-preflight-blocked")
        c.ok(res["rc"] != 0 and "catalog" in res["all"].lower(),
             f"switch blocked by the preflight (exit {res['rc']}): {next((l.strip() for l in res['all'].splitlines() if 'catalog' in l.lower()), '')[:240]}")
        c.ok(file_sha(run.codex_home / "config.toml") == cfg_sha and superba_files(run) == files0,
             f"zero writes: config.toml unchanged, catalog files unchanged {superba_files(run)}")
    finally:
        fake.unlink(missing_ok=True)
        fake.symlink_to(target)
    edit_and_switch(run, c, "s18-reset-compat", "models", "reset", "--all", "--profile", "compat")
    cfg = flatten(config(run))
    c.ok("model_catalog_json" not in cfg and cfg.get("model") == slug, f"compat without overrides again: no catalog key, model {cfg.get('model')!r}")
    finish(run, "s18", c)


# ---------------------------------------------------------------------------
# s19 speed tiers in API-key direct mode
# ---------------------------------------------------------------------------


@scenario("s19")
def s19(run):
    c = Check()
    reset_e2e_lines(run)
    if flatten(config(run)).get("openai_base_url") != f"{run.line_url('d')}/codex/v1":
        switch_to(run, c, "compat", "s19-use-compat")
    edit_and_switch(run, c, "s19-fast-compat", "speed", "fast", "--profile", "compat")
    cfg = flatten(config(run))
    c.ok(cfg.get("service_tier") == "fast" and "model_catalog_json" not in cfg,
         f"spb speed fast (custom site D): service_tier = {cfg.get('service_tier')!r}, no catalog file needed")
    sp = json_of(run.spb("speed", json_out=True, label="s19 speed compat"))
    sup = sp.get("support") or {}
    c.ok((sup.get("cli") or {}).get("kind") == "applies" and (sup.get("app") or {}).get("kind") == "app_forces_standard",
         f"spb speed --json: terminal Codex {sup.get('cli')}, desktop app {sup.get('app')} (API-key mode forces Standard); offered {sup.get('offered')}")
    since = time.time()
    r = exec_turn(run, "E2E-SP1: reply with exactly the text PONG-SP1", label="s19 exec fast")
    e = wait_post(run, "d", "E2E-SP1", since - 1)
    c.ok(r["rc"] == 0 and e.get("originator") == "codex_exec" and e.get("service_tier") == "priority",
         f"codex exec through forwarder D sends service_tier priority: {fmt(e)}")
    line, e = app_turn(run, "E2E-SP2")
    c.info(f"the app was opened in PAT mode (s2) and only hot-switched to the API key since: its renderer keeps the "
           f"PAT-era speed handling and sends the configured tier: {fmt(e)}")
    # The claim of the effect line is about an app opened in API-key mode.
    switch_to(run, c, "e2e-key", "s19-use-e2e-key")
    main0, _ = app_servers(run)
    res = spb_stream(run, "launch", "--restart", "--yes", label="s19-relaunch-apikey")
    main1, servers1 = infra.wait_for("app-server after the relaunch", lambda: (lambda m, s: (m, s) if m and s else None)(*app_servers(run)), 90)
    c.ok(res["rc"] == 0 and main1 != main0, f"spb launch --restart: the app opened again in API-key mode (main {main0} -> {main1}) [{res['log']}]")
    onboarding(run, c)
    switch_to(run, c, "compat", "s19-use-compat-fast")
    line, e = app_turn(run, "E2E-SP2B")
    c.ok(line == "d" and e.get("originator") == "Codex Desktop" and not e.get("tier_sent"),
         f"the desktop app opened in API-key mode sends no tier (config service_tier = fast): {fmt(e)}")
    edit_and_switch(run, c, "s19-standard-compat", "speed", "standard", "--profile", "compat")

    # Ultrafast: needs a catalog file whose entries list `ultrafast` where the
    # line's list does (Krill: gpt-6-astra only).
    switch_to(run, c, "e2e-key", "s19-use-e2e-key-again")
    r = run.spb("models", "default", "gpt-6-astra", "--profile", "e2e-key", "--write-only", label="s19 default astra")
    res = edit_and_switch(run, c, "s19-ultrafast-key", "speed", "ultrafast", "--profile", "e2e-key")
    cfg = flatten(config(run))
    path = cfg.get("model_catalog_json") or ""
    entries = catalog_entries(path) if path else {}
    ultra = sorted(s for s, m in entries.items() if any(t.get("id") == "ultrafast" for t in (m.get("service_tiers") or [])))
    c.ok(cfg.get("service_tier") == "ultrafast" and cfg.get("model") == "gpt-6-astra" and ultra == ["gpt-6-astra"],
         f"spb models default gpt-6-astra && spb speed ultrafast (Krill, API key): service_tier {cfg.get('service_tier')!r}, model {cfg.get('model')!r}, "
         f"catalog lists ultrafast for {ultra} only")
    since = time.time()
    r = exec_turn(run, "E2E-SP3: reply with exactly the text PONG-SP3", label="s19 exec ultrafast")
    e = wait_post(run, "a", "E2E-SP3", since - 1)
    c.ok(r["rc"] == 0 and e.get("model") == "gpt-6-astra" and e.get("service_tier") == "ultrafast",
         f"codex exec sends ultrafast: {fmt(e)}")
    sp = json_of(run.spb("speed", json_out=True, label="s19 speed e2e-key"))
    c.info(f"spb speed --json (Krill API key, no tier names): {json.dumps(sp.get('support'))}")
    edit_and_switch(run, c, "s19-standard-key", "speed", "standard", "--profile", "e2e-key")
    run.spb("models", "default", "--clear", "--profile", "e2e-key", "--write-only", label="s19 clear default")

    # Tier-suffixed model names (the Krill-kind test site whose relay maps them).
    r = add_key_profile(run, "sfx", "krill-sfx", label="s19 profile add sfx")
    run.spb("models", "default", "gpt-6-astra", "--profile", "sfx", label="s19 sfx default")
    run.spb("speed", "ultrafast", "--profile", "sfx", label="s19 sfx ultrafast")
    sp = json_of(run.spb("speed", "--profile", "sfx", json_out=True, label="s19 speed sfx"))
    c.ok(((sp.get("support") or {}).get("app") or {}).get("model") == "gpt-6-astra-ultrafast",
         f"spb speed --profile sfx: desktop app {json.dumps((sp.get('support') or {}).get('app'))}")
    switch_to(run, c, "sfx", "s19-use-sfx")
    cfg = flatten(config(run))
    path = cfg.get("model_catalog_json") or ""
    entries = catalog_entries(path) if path else {}
    sfx = entries.get("gpt-6-astra-ultrafast") or {}
    c.ok(cfg.get("model") == "gpt-6-astra-ultrafast" and cfg.get("service_tier") == "default" and sfx and not sfx.get("service_tiers"),
         f"site with tier names: model = {cfg.get('model')!r}, service_tier = {cfg.get('service_tier')!r}; catalog entry "
         f"{sfx.get('display_name')!r} with service_tiers {sfx.get('service_tiers')}")
    line, e = app_turn(run, "E2E-SP4")
    c.ok(line == "e" and e.get("model") == "gpt-6-astra-ultrafast" and not e.get("service_tier"),
         f"the desktop app's turn: model gpt-6-astra-ultrafast, no tier: {fmt(e)}")
    label, picker, png = model_picker(run, "s19-app-model-picker")
    c.ok("Ultrafast" in (label or "") or any("Ultrafast" in m for m in picker),
         f"the app shows the model by its tier name: composer {label!r}, picker {[m for m in picker if 'Astra' in m]} [{png}]")
    edit_and_switch(run, c, "s19-standard-sfx", "speed", "standard", "--profile", "sfx")
    cfg = flatten(config(run))
    c.ok(cfg.get("service_tier") == "default" and cfg.get("model") == "gpt-6-astra", f"spb speed standard: service_tier = {cfg.get('service_tier')!r}, model {cfg.get('model')!r}")
    since = time.time()
    r = exec_turn(run, "E2E-SP5: reply with exactly the text PONG-SP5", "-m", "gpt-6-luna", label="s19 exec standard luna")
    e = wait_post(run, "e", "E2E-SP5", since - 1)
    c.ok(r["rc"] == 0 and e.get("model") == "gpt-6-luna" and not e.get("tier_sent"),
         f"Standard: a codex exec run on gpt-6-luna sends no tier (bundled default priority is not applied): {fmt(e)}")
    sp = json_of(run.spb("speed", "--profile", "official", json_out=True, label="s19 speed official"))
    r = run.spb("speed", "fast", "--profile", "official", label="s19 speed official set")
    c.ok(((sp.get("support") or {}).get("app") or {}).get("kind") == "app_menu" and r["rc"] != 0,
         f"official login: the app's own menu decides ({json.dumps(sp.get('support'))}); setting a tier refused (exit {r['rc']})")
    switch_to(run, c, "e2e", "s19-back-to-e2e")
    cfg = flatten(config(run))
    c.ok(cfg.get("openai_base_url") == run.line_url("a") + "/backend-api/codex" and auth_shape(run) == ["OPENAI_API_KEY", "personal_access_token"],
         "back to profile e2e (PAT, line A)")
    line, e = app_turn(run, "E2E-SP6")
    c.ok(line == "a" and e.get("auth_kind") == "pat", f"PAT works after the hot switch in the app opened in API-key mode: {fmt(e)}")
    finish(run, "s19", c)


# ---------------------------------------------------------------------------
# s20 request log and stats line
# ---------------------------------------------------------------------------

# The Chinese default template; {tps} is "—" when unmeasurable (output < 10
# tokens or a window < 0.2 s, spike RS 1).
LINE_RE = re.compile("^\u26a1 ([\\d,]+|\u2014) tok/s \u00b7 \u9996\u5b57 ([\\d.]+)s \u00b7 \u672c\u8f6e ([\\d,]+) in / ([\\d,]+) out$")
ST1_PROMPT = "reply with the text PONG-ST1, then one sentence of about forty words about lighthouses"
TOOL_PROMPT = ("use your shell tool to run the command `echo superba-tool-$((6*7))`, then reply with exactly the "
               "text DONE-ST3 followed by the number the command printed")


def last_reply_quotes(run):
    """Text of the blockquotes in the last assistant message of the open thread,
    and the message's full text (the WORD JOINER of the marker kept)."""
    js = r"""(() => {
      const turns = [...document.querySelectorAll('[data-markdown-text-style="assistant-message"]')];
      const last = turns[turns.length - 1];
      if (!last) return null;
      return {quotes: [...last.querySelectorAll('blockquote')].map((b) => b.innerText.trim()), text: last.innerText};
    })()"""
    return acall(_with_cdp(run, lambda c: c.eval(js)))


def all_quotes(run):
    js = r"""[...document.querySelectorAll('[data-markdown-text-style="assistant-message"]')].map((m) =>
      ({final: !!m.closest('[data-local-conversation-final-assistant="true"]'),
        quotes: [...m.querySelectorAll('blockquote')].map((b) => b.innerText.trim())}))"""
    return acall(_with_cdp(run, lambda c: c.eval(js))) or []


def stats_line(text):
    """The stats line in `text` with the WORD JOINER removed, matched against the
    Chinese default template; returns the match or None."""
    for line in (text or "").splitlines():
        line = line.strip().lstrip(">").strip().replace("⁠", "")
        m = LINE_RE.match(line)
        if m:
            return m
    return None


def router_rows(run, label, *args):
    r = run.spb("usage", "log", "--since", "1h", "--limit", "500", *args, json_out=True, label=label)
    return json_of(r).get("rows") or [], json_of(r)


def header_values(value, name):
    """Values of the header `name` anywhere in recorded content (maps,
    [name, value] pairs or {name, value} objects)."""
    out = []
    if isinstance(value, dict):
        for k, v in value.items():
            if k.lower() == name and isinstance(v, str):
                out.append(v)
            else:
                out += header_values(v, name)
        if str(value.get("name", "")).lower() == name and isinstance(value.get("value"), str):
            out.append(value["value"])
    elif isinstance(value, list):
        if len(value) == 2 and isinstance(value[0], str) and value[0].lower() == name and isinstance(value[1], str):
            out.append(value[1])
        else:
            for v in value:
                out += header_values(v, name)
    return out


# Timing marks of a request in the router's log, in the order they happen
# (request sent, response headers, first event, first output, last delta, end).
TIMING_MARKS = ("sent_ms", "ttfb_ms", "first_event_ms", "first_token_ms", "last_delta_ms", "duration_ms")


def router_timing_rows(run, since):
    """The router's own rows since `since` (epoch s) with their timing marks,
    read from its request log (read-only; numbers only)."""
    db = run.router_dir / "usage.sqlite3"
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    try:
        cols = {r[1] for r in con.execute("PRAGMA table_info(requests)")}
        sel = ["id", "started_ms", "transport", "status_kind", "output_tokens", "tps_milli", "injected"]
        sel += [m for m in TIMING_MARKS if m in cols]
        con.row_factory = sqlite3.Row
        return [dict(r) for r in con.execute(f"SELECT {', '.join(sel)} FROM requests WHERE started_ms >= ? ORDER BY id",
                                             (int(since * 1000),))]
    finally:
        con.close()


def expected_tps_milli(output, first_event, end):
    """superba-core requests::tps_milli: output tokens over first event -> end,
    none below 10 tokens or a 200 ms window."""
    if output is None or first_event is None or end is None or end < first_event:
        return None
    window = end - first_event
    if window < 200 or output < 10:
        return None
    return output * 1_000_000 // window


def timing_problems(row):
    """What is wrong with one row's timing marks: missing marks of a completed
    request, marks out of order, a TPS that does not follow from them."""
    problems = []
    marks = [(m, row.get(m)) for m in TIMING_MARKS]
    needed = ["sent_ms", "ttfb_ms", "first_event_ms", "duration_ms"] + (["first_token_ms"] if row.get("output_tokens") else [])
    missing = [m for m in needed if row.get(m) is None]
    if missing:
        problems.append(f"missing {missing}")
    present = [(m, v) for m, v in marks if v is not None]
    for (m1, v1), (m2, v2) in zip(present, present[1:]):
        if v2 < v1 or v1 < 0:
            problems.append(f"{m1} {v1} > {m2} {v2}")
    # The router divides by its own whole-ms marks before they are normalized
    # for storage, so allow 2 % and a few ms at the 200 ms threshold.
    got = row.get("tps_milli")
    want = expected_tps_milli(row.get("output_tokens"), row.get("first_event_ms"), row.get("duration_ms"))
    window = (row.get("duration_ms") or 0) - (row.get("first_event_ms") or 0)
    if (got is None) != (want is None):
        if not 195 <= window <= 205:
            problems.append(f"tps_milli {got} but {want} expected (window {window} ms, {row.get('output_tokens')} tokens)")
    elif got is not None and abs(got - want) > 0.02 * want:
        problems.append(f"tps_milli {got} != {want}")
    return problems


def scan_for_secrets(paths, secrets):
    hits = []
    for p in paths:
        p = Path(p)
        if not p.exists():
            continue
        data = p.read_bytes()
        for name, sec in secrets.items():
            if sec and sec.encode() in data:
                hits.append(f"{p.name}:{name}")
    return hits


@scenario("s20")
def s20(run):
    c = Check()
    run.d["ui_locale"] = "zh-CN"
    run.save()
    try:
        _s20(run, c)
    finally:
        run.d.pop("ui_locale", None)
        run.save()
        # Leave no router behind for the next scenarios, also after a failure.
        if router_status(run, "s20 status at the end").get("installed"):
            run.spb("router", "stats-line", "off", label="s20 cleanup stats-line off")
            run.spb("router", "disable", "--countdown", "0", "--yes", "--force", label="s20 cleanup disable", timeout=300)
            run.spb("router", "uninstall", label="s20 cleanup uninstall")
    finish(run, "s20", c)


def _s20(run, c):
    c.info("SUPERBA_LOCALE=zh-CN: the UI language Superba writes the default stats line in (output kept English with --locale en)")
    r = spb_stream(run, "--locale", "en", "router", "install", "--member", "e2e", label="s20-router-install")
    st = router_status(run, "s20 router status")
    c.ok(r["rc"] == 0 and st.get("running"), f"router installed with member e2e (lines a, b, c), port {st.get('port')} [{r['log']}]")
    r = run.spb("--locale", "en", "router", "stats-line", "on", label="s20 stats-line on")
    c.ok(r["rc"] == 0, f"spb router stats-line on: {r['out'].strip()[:200]}")
    before = app_servers(run)
    res = spb_stream(run, "--locale", "en", "router", "enable", "--countdown", "0", "--yes", label="s20-router-enable")
    hot_switch_ok(c, res, before, app_servers(run), "spb router enable")
    st = router_status(run, "s20 status enabled")
    stats = st.get("stats") or (infra.router_admin(run) or {}).get("stats") or {}
    c.ok(st.get("codex_routed"), f"Codex routed through 127.0.0.1:{st.get('port')}; recorder {json.dumps(stats)[:200]}")
    token = router_token(run)

    # A user turn: the reply ends with the stats line.
    since = time.time()
    line, e1 = app_turn(run, "E2E-ST1", ST1_PROMPT)
    c.ok(line == "a" and e1["status"] == 200, f"turn through the router: {fmt(e1)}")
    time.sleep(1.5)
    reply = last_reply_quotes(run) or {}
    quotes = reply.get("quotes") or []
    m = next((stats_line(q) for q in quotes if stats_line(q)), None)
    ends = bool(quotes) and reply.get("text", "").rstrip().endswith(quotes[-1])
    c.ok(m is not None and m.group(1) != "\u2014" and ends,
         f"the reply's DOM ends with a blockquote matching `\u26a1 <n> tok/s · 首字 <s>s · 本轮 <in> in / <out> out`: {quotes[-1:]!r}")
    c.info("screenshot: " + shot(run, "s20-app-stats-line"))
    ro = rollout_of_marker(run, "E2E-ST1", since)
    held = ro and MARKER in Path(ro).read_text(encoding="utf-8")
    c.ok(bool(held), f"the rollout holds the line (the app stored the reply with it): {Path(ro).name if ro else None}")
    thread = probe(run)["conversation"]

    # Next turn in the same thread: the line is stripped from the request.
    since2 = time.time()
    send_turn(run, "E2E-ST2: reply with exactly the text PONG-ST2")
    line, e2 = wait_turn(run, "E2E-ST2", since2)
    c.ok(e2.get("markers") == 0 and e2.get("n_input", 0) > e1.get("n_input", 0),
         f"next turn: the forwarder sees {e2.get('markers')} stats markers in a request of {e2.get('n_input')} input items (the earlier reply included): {fmt(e2)}")

    # A tool loop: the line only under the final answer, with the turn's totals.
    new_chat(run)
    since3 = time.time()
    send_turn(run, f"E2E-ST3: {TOOL_PROMPT}")
    # The app's sandbox cannot start inside the test namespace, so the model asks
    # to run the command outside it: approve like a user would.
    approve_js = ("(() => { const b = [...document.querySelectorAll('button')].find((b) =>"
                  " (b.innerText || '').trim().startsWith('Allow once')); if (!b) return null; b.click(); return 'Allow once'; })()")
    final_js = ("[...document.querySelectorAll('[data-local-conversation-final-assistant=\"true\"] "
                "[data-markdown-text-style=\"assistant-message\"]')].map((m) => m.innerText).join(' | ')")
    end = time.time() + 240
    approved = []
    while time.time() < end:
        clicked = acall(_with_cdp(run, lambda cc: cc.eval(approve_js)))
        if clicked:
            approved.append(clicked)
        final = acall(_with_cdp(run, lambda cc: cc.eval(final_js))) or ""
        if "DONE-ST3" in final and not probe(run)["stop"]:
            break
        time.sleep(2)
    reqs = posts(run, "a", "E2E-ST3", since3) + posts(run, "b", "E2E-ST3", since3)
    time.sleep(1.5)
    msgs = all_quotes(run)
    with_line = [(m["final"], stats_line("\n".join(m["quotes"]))) for m in msgs if stats_line("\n".join(m["quotes"]))]
    c.info(f"tool turn: {len(reqs)} Responses requests carried the prompt; approvals clicked: {approved}; "
           f"{len(msgs)} assistant messages rendered")
    c.ok(len(reqs) >= 2 and len(with_line) == 1 and with_line[0][0],
         f"tool-loop turn ({len(reqs)} requests): exactly one stats line, on the final answer: "
         f"{[m['quotes'] for m in msgs][-3:]}")
    rows, _ = router_rows(run, "s20 router log tool turn")
    rows = [r for r in rows if r.get("source", "router") == "router"]
    turn_ids = {r.get("turn_id") for r in rows if r.get("started_at", 0) >= since3 * 1000 - 1000 and r.get("turn_id")}
    if with_line and turn_ids:
        m3 = with_line[0][1]
        shown_in, shown_out = int(m3.group(3).replace(",", "")), int(m3.group(4).replace(",", ""))
        tr = [r for r in rows if r.get("turn_id") in turn_ids and r.get("thread_source") in (None, "user")]
        by_turn = {}
        for r in tr:
            t = by_turn.setdefault(r["turn_id"], [0, 0, 0])
            t[0] += r.get("input_tokens") or 0
            t[1] += r.get("output_tokens") or 0
            t[2] += 1
        c.ok(any(v[0] == shown_in and v[1] == shown_out for v in by_turn.values()),
             f"the line shows the turn's totals ({shown_in:,} in / {shown_out:,} out) = the sum of the turn's router rows "
             f"{ {k[:8]: v for k, v in by_turn.items()} } (in, out, requests)")
    c.info("screenshot: " + shot(run, "s20-app-tool-turn"))

    # codex exec through the router: never a line.
    out_file = Path(run.home) / "proj" / "exec-last.txt"
    since4 = time.time()
    r = exec_turn(run, "E2E-ST4: reply with exactly the text PONG-ST4", out_file=out_file, label="s20 exec -o")
    text = out_file.read_text() if out_file.exists() else ""
    c.ok(r["rc"] == 0 and "PONG-ST4" in text and MARKER not in text and "⚡" not in text,
         f"codex exec -o through the router: {text.strip()[:80]!r}, no stats line")

    # The request log.
    rows, page = router_rows(run, "s20 router log")
    c.ok(len(rows) >= 4, f"spb usage log --json: {len(rows)} rows, totals {json.dumps(page.get('totals'))[:300]}")
    # Unknown values are left out of the JSON (tps below its noise thresholds,
    # cost without a price).
    want = ("input_tokens", "output_tokens", "cached_tokens", "cache_write_tokens", "first_token_ms", "duration_ms", "status", "prompt_cache_key")
    # The log merges Codex's rollouts (source "rollout": no first token, no
    # cache key) with the router's own rows (source "router").
    routed_rows = [r for r in rows if r.get("source", "router") == "router"]
    c.info(f"log sources: {len(routed_rows)} router rows, {len(rows) - len(routed_rows)} rows from Codex rollouts")
    r0 = next((r for r in routed_rows if r.get("output_tokens")), routed_rows[0] if routed_rows else {})
    missing = [k for k in want if k not in r0]
    with_tps = [r for r in routed_rows if r.get("tps_milli")]
    c.ok(rows and not missing and with_tps, f"rows carry the numbers (missing: {missing}); example: "
         + json.dumps({k: r0.get(k) for k in want if k != "prompt_cache_key"}) + f", prompt_cache_key {'set' if r0.get('prompt_cache_key') else 'absent'}; "
         f"{len(with_tps)} rows with tps (e.g. {with_tps[0]['tps_milli'] / 1000 if with_tps else None} tok/s)")
    # Timing (user bug 12): every mark taken when the router processed it, in
    # order (0 ms phases are valid, e.g. headers and the first event in one
    # read), TPS = output tokens over first event -> end with the noise guard,
    # and the stats lines show the numbers of the request they were added to.
    trows = router_timing_rows(run, since)
    done = [r for r in trows if r.get("status_kind") == "ok"]
    bad = {r["id"]: timing_problems(r) for r in done if timing_problems(r)}
    zero = [r["id"] for r in done if r.get("ttfb_ms") is not None and r.get("ttfb_ms") == r.get("first_event_ms")]
    c.ok(done and not bad, f"timing marks of {len(done)} completed router requests: sent <= headers <= first event <= "
         f"first output <= last delta <= end, TPS from first event -> end ({len(zero)} with headers and the first event "
         f"at the same ms); problems: {bad or 'none'}")
    ex = done[-1] if done else {}
    c.info("example marks (ms): " + json.dumps({m: ex.get(m) for m in TIMING_MARKS + ("output_tokens", "tps_milli")}))
    injected = [r for r in trows if r.get("injected")]
    shown = [(n, l) for n, l in (("ST1", m), ("ST3", with_line[0][1] if with_line else None)) if l]
    mism = []
    for name, ln in shown:
        tps_txt, ttft_s = ln.group(1), float(ln.group(2))
        match = [r for r in injected if r.get("first_token_ms") is not None and abs(r["first_token_ms"] - ttft_s * 1000) <= 10
                 and ((tps_txt == "—" and r.get("tps_milli") is None)
                      or (tps_txt != "—" and r.get("tps_milli") is not None and abs(r["tps_milli"] / 1000 - int(tps_txt.replace(",", ""))) <= 1))]
        if not match:
            mism.append(f"{name}: {ln.group(0)!r}")
    c.ok(shown and injected and not mism,
         f"the stats lines' tok/s and 首字 are the numbers of the request they were added to ({len(injected)} rows with "
         f"the line: {[(r.get('first_token_ms'), r.get('tps_milli')) for r in injected]}); mismatches: {mism or 'none'}")
    tot = page.get("totals") or {}
    c.info(f"estimated cost at list prices: {tot.get('cost')} USD over {tot.get('requests')} requests ({tot.get('unpriced')} unpriced)")
    save(run, "s20-router-log.json", json.dumps(page, indent=1, ensure_ascii=False))
    stats_json = json_of(run.spb("usage", "stats", "--by", "model", "--since", "1h", json_out=True, label="s20 usage stats"))
    c.info("spb usage stats --by model: " + json.dumps(stats_json)[:400])
    tr = run.spb("usage", "log", "--since", "1h", "--limit", "20", label="s20 usage log text")
    save(run, "s20-router-log.txt", tr["out"])

    # The GUI Usage page: rows with session names, totals, detail pane.
    log_f = open(run.out / "gui.log", "a")
    shots = {}
    for theme, lang in (("dark", "zh-CN"), ("light", "en")):
        p = gui(run, "usage", theme, lang, log_f)
        try:
            time.sleep(9)
            suffix = f"{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
            shots[f"router-{suffix}"] = xshot(run, f"s20-router-{suffix}")
            xdo(run, "mousemove", "700", "274", "click", "1")
            time.sleep(1.5)
            shots[f"router-detail-{suffix}"] = xshot(run, f"s20-router-detail-{suffix}")
            # The detail pane's second tab (计时 / Timing).
            xdo(run, "mousemove", "478" if lang == "zh-CN" else "506", "571", "click", "1")
            time.sleep(1.5)
            shots[f"router-timing-{suffix}"] = xshot(run, f"s20-router-timing-{suffix}")
        finally:
            stop(p)
    c.info(f"Usage page screenshots (look at them: session titles from state_5.sqlite, totals, detail pane with 通用 and the 计时 tab): {sorted(shots.values())}")

    # Request content is never recorded (user decision 22): no flag for it, no
    # content database after a turn through the router.
    r = run.spb("--locale", "en", "router", "record", "on", "--content", "on", label="s20 record content on")
    line, e5 = app_turn(run, "E2E-ST5")
    time.sleep(2)
    content = sorted(p.name for p in run.router_dir.glob("content*"))
    c.ok(r["rc"] != 0 and not content,
         f"request content capture is gone: `spb router record --content on` refused (exit {r['rc']}), "
         f"no content database after a routed turn ({content or 'none'})")
    secrets = {"e2e key": run.key, "router token": token or ""}
    rt = infra.router_runtime(run) or {}
    secrets["admin token"] = rt.get("admin_token", "")
    files = [run.router_dir / n for n in ("usage.sqlite3", "usage.sqlite3-wal", "usage.sqlite3-shm", "router.log")]
    hits = scan_for_secrets(files, secrets)
    c.ok(not hits, f"key-bytes scan of {[f.name for f in files if f.exists()]}: {hits or 'no key, router token or admin token'}")
    c.ok(all(os.stat(f).st_mode & 0o077 == 0 for f in files if f.exists() and f.suffix == ".sqlite3"), "the router databases are 0600")

    # Off again: stats line off, router mode off, router removed.
    run.spb("--locale", "en", "router", "stats-line", "off", label="s20 stats-line off")
    before = app_servers(run)
    res = spb_stream(run, "--locale", "en", "router", "disable", "--countdown", "0", "--yes", label="s20-router-disable")
    hot_switch_ok(c, res, before, app_servers(run), "spb router disable")
    r = run.spb("--locale", "en", "router", "uninstall", label="s20 router uninstall")
    c.ok(r["rc"] == 0 and not router_status(run, "s20 status after").get("running"), f"router uninstalled (exit {r['rc']})")


# ---------------------------------------------------------------------------
# s21 pinning the desktop app's Codex
# ---------------------------------------------------------------------------


def pkg_log(run, since):
    path = run.out / "pkgcache.jsonl"
    return [json.loads(l) for l in path.read_text().splitlines() if json.loads(l).get("ts", 0) >= since] if path.exists() else []


def store_bin(run):
    return run.data_dir / "codex" / f"{PKG_VERSION}-{STORE_TARGET}" / "bin" / "codex"


def full_restart(run, c, label, *args, env=None):
    """A Superba command that fully restarts the app; returns (res, main, servers)."""
    main0, _ = app_servers(run)
    if env:
        r = run.run([run.d["spb"], *args], env=run.base_env(**env), timeout=300, label=label)
        res = {"rc": r["rc"], "all": r["out"] + r["err"], "secs": r["secs"], "log": "cmds.log"}
    else:
        res = spb_stream(run, *args, label=label)
    main1, servers = infra.wait_for(f"{label}: app-server after the restart",
                                    lambda: (lambda m, s: (m, s) if m and m != main0 and s else None)(*app_servers(run)), 120)
    onboarding(run, c)
    return res, main1, servers


def newest_session_meta(run, marker, since):
    ro = rollout_of_marker(run, marker, since)
    if not ro:
        return None, None
    for e in rollout_events(ro):
        if e.get("type") == "session_meta":
            return ro, e.get("payload") or {}
    return ro, None


@scenario("s21")
def s21(run):
    c = Check()
    reset_e2e_lines(run)
    sbin = store_bin(run)
    infra.set_pkg_mode(run, "corrupt")
    t0 = time.time()
    r = run.spb("codex", "install", PKG_VERSION, label="s21 install corrupted")
    got = [e for e in pkg_log(run, t0) if e["path"].endswith(".tar.gz")]
    c.ok(r["rc"] != 0 and not sbin.exists() and got,
         f"a corrupted package from the package source is rejected (exit {r['rc']}, nothing stored): "
         f"{(r['err'] or r['out']).strip().splitlines()[-1][:220] if (r['err'] or r['out']).strip() else ''}")
    leftovers = [p.name for p in (run.data_dir / "codex").glob(f"*{PKG_VERSION}*")] if (run.data_dir / "codex").exists() else []
    c.info(f"store after the rejected download: {leftovers}")
    infra.set_pkg_mode(run, "normal")
    t1 = time.time()
    r = run.spb("codex", "install", PKG_VERSION, label="s21 install")
    ver = run.run([str(sbin), "--version"], label="store codex --version") if sbin.exists() else {"out": ""}
    c.ok(r["rc"] == 0 and sbin.exists() and ver["out"].strip() == f"codex-cli {PKG_VERSION}",
         f"spb codex install {PKG_VERSION}: verified (sha256 from the feed) and extracted into the store: {ver['out'].strip()!r} "
         f"({len([e for e in pkg_log(run, t1) if e['path'].endswith('.tar.gz')])} package request(s) to the local source)")

    res, main, servers = full_restart(run, c, "s21-pin", "codex", "pin", PKG_VERSION, "--yes")
    exe = exe_of(servers[0]) if servers else None
    c.ok(res["rc"] == 0 and "Full restart" in res["all"] and same_file(exe, sbin),
         f"spb codex pin {PKG_VERSION}: full restart, app-server /proc/<pid>/exe = {exe} [{res['log']}]")
    env_main = infra.environ(main)
    c.ok(env_main.get("CODEX_CLI_PATH") == str(sbin), "the app was launched with CODEX_CLI_PATH = the store binary")
    since = time.time()
    line, e = app_turn(run, "E2E-PIN1")
    ro, meta = newest_session_meta(run, "E2E-PIN1", since)
    c.ok(line == "a" and e["status"] == 200 and (meta or {}).get("cli_version") == PKG_VERSION,
         f"a turn completes on the pinned Codex: {fmt(e)}; rollout session_meta.cli_version = {(meta or {}).get('cli_version')}")
    lst = json_of(run.spb("codex", "list", json_out=True, label="s21 codex list pinned"))
    c.info("spb codex list --json (app section): " + json.dumps({k: v for k, v in lst.items() if k in ("app_codex", "pin", "effective")})[:400])
    log_f = open(run.out / "gui.log", "a")
    shots = []
    for theme, lang in (("light", "zh-CN"), ("dark", "en")):
        p = gui(run, "updates", theme, lang, log_f)
        try:
            time.sleep(8)
            shots.append(xshot(run, f"s21-updates-pinned-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"))
        finally:
            stop(p)
    c.info(f"Updates page with the pin (look at them: 已固定 / fixed version): {shots}")

    res, main, servers = full_restart(run, c, "s21-unpin", "codex", "unpin", "--yes")
    exe = exe_of(servers[0]) if servers else None
    bundled = str(Path(run.d["app_dir"]) / "resources" / "codex")
    c.ok(res["rc"] == 0 and same_file(exe, bundled) and "CODEX_CLI_PATH" not in infra.environ(main),
         f"spb codex unpin: full restart, app-server exe = the bundled {exe}")
    res, main, servers = full_restart(run, c, "s21-inherited", "launch", "--restart", "--yes",
                                      env={"CODEX_CLI_PATH": str(sbin)})
    exe = exe_of(servers[0]) if servers else None
    c.ok(res["rc"] == 0 and same_file(exe, bundled) and "CODEX_CLI_PATH" not in infra.environ(main),
         f"spb launch --restart with CODEX_CLI_PATH={sbin.name} inherited from the launcher's environment and no pin: "
         f"the app still runs the bundled Codex ({exe})")
    line, e = app_turn(run, "E2E-PIN2")
    c.ok(line == "a" and e["status"] == 200, f"a turn after unpinning: {fmt(e)}")
    finish(run, "s21", c, screenshots=shots)


# ---------------------------------------------------------------------------
# s22 remote host (Docker sshd)
# ---------------------------------------------------------------------------

REMOTE_IMAGE = "superba-remote-it:v1"
DOCKER_IT = infra.REPO / "crates" / "superba-remote" / "tests" / "docker_it.rs"
SSH_WRAPPER = """#!/bin/sh
# e2e: ssh only ever reads the isolated home's config (OpenSSH takes ~ from the
# password database, not $HOME, so without -F it would read the real ~/.ssh).
exec /usr/bin/ssh -F "$HOME/.ssh/config" "$@"
"""


def docker(*args, stdin=None, timeout=300):
    """docker with the driver's own environment (never the isolated one)."""
    p = subprocess.run(["docker", *args], input=stdin, capture_output=True, text=True, timeout=timeout)
    return p.returncode, p.stdout.strip(), p.stderr.strip()


def ensure_remote_image(run):
    """superba-remote's Docker test image (built from docker_it.rs when missing)."""
    if docker("image", "inspect", REMOTE_IMAGE)[0] == 0:
        return "present"
    src = DOCKER_IT.read_text()
    dockerfile = re.search(r'const DOCKERFILE: &str = r#"(.*?)"#;', src, re.S).group(1)
    fake = re.search(r'const FAKE_CODEX: &str = r#"(.*?)"#;', src, re.S).group(1)
    ctx = run.work / "remote-image"
    ctx.mkdir(exist_ok=True)
    (ctx / "Dockerfile").write_text(dockerfile)
    (ctx / "codex").write_text(fake)
    rc, out, err = docker("build", "-q", "-t", REMOTE_IMAGE, str(ctx), timeout=900)
    if rc != 0:
        raise RuntimeError(f"docker build: {err[-300:]}")
    return "built"


def start_remote(run):
    """A throwaway sshd container (users alice/bob with a fake codex, carol
    without), a key, the isolated home's ssh config with an app host and an
    ssh-config-only host, and the ssh wrapper on the run's PATH."""
    how = ensure_remote_image(run)
    name = f"superba-e2e-remote-{run.d['run_id']}"
    docker("rm", "-f", name)
    rc, cid, err = docker("run", "-d", "--name", name, "-p", "127.0.0.1::22", REMOTE_IMAGE)
    if rc != 0:
        raise RuntimeError(f"docker run: {err[-300:]}")
    run.d["remote_container"] = cid
    run.save()
    port = int(docker("port", cid, "22/tcp")[1].splitlines()[0].rsplit(":", 1)[1])
    sshdir = run.work / "ssh"
    sshdir.mkdir(mode=0o700, exist_ok=True)
    key = sshdir / "id"
    if not key.exists():
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "superba-e2e", "-f", str(key)], check=True)
    # Default ACLs of the work tree can leave the key group-readable; ssh then
    # ignores it.
    subprocess.run(["setfacl", "-b", str(key)], capture_output=True)
    key.chmod(0o600)
    pub = (sshdir / "id.pub").read_text()
    for user in ("carol", "alice"):
        docker("exec", "-i", cid, "sh", "-c",
               f"install -d -m 700 -o {user} -g {user} /home/{user}/.ssh && cat > /home/{user}/.ssh/authorized_keys"
               f" && chown {user}:{user} /home/{user}/.ssh/authorized_keys && chmod 600 /home/{user}/.ssh/authorized_keys",
               stdin=pub)
    home_ssh = Path(run.home) / ".ssh"
    home_ssh.mkdir(mode=0o700, exist_ok=True)
    common = (f"  HostName 127.0.0.1\n  Port {port}\n  IdentityFile {key}\n  IdentitiesOnly yes\n"
              f"  UserKnownHostsFile {sshdir / 'known_hosts'}\n  GlobalKnownHostsFile /dev/null\n"
              f"  StrictHostKeyChecking accept-new\n  LogLevel ERROR\n")
    cfg = home_ssh / "config"
    cfg.write_text(f"Host e2e-remote\n  User carol\n{common}\nHost e2e-sshonly\n  User alice\n{common}")
    cfg.chmod(0o600)
    wrapper = run.bin_dir / "ssh"
    wrapper.write_text(SSH_WRAPPER)
    wrapper.chmod(0o755)
    return cid, port, how


def register_app_host(run, c):
    """Add e2e-remote to the app's own record of its SSH connections
    (`.codex-global-state.json`; only hosts listed there, reported by the
    renderer or seen as live app processes count as the app's hosts). The app
    keeps that file in memory and rewrites it, so it is written while the app is
    closed and the app is opened again through Superba."""
    main, _ = app_servers(run)
    if main:
        infra.kill_pids([main], grace=20)
        infra.wait_for("the app closed", lambda: not app_servers(run)[0], 30)
    gs = run.codex_home / ".codex-global-state.json"
    state = json.loads(gs.read_text()) if gs.exists() else {}
    # The shape the app itself writes when an SSH alias is added (its loader
    # treats an entry without both `alias` and `hostname` as the legacy form).
    state["codex-managed-remote-connections"] = [{
        "hostId": "remote-ssh-discovered:e2e-remote", "displayName": "e2e-remote", "source": "discovered",
        "alias": "e2e-remote", "hostname": None, "sshPort": None, "identity": None}]
    gs.write_text(json.dumps(state))
    res = spb_stream(run, "launch", label="s22-launch")
    main1, servers = infra.wait_for("app-server after the launch", lambda: (lambda m, s: (m, s) if m and s else None)(*app_servers(run)), 120)
    onboarding(run, c)
    kept = any(e.get("alias") == "e2e-remote" for e in json.loads(gs.read_text()).get("codex-managed-remote-connections") or [])
    c.ok(res["rc"] == 0 and kept, f"the app (closed, host added to its connections, opened again with spb launch: main {main} -> {main1}) "
         f"keeps e2e-remote in .codex-global-state.json: {kept}")


def stop_remote(run):
    cid = run.d.pop("remote_container", None)
    if cid:
        docker("rm", "-f", cid)
    (run.bin_dir / "ssh").unlink(missing_ok=True)
    run.save()


# Key names of the host's auth.json (never values) and the rc files that export
# the PAT auth origin.
AUTH_KEYS_SH = r"""test -f ~/.codex/auth.json && grep -o '"[A-Za-z_]*":' ~/.codex/auth.json | tr -d '":' | sort | tr '\n' ' '"""
RC_ENV_SH = r"""grep -l CODEX_AUTHAPI_BASE_URL ~/.bashrc ~/.profile ~/.zshenv 2>/dev/null | tr '\n' ' '"""


def remote_sh(cid, cmd, user="carol"):
    rc, out, err = docker("exec", "-u", user, cid, "sh", "-lc", cmd)
    return rc, out, err


def remote_file_sha(cid, path):
    rc, out, _ = remote_sh(cid, f"sha256sum {path} 2>/dev/null | cut -c1-16")
    return out or None


@scenario("s22")
def s22(run):
    c = Check()
    try:
        _s22(run, c)
    finally:
        stop_remote(run)
    finish(run, "s22", c)


def _s22(run, c):
    cid, port, how = start_remote(run)
    register_app_host(run, c)
    c.info(f"Docker sshd from {REMOTE_IMAGE} ({how}) on 127.0.0.1:{port}; ~/.ssh/config of the isolated home has the app host "
           "e2e-remote (user carol, no codex) and the ssh-config-only host e2e-sshonly; .codex-global-state.json lists e2e-remote")
    hosts = json_of(run.spb("remote", "list", json_out=True, label="s22 remote list"))
    items = hosts if isinstance(hosts, list) else hosts.get("hosts", [])
    labels = [(h.get("host") or h).get("label") for h in items]
    ids = [((h.get("host") or h).get("target_id") or (h.get("host") or h).get("id")) for h in items]
    c.ok(len(items) == 1 and "e2e-remote" in json.dumps(items) and "e2e-sshonly" not in json.dumps(items),
         f"spb remote list: only the app's host {labels} {ids} (the ssh-config-only host is not listed)")
    target = ids[0] if ids else "ssh:e2e-remote"
    pr = run.spb("remote", "probe", target, json_out=True, label="s22 remote probe")
    view = json_of(pr)
    rs = view.get("state") or {}
    c.ok(pr["rc"] == 0 and str(rs.get("os")).lower() == "linux" and rs.get("arch") == "x86_64" and not rs.get("codex_version"),
         f"spb remote probe: os {rs.get('os')}, arch {rs.get('arch')}, kernel {rs.get('kernel')}, free {rs.get('disk_free_kib')} KiB, "
         f"codex {rs.get('codex_version')} ({rs.get('codex_install')})")
    save(run, "s22-probe.json", json.dumps(view, indent=1))
    # The user's own codex command (an older one) in the app's install dir: the
    # first install keeps it, and a rollback brings it back.
    remote_sh(cid, "mkdir -p ~/.local/bin && printf '#!/bin/sh\\necho codex-cli 0.161.0\\n' > ~/.local/bin/codex && chmod 755 ~/.local/bin/codex")
    rc, own, _ = remote_sh(cid, "~/.local/bin/codex --version")
    c.info(f"the user's own command on the host: ~/.local/bin/codex --version = {own!r}")
    t0 = time.time()
    r = run.spb("remote", "codex", target, "install", PKG_VERSION, label="s22 remote install", timeout=600)
    rc, out, err = remote_sh(cid, "~/.local/bin/codex --version")
    c.ok(r["rc"] == 0 and out == f"codex-cli {PKG_VERSION}",
         f"spb remote codex {target} install {PKG_VERSION} (package streamed over the SSH session's stdin): "
         f"~/.local/bin/codex --version in the container = {out!r} [{time.time() - t0:.0f} s]")
    rs = json_of(run.spb("remote", "probe", target, json_out=True, label="s22 remote probe after install")).get("state") or {}
    c.info(f"probe after the install: codex {rs.get('codex_version')} ({rs.get('codex_install')}) at {rs.get('codex_resolved_path') or rs.get('codex_path')}")

    # Settings sync filtered by the host's Codex version.
    dry = run.spb("remote", "sync", target, "--dry-run", json_out=True, label="s22 sync dry run")
    plan = json_of(dry)
    rows = plan.get("rows") or []
    acts = {r.get("path"): (r.get("action") or {}) for r in rows}
    kind = lambda a: a if isinstance(a, str) else (a.get("action") or a.get("kind"))  # noqa: E731
    newer = acts.get("include_environment_context_time")
    writes = [p for p, a in acts.items() if kind(a) == "write"]
    c.ok(dry["rc"] == 0 and newer and kind(newer) == "skip" and "0.162.0" in json.dumps(newer) and writes,
         f"spb remote sync --dry-run: include_environment_context_time (Codex 0.163+) skipped for the host's 0.162.0 "
         f"({json.dumps(newer.get('reason') if isinstance(newer, dict) else newer)}); writes {writes}")
    save(run, "s22-sync-plan.json", json.dumps(plan, indent=1))
    dry_text = run.spb("remote", "sync", target, "--dry-run", label="s22 sync dry run text")
    skip_line = next((l.strip() for l in dry_text["out"].splitlines() if l.startswith("include_environment_context_time")), "")
    c.info(f"dry run (text): {skip_line!r}")
    before = remote_file_sha(cid, "~/.codex/config.toml")
    res = spb_stream(run, "remote", "sync", target, "--yes", "--countdown", "0", label="s22-sync")
    rc, cfg_text, _ = remote_sh(cid, "cat ~/.codex/config.toml")
    rcfg = tomllib.loads(cfg_text) if rc == 0 and cfg_text else {}
    flat = flatten(rcfg)
    written = [p for p in writes if p in flat]
    c.ok(res["rc"] == 0 and written and "include_environment_context_time" not in flat,
         f"spb remote sync --yes: the host's config.toml has {written}, not the 0.163-only key [{res['log']}]")
    save(run, "s22-remote-config-after-sync.toml", cfg_text)

    # A local switch: follow off = the host untouched; follow on = connection only.
    sha0 = remote_file_sha(cid, "~/.codex/config.toml")
    res = spb_stream(run, "switch", "--line", "b", "--countdown", "0", "--yes", label="s22-switch-follow-off")
    c.ok(res["rc"] == 0 and remote_file_sha(cid, "~/.codex/config.toml") == sha0,
         f"local switch to line B with follow off: the host's config.toml is byte-identical ({sha0})")
    r = run.spb("remote", "follow", target, "on", label="s22 follow on")
    c.ok(r["rc"] == 0, f"spb remote follow {target} on: {r['out'].strip()[:160]}")
    res = spb_stream(run, "switch", "--line", "a", "--countdown", "0", "--yes", label="s22-switch-follow-on")
    rc, cfg2, _ = remote_sh(cid, "cat ~/.codex/config.toml")
    f2 = flatten(tomllib.loads(cfg2)) if cfg2 else {}
    changed = sorted(k for k in set(flat) | set(f2) if flat.get(k) != f2.get(k))
    settings_kept = all(f2.get(p) == flat.get(p) for p in written)
    c.ok(res["rc"] == 0 and changed and settings_kept and set(changed) <= {"openai_base_url", "chatgpt_base_url", "model_provider", "model", "model_reasoning_effort"},
         f"local switch back to line A with follow on: the host changed only connection keys {changed}; synced settings kept")
    rc, keys, _ = remote_sh(cid, AUTH_KEYS_SH)
    rc2, rcs, _ = remote_sh(cid, RC_ENV_SH)
    c.info(f"the host's auth.json (key names only): {keys or 'absent'}; rc files with CODEX_AUTHAPI_BASE_URL: {rcs or 'none'}")
    save(run, "s22-remote-config-after-follow.toml", cfg2)
    run.spb("remote", "follow", target, "off", label="s22 follow off")

    # Sessions per host: crafted rollouts on the host.
    script = ("set -e; d=~/.codex/sessions/2026/10/17; mkdir -p $d ~/.codex/archived_sessions; "
              "for i in 1 2; do printf '%s\\n' '{\"type\":\"session_meta\",\"payload\":{\"id\":\"0000000'$i'-e2e\",\"model_provider\":\"openai\"}}' > $d/rollout-2026-10-17T00-00-0$i-0000000$i-e2e.jsonl; done; "
              "printf '%s\\n' '{\"type\":\"session_meta\",\"payload\":{\"id\":\"00000003-e2e\",\"model_provider\":\"krill\"}}' > ~/.codex/archived_sessions/rollout-2026-10-16T00-00-03-00000003-e2e.jsonl")
    remote_sh(cid, script)
    se = run.spb("sessions", "--host", target, json_out=True, label="s22 sessions host")
    sj = json.dumps(json_of(se))
    by = ((json_of(se).get("report") or {}).get("by_provider")) or {}
    counts = {p: (v.get("rollouts", 0) + v.get("archived", 0)) for p, v in by.items()}
    c.ok(se["rc"] == 0 and counts.get("openai") == 2 and counts.get("krill") == 1,
         f"spb sessions --host {target}: sessions per provider on the host {counts} (2 live openai, 1 archived krill)")
    save(run, "s22-sessions.json", sj)

    # GUI: Sessions with the host switcher, Remote hosts.
    log_f = open(run.out / "gui.log", "a")
    shots = []
    for page in ("hosts", "sessions"):
        p = gui(run, page, "light", "zh-CN", log_f)
        try:
            time.sleep(9)
            shots.append(xshot(run, f"s22-{page}-light-zh"))
            if page == "sessions":
                # The host switcher (本机 | e2e-remote) under the page header.
                xdo(run, "mousemove", "355", "136", "click", "1")
                time.sleep(8)
                shots.append(xshot(run, "s22-sessions-remote-light-zh"))
        finally:
            stop(p)
    c.info(f"Remote hosts and Sessions pages (look at them): {shots}")

    r = run.spb("remote", "codex", target, "rollback", label="s22 remote rollback", timeout=300)
    rc, out, _ = remote_sh(cid, "~/.local/bin/codex --version 2>&1 || echo absent")
    c.ok(r["rc"] == 0 and out == own,
         f"spb remote codex {target} rollback: exit {r['rc']}; the host's own command is back: {out!r} "
         f"({(r['out'] + r['err']).strip().splitlines()[-1][:160] if (r['out'] + r['err']).strip() else ''})")



# ---------------------------------------------------------------------------
# s23 desktop app update check
# ---------------------------------------------------------------------------


@scenario("s23")
def s23(run):
    c = Check()
    fx = run.d["feeds"]
    # The newer signed feed (the fixture build and its bundled Codex) comes from the
    # local mirrors; do not depend on s17 having switched them on in this run.
    if not run.d.get("feed_mirrors_on"):
        run.d["feed_mirrors_on"] = True
        run.save()
    infra.set_feed_modes(run, m1="404", m2="html", m3="stale", m4="valid")
    run.spb("feeds", "refresh", json_out=True, label="s23 feeds refresh")
    t0 = time.time()
    r = run.spb("app", "check", "--refresh", json_out=True, label="s23 app check")
    j = json_of(r)
    inst, latest, action = j.get("install") or {}, j.get("latest") or {}, j.get("action") or {}
    c.ok(r["rc"] == 0 and bool(inst.get("app_version")) and bool(inst.get("bundled_codex_version")),
         f"spb app check: installed {inst.get('app_version')} with its bundled Codex {inst.get('bundled_codex_version')}")
    c.ok(latest.get("version") == infra.FIXTURE_APP_VERSION and latest.get("bundled_codex") == infra.FIXTURE_APP_CODEX and j.get("update_available"),
         f"latest {latest.get('version')} (APT index of the local download fixture) with the Codex it bundles "
         f"{latest.get('bundled_codex')} (from the signed feed, serial {fx['valid_serial']})")
    cmd = action.get("command") or ""
    c.ok(action.get("kind") == "linux_package" and cmd == f"sudo apt install ./chatgpt_{infra.FIXTURE_APP_VERSION}_amd64.deb",
         f"Linux action: download + verify + the command to run yourself: {cmd!r}")
    hits = [e for e in feed_log(run, t0) if e["listener"] == "app"]
    c.info(f"requests to the app download fixture: {sorted({(e['path'], e['status']) for e in hits})}")
    r = run.spb("app", "download", json_out=True, label="s23 app download", timeout=300)
    d = json_of(r)
    path = Path(d.get("path") or "")
    c.ok(r["rc"] == 0 and path.is_file() and d.get("sha256") == fx["app_sha256"] == hashlib.sha256(path.read_bytes()).hexdigest(),
         f"spb app download: {path.name} saved under the data dir and verified (sha256 {str(d.get('sha256'))[:16]}… = the index's)")
    c.ok(d.get("command") == f"sudo apt install {path}", f"command after the download: {d.get('command')!r}")
    priv = run.work / "privileged-calls.log"
    c.ok(not priv.exists(), f"no privileged command was run (sudo / pkexec / doas stand-ins on PATH never called: "
                            f"{priv.read_text().strip()[:120] if priv.exists() else 'no calls'})")
    log_f = open(run.out / "gui.log", "a")
    shots = []
    for theme, lang in (("dark", "zh-CN"), ("light", "en")):
        p = gui(run, "updates", theme, lang, log_f)
        try:
            time.sleep(8)
            shots.append(xshot(run, f"s23-updates-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"))
        finally:
            stop(p)
    c.info(f"Updates page (look at them): {shots}")
    finish(run, "s23", c, screenshots=shots)


# ---------------------------------------------------------------------------
# s16 setup wizard (fresh home, its own nested namespace)
# ---------------------------------------------------------------------------

NSINIT = infra.LIB / "nsinit.py"


class NestedNs:
    """A user + PID namespace inside the run's (nsinit --hold as its init):
    the wizard's Superba and desktop app see only each other, never the run's
    app (Superba would otherwise find and hot-switch that app)."""

    def __init__(self, run, name):
        import sys

        self.run = run
        ready = run.work / f"ns-{name}.ready"
        ready.unlink(missing_ok=True)
        self.p = subprocess.Popen(["unshare", "--user", "--map-current-user", "--pid", "--fork", "--mount-proc",
                                   sys.executable, "-B", str(NSINIT), "--hold", str(ready)],
                                  stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  start_new_session=True)
        self.init = None
        for _ in range(100):
            kids = Path(f"/proc/{self.p.pid}/task/{self.p.pid}/children")
            child = kids.read_text().split() if kids.exists() else []
            if ready.exists() and child:
                self.init = int(child[0])
                break
            time.sleep(0.1)
        if not self.init:
            raise RuntimeError("nested namespace did not start")
        run.d["pids"][f"ns-{name}"] = self.p.pid
        run.d["pids"][f"ns-{name}-init"] = self.init
        run.save()
        self.name = name

    def argv(self, argv):
        return ["nsenter", "-t", str(self.init), "-U", "-p", "-m", "--preserve-credentials", *map(str, argv)]

    def popen(self, argv, env, log):
        return subprocess.Popen(self.argv(argv), env=env, cwd=env["HOME"], stdout=log, stderr=log, start_new_session=True)

    def call(self, argv, env, timeout=120):
        p = subprocess.run(self.argv(argv), env=env, cwd=env["HOME"], capture_output=True, text=True, timeout=timeout)
        return p.returncode, p.stdout, p.stderr

    def close(self):
        infra.kill_pids([self.init, self.p.pid], grace=10)
        self.run.d["pids"].pop(f"ns-{self.name}", None)
        self.run.d["pids"].pop(f"ns-{self.name}-init", None)
        self.run.save()


def wizard_env(run, home, xdg):
    """The fresh home's environment: the built-in Krill preset gets the run's
    lines A and B as test lines (SUPERBA_TEST_LINES); its production lines are
    made unreachable through a dead proxy (loopback is never proxied), so the
    e2e key is never sent to a production line."""
    env = run.base_env(HOME=home, XDG_RUNTIME_DIR=xdg,
                       SUPERBA_TEST_LINES=f"e2e-a={run.line_url('a')},e2e-b={run.line_url('b')}",
                       HTTPS_PROXY="http://127.0.0.1:9", HTTP_PROXY="http://127.0.0.1:9",
                       https_proxy="http://127.0.0.1:9", http_proxy="http://127.0.0.1:9",
                       NO_PROXY="127.0.0.1,localhost", no_proxy="127.0.0.1,localhost")
    env.pop("SUPERBA_TEST_SITES", None)
    return env


def primary_button(png):
    """Centre of the dark (enabled primary) pill button in the wizard's
    right column, or None (a disabled button is light grey)."""
    from PIL import Image

    with Image.open(png) as img:
        im = img.convert("RGB")
    px = im.load()
    w, h = im.size
    rows = {}
    for y in range(140, min(h, 795)):
        xs = [x for x in range(700, min(w, 1000)) if max(px[x, y]) < 90 and max(px[x, y]) - min(px[x, y]) < 8]
        if len(xs) >= 24 and xs[-1] - xs[0] >= 45:
            rows[y] = xs
    blocks, cur = [], []
    for y in sorted(rows):
        if cur and y - cur[-1] > 1:
            blocks.append(cur)
            cur = []
        cur.append(y)
    if cur:
        blocks.append(cur)
    blocks = [b for b in blocks if len(b) >= 18]
    if not blocks:
        return None
    b = blocks[-1]
    xs = [x for y in b for x in rows[y]]
    return ((min(xs) + max(xs)) // 2, (b[0] + b[-1]) // 2)


def is_wizard(png):
    """The wizard has no sidebar: the left column is white, the shell's grey."""
    from PIL import Image

    with Image.open(png) as img:
        return img.convert("RGB").getpixel((100, 400)) == (255, 255, 255)


def wizard_files(home):
    data = Path(home) / ".local" / "share" / "superba"
    return {
        "profiles.json": (data / "profiles.json").exists(),
        "secrets": any((data / "secrets").glob("*")) if (data / "secrets").exists() else False,
        "config.toml": (Path(home) / ".codex" / "config.toml").exists(),
        "auth.json": (Path(home) / ".codex" / "auth.json").exists(),
    }


def krill_profiles(home):
    f = Path(home) / ".local" / "share" / "superba" / "profiles.json"
    if not f.exists():
        return []
    return [p for p in json.loads(f.read_text()).get("profiles", []) if p.get("site_id") == "krill"]


class WizardGui:
    """The Superba GUI of the wizard's home, inside the nested namespace."""

    def __init__(self, run, ns, env, c):
        self.run, self.ns, self.env, self.c = run, ns, env, c
        self.p = None
        self.log = open(run.out / "gui-wizard.log", "a")
        self.shots = []

    def start(self, lang="en", page=None, wait=8):
        argv = [self.run.d["superba"], "--size", "1280x800", "--theme", "light", "--lang", lang]
        if page:
            argv += ["--page", page]
        self.p = self.ns.popen(argv, self.env, self.log)
        time.sleep(wait)
        xdo(self.run, "search", "--sync", "--onlyvisible", "--name", "^Superba$", "windowfocus", "--sync")

    def shot(self, name):
        rel = xshot(self.run, f"s16-{name}")
        self.shots.append(rel)
        return self.run.out / rel

    def click(self, x, y, wait=1.5):
        xdo(self.run, "mousemove", str(x), str(y), "click", "1")
        time.sleep(wait)

    def primary(self, name, wait=2.0):
        """Click the step's enabled primary button (Continue / Apply)."""
        png = self.shot(name)
        pos = primary_button(png)
        if not pos:
            raise RuntimeError(f"no enabled primary button on {png.name}")
        self.click(*pos, wait=wait)
        return pos

    def gui_window(self):
        """The id of the one visible Superba window, or None."""
        r = xdo(self.run, "search", "--onlyvisible", "--name", "^Superba$")
        ids = r.stdout.split()
        return ids[0] if len(ids) == 1 else None

    def type_key(self, x=630, y=263):
        """Type the key into the wizard's key field from a 0600 file (never on
        argv). Keystrokes go to whatever X window has the keyboard focus, so the
        key is typed only after the focus and the pointer are both verified to be
        on the one Superba window; otherwise nothing is typed."""
        win = self.gui_window()
        if not win:
            raise RuntimeError("refusing to type the key: not exactly one visible Superba window")
        xdo(self.run, "windowfocus", "--sync", win)
        self.click(x, y, wait=0.5)
        focus = xdo(self.run, "getwindowfocus").stdout.strip()
        under = dict(l.split("=", 1) for l in xdo(self.run, "getmouselocation", "--shell").stdout.split() if "=" in l).get("WINDOW")
        if focus != win or under != win:
            raise RuntimeError(f"refusing to type the key: focus {focus}, pointer over {under}, Superba window {win}")
        key_file = self.run.work / "wizard-key"
        fd = os.open(key_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        try:
            os.write(fd, self.run.key.encode())
        finally:
            os.close(fd)
        try:
            xdo(self.run, "type", "--delay", "15", "--file", str(key_file))
        finally:
            key_file.unlink(missing_ok=True)
        time.sleep(0.8)

    def stop(self):
        """Stop the GUI (nsenter forks it: the GUI is nsenter's child)."""
        if self.p:
            infra.kill_pids(infra.descendants(self.p.pid) + [self.p.pid])
            try:
                self.p.wait(5)
            except subprocess.TimeoutExpired:
                pass
            self.p = None


def wizard_app(run, home):
    """(main pid, app-server pids) of the desktop app of the wizard's home."""
    mains = [p for p in infra.pids_with_home(home)
             if infra.cmdline(p).startswith(run.d["app_dir"] + "/ChatGPT") and "--type=" not in infra.cmdline(p)]
    if not mains:
        return None, []
    kids = [p for p in infra.pids_with_home(home) if parent(p) == mains[0] and " app-server" in infra.cmdline(p)]
    return mains[0], kids


def go_through(g, name, key=True, line_wait=12):
    """Detect -> Provider -> Credentials -> Line & models -> Confirm -> Apply."""
    g.primary(f"{name}-1-detect")
    g.primary(f"{name}-2-provider")
    if key:
        g.type_key()
    g.primary(f"{name}-3-credentials")
    time.sleep(line_wait)
    g.primary(f"{name}-4-lines")
    g.primary(f"{name}-5-confirm", wait=4)


@scenario("s16")
def s16(run):
    c = Check()
    wh = run.work / "home-wizard"
    shutil.rmtree(wh, ignore_errors=True)
    wh.mkdir()
    link = f"{run.home}w"
    if os.path.lexists(link):
        os.remove(link)
    os.symlink(wh, link)
    xdg = run.work / "xdg-wizard"
    xdg.mkdir(mode=0o700, exist_ok=True)
    env = wizard_env(run, link, str(xdg))
    ns = NestedNs(run, "wizard")
    g = WizardGui(run, ns, env, c)
    try:
        _s16(run, c, ns, g, link, env)
    finally:
        g.stop()
        ns.close()
        if os.path.islink(link):
            os.remove(link)
        shutil.rmtree(wh / ".cache", ignore_errors=True)
    finish(run, "s16", c, screenshots=g.shots)


def _s16(run, c, ns, g, home, env):
    c.info(f"fresh home {home} in its own user + PID namespace; the Krill preset's production lines are unreachable "
           "(dead proxy), the run's lines A and B are its test lines e2e-a / e2e-b")
    # First start: the wizard; Skip writes nothing.
    g.start()
    png = g.shot("first-start")
    c.ok(is_wizard(png), f"first start opens the setup wizard (no sidebar) [{png.name}]")
    g.click(935, 50, wait=3)
    png = g.shot("after-skip")
    files = wizard_files(home)
    c.ok(not is_wizard(png) and not any(files.values()), f"\"Skip setup\" closes it and writes nothing: {files}")
    g.stop()

    # Second start: the wizard again; Krill is pre-selected; key; lines; apply.
    g.start()
    png = g.shot("second-start")
    c.ok(is_wizard(png), "nothing was saved, so the next start opens the wizard again")
    go_through(g, "first-run")
    time.sleep(6)
    png = g.shot("after-apply")
    profiles = krill_profiles(home)
    line = profiles[0].get("line_id") if profiles else None
    c.ok(len(profiles) == 1 and line in ("e2e-a", "e2e-b"),
         f"Apply: one Krill profile {[(p.get('id'), p.get('line_id'), p.get('auth')) for p in profiles]} on the pre-selected (fastest) test line")
    want = run.line_url(line[-1]) if line else None
    cfg = Path(home) / ".codex" / "config.toml"
    flat = flatten(tomllib.loads(cfg.read_text())) if cfg.exists() else {}
    c.ok(want and flat.get("openai_base_url") == want + "/backend-api/codex" and wizard_files(home)["auth.json"],
         f"the switch wrote config.toml (openai_base_url = {flat.get('openai_base_url')}) and auth.json")
    # Open ChatGPT through Superba (CDP) in the wizard's home: `spb launch` in
    # the same namespace (the GUI's own button moves with the page design).
    rc, out, err = ns.call([run.d["spb"], "launch"], env, timeout=180)
    main, servers = infra.wait_for("the wizard's app", lambda: (lambda m, s: (m, s) if m and s else None)(*wizard_app(run, home)), 90)
    menv = infra.environ(main)
    shim = type("R", (), {"home": home})()
    port = scenarios_cdp_port(shim)
    c.ok(rc == 0 and menv.get("CODEX_AUTHAPI_BASE_URL") == want and "--remote-debugging-port" in infra.cmdline(main) and port,
         f"spb launch (wizard's home): app main {main} with CDP port {port}, CODEX_AUTHAPI_BASE_URL = the chosen line")
    onboarding(shim, c)
    since = time.time()
    acall(_with_cdp(shim, lambda cc: cc.send_turn("E2E-WZ1: reply with exactly the text PONG-WZ1")))
    e = infra.wait_for("E2E-WZ1 on the chosen line", lambda: posts(run, line[-1], "E2E-WZ1", since), 180)[-1]
    c.ok(e.get("auth_kind") == "pat" and e["status"] == 200, f"a turn in the wizard's app: {fmt(e)}")
    final_js = ("[...document.querySelectorAll('[data-local-conversation-final-assistant=\"true\"]')]"
                ".map((m) => m.innerText).join(' ')")
    try:
        infra.wait_for("PONG-WZ1 rendered", lambda: "PONG-WZ1" in (acall(_with_cdp(shim, lambda cc: cc.eval(final_js))) or ""), 120, 2)
        c.info("the reply PONG-WZ1 rendered in the wizard's app")
    except TimeoutError:
        c.info("the reply did not render within 120 s (the request above reached the line with the PAT and answered 200)")
    g.shot("app-after-turn")
    # The re-runs below change a closed app's configuration (no running turn can
    # hold them; GUI hot switches are covered by s10).
    infra.kill_pids([main] + infra.descendants(main), grace=15)
    g.stop()
    # Re-run from Preferences.
    g.start(page="preferences")
    for _ in range(4):
        xdo(run, "mousemove", "700", "500", "click", "--repeat", "5", "5")
        time.sleep(0.4)
    g.shot("preferences-scrolled")
    g.click(1168, 254, wait=3)
    png = g.shot("rerun-prefs-start")
    c.ok(is_wizard(png), "Preferences → \"Run setup again\" opens the wizard")
    other = "e2e-b" if line == "e2e-a" else "e2e-a"
    g.primary("rerun-prefs-1-detect")
    g.primary("rerun-prefs-2-provider")
    g.primary("rerun-prefs-3-credentials")
    time.sleep(10)
    # The line tiles: ... Test e2e-a, then Test e2e-b at the bottom of the list.
    g.click(640, 640 if other == "e2e-a" else 712, wait=1.5)
    g.primary("rerun-prefs-4-lines")
    g.primary("rerun-prefs-5-confirm", wait=3)

    def rerun_written():
        ps = krill_profiles(home)
        f = flatten(tomllib.loads(cfg.read_text()))
        return (ps, f) if ps and ps[0].get("line_id") == other and f.get("openai_base_url") == run.line_url(other[-1]) + "/backend-api/codex" else None

    try:
        profiles, flat = infra.wait_for("the re-run's switch", rerun_written, 60)
    except TimeoutError:
        profiles, flat = krill_profiles(home), flatten(tomllib.loads(cfg.read_text()))
    g.shot("rerun-prefs-after")
    c.ok(len(profiles) == 1 and profiles[0].get("line_id") == other
         and flat.get("openai_base_url") == run.line_url(other[-1]) + "/backend-api/codex",
         f"re-run from Preferences edits the existing profile (still one Krill profile, now on {profiles[0].get('line_id') if profiles else None}) "
         f"and switches: openai_base_url = {flat.get('openai_base_url')}")
    g.stop()
    # Re-run from the command palette (Chinese UI): "运行设置向导".
    g.start(lang="zh-CN")
    xdo(run, "mousemove", "640", "700", "click", "1")
    xdo(run, "key", "ctrl+k")
    time.sleep(0.8)
    xdo(run, "type", "--delay", "80", "设置向导")
    time.sleep(1.0)
    g.shot("palette-wizard")
    xdo(run, "key", "Return")
    time.sleep(3)
    png = g.shot("rerun-palette-start")
    c.ok(is_wizard(png), "palette \"运行设置向导\" opens the wizard (zh-CN)")
    go_through(g, "rerun-palette", key=False, line_wait=8)
    time.sleep(12)
    g.shot("rerun-palette-after")
    profiles = krill_profiles(home)
    names = [p.get("id") for p in json.loads((Path(home) / ".local/share/superba/profiles.json").read_text()).get("profiles", [])]
    c.ok(len(profiles) == 1, f"re-run from the palette edits again: profiles.json holds {names} (one Krill profile, no duplicate)")
