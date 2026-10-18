#!/usr/bin/env python3
"""Claude Code through the local router, end to end (Linux).

    SUPERBA_E2E_ENV=/path/to/e2e.env python3 -I e2e/claude_router.py \
        --router target/debug/superba-router [--claude /path/to/claude] OUT_DIR

A real `claude` in a temporary HOME and CLAUDE_CONFIG_DIR is pointed at a
`superba-router` started from a temporary data directory: its Claude Code
ingress (`http://127.0.0.1:<port>/claude`) with a Claude Code harness token,
two members (one unreachable, so the first request fails over) and the test
relay's Anthropic endpoint (`--anthropic-path`, default `/codex`: the test relay
serves `/codex/v1/messages`). Scenarios: a plain prompt (the stats line ends the
answer), a continued conversation (the line is stripped from the history), a
prompt that has Claude Code read a file with its Read tool, a long streamed
answer, `/claude/v1/models` and `count_tokens`; then the request log is checked
(harness, version, protocol, session and prompt ids, failover, usage,
injected / stripped lines, the upstream message and request ids).

The relay's address and key come from the env file (SUPERBA_E2E_UPSTREAM,
SUPERBA_E2E_KEY) and are never printed or written to OUT_DIR; the real
~/.claude* directories are never read or written. Exit 0 when every check
passes; OUT_DIR/report.json holds the checks, run times and log rows.
"""

import argparse
import hashlib
import hmac
import json
import os
import shutil
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.request
import uuid
from pathlib import Path

MARKER = "⚡⁠"


def load_env(path):
    env = {}
    for line in open(path):
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            env[k] = v.strip().strip('"').strip("'")
    return env


def free_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def harness_token(router_token, source):
    """superba_router::token::harness_token for this computer."""
    mac = hmac.new(
        router_token.encode(), b"superba-router-source-v1\0" + source.encode(), hashlib.sha256
    ).hexdigest()
    return f"sbs_{source.encode().hex()}_{mac}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--router", required=True)
    ap.add_argument("--claude", default=shutil.which("claude"))
    ap.add_argument("--anthropic-path", default="/codex")
    ap.add_argument("--model", default="gpt-5.6-sol")
    ap.add_argument("--small-model", default="gpt-5.6-luna")
    a = ap.parse_args()
    secrets = load_env(os.environ["SUPERBA_E2E_ENV"])
    upstream = secrets["SUPERBA_E2E_UPSTREAM"].rstrip("/")
    key = secrets["SUPERBA_E2E_KEY"]
    work = Path(a.out).resolve()
    work.mkdir(parents=True, exist_ok=True)
    data = work / "data"
    rdir = data / "router"
    rdir.mkdir(parents=True, exist_ok=True)
    home = work / "home"
    (home / "cfg").mkdir(parents=True, exist_ok=True)
    proj = work / "proj"
    proj.mkdir(exist_ok=True)
    port, dead = free_port(), free_port()
    router_token = "sbr_" + uuid.uuid4().hex + uuid.uuid4().hex
    models = [{"id": a.model}, {"id": a.small_model}]
    cfg = {
        "format": 1,
        "listen_port": port,
        "use_system_proxy": False,
        "members": [
            {"id": "dead", "name": "Unreachable", "kind": "compatible", "websocket": False,
             "lines": [{"id": "main", "name": "Main", "base_url": f"http://127.0.0.1:{dead}/v1"}]},
            {"id": "krill", "name": "Test relay", "kind": "krill", "websocket": True,
             "lines": [{"id": "main", "name": "Main", "base_url": upstream + "/backend-api/codex"}]},
        ],
        "tuning": {"active_probes": False},
        "stats": {
            "record": True, "usage_days": 90, "max_rows": 200000, "prices": {},
            "line": {"template": "⚡ {tps} tok/s · first token {ttft} · turn {input} in / {output} out",
                     "style": "quote", "originators": ["Codex Desktop"],
                     "harnesses": ["codex", "claude_code"]},
        },
        "claude": {"members": {
            "dead": {"anthropic": {"path": ""}, "models": models},
            "krill": {"anthropic": {"path": a.anthropic_path}, "models": models},
        }},
    }
    (rdir / "config.json").write_text(json.dumps(cfg, indent=1))
    os.chmod(rdir / "config.json", 0o600)
    fd = os.open(rdir / "secrets.json", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, json.dumps({"format": 1, "router_token": router_token,
                             "member_keys": {"dead": "nb_unreachable_member_000", "krill": key}}).encode())
    os.close(fd)
    rlog = open(work / "router.stderr", "w")
    router = subprocess.Popen(
        [a.router, "run", "--data-dir", str(data)], stdout=rlog, stderr=rlog,
        env={"PATH": "/usr/bin:/bin", "HOME": str(home), "RUST_LOG": "info"}, start_new_session=True,
    )
    report = {"checks": [], "runs": []}

    def check(ok, what):
        report["checks"].append({"ok": bool(ok), "what": what})
        print(("PASS " if ok else "FAIL ") + what, flush=True)

    try:
        for _ in range(100):
            if (rdir / "runtime.json").exists():
                break
            time.sleep(0.1)
        check((rdir / "runtime.json").exists(), "router started")
        token = harness_token(router_token, "h:claude-code")
        cenv = {
            "PATH": "/usr/bin:/bin", "HOME": str(home), "CLAUDE_CONFIG_DIR": str(home / "cfg"),
            "LANG": "C.UTF-8",
            "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}/claude", "ANTHROPIC_AUTH_TOKEN": token,
            "ANTHROPIC_MODEL": a.model, "ANTHROPIC_DEFAULT_OPUS_MODEL": a.model,
            "ANTHROPIC_DEFAULT_SONNET_MODEL": a.model, "ANTHROPIC_DEFAULT_HAIKU_MODEL": a.small_model,
            "CLAUDE_CODE_GATEWAY_HINT_HEADERS": "1", "DISABLE_AUTOUPDATER": "1",
            "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1",
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
        }

        def claude(name, args, timeout=300):
            t0 = time.time()
            p = subprocess.run([a.claude, *args], cwd=proj, env=cenv, capture_output=True, text=True,
                               timeout=timeout, stdin=subprocess.DEVNULL)
            (work / f"{name}.stdout").write_text(p.stdout)
            (work / f"{name}.stderr").write_text(p.stderr)
            res = None
            for line in reversed(p.stdout.splitlines()):
                try:
                    v = json.loads(line)
                except ValueError:
                    continue
                if isinstance(v, dict) and v.get("type") == "result":
                    res = v
                    break
            report["runs"].append({"name": name, "rc": p.returncode, "secs": round(time.time() - t0, 1),
                                   "result_chars": len((res or {}).get("result") or "")})
            return p.returncode, (res or {}).get("result") or ""

        rc, text = claude("plain", ["-p", "Reply with exactly the word PONG and nothing else.",
                                    "--output-format", "json"])
        check(rc == 0 and "PONG" in text, f"plain prompt answered ({len(text)} chars)")
        check(MARKER in text, "the answer ends with the stats line")
        rc, text = claude("continue", ["-p", "-c", "Reply with exactly the word AGAIN and nothing else.",
                                       "--output-format", "json"])
        check(rc == 0 and "AGAIN" in text, "continued conversation answered")
        secret = "e2e-" + uuid.uuid4().hex[:12]
        (proj / "note.txt").write_text(secret + "\n")
        rc, text = claude("tool", ["-p", "Use the Read tool to read the file note.txt in the current "
                                   "directory, then reply with only its contents.",
                                   "--allowedTools", "Read", "--output-format", "json"])
        check(rc == 0 and secret in text, "Claude Code read the file with its Read tool")
        rc, text = claude("long", ["-p", "Write about 400 words about lighthouses. Plain prose, no headings.",
                                   "--output-format", "stream-json", "--verbose"], timeout=600)
        check(rc == 0 and len(text) > 1200, f"long streamed answer ({len(text)} chars)")

        def call(path, body=None):
            req = urllib.request.Request(
                f"http://127.0.0.1:{port}/claude{path}",
                data=json.dumps(body).encode() if body else None,
                headers={"x-api-key": token, "content-type": "application/json",
                         "anthropic-version": "2023-06-01"})
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.load(r)

        m = call("/v1/models")
        check([x["id"] for x in m["data"]] == [a.model, a.small_model], "/claude/v1/models lists the members' models")
        c = call("/v1/messages/count_tokens",
                 {"model": a.model, "messages": [{"role": "user", "content": "hello world " * 50}]})
        check(c.get("input_tokens", 0) > 0, f"count_tokens answered ({c.get('input_tokens')})")

        time.sleep(1.5)
        db = sqlite3.connect(f"file:{rdir / 'usage.sqlite3'}?mode=ro", uri=True)
        db.row_factory = sqlite3.Row
        rows = [dict(r) for r in db.execute("SELECT * FROM requests ORDER BY id")]
        keep = ["id", "harness", "harness_version", "protocol", "requested_model", "model", "member_id",
                "failovers", "status_kind", "http_status", "agent_kind", "request_kind", "input_tokens",
                "cached_tokens", "output_tokens", "first_token_ms", "duration_ms", "tps_milli", "injected",
                "stripped"]
        report["rows"] = [{k: r.get(k) for k in keep}
                          | {"session": bool(r.get("session_id")), "turn": bool(r.get("turn_id"))} for r in rows]
        check(len(rows) >= 5, f"{len(rows)} request rows recorded")
        check(all(r["harness"] == "claude_code" and r["harness_version"] and r["protocol"] == "messages"
                  for r in rows), "every row: harness claude_code with its version, protocol messages")
        check(all(r["session_id"] and r["turn_id"] for r in rows), "every row has a session id and a prompt id")
        check(rows and rows[0]["failovers"] >= 1 and rows[0]["member_id"] == "krill",
              "the first request failed over from the unreachable member")
        ok_rows = [r for r in rows if r["status_kind"] == "ok"]
        check(ok_rows and all(r["input_tokens"] and r["output_tokens"] for r in ok_rows),
              "answered rows carry input and output tokens")
        check(ok_rows and all(r["response_id"] and r["upstream_request_id"] for r in ok_rows),
              "answered rows carry the upstream message id and request id")
        check(any(r["injected"] for r in rows), "answers got the stats line")
        check(any(r["stripped"] for r in rows), "a later request had the line stripped from its history")
        turns = {}
        for r in rows:
            turns.setdefault(r["turn_id"], []).append(r)
        check(any(len(v) >= 2 for v in turns.values()), "the tool turn made several requests under one prompt id")
        long_rows = [r for r in ok_rows if (r["output_tokens"] or 0) > 300]
        check(long_rows and all(r["first_token_ms"] < r["duration_ms"] for r in long_rows),
              "the long answer streamed (first token before the end)")
    finally:
        os.killpg(router.pid, 15)
        router.wait(timeout=10)
        rlog.close()
        leaked = False
        for f in list(work.glob("*.std*")):
            t = f.read_text(errors="replace")
            if key in t or router_token in t:
                leaked = True
                f.write_text("<removed: held a secret>\n")
        check(not leaked, "no output or log holds the member key or the router token")
        (work / "report.json").write_text(json.dumps(report, indent=1))
        (rdir / "secrets.json").unlink(missing_ok=True)
    fails = [c for c in report["checks"] if not c["ok"]]
    print(f"{len(report['checks']) - len(fails)}/{len(report['checks'])} checks passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
