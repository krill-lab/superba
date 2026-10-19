#!/usr/bin/env python3
"""The other coding tools through the router's OpenAI ingress, end to end (Linux).

    SUPERBA_E2E_ENV=/path/to/e2e.env python3 -I e2e/openai_router.py \
        --router target/debug/superba-router --dsh /path/to/dsh \
        --opencode /path/to/opencode --pi /path/to/pi [--node-dir DIR] OUT_DIR

Real dsh (OpenAI Responses), OpenCode 2 (Responses and Chat Completions) and
pi (Responses) run in a temporary home that is bind-mounted over the real one
inside a user + mount + PID namespace (the real ~/.dsh, ~/.config/opencode,
~/.pi and rc files are never seen; /tmp is a private tmpfs). Each tool is
configured with its own harness token and the router's ingress
(`http://127.0.0.1:<port>/openai/v1`). The router is started from a temporary
data directory with two members: one unreachable compatible site (it lists
the model, so the first Responses request fails over) and the test relay as a
Krill member (Responses and Chat Completions at `/codex/v1`). The stats line
is on for the three tools.

Scenarios: a plain prompt per tool and protocol (the answer ends with the
stats line), a continued conversation (the line is stripped from the history
the tool sends back), prompts that make OpenCode and pi read a file with
their tools, and `/openai/v1/models`; then the request log is checked
(harness by token, version, protocol, session ids, failover, usage, upstream
response and request ids, injected / stripped lines).

The relay's address and key come from the env file (SUPERBA_E2E_UPSTREAM,
SUPERBA_E2E_KEY) and are never printed or written to OUT_DIR. Exit 0 when
every check passes; OUT_DIR/report.json holds the checks, run times and log
rows. Delete OUT_DIR/home afterwards: the tools' files hold harness tokens of
the run.
"""

import argparse
import glob
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


def harness_token(router_token, harness):
    """superba_router::token::harness_token for this computer."""
    source = f"h:{harness}"
    mac = hmac.new(
        router_token.encode(), b"superba-router-source-v1\0" + source.encode(), hashlib.sha256
    ).hexdigest()
    return f"sbs_{source.encode().hex()}_{mac}"


def write_private(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.write(fd, text.encode())
    os.close(fd)


class Namespace:
    """Runs commands with `home` mounted over the real home directory."""

    def __init__(self, home, path_dirs):
        self.home = home
        self.real_home = os.path.expanduser("~")
        self.path = ":".join([*path_dirs, "/usr/local/bin", "/usr/bin", "/bin"])

    def run(self, command, env, timeout):
        uid, gid = os.getuid(), os.getgid()
        inner = (
            'set -e; mount --bind "$0" "$1"; mount -t tmpfs tmpfs /tmp; shift 1; '
            f'exec unshare --map-user={uid} --map-group={gid} -- "$@"'
        )
        base = {"PATH": self.path, "HOME": self.real_home, "TERM": "dumb", "LANG": "C.UTF-8"}
        argv = [
            "unshare", "--map-root-user", "--mount", "--pid", "--fork", "--mount-proc", "--",
            "bash", "-c", inner, str(self.home), self.real_home,
            "env", "-i", *[f"{k}={v}" for k, v in {**base, **env}.items()],
            "bash", "-c", f'cd "$HOME/proj" && {command}',
        ]
        return subprocess.run(argv, capture_output=True, text=True, timeout=timeout,
                              stdin=subprocess.DEVNULL)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--router", required=True)
    ap.add_argument("--dsh", required=True)
    ap.add_argument("--opencode", required=True)
    ap.add_argument("--pi", required=True)
    ap.add_argument("--node-dir", default=os.path.dirname(shutil.which("node") or "/usr/bin/node"))
    ap.add_argument("--model", default="gpt-5.6-luna")
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
    (home / "proj").mkdir(parents=True, exist_ok=True)
    port, dead = free_port(), free_port()
    router_token = "sbr_" + uuid.uuid4().hex + uuid.uuid4().hex
    base = f"http://127.0.0.1:{port}/openai/v1"
    model = a.model
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
                     "harnesses": ["dsh", "opencode", "pi"]},
        },
        # The unreachable member lists the model (so it is tried first); the
        # relay serves the defaults of a Krill member.
        "openai": {"members": {"dead": {"responses": {}, "models": [{"id": model}]}}},
    }
    (rdir / "config.json").write_text(json.dumps(cfg, indent=1))
    os.chmod(rdir / "config.json", 0o600)
    write_private(rdir / "secrets.json", json.dumps(
        {"format": 1, "router_token": router_token,
         "member_keys": {"dead": "nb_unreachable_member_000", "krill": key}}))
    tokens = {h: harness_token(router_token, h) for h in ("dsh", "opencode", "pi")}

    # The tools' own configuration, as Superba writes it in routed mode.
    write_private(home / ".dsh" / ".env", f"SUPERBA_ROUTER_TOKEN={tokens['dsh']}\n")
    dsh_patch = f"""- id: llm-pi-ai
  config:
    providers:
      superba-openai:
        displayName: Superba (Responses)
        apiKeyEnv: SUPERBA_ROUTER_TOKEN
        api: openai-responses
        baseURL: {base}
        models:
          - id: {model}
            name: {model}
            contextWindow: 272000
- id: agent-default-model
  config:
    provider: superba-openai
    model: {model}
- id: session-telemetry-otel
  disabled: true
"""
    oc_model = {"name": model, "limit": {"context": 272000, "output": 32000}}
    write_private(home / ".config" / "opencode" / "opencode.json", json.dumps({
        "$schema": "https://opencode.ai/config.json",
        "update": "disable", "share": "disabled",
        "model": f"superba/{model}",
        "agents": {"title": {"model": f"superba/{model}"}},
        "disabled_providers": ["opencode"],
        "providers": {
            "superba": {"name": "Superba (Responses)", "package": "@opencode/ai/providers/openai",
                        "settings": {"baseURL": base, "apiKey": tokens["opencode"]},
                        "models": {model: oc_model}},
            "superba-chat": {"name": "Superba (Chat)",
                             "package": "@opencode/ai/providers/openai-compatible",
                             "settings": {"baseURL": base, "apiKey": tokens["opencode"]},
                             "models": {model: oc_model}},
        },
    }, indent=1))
    write_private(home / ".pi" / "agent" / "models.json", json.dumps({"providers": {"superba": {
        "name": "Superba", "baseUrl": base, "api": "openai-responses", "apiKey": tokens["pi"],
        "models": [{"id": model, "reasoning": True, "contextWindow": 272000, "maxTokens": 32000}],
    }}}, indent=1))

    rlog = open(work / "router.stderr", "w")
    router = subprocess.Popen(
        [a.router, "run", "--data-dir", str(data)], stdout=rlog, stderr=rlog,
        env={"PATH": "/usr/bin:/bin", "HOME": str(work / "router-home"), "RUST_LOG": "info"},
        start_new_session=True,
    )
    ns = Namespace(home, [os.path.dirname(os.path.realpath(p)) for p in (a.opencode, a.pi)]
                   + [os.path.dirname(a.dsh), a.node_dir])
    report = {"checks": [], "runs": []}

    def check(ok, what):
        report["checks"].append({"ok": bool(ok), "what": what})
        print(("PASS " if ok else "FAIL ") + what, flush=True)

    tool_env = {
        "DSH_TELEMETRY_DISABLED": "1",
        "OPENCODE_DISABLE_AUTOUPDATE": "1", "OPENCODE_DISABLE_MODELS_FETCH": "1",
        "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0",
    }

    def tool(name, command, timeout=300):
        t0 = time.time()
        try:
            p = ns.run(command, tool_env, timeout)
            rc, out, err = p.returncode, p.stdout, p.stderr
        except subprocess.TimeoutExpired as e:
            rc, out, err = 124, e.stdout or "", e.stderr or ""
            out = out.decode(errors="replace") if isinstance(out, bytes) else out
            err = err.decode(errors="replace") if isinstance(err, bytes) else err
        (work / f"{name}.stdout").write_text(out)
        (work / f"{name}.stderr").write_text(err)
        report["runs"].append({"name": name, "rc": rc, "secs": round(time.time() - t0, 1),
                               "stdout_chars": len(out)})
        return rc, out

    try:
        for _ in range(100):
            if (rdir / "runtime.json").exists():
                break
            time.sleep(0.1)
        check((rdir / "runtime.json").exists(), "router started")
        secret_a, secret_b = "e2e-" + uuid.uuid4().hex[:12], "e2e-" + uuid.uuid4().hex[:12]
        (home / "proj" / "note.txt").write_text(secret_a + "\n")
        (home / "proj" / "other.txt").write_text(secret_b + "\n")
        pong = "Reply with exactly the word PONG and nothing else."
        again = "Reply with exactly the word AGAIN and nothing else."

        # dsh: its headless profile is created on first launch, then patched.
        dsh = f'"{a.dsh}"'
        tool("dsh-init", f"{dsh} headless --dump-config > /dev/null")
        write_private(home / ".dsh" / "profiles" / "headless" / "cordis.patch.yml", dsh_patch)
        rc, out = tool("dsh-plain", f"{dsh} headless '{pong}'")
        check(rc == 0 and "PONG" in out, f"dsh answered over Responses ({len(out)} chars)")
        check(MARKER in out, "dsh: the answer ends with the stats line")
        sessions = sorted(glob.glob(str(home / ".dsh" / "sessions" / "*" / "session-*")))
        sid = os.path.basename(sessions[-1]) if sessions else ""
        check(bool(sid), "dsh kept its session")
        rc, out = tool("dsh-continue", f"{dsh} headless --session-id {sid} '{again}'")
        check(rc == 0 and "AGAIN" in out, "dsh continued its session")

        oc = f'"{a.opencode}" run --standalone'
        rc, out = tool("oc2-responses", f"{oc} -m superba/{model} '{pong}'")
        check(rc == 0 and "PONG" in out, "OpenCode 2 answered over Responses")
        check(MARKER in out, "OpenCode 2 (Responses): the answer ends with the stats line")
        rc, out = tool("oc2-responses-continue", f"{oc} -c -m superba/{model} '{again}'")
        check(rc == 0 and "AGAIN" in out, "OpenCode 2 continued its session")
        rc, out = tool("oc2-chat", f"{oc} -m superba-chat/{model} '{pong}'")
        check(rc == 0 and "PONG" in out, "OpenCode 2 answered over Chat Completions")
        check(MARKER in out, "OpenCode 2 (Chat): the answer ends with the stats line")
        rc, out = tool("oc2-chat-tool", f"{oc} --auto -m superba-chat/{model} 'Use your read tool to "
                       "read the file note.txt in the current directory, then reply with only its contents.'")
        check(rc == 0 and secret_a in out, "OpenCode 2 read a file with its tool over Chat Completions")

        pi = f'"{a.pi}" -p --model superba/{model}'
        rc, out = tool("pi-plain", f"{pi} '{pong}'")
        check(rc == 0 and "PONG" in out, "pi answered over Responses")
        check(MARKER in out, "pi: the answer ends with the stats line")
        rc, out = tool("pi-continue", f"{pi} -c '{again}'")
        check(rc == 0 and "AGAIN" in out, "pi continued its session")
        rc, out = tool("pi-tool", f"{pi} 'Use the read tool to read the file other.txt in the current "
                       "directory, then reply with only its contents.'")
        check(rc == 0 and secret_b in out, "pi read a file with its tool")

        req = urllib.request.Request(f"{base}/models",
                                     headers={"authorization": "Bearer " + tokens["pi"]})
        with urllib.request.urlopen(req, timeout=30) as r:
            ids = [m["id"] for m in json.load(r)["data"]]
        check(model in ids, f"/openai/v1/models lists {len(ids)} models, {model} among them")

        time.sleep(1.5)
        db = sqlite3.connect(f"file:{rdir / 'usage.sqlite3'}?mode=ro", uri=True)
        db.row_factory = sqlite3.Row
        rows = [dict(r) for r in db.execute("SELECT * FROM requests ORDER BY id")]
        keep = ["id", "harness", "harness_version", "protocol", "requested_model", "model", "member_id",
                "failovers", "status_kind", "http_status", "agent_kind", "request_kind", "input_tokens",
                "cached_tokens", "output_tokens", "first_token_ms", "duration_ms", "tps_milli",
                "injected", "stripped"]
        report["rows"] = [{k: r.get(k) for k in keep}
                          | {"session": bool(r.get("session_id")), "turn": bool(r.get("turn_id")),
                             "response_id": (r.get("response_id") or "")[:9],
                             "upstream_request_id": bool(r.get("upstream_request_id"))} for r in rows]
        by = lambda h, p=None: [r for r in rows if r["harness"] == h and (p is None or r["protocol"] == p)]
        check(len(rows) >= 10, f"{len(rows)} request rows recorded")
        check(by("dsh", "responses") and all(r["harness_version"] == "0.2.1-rc.2" for r in by("dsh")),
              "dsh rows: harness dsh by its token, version 0.2.1-rc.2, protocol responses")
        check(by("opencode", "responses") and by("opencode", "chat_completions")
              and all((r["harness_version"] or "").startswith("2.") for r in by("opencode")),
              "OpenCode rows on both protocols, version 2.x")
        check(by("pi", "responses") and all(r["harness_version"] is None for r in by("pi")),
              "pi rows: harness pi by its token (it sends no version)")
        check(not [r for r in rows if r["harness"] not in ("dsh", "opencode", "pi")],
              "no row of another or unknown tool")
        check(all((r["session_id"] or "").startswith("session-") for r in by("dsh")),
              "dsh rows carry its session id")
        check(len({r["session_id"] for r in by("dsh")}) == 1, "both dsh turns share one session")
        check(all((r["session_id"] or "").startswith("ses_") for r in by("opencode")),
              "OpenCode rows carry its session id")
        check(all(r["session_id"] for r in by("pi")), "pi rows carry its session id")
        check(rows and rows[0]["failovers"] >= 1 and rows[0]["member_id"] == "krill",
              "the first request failed over from the unreachable member")
        ok = [r for r in rows if r["status_kind"] == "ok"]
        # Headless clients may exit after their foreground answer without
        # waiting for an asynchronous title request. Such client cancellations
        # are valid background rows; foreground cancellations and errors still
        # fail the run.
        cancelled_background = [r for r in rows if r["status_kind"] == "cancelled"
                                and r["agent_kind"] == "background"]
        check(len(ok) + len(cancelled_background) == len(rows),
              f"{len(ok)}/{len(rows)} rows ok; {len(cancelled_background)} background requests "
              "cancelled by the client, no foreground failure")
        check(ok and all(r["input_tokens"] and r["output_tokens"] for r in ok),
              "answered rows carry input and output tokens")
        check(ok and all(r["response_id"] and r["upstream_request_id"] for r in ok),
              "answered rows carry the upstream response id and request id")
        check(all((r["response_id"] or "").startswith("chatcmpl") for r in by("opencode", "chat_completions")),
              "Chat rows carry the chatcmpl id")
        for h in ("dsh", "opencode", "pi"):
            check(any(r["injected"] for r in by(h)), f"{h}: answers got the stats line")
            check(any(r["stripped"] for r in by(h)), f"{h}: a later request had the line stripped")
        check(any(r["agent_kind"] == "background" for r in by("opencode")),
              "OpenCode's title requests are background work")
        check(len(by("opencode", "chat_completions")) >= 3, "the Chat tool turn made several requests")
    finally:
        os.killpg(router.pid, 15)
        router.wait(timeout=10)
        rlog.close()
        leaked = False
        for f in list(work.glob("*.std*")):
            t = f.read_text(errors="replace")
            if key in t or router_token in t or any(tok in t for tok in tokens.values()):
                leaked = True
                f.write_text("<removed: held a secret>\n")
        check(not leaked, "no output or log holds the member key, the router token or a harness token")
        (work / "report.json").write_text(json.dumps(report, indent=1))
        (rdir / "secrets.json").unlink(missing_ok=True)
    fails = [c for c in report["checks"] if not c["ok"]]
    print(f"{len(report['checks']) - len(fails)}/{len(report['checks'])} checks passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
