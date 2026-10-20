#!/usr/bin/env python3
"""Requests of coding tools other than Codex for the Overview's screenshots
(user request 118; Python 3.11, stdlib only). Into an isolated home only:

- rows in `<data dir>/router/usage.sqlite3` (written by fixtures.py first) for
  the tools after Codex, tagged with their harness the way the router tags
  them: Claude Code (Anthropic Messages), opencode, pi, dsh, Gemini CLI,
  ZCode — `--tools N` counts Codex, so 3 adds Claude Code and opencode;
- a Claude Code transcript (`~/.claude/projects/<dir>/<session>.jsonl`) with
  requests that did not go through the router (Superba reads those too).

Numbers only, neutral titles, loopback hosts. Deterministic for a given `now`.

    python3 -I tools_usage.py --data-dir HOME/.local/share/superba --home HOME \\
        --tools 3 [--now EPOCH_MS]
"""

import argparse
import json
import random
import sqlite3
import time
from pathlib import Path

# (harness, protocol, member, model, requests today, requests in the week, price $/1M in/out)
TOOLS = [
    ("claude_code", "messages", "krill", "claude-sonnet-4-5", 34, 120, None),
    ("opencode", "chat_completions", "example-api", "gpt-5.5", 12, 40, (1.25, 10.0)),
    ("pi", "chat_completions", "krill", "gpt-5.5", 6, 18, (1.25, 10.0)),
    ("dsh", "responses", "krill", "gpt-6-astra", 4, 10, (2.5, 15.0)),
    ("gemini", "gemini", "example-api", "gemini-2.5-pro", 3, 6, None),
    ("zcode", "chat_completions", "krill", "gpt-5.5", 1, 3, (1.25, 10.0)),
]


def rows_for(template, now_ms, tools):
    rng = random.Random(20261021)
    local = time.localtime(now_ms / 1000)
    midnight = now_ms - ((local.tm_hour * 60 + local.tm_min) * 60 + local.tm_sec) * 1000
    out = []
    for harness, protocol, member, model, today, week, price in TOOLS[: max(0, tools - 1)]:
        session = f"{harness}-session-{rng.randint(1000, 9999)}"
        times = [rng.randint(midnight, now_ms - 60_000) for _ in range(today)]
        times += [rng.randint(midnight - 6 * 86_400_000, midnight - 1) for _ in range(week - today)]
        for i, started in enumerate(sorted(times)):
            context = rng.randint(8_000, 60_000)
            cached = int(context * rng.uniform(0.6, 0.9))
            output = rng.randint(120, 1_800)
            cost = None
            if price:
                p_in, p_out = price
                cost = int((context - cached) * p_in + cached * p_in / 10 + output * p_out)
            row = dict(template)
            row.pop("id", None)
            row.update({
                "started_ms": started, "member_id": member,
                "member_name": "Krill" if member == "krill" else "Example API",
                "harness": harness, "protocol": protocol, "session_id": session,
                "thread_id": None, "prompt_cache_key": None,
                "turn_id": f"{harness}-turn-{i}", "originator": None, "client": "other",
                "model": model, "upstream_model": model, "response_model": model,
                "requested_model": model, "input_tokens": context, "cached_tokens": cached,
                "output_tokens": output, "reasoning_tokens": 0, "cost_micros": cost,
                "price_rev": "mock-1" if cost is not None else None,
                "status_kind": "ok", "http_status": 200,
            })
            out.append({k: v for k, v in row.items() if k in template})
    return out


def write_rows(data_dir, now_ms, tools):
    db = Path(data_dir) / "router" / "usage.sqlite3"
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row
    template = dict(conn.execute("SELECT * FROM requests ORDER BY started_ms DESC LIMIT 1").fetchone())
    rows = rows_for(template, now_ms, tools)
    if rows:
        cols = [c for c in rows[0].keys() if c != "id"]
        conn.executemany(
            f"INSERT INTO requests ({', '.join(cols)}) VALUES ({', '.join('?' for _ in cols)})",
            [tuple(r[c] for c in cols) for r in rows])
    conn.commit()
    conn.close()
    return len(rows)


def write_transcript(home, now_ms, n=8):
    """A Claude Code session that went straight to its provider."""
    if n <= 0:
        return 0
    rng = random.Random(7)
    d = Path(home) / ".claude" / "projects" / "-tmp-demo-project"
    d.mkdir(parents=True, exist_ok=True)
    session = "5f0c2a1e-8d4b-4c7a-9e21-3b6d7f8a9c10"
    lines = []
    for i in range(n):
        at = now_ms - (n - i) * 7 * 60_000
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(at / 1000)) + ".000Z"
        lines.append(json.dumps({
            "type": "assistant", "sessionId": session, "timestamp": stamp,
            "requestId": f"req_demo{i:04d}", "isSidechain": False,
            "message": {
                "id": f"msg_demo{i:04d}", "model": "claude-sonnet-4-5", "role": "assistant",
                "content": [{"type": "text", "text": "…"}],
                "usage": {"input_tokens": rng.randint(20, 400),
                          "cache_creation_input_tokens": rng.randint(0, 3_000),
                          "cache_read_input_tokens": rng.randint(10_000, 40_000),
                          "output_tokens": rng.randint(100, 900)},
            },
        }))
    (d / f"{session}.jsonl").write_text("\n".join(lines) + "\n")
    return n


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data-dir", required=True)
    ap.add_argument("--home", required=True)
    ap.add_argument("--tools", type=int, default=3)
    ap.add_argument("--now", type=int, default=int(time.time() * 1000))
    a = ap.parse_args()
    n = write_rows(a.data_dir, a.now, a.tools)
    t = write_transcript(a.home, a.now, 8 if a.tools >= 2 else 0)
    print(f"{n} tool rows, {t} Claude Code transcript requests")


if __name__ == "__main__":
    main()
