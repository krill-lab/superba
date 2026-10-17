#!/usr/bin/env python3
"""Codex rollout fixtures for the Usage page screenshots (Python 3.11, stdlib only).

Writes, into an isolated home only, `CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`
files the way Codex 0.162 / 0.163 writes them (session_meta, task_started,
turn_context, response items without text, token_usage_record per response,
token_count, task_complete; a failed and a stopped turn), so the Usage page has
rows without a router:

- every session of the router log (`<data dir>/router/usage.sqlite3`, when it
  exists) gets the same requests (same thread, tokens and end time), which the
  Usage page lists once (the router's row);
- one more session ("Plan the database migration") went direct and exists only
  in the rollout; without a router log the four router sessions are generated
  from scratch as well.

With `--backdate-state` the generations in Superba's `state.json` are moved
three days back, so the fixture requests (spread over the last day) are
attributed to the provider applied then instead of "openai" (before Superba).

Everything is synthetic: no prompt or answer text, no keys, loopback only.

    python3 -I rollouts.py --codex-home HOME/.codex [--data-dir DATA] \\
        [--backdate-state] [--now EPOCH_MS]
"""

import argparse
import json
import random
import sqlite3
import time
from datetime import datetime, timezone
from pathlib import Path

# The router log's sessions (fixtures.py SESSIONS) and one direct-only session.
ROUTER_THREADS = [
    ("019a2f10-5b1c-7c3e-9d41-0a6b2c4d8e01", "Codex Desktop", "gpt-6-astra"),
    ("019a2f22-7e3a-7a19-8b52-1c7d3e5f9a02", "Codex Desktop", "gpt-5.5"),
    ("019a2f35-1d4e-7b28-9c63-2d8e4f6a0b03", "codex_cli_rs", "gpt-5.5"),
    ("019a2f48-3f5a-7c37-8d74-3e9f5a7b1c04", "Codex Desktop", "gpt-6-astra"),
]
DIRECT_THREAD = ("019a2f5b-6a7b-7d46-8e85-4f0a6b8c2d05", "Codex Desktop", "gpt-6-astra",
                 "Plan the database migration")


def iso(ms):
    return datetime.fromtimestamp(ms / 1000, tz=timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%S.") + f"{ms % 1000:03d}Z"


def line(ms, kind, payload):
    return json.dumps({"timestamp": iso(ms), "type": kind, "payload": payload},
                      separators=(",", ":")) + "\n"


def usage(inp, cached, out, reasoning=0):
    return {"input_tokens": inp, "cached_input_tokens": cached, "cache_write_input_tokens": 0,
            "output_tokens": out, "reasoning_output_tokens": reasoning,
            "total_tokens": inp + out}


class Rollout:
    """One thread's rollout, written in time order."""

    def __init__(self, thread, originator, model, start_ms):
        self.thread, self.model = thread, model
        self.lines = []
        self.total = usage(0, 0, 0)
        self.turns = 0
        self.lines.append(line(start_ms, "session_meta", {
            "session_id": thread, "id": thread, "timestamp": iso(start_ms), "cwd": "/tmp/project",
            "originator": originator, "cli_version": "0.163.0", "source": "vscode",
            "thread_source": "user", "model_provider": "openai", "history_mode": "paginated"}))

    def turn(self, start_ms, requests, end=None):
        """A turn with `requests` = [(gap_ms, duration_ms, input, cached, output)];
        `end` = None (completed), "failed" or "stopped"."""
        self.turns += 1
        tid = f"{self.thread[:8]}-turn-{self.turns:02d}"
        t = start_ms
        self.lines.append(line(t, "event_msg", {"type": "task_started", "turn_id": tid,
                                                "started_at": t // 1000,
                                                "model_context_window": 272000}))
        self.lines.append(line(t + 20, "turn_context", {
            "turn_id": tid, "cwd": "/tmp/project", "model": self.model, "effort": "medium",
            "approval_policy": "never", "sandbox_policy": {"type": "workspace-write"}}))
        t += 40
        self.lines.append(line(t, "response_item", {"type": "message", "role": "user",
                                                    "content": []}))
        for i, (gap, duration, inp, cached, out) in enumerate(requests):
            if i:
                t += gap
                self.lines.append(line(t, "response_item", {
                    "type": "function_call_output", "call_id": f"call_{i}", "output": ""}))
            end_ms = t + duration
            last = i == len(requests) - 1
            kind = ("message", "assistant") if last else ("function_call", None)
            item = {"type": kind[0]}
            if kind[1]:
                item.update(role=kind[1], content=[], phase="final_answer")
            self.lines.append(line(end_ms - 5, "response_item", item))
            u = usage(inp, cached, out)
            for k in self.total:
                self.total[k] += u[k]
            self.lines.append(line(end_ms, "token_usage_record", {
                "thread_id": self.thread, "turn_id": tid, "session_id": self.thread,
                "root_turn_id": tid, "response_id": f"resp_{self.thread[-6:]}{self.turns:02d}{i}",
                "usage": u, "turn_token_usage": u, "thread_token_usage": dict(self.total)}))
            self.lines.append(line(end_ms + 1, "event_msg", {
                "type": "token_count",
                "info": {"total_token_usage": dict(self.total), "last_token_usage": u,
                         "model_context_window": 272000},
                "rate_limits": {"limit_id": "codex"}}))
            t = end_ms
        if end == "failed":
            t += 4200
            self.lines.append(line(t, "event_msg", {
                "type": "task_complete", "turn_id": tid, "last_agent_message": None,
                "error": {"message": "stream disconnected before completion",
                          "codex_error_info": {"response_stream_disconnected":
                                               {"http_status_code": 502}}}}))
        elif end == "stopped":
            t += 3100
            self.lines.append(line(t, "event_msg", {"type": "turn_aborted", "turn_id": tid,
                                                    "reason": "interrupted"}))
        else:
            self.lines.append(line(t + 15, "event_msg", {"type": "task_complete",
                                                         "turn_id": tid,
                                                         "last_agent_message": ""}))

    def write(self, sessions_dir, start_ms):
        day = datetime.fromtimestamp(start_ms / 1000, tz=timezone.utc)
        d = Path(sessions_dir) / day.strftime("%Y/%m/%d")
        d.mkdir(parents=True, exist_ok=True)
        name = f"rollout-{day.strftime('%Y-%m-%dT%H-%M-%S')}-{self.thread}.jsonl"
        (d / name).write_text("".join(self.lines))


def router_requests(data_dir):
    """Router log rows by thread: (end_ms, input, cached, output, status)."""
    db = Path(data_dir) / "router" / "usage.sqlite3" if data_dir else None
    if not db or not db.exists():
        return None
    conn = sqlite3.connect(db)
    rows = conn.execute(
        "SELECT COALESCE(thread_id, prompt_cache_key), started_ms, duration_ms, input_tokens,"
        " cached_tokens, output_tokens, status_kind FROM requests ORDER BY started_ms").fetchall()
    conn.close()
    out = {}
    for thread, start, duration, inp, cached, outp, status in rows:
        if thread and status == "ok" and inp is not None and outp is not None:
            out.setdefault(thread, []).append((start, duration or 1500, inp, cached or 0, outp))
    return out


def mirrored(thread, originator, model, rows):
    """The rollout of a routed thread: each router request as its own turn,
    ending when the router's request ended (the Usage page lists them once)."""
    first = rows[0][0] - 3000
    r = Rollout(thread, originator, model, first)
    for start, duration, inp, cached, outp in rows:
        # The derived start is the user message 40 ms after the turn start.
        r.turn(start - 60, [(0, duration + 20, inp, cached, outp)])
    return r, first


def synthetic(thread, originator, model, now_ms, rng, hours_ago):
    start = now_ms - int(hours_ago * 3600_000)
    r = Rollout(thread, originator, model, start)
    t = start + 2000
    for turn in range(4):
        n = rng.choice([1, 1, 2, 3])
        reqs = []
        ctx = 14_000 + turn * 2_500
        for i in range(n):
            reqs.append((rng.randint(400, 2600), rng.randint(1800, 9000), ctx + i * 900,
                         ctx - 1200 if turn or i else 0, rng.randint(40, 900)))
        r.turn(t, reqs)
        t += sum(q[0] + q[1] for q in reqs) + rng.randint(60_000, 600_000)
    return r, start


def backdate_state(data_dir, now_ms):
    path = Path(data_dir) / "state.json"
    if not path.exists():
        return 0
    data = json.loads(path.read_text())
    n = 0
    for home in (data.get("homes") or {}).values():
        for g in home.get("history") or []:
            rec = g.get("record", g)
            if "applied_at" in rec:
                rec["applied_at"] = now_ms - 3 * 86_400_000 + n * 1000
                n += 1
    path.write_text(json.dumps(data, indent=2))
    return n


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--codex-home", required=True)
    ap.add_argument("--data-dir")
    ap.add_argument("--backdate-state", action="store_true")
    ap.add_argument("--now", type=int, default=int(time.time() * 1000))
    args = ap.parse_args(argv)
    rng = random.Random(20261017)
    sessions = Path(args.codex_home) / "sessions"
    routed = router_requests(args.data_dir)
    written = 0
    for i, (thread, originator, model) in enumerate(ROUTER_THREADS):
        if routed is not None and thread in routed:
            r, start = mirrored(thread, originator, model, routed[thread])
        else:
            r, start = synthetic(thread, originator, model, args.now, rng, 20 - i * 4.5)
        r.write(sessions, start)
        written += 1
    thread, originator, model, title = DIRECT_THREAD
    r, start = synthetic(thread, originator, model, args.now, rng, 6)
    # A failed and a stopped turn at the end.
    t = args.now - 25 * 60_000
    r.turn(t, [(0, 2400, 31_000, 29_800, 210)])
    r.turn(t + 6 * 60_000, [], end="failed")
    r.turn(t + 12 * 60_000, [], end="stopped")
    r.write(sessions, start)
    written += 1
    # Its name in state_5.sqlite (fixtures.py writes the others).
    db = Path(args.codex_home) / "state_5.sqlite"
    if db.exists():
        conn = sqlite3.connect(db)
        created = start // 1000
        conn.execute(
            "INSERT OR REPLACE INTO threads (id, rollout_path, created_at, updated_at, source,"
            " model_provider, cwd, title, first_user_message) VALUES (?,?,?,?,?,?,?,?,?)",
            (thread, None, created, created + 1800, "vscode", "openai", "/tmp/project", title,
             title))
        conn.commit()
        conn.close()
    moved = backdate_state(args.data_dir, args.now) if args.backdate_state and args.data_dir else 0
    print(f"{written} rollouts, router log {'mirrored' if routed is not None else 'absent'}, "
          f"{moved} generations backdated")


if __name__ == "__main__":
    main()
