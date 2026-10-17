#!/usr/bin/env python3
"""Fixture databases for Superba screenshots (Python 3.11, stdlib only).

Writes, into an isolated home only:
- `<data dir>/router/usage.sqlite3`: a router request log (schema version 2 of
  `superba_router::store`, numbers only, like the real recorder writes) with a
  few sessions over the last two days and a working-hours rhythm over the six
  days before (the Overview's 7 x 24 heat-strip and its delta against
  yesterday); two lines carry requests in the last 30 minutes (the Overview's
  line traces) and one session uses a model without a list price; the two
  newest rows show the Timing tab with a first event that arrived together with
  the response headers and with a normal one;
- `<CODEX_HOME>/state_5.sqlite`: a Codex `threads` table whose ids are the
  sessions' `prompt_cache_key`s, so the log shows session names. With `--codex`
  the database is Codex's own (that Codex creates and migrates it first, see
  `codex_state_db`), so a desktop app started on this home later can open it; a
  hand-made `threads` table makes Codex fail its first migration ("failed to
  initialize sqlite state runtime"), and desktop app 26.1007 then never opens its
  window. Without `--codex` (no app will run on this home) the table is a minimal
  one.

Everything is synthetic: no prompt text beyond short neutral titles, no keys,
no hosts other than loopback. Deterministic for a given `now`.

The log has two providers (router members): Krill, whose requests alternate
between two of its lines (one request failed before any line answered, so only
the last line tried is known), and a custom site with its single line.

    python3 -I fixtures.py --data-dir HOME/.local/share/superba \\
        --codex-home HOME/.codex --upstream 127.0.0.1:PORT \\
        [--line-host LINE=HOST:PORT ...] [--codex APP/resources/codex] [--now EPOCH_MS]

Other scripts load this file (importlib) for `insert_threads`.
"""

import argparse
import json
import os
import queue
import random
import sqlite3
import subprocess
import threading
import time
from pathlib import Path

USAGE_SCHEMA = """
CREATE TABLE IF NOT EXISTS requests (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  started_ms INTEGER NOT NULL,
  ttfb_ms INTEGER, first_event_ms INTEGER, first_token_ms INTEGER, duration_ms INTEGER,
  member_id TEXT NOT NULL, member_name TEXT NOT NULL, line_id TEXT, line_name TEXT,
  upstream_host TEXT, failovers INTEGER NOT NULL DEFAULT 0,
  transport TEXT NOT NULL, originator TEXT, client TEXT NOT NULL,
  request_kind TEXT, thread_source TEXT, thread_id TEXT, turn_id TEXT, prompt_cache_key TEXT,
  model TEXT, upstream_model TEXT, response_model TEXT,
  service_tier TEXT, response_service_tier TEXT,
  status_kind TEXT NOT NULL, status_code TEXT, status_http INTEGER, http_status INTEGER, error TEXT,
  input_tokens INTEGER, cached_tokens INTEGER, cache_write_tokens INTEGER,
  output_tokens INTEGER, reasoning_tokens INTEGER,
  tps_milli INTEGER, cost_micros INTEGER, price_rev TEXT,
  injected INTEGER NOT NULL DEFAULT 0, stripped INTEGER NOT NULL DEFAULT 0,
  upstream_request_id TEXT, attempts TEXT, log TEXT,
  sent_ms INTEGER, last_delta_ms INTEGER
);
CREATE INDEX IF NOT EXISTS requests_time ON requests(started_ms);
"""

# (thread id, title, client, model, tier)
SESSIONS = [
    ("019a2f10-5b1c-7c3e-9d41-0a6b2c4d8e01", "Refactor the settings loader", "desktop",
     "gpt-6-astra", None),
    ("019a2f22-7e3a-7a19-8b52-1c7d3e5f9a02", "Fix flaky router tests", "desktop",
     "gpt-5.5", "priority"),
    ("019a2f35-1d4e-7b28-9c63-2d8e4f6a0b03", "Write the release notes", "tui", "gpt-5.5", None),
    ("019a2f48-3f5a-7c37-8d74-3e9f5a7b1c04", "Review the catalog overlay", "desktop",
     "gpt-6-astra", "priority"),
]
# Provider (router member id) of each session, in SESSIONS order.
PROVIDER_OF = ["krill", "krill", "example-api", "krill"]
# Router members with their name and lines (id, name; None = the host, like the
# single line of a custom site).
PROVIDERS = {
    "krill": ("Krill", [("cn-fast", "China Express"), ("overseas", "Overseas")]),
    "example-api": ("Example API", [("default", None)]),
}
# Upstream host per line id (main fills it from --line-host; other lines use
# --upstream).
LINE_HOSTS = {}
# USD per 1M tokens: input, cached input, output (public list prices of the mock).
PRICES = {"gpt-5.5": (1.25, 0.125, 10.0), "gpt-6-astra": (2.5, 0.25, 15.0)}
ORIGINATORS = {"desktop": "Codex Desktop", "tui": "codex_cli_rs", "exec": "codex_exec"}


def usage_rows(now_ms, upstream):
    rng = random.Random(20261017)
    rows = []
    for index, (thread, _title, client, model, tier) in enumerate(SESSIONS):
        provider = PROVIDER_OF[index]
        member_name, lines = PROVIDERS[provider]
        start = now_ms - (len(SESSIONS) - index) * 5 * 3_600_000
        turns = 6 + index * 3
        context = 18_000
        for turn in range(turns):
            for step in range(rng.randint(1, 4)):
                started = start + turn * 240_000 + step * 21_000 + rng.randint(0, 9_000)
                context += rng.randint(900, 4_000)
                cached = int(context * rng.uniform(0.82, 0.96)) if turn or step else 0
                output = rng.randint(120, 2_400)
                ttfb = rng.randint(380, 1_400)
                duration = ttfb + int(output / rng.uniform(55, 95) * 1000)
                status = ("ok", None, None)
                line_id, line_name = lines[0] if rng.random() < 0.6 else lines[-1]
                host = LINE_HOSTS.get(line_id, upstream)
                line_name = line_name or host.split(":")[0]
                answered = True
                if rng.random() < 0.03:
                    status = ("http", "rate_limit_exceeded", 429)
                    output = 0
                elif (index, turn, step) == (1, 2, 0):
                    # Every line refused the connection: no line answered and the
                    # row keeps the last line tried.
                    status = ("failed", "upstream_unavailable", None)
                    output = 0
                    answered = False
                p_in, p_cached, p_out = PRICES[model]
                cost = ((context - cached) * p_in + cached * p_cached + output * p_out)
                if tier == "priority":
                    cost *= 2
                ok = status[0] == "ok"
                rows.append({
                    "started_ms": started,
                    "sent_ms": 2,
                    "ttfb_ms": ttfb,
                    "first_event_ms": ttfb + 25,
                    "first_token_ms": ttfb + 120,
                    "last_delta_ms": duration - 15,
                    "duration_ms": duration,
                    "member_id": provider,
                    "member_name": member_name,
                    "line_id": line_id if answered else None,
                    "line_name": line_name,
                    "upstream_host": host,
                    "transport": "http",
                    "originator": ORIGINATORS[client],
                    "client": client,
                    "request_kind": "turn",
                    "thread_source": "user",
                    "thread_id": thread,
                    "turn_id": f"{thread[:8]}-turn-{turn}",
                    "prompt_cache_key": thread,
                    "model": model,
                    "upstream_model": model,
                    "response_model": model,
                    "service_tier": tier,
                    "response_service_tier": tier or "default",
                    "status_kind": status[0],
                    "status_code": status[1],
                    "status_http": status[2],
                    "http_status": (status[2] or 200) if answered else None,
                    "input_tokens": context if ok else None,
                    "cached_tokens": cached if ok else None,
                    "cache_write_tokens": 0 if ok else None,
                    "output_tokens": output if ok else None,
                    "reasoning_tokens": output // 3 if ok else None,
                    "tps_milli": tps_milli(output, ttfb + 25, duration) if ok else None,
                    "cost_micros": int(cost) if ok else None,
                    "price_rev": "mock-1",
                    "injected": 0,
                    "stripped": 0,
                    "attempts": json.dumps([]),
                    "log": json.dumps([]),
                })
    rows.extend(timing_rows(now_ms, upstream))
    rows.extend(dashboard_rows(now_ms, upstream))
    rows.extend(week_rows(now_ms, upstream))
    rows.sort(key=lambda r: r["started_ms"])
    return rows


def tps_milli(output, first_event_ms, end_ms):
    """Output tokens per second x 1000 from the first event to the end, the way the
    router computes it (`superba_core::tps_milli`): none below 200 ms or 10 tokens."""
    window = end_ms - first_event_ms
    if window < 200 or output < 10:
        return None
    return output * 1_000_000 // window


def timing_rows(now_ms, upstream):
    """The two newest rows, for the Timing tab: one whose first event (response.created
    and the first output delta) arrived in the same read as the response headers, so
    headers, first event and first token share one timestamp, and a normal one."""
    thread, _title, client, model, tier = SESSIONS[-1]
    p_in, p_cached, p_out = PRICES[model]
    out = []
    # (started before now, sent, headers, first event, first token, last delta, end, output)
    for i, (ago, sent, headers, event, token, last, end, output) in enumerate([
            (95_000, 3, 640, 640, 640, 4_210, 4_226, 287),
            (40_000, 2, 520, 548, 1_310, 5_020, 5_041, 342)]):
        context, cached = 64_000 + i * 1_800, 61_400 + i * 1_700
        cost = ((context - cached) * p_in + cached * p_cached + output * p_out) * 2
        out.append({
            "started_ms": now_ms - ago, "sent_ms": sent, "ttfb_ms": headers,
            "first_event_ms": event, "first_token_ms": token, "last_delta_ms": last,
            "duration_ms": end, "member_id": "krill", "member_name": "Krill",
            "line_id": "overseas", "line_name": "Overseas", "upstream_host": upstream,
            "transport": "http", "originator": ORIGINATORS[client], "client": client,
            "request_kind": "turn", "thread_source": "user", "thread_id": thread,
            "turn_id": f"{thread[:8]}-turn-live-{i}", "prompt_cache_key": thread,
            "model": model, "upstream_model": model, "response_model": model,
            "service_tier": tier, "response_service_tier": tier or "default",
            "status_kind": "ok", "status_code": None, "status_http": None, "http_status": 200,
            "input_tokens": context, "cached_tokens": cached, "cache_write_tokens": 0,
            "output_tokens": output, "reasoning_tokens": output // 4,
            "tps_milli": tps_milli(output, event, end), "cost_micros": int(cost),
            "price_rev": "mock-1", "injected": 1, "stripped": 0,
            "attempts": json.dumps([{"member_id": "krill", "line_name": "Overseas",
                                     "upstream_host": upstream, "status": 200,
                                     "outcome": "answered", "elapsed_ms": headers}]),
            "log": json.dumps([f"krill/overseas: HTTP 200 after {headers} ms",
                               "stats line added"]),
        })
    return out


# Sessions for the Overview dashboard (user request 24): (thread id, title, client,
# model, tier, provider, line, first request before now in minutes, last one,
# seconds between requests). The live one keeps the per-minute sparklines and the
# request rate busy; the others spread usage over the 24-hour bars.
DASHBOARD_SESSIONS = [
    ("019a2f5b-6a2d-7e41-8f85-4fa06b8c2d05", "Polish the overview cards", "desktop",
     "gpt-6-astra", "priority", "krill", "overseas", 58, 2, 75),
    ("019a2f6e-8b3e-7f52-9096-50b17c9d3e06", "Port the sync dialog", "tui", "gpt-5.5", None,
     "example-api", "default", 23 * 60 + 40, 21 * 60 + 50, 140),
    ("019a2f81-9c4f-7063-a1a7-61c28dae4f07", "Triage nightly failures", "exec", "gpt-5.5",
     None, "krill", "cn-fast", 8 * 60 + 30, 7 * 60 + 20, 200),
    ("019a2f94-ad50-7174-b2b8-72d39ebf5008", "Explain the stats line", "desktop",
     "gpt-6-astra", None, "krill", "overseas", 14 * 60, 2 * 60, 2900),
    # The second Krill line in the last 30 minutes (two line traces).
    ("019a2fa7-be61-7285-83c9-83e4a0c06109", "Check the failover order", "tui", "gpt-5.5",
     None, "krill", "cn-fast", 29, 1, 110),
    # A model the list prices do not cover (cost unknown; a to-do on the Overview).
    ("019a2fba-cf72-7396-94da-94f5b1d1720a", "Sketch the line trace", "desktop",
     "gpt-5.3-codex-spark", None, "krill", "overseas", 5 * 60, 4 * 60 + 10, 260),
]


# Router member name and line names of the dashboard sessions (None = the host).
DASHBOARD_MEMBERS = {
    "krill": {"overseas": ("Krill", "Overseas"), "cn-fast": ("Krill", "China Express")},
    "example-api": {"default": ("Example API", None)},
}


def dashboard_rows(now_ms, upstream):
    """Steady requests for the Overview's cards (sparklines, hourly bars, shares)."""

    def tps(output, first_event_ms, end_ms):
        # As the router computes it: none below 200 ms or 10 tokens.
        window = end_ms - first_event_ms
        return None if window < 200 or output < 10 else output * 1_000_000 // window

    rng = random.Random(20261024)
    rows = []
    for thread, _title, client, model, tier, provider, line_id, first, last, every in \
            DASHBOARD_SESSIONS:
        member_name, line_name = DASHBOARD_MEMBERS[provider][line_id]
        host = globals().get("LINE_HOSTS", {}).get(line_id, upstream)
        line_name = line_name or host.split(":")[0]
        context = 22_000
        started = now_ms - first * 60_000
        turn = 0
        while started <= now_ms - last * 60_000:
            context = min(context + rng.randint(1_200, 5_000), 180_000)
            cached = int(context * rng.uniform(0.85, 0.96))
            output = rng.randint(180, 2_600)
            ttfb = rng.randint(360, 1_200)
            duration = ttfb + int(output / rng.uniform(60, 110) * 1000)
            price = PRICES.get(model)
            cost = None
            if price:
                p_in, p_cached, p_out = price
                cost = (context - cached) * p_in + cached * p_cached + output * p_out
                if tier == "priority":
                    cost *= 2
            row = {
                "started_ms": started, "sent_ms": 2, "ttfb_ms": ttfb,
                "first_event_ms": ttfb + 25, "first_token_ms": ttfb + 110,
                "last_delta_ms": duration - 15, "duration_ms": duration,
                "member_id": provider, "member_name": member_name, "line_id": line_id,
                "line_name": line_name, "upstream_host": host, "transport": "http",
                "originator": ORIGINATORS[client], "client": client, "request_kind": "turn",
                "thread_source": "user", "thread_id": thread,
                "turn_id": f"{thread[:8]}-turn-{turn}", "prompt_cache_key": thread,
                "model": model, "upstream_model": model, "response_model": model,
                "service_tier": tier, "response_service_tier": tier or "default",
                "status_kind": "ok", "status_code": None, "status_http": None,
                "http_status": 200, "input_tokens": context, "cached_tokens": cached,
                "cache_write_tokens": 0, "output_tokens": output,
                "reasoning_tokens": output // 3,
                "tps_milli": tps(output, ttfb + 25, duration),
                "cost_micros": None if cost is None else int(cost),
                "price_rev": "mock-1" if cost is not None else None,
                "injected": 0, "stripped": 0,
                "attempts": json.dumps([]), "log": json.dumps([]),
            }
            # Only the columns this schema has.
            rows.append({k: v for k, v in row.items() if k in USAGE_SCHEMA})
            turn += 1
            started += int(every * rng.uniform(0.55, 1.45) * 1000)
    return rows


# Sessions of the six days before yesterday's dashboard sessions, one or two a
# day in working hours (the heat-strip's rhythm): (title, client, model, line).
WEEK_TITLES = [
    ("Add the heat-strip legend", "desktop", "gpt-6-astra", "overseas"),
    ("Bisect the flaky sync test", "tui", "gpt-5.5", "cn-fast"),
    ("Draft the 0.3 changelog", "desktop", "gpt-5.5", "overseas"),
    ("Trim the router's log retention", "exec", "gpt-5.5", "cn-fast"),
    ("Review the price feed diff", "desktop", "gpt-6-astra", "overseas"),
    ("Port the settings editor", "desktop", "gpt-6-astra", "overseas"),
    ("Fix the wizard's focus order", "tui", "gpt-5.5", "cn-fast"),
    ("Profile the rollout reader", "desktop", "gpt-6-astra", "overseas"),
    ("Rename the usage columns", "tui", "gpt-5.5", "overseas"),
    ("Tidy the provider cards", "desktop", "gpt-6-astra", "cn-fast"),
]


def week_thread(i):
    return f"019a1{i:03x}0-4d2e-7a{i:02d}-9b3c-{0x5e6f7a8b9c00 + i:012x}"


def week_rows(now_ms, upstream):
    """Working-hours sessions on each of the six days before today (local time):
    a morning block on most days, an afternoon block on weekdays, quieter
    weekends."""
    rng = random.Random(20261031)
    rows = []
    local = time.localtime(now_ms / 1000)
    midnight = now_ms - ((local.tm_hour * 60 + local.tm_min) * 60 + local.tm_sec) * 1000
    n = 0
    for days_ago in range(6, 0, -1):
        day = midnight - days_ago * 86_400_000
        weekday = time.localtime(day / 1000 + 43_200).tm_wday
        blocks = [(9.5, 2.5), (14.0, 3.5)] if weekday < 5 else [(10.5, 1.5)]
        if weekday in (1, 3):
            blocks.append((20.5, 1.2))
        for start_h, hours in blocks:
            title, client, model, line_id = WEEK_TITLES[n % len(WEEK_TITLES)]
            thread = week_thread(n)
            n += 1
            member_name, line_name = DASHBOARD_MEMBERS["krill"][line_id]
            host = LINE_HOSTS.get(line_id, upstream)
            p_in, p_cached, p_out = PRICES[model]
            started = day + int((start_h + rng.uniform(-0.3, 0.3)) * 3_600_000)
            end = started + int(hours * 3_600_000)
            context = 20_000
            turn = 0
            while started < end:
                context = min(context + rng.randint(1_000, 4_500), 170_000)
                cached = int(context * rng.uniform(0.84, 0.95))
                output = rng.randint(150, 2_200)
                ttfb = rng.randint(380, 1_100)
                duration = ttfb + int(output / rng.uniform(60, 105) * 1000)
                cost = (context - cached) * p_in + cached * p_cached + output * p_out
                rows.append({
                    "started_ms": started, "sent_ms": 2, "ttfb_ms": ttfb,
                    "first_event_ms": ttfb + 25, "first_token_ms": ttfb + 110,
                    "last_delta_ms": duration - 15, "duration_ms": duration,
                    "member_id": "krill", "member_name": member_name, "line_id": line_id,
                    "line_name": line_name, "upstream_host": host, "transport": "http",
                    "originator": ORIGINATORS[client], "client": client,
                    "request_kind": "turn", "thread_source": "user", "thread_id": thread,
                    "turn_id": f"{thread[:8]}-turn-{turn}", "prompt_cache_key": thread,
                    "model": model, "upstream_model": model, "response_model": model,
                    "service_tier": None, "response_service_tier": "default",
                    "status_kind": "ok", "status_code": None, "status_http": None,
                    "http_status": 200, "input_tokens": context, "cached_tokens": cached,
                    "cache_write_tokens": 0, "output_tokens": output,
                    "reasoning_tokens": output // 3,
                    "tps_milli": tps_milli(output, ttfb + 25, duration),
                    "cost_micros": int(cost), "price_rev": "mock-1", "injected": 0,
                    "stripped": 0, "attempts": json.dumps([]), "log": json.dumps([]),
                })
                turn += 1
                started += int(rng.uniform(70, 420) * 1000)
    return rows


def week_threads(now_ms):
    """(thread id, title, minutes before now) of the week's sessions."""
    out = []
    for i in range(len(WEEK_TITLES) * 2):
        out.append((week_thread(i), WEEK_TITLES[i % len(WEEK_TITLES)][0], 6 * 24 * 60))
    return out


def write_usage(data_dir, now_ms, upstream):
    db = Path(data_dir) / "router" / "usage.sqlite3"
    db.parent.mkdir(parents=True, exist_ok=True)
    if db.exists():
        db.unlink()
    conn = sqlite3.connect(db)
    conn.executescript(USAGE_SCHEMA)
    conn.execute("PRAGMA user_version = 2")
    rows = usage_rows(now_ms, upstream)
    cols = list(rows[0].keys())
    conn.executemany(
        f"INSERT INTO requests ({', '.join(cols)}) VALUES ({', '.join('?' for _ in cols)})",
        [tuple(r[c] for c in cols) for r in rows])
    conn.commit()
    conn.close()
    db.chmod(0o600)
    return len(rows)


STATE_DB = "state_5.sqlite"

# A minimal `threads` table (what Superba reads), only for a home no Codex opens.
MINIMAL_THREADS = """
CREATE TABLE IF NOT EXISTS threads (
  id TEXT PRIMARY KEY,
  rollout_path TEXT,
  created_at INTEGER,
  updated_at INTEGER,
  source TEXT,
  model_provider TEXT,
  cwd TEXT,
  title TEXT,
  first_user_message TEXT,
  archived INTEGER NOT NULL DEFAULT 0
);
"""

# Values for required columns of Codex's own `threads` table that a fixture row
# does not give (Codex's migration tests write the same two).
THREAD_DEFAULTS = {"rollout_path": "", "sandbox_policy": "read-only",
                   "approval_mode": "on-request"}


def made_by_codex(db):
    """True when `db` is a state database Codex created (it records its
    migrations in `_sqlx_migrations`)."""
    if not Path(db).exists():
        return False
    conn = sqlite3.connect(db)
    try:
        return conn.execute("SELECT 1 FROM sqlite_master WHERE type = 'table'"
                            " AND name = '_sqlx_migrations'").fetchone() is not None
    finally:
        conn.close()


def codex_state_db(codex, codex_home, timeout=60):
    """Let `codex` create and migrate CODEX_HOME's state database: `codex
    app-server` opens it at start, before it answers `initialize`; it is then
    stopped (end of input, else by PID). A database not made by Codex is
    replaced. Leaves Codex's other start-up files (installation id, logs) in
    CODEX_HOME, as any Codex would."""
    home = Path(codex_home)
    home.mkdir(parents=True, exist_ok=True)
    for name in (STATE_DB, STATE_DB + "-wal", STATE_DB + "-shm"):
        (home / name).unlink(missing_ok=True)
    env = dict(os.environ, CODEX_HOME=str(home))
    p = subprocess.Popen([str(codex), "app-server"], stdin=subprocess.PIPE,
                         stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, cwd=home,
                         text=True)
    messages = queue.Queue()
    errors = []

    def read_stdout():
        for line in p.stdout:
            try:
                messages.put(json.loads(line))
            except ValueError:
                pass
        messages.put(None)

    def read_stderr():
        for line in p.stderr:
            errors.append(line.rstrip())
            del errors[:-20]

    threading.Thread(target=read_stdout, daemon=True).start()
    threading.Thread(target=read_stderr, daemon=True).start()
    try:
        p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "superba-screenshots", "version": "1"}}}) + "\n")
        p.stdin.flush()
        deadline = time.monotonic() + timeout
        while True:
            try:
                msg = messages.get(timeout=max(0.1, deadline - time.monotonic()))
            except queue.Empty:
                raise SystemExit(f"{codex} app-server did not answer initialize in {timeout} s")
            if msg is None:
                p.wait()
                raise SystemExit(f"{codex} app-server exited ({p.returncode}): "
                                 + " / ".join(errors))
            if isinstance(msg, dict) and msg.get("id") == 1:
                if "error" in msg:
                    raise SystemExit(f"{codex} app-server: initialize failed: {msg['error']}")
                break
    finally:
        p.stdin.close()
        try:
            p.wait(timeout=15)
        except subprocess.TimeoutExpired:
            p.kill()
            p.wait()
    if not made_by_codex(home / STATE_DB):
        raise SystemExit(f"{codex} app-server left no state database in {home}")


def insert_threads(db, rows):
    """Insert or replace `rows` (dicts of `threads` columns) in the `threads`
    table of `db`, creating the minimal table when there is none. Only the
    columns the table has are written; Codex's required ones a row leaves out
    get THREAD_DEFAULTS, or an empty value of their type."""
    conn = sqlite3.connect(db)
    conn.executescript(MINIMAL_THREADS)
    columns = conn.execute("PRAGMA table_info(threads)").fetchall()
    for row in rows:
        values = {}
        for _cid, name, kind, notnull, default, pk in columns:
            value = row.get(name)
            if value is None and notnull and default is None and not pk:
                value = THREAD_DEFAULTS.get(
                    name, 0 if "INT" in (kind or "").upper() else "")
            if value is not None:
                values[name] = value
        conn.execute(
            f"INSERT OR REPLACE INTO threads ({', '.join(values)})"
            f" VALUES ({', '.join('?' for _ in values)})", tuple(values.values()))
    conn.commit()
    conn.close()


def thread_row(thread, title, created):
    """A `threads` row of a session (VS Code source, like the desktop app's)."""
    return {"id": thread, "created_at": created, "updated_at": created + 1800,
            "created_at_ms": created * 1000, "updated_at_ms": (created + 1800) * 1000,
            "source": "vscode", "model_provider": "openai", "cwd": "/tmp/project",
            "title": title, "first_user_message": title}


def write_threads(codex_home, now_ms, codex=None):
    db = Path(codex_home) / STATE_DB
    db.parent.mkdir(parents=True, exist_ok=True)
    if codex and not made_by_codex(db):
        codex_state_db(codex, codex_home)
    rows = []
    dashboard = [(t[0], t[1], t[7]) for t in DASHBOARD_SESSIONS] + week_threads(now_ms)
    for thread, title, first in dashboard:
        rows.append(thread_row(thread, title, (now_ms // 1000) - first * 60))
    for index, (thread, title, *_rest) in enumerate(SESSIONS):
        rows.append(thread_row(thread, title,
                               (now_ms // 1000) - (len(SESSIONS) - index) * 5 * 3600))
    insert_threads(db, rows)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--codex-home", required=True)
    ap.add_argument("--upstream", required=True,
                    help="host:port of every line without its own --line-host")
    ap.add_argument("--line-host", action="append", default=[], metavar="LINE=HOST:PORT",
                    help="upstream host:port of one line (cn-fast, overseas or default)")
    ap.add_argument("--codex", help="Codex binary that creates the state database first "
                    "(the desktop app's resources/codex when an app runs on this home)")
    ap.add_argument("--now", type=int, default=int(time.time() * 1000))
    args = ap.parse_args(argv)
    known = {line for _, lines in PROVIDERS.values() for line, _ in lines}
    for spec in args.line_host:
        line, _, host = spec.partition("=")
        if line not in known or not host:
            ap.error(f"--line-host {spec}: expected one of {sorted(known)}=HOST:PORT")
        LINE_HOSTS[line] = host
    n = write_usage(args.data_dir, args.now, args.upstream)
    write_threads(args.codex_home, args.now, args.codex)
    print(json.dumps({"requests": n, "sessions": len(SESSIONS)}))


if __name__ == "__main__":
    main()
