#!/usr/bin/env python3
"""The Claude Code harness end to end (Linux): `spb` sets Claude Code up through
the local router, a real `claude` runs through it, then Claude Code goes back to
its provider and Superba's keys are restored.

    SUPERBA_E2E_ENV=/path/to/e2e.env python3 -I e2e/claude_harness.py \
        --bin target/debug [--claude /path/to/claude] OUT_DIR

Everything runs in a temporary HOME (Superba's data, the router's service as an
XDG autostart entry with a detached supervisor, Claude Code's CLAUDE_CONFIG_DIR);
the real ~/.claude*, ~/.codex, keyring and service manager are never used (keys
in the 0600 file backend). Two providers with Anthropic endpoints: an unreachable
one and the test relay (its Anthropic endpoint at `--anthropic-path`, default
`/codex`); the opus / sonnet / haiku slots map to GPT models (the test relay's gpt-6.1-sol,
gpt-5.6-sol and gpt-5.6-luna).

1. `spb site/profile add`, `spb claude endpoint`, `spb router install`, the stats
   line on, `spb claude provider dead`, slots, `spb claude route router`,
   `spb claude apply`: settings.json holds the router's URL, Claude Code's
   harness token and the gateway hint headers; the router's config holds the
   Claude Code section.
2. `claude -p` with `--model opus|sonnet|haiku` (the first request fails over from
   the unreachable provider), the stats line, a prompt that uses the Read tool;
   the request log rows (harness, version, protocol, models, session, prompt id,
   agent, failover, usage, ids).
3. `spb claude provider relay` + apply (still through the router), then
   `spb router uninstall`: Claude Code goes back to the relay directly and spb
   says so; `claude -p` works direct; `spb claude restore` takes Superba's keys
   out of settings.json.

Secrets come from the env file, reach spb on stdin, and are never printed or
written to OUT_DIR. Exit 0 when every check passes (OUT_DIR/report.json).
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("out")
    ap.add_argument("--bin", required=True, help="directory with spb and superba-router")
    ap.add_argument("--claude", default=shutil.which("claude"))
    ap.add_argument("--anthropic-path", default="/codex")
    a = ap.parse_args()
    secrets = load_env(os.environ["SUPERBA_E2E_ENV"])
    upstream = secrets["SUPERBA_E2E_UPSTREAM"].rstrip("/")
    key = secrets["SUPERBA_E2E_KEY"]
    spb_bin = str(Path(a.bin).resolve() / "spb")
    work = Path(a.out).resolve()
    work.mkdir(parents=True, exist_ok=True)
    home = work / "home"
    home.mkdir(exist_ok=True)
    cfg_dir = home / "claude-config"
    cfg_dir.mkdir(exist_ok=True)
    proj = work / "proj"
    proj.mkdir(exist_ok=True)
    data = home / ".local" / "share" / "superba"
    dead = free_port()
    env = {"PATH": "/usr/bin:/bin", "HOME": str(home), "CLAUDE_CONFIG_DIR": str(cfg_dir),
           "SUPERBA_SECRET_BACKEND": "file", "SUPERBA_LOCALE": "en", "LANG": "C.UTF-8"}
    report = {"checks": [], "steps": []}
    log = open(work / "steps.log", "w")

    def check(ok, what):
        report["checks"].append({"ok": bool(ok), "what": what})
        print(("PASS " if ok else "FAIL ") + what, flush=True)

    def scrub(text):
        for s in (key, router_secret()):
            if s:
                text = text.replace(s, "<secret>")
        return text.replace(upstream, "<relay>")

    def router_secret():
        try:
            return json.loads((data / "router" / "secrets.json").read_text())["router_token"]
        except (OSError, ValueError, KeyError):
            return None

    def spb(*args, stdin=None):
        p = subprocess.run([spb_bin, *args], env=env, capture_output=True, text=True, input=stdin,
                           timeout=180)
        out = scrub(p.stdout + p.stderr)
        log.write(f"$ spb {scrub(' '.join(args))}\n[exit {p.returncode}]\n{out}\n")
        log.flush()
        report["steps"].append({"spb": scrub(" ".join(args)), "rc": p.returncode})
        return p.returncode, out

    def settings():
        try:
            return json.loads((cfg_dir / "settings.json").read_text())
        except (OSError, ValueError):
            return {}

    def claude(name, args, timeout=300):
        cenv = {"PATH": "/usr/bin:/bin", "HOME": str(home), "CLAUDE_CONFIG_DIR": str(cfg_dir),
                "LANG": "C.UTF-8", "DISABLE_AUTOUPDATER": "1"}
        t0 = time.time()
        p = subprocess.run([a.claude, *args], cwd=proj, env=cenv, capture_output=True, text=True,
                           timeout=timeout, stdin=subprocess.DEVNULL)
        (work / f"{name}.stdout").write_text(scrub(p.stdout))
        (work / f"{name}.stderr").write_text(scrub(p.stderr))
        res = None
        for line in reversed(p.stdout.splitlines()):
            try:
                v = json.loads(line)
            except ValueError:
                continue
            if isinstance(v, dict) and v.get("type") == "result":
                res = v
                break
        report["steps"].append({"claude": name, "rc": p.returncode, "secs": round(time.time() - t0, 1)})
        return p.returncode, (res or {}).get("result") or ""

    def rows():
        db = sqlite3.connect(f"file:{data / 'router' / 'usage.sqlite3'}?mode=ro", uri=True)
        db.row_factory = sqlite3.Row
        return [dict(r) for r in db.execute("SELECT * FROM requests ORDER BY id")]

    try:
        # 1. Set up.
        rc, _ = spb("site", "add", "relay", "--name", "Relay", "--base-url", upstream + "/codex/v1")
        check(rc == 0, "spb site add relay")
        rc, _ = spb("profile", "add", "relay", "--name", "Relay", "--site", "relay", "--auth", "apikey",
                    "--key-stdin", stdin=key + "\n")
        check(rc == 0, "spb profile add relay (key on stdin)")
        rc, _ = spb("site", "add", "dead", "--name", "Unreachable", "--base-url", f"http://127.0.0.1:{dead}/v1")
        check(rc == 0, "spb site add dead")
        rc, _ = spb("profile", "add", "dead", "--name", "Unreachable", "--site", "dead", "--auth", "apikey",
                    "--key-stdin", stdin="sk-unreachable-0000\n")
        check(rc == 0, "spb profile add dead")
        rc, _ = spb("claude", "endpoint", upstream + a.anthropic_path, "--provider", "relay", "--auth", "bearer")
        check(rc == 0, "spb claude endpoint (relay)")
        rc, _ = spb("claude", "endpoint", f"http://127.0.0.1:{dead}", "--provider", "dead", "--auth", "x-api-key")
        check(rc == 0, "spb claude endpoint (unreachable)")
        rc, out = spb("claude", "route", "router")
        check(rc != 0 and "spb router install" in out, "the router route needs the router set up first")
        rc, _ = spb("router", "install", "--member", "dead", "--member", "relay")
        check(rc == 0, "spb router install --member dead --member relay")
        rc, _ = spb("router", "stats-line", "on")
        check(rc == 0, "spb router stats-line on")
        for provider in ("dead", "relay"):
            spb("claude", "provider", provider)
            for slot, model in (("opus", "gpt-6.1-sol"), ("sonnet", "gpt-5.6-sol"), ("haiku", "gpt-5.6-luna")):
                rc, _ = spb("claude", "slot", slot, model)
                check(rc == 0, f"spb claude slot {slot} {model} ({provider})")
        rc, _ = spb("claude", "provider", "dead")
        check(rc == 0, "spb claude provider dead (unreachable first)")
        rc, out = spb("claude", "route", "router")
        check(rc == 0 and "127.0.0.1" in out, "spb claude route router")
        rc, out = spb("claude", "apply", "--yes")
        check(rc == 0, "spb claude apply (through the router)")
        s = settings().get("env", {})
        port = json.loads((data / "router" / "config.json").read_text())["listen_port"]
        check(s.get("ANTHROPIC_BASE_URL") == f"http://127.0.0.1:{port}/claude", "settings.json: the router's base URL")
        token = router_secret()
        mac = hmac.new(token.encode(), b"superba-router-source-v1\0h:claude-code", hashlib.sha256).hexdigest()
        check(s.get("ANTHROPIC_AUTH_TOKEN") == f"sbs_{b'h:claude-code'.hex()}_{mac}",
              "settings.json: Claude Code's own harness token (not the router token)")
        check(s.get("CLAUDE_CODE_GATEWAY_HINT_HEADERS") == "1", "settings.json: gateway hint headers on")
        check(s.get("ANTHROPIC_DEFAULT_OPUS_MODEL") == "gpt-6.1-sol" and s.get("ANTHROPIC_DEFAULT_HAIKU_MODEL") == "gpt-5.6-luna",
              "settings.json: slots mapped to GPT models")
        section = json.loads((data / "router" / "config.json").read_text()).get("claude", {})
        check(section.get("order") == ["dead", "relay"] and set(section.get("members", {})) == {"dead", "relay"},
              "router config: Claude Code section, unreachable provider first")
        check(any(al.get("from") == "claude-*haiku*" and al.get("to") == "gpt-5.6-luna" for al in section.get("aliases", [])),
              "router config: Claude Code's haiku ids map to the haiku slot")
        rc, out = spb("claude", "status")
        check(rc == 0 and "Route" in out and "Unreachable" in out and "Relay" in out,
              "spb claude status: the route and the providers on the router")

        # 2. Claude Code through the router.
        rc, text = claude("opus", ["-p", "Reply with exactly the word PONG and nothing else.",
                                   "--model", "opus", "--output-format", "json"])
        check(rc == 0 and "PONG" in text, f"opus slot answered through the router ({len(text)} chars)")
        check(MARKER in text, "the answer ends with the stats line")
        rc, text = claude("sonnet", ["-p", "Reply with exactly the word SONNET and nothing else.",
                                     "--model", "sonnet", "--output-format", "json"])
        check(rc == 0 and "SONNET" in text, "sonnet slot answered")
        rc, text = claude("haiku", ["-p", "Reply with exactly the word HAIKU and nothing else.",
                                    "--model", "haiku", "--output-format", "json"])
        check(rc == 0 and "HAIKU" in text, "haiku slot answered")
        secret = "e2e-" + uuid.uuid4().hex[:12]
        (proj / "note.txt").write_text(secret + "\n")
        rc, text = claude("tool", ["-p", "Use the Read tool to read the file note.txt in the current directory, "
                                   "then reply with only its contents.", "--model", "sonnet",
                                   "--allowedTools", "Read", "--output-format", "json"])
        check(rc == 0 and secret in text, "Claude Code read the file with its Read tool")
        time.sleep(1.5)
        r = rows()
        keep = ["id", "harness", "harness_version", "protocol", "requested_model", "model", "member_id",
                "failovers", "status_kind", "agent_kind", "request_kind", "input_tokens", "cached_tokens",
                "output_tokens", "injected", "stripped"]
        report["rows_router"] = [{k: x.get(k) for k in keep}
                                 | {"session": bool(x.get("session_id")), "turn": bool(x.get("turn_id")),
                                    "response_id": bool(x.get("response_id")),
                                    "request_id": bool(x.get("upstream_request_id"))} for x in r]
        check(len(r) >= 5, f"{len(r)} request rows")
        check(all(x["harness"] == "claude_code" and x["harness_version"] and x["protocol"] == "messages" for x in r),
              "rows: harness claude_code with its version, protocol messages")
        models = {x["requested_model"] for x in r}
        check({"gpt-6.1-sol", "gpt-5.6-sol", "gpt-5.6-luna"} <= models, f"rows: the three slot models ({sorted(models)})")
        check(r[0]["failovers"] >= 1 and r[0]["member_id"] == "relay", "the first request failed over to the relay")
        check(all(x["member_id"] == "relay" for x in r if x["status_kind"] == "ok"), "every answer came from the relay")
        check(all(x["session_id"] and x["turn_id"] and x["agent_kind"] for x in r), "rows: session, prompt id, agent kind")
        ok_rows = [x for x in r if x["status_kind"] == "ok"]
        check(ok_rows and all(x["input_tokens"] and x["output_tokens"] and x["response_id"] for x in ok_rows),
              "rows: usage and the upstream message ids")
        check(any(x["injected"] for x in r), "rows: stats line injected")

        # 3. Another provider, then the router goes and Claude Code goes direct.
        rc, _ = spb("claude", "provider", "relay")
        rc2, _ = spb("claude", "apply", "--yes")
        check(rc == 0 and rc2 == 0, "spb claude provider relay + apply (still through the router)")
        section = json.loads((data / "router" / "config.json").read_text()).get("claude", {})
        check(section.get("order", [None])[0] == "relay", "router config: the relay first now")
        n_before = len(rows())
        rc, text = claude("relay-router", ["-p", "Reply with exactly the word ROUTED and nothing else.",
                                           "--output-format", "json"])
        time.sleep(1.0)
        last = rows()[-1]
        check(rc == 0 and "ROUTED" in text and len(rows()) > n_before and last["failovers"] == 0,
              "through the router with the relay first: no failover")
        rc, out = spb("router", "uninstall")
        check(rc == 0 and "Claude Code goes directly to Relay" in out,
              "spb router uninstall: Claude Code goes back to its provider and spb says so")
        s = settings().get("env", {})
        check(s.get("ANTHROPIC_BASE_URL") == upstream + a.anthropic_path, "settings.json: the relay's endpoint again")
        check(s.get("ANTHROPIC_AUTH_TOKEN") == key and "CLAUDE_CODE_GATEWAY_HINT_HEADERS" not in s,
              "settings.json: the relay's key, no gateway hint headers")
        rc, text = claude("direct", ["-p", "Reply with exactly the word DIRECT and nothing else.",
                                     "--output-format", "json"])
        check(rc == 0 and "DIRECT" in text and MARKER not in text, "direct to the relay (no router, no stats line)")
        rc, _ = spb("claude", "restore", "--yes")
        s = settings().get("env", {})
        check(rc == 0 and not any(k.startswith("ANTHROPIC_") for k in s), "spb claude restore: Superba's keys are out")
    finally:
        # Nothing of this run may keep running.
        left = subprocess.run(["pgrep", "-f", str(Path(a.bin).resolve() / "superba-router")],
                              capture_output=True, text=True).stdout.split()
        for pid in left:
            try:
                os.kill(int(pid), 15)
            except (ProcessLookupError, ValueError):
                pass
        check(not left, f"no router process left after the uninstall ({len(left)})")
        log.close()
        t = (work / "steps.log").read_text()
        for f in [work / "steps.log", *work.glob("*.std*")]:
            body = f.read_text(errors="replace")
            if key in body or (router_secret() or "\0") in body:
                f.write_text("<removed: held a secret>\n")
        check(key not in t, "no log holds the key")
        (work / "report.json").write_text(json.dumps(report, indent=1))
    fails = [c for c in report["checks"] if not c["ok"]]
    print(f"{len(report['checks']) - len(fails)}/{len(report['checks'])} checks passed")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
