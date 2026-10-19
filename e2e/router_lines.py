#!/usr/bin/env python3
"""Issue 99: change router line through spb, with isolated HOME and two origins.

python3 -I e2e/router_lines.py --spb TARGET/debug/spb \
    --router TARGET/debug/superba-router OUT_DIR

--expect-bug records the original failure (a saved line ignored by the live
router). --real uses SUPERBA_E2E_ENV's SUPERBA_E2E_KEY and SUPERBA_E2E_UPSTREAM:
two loopback line forwarders send actual requests to that Krill test relay.
Credentials, upstream addresses and response bodies never enter the report;
the temporary home, router configuration and tokens are removed on exit.
"""

import argparse
import contextlib
import http.server
import http.client
import json
import os
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
import urllib.parse
import uuid
from pathlib import Path


def private_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        json.dump(value, f)


def env_file(path):
    values = {}
    for row in Path(path).read_text().splitlines():
        if row.strip() and not row.lstrip().startswith("#") and "=" in row:
            key, value = row.split("=", 1)
            values[key.strip()] = value.strip().strip("\"'")
    return values


class Upstream(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.server.upstream:
            return self.forward(None)
        body = b'{"object":"list","data":[{"id":"gpt-6.1-sol"}]}'
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        request_body = self.rfile.read(int(self.headers.get("content-length", 0)))
        self.server.hits.append(self.server.line)
        if self.server.upstream:
            return self.forward(request_body)
        body = b'{"id":"rline-test","object":"response","output":[]}'
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def forward(self, body):
        target = urllib.parse.urlsplit(self.server.upstream)
        connection = (http.client.HTTPSConnection if target.scheme == "https"
                      else http.client.HTTPConnection)(target.hostname, target.port, timeout=120)
        hop = {"host", "connection", "transfer-encoding", "content-length", "keep-alive"}
        try:
            headers = {k: v for k, v in self.headers.items() if k.lower() not in hop}
            connection.request(self.command, target.path.rstrip("/") + self.path, body, headers)
            response = connection.getresponse()
            answer = response.read()
            self.send_response(response.status)
            for k, v in response.getheaders():
                if k.lower() not in hop:
                    self.send_header(k, v)
            self.send_header("content-length", str(len(answer)))
            self.end_headers()
            self.wfile.write(answer)
        finally:
            connection.close()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--spb", required=True, type=Path)
    p.add_argument("--router", required=True, type=Path)
    p.add_argument("--expect-bug", action="store_true")
    p.add_argument("--real", action="store_true")
    p.add_argument("out", type=Path)
    args = p.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    report = {"real": args.real, "checks": [], "routes": []}

    def check(name, ok):
        report["checks"].append({"name": name, "ok": bool(ok)})
        assert ok, name

    with contextlib.ExitStack() as stack:
        home = Path(stack.enter_context(tempfile.TemporaryDirectory(dir=args.out)))
        data = home / "data"
        test_env = env_file(os.environ["SUPERBA_E2E_ENV"]) if args.real else {}
        if args.real and not all(test_env.get(name) for name in
                                 ("SUPERBA_E2E_KEY", "SUPERBA_E2E_UPSTREAM")):
            raise ValueError("real mode requires SUPERBA_E2E_KEY and SUPERBA_E2E_UPSTREAM")
        key = test_env.get("SUPERBA_E2E_KEY", "rline-fake-upstream-key")
        env = {"HOME": str(home), "USERPROFILE": str(home), "PATH": "/usr/bin:/bin",
               "XDG_CONFIG_HOME": str(home / "config"), "XDG_DATA_HOME": str(home / "share"),
               "XDG_CACHE_HOME": str(home / "cache"), "CODEX_HOME": str(home / ".codex"),
               "SUPERBA_DATA_DIR": str(data), "SUPERBA_LOCALE": "en", "LANG": "C.UTF-8",
               "RLINE_API_KEY": key}

        def cli(*command):
            r = subprocess.run([str(args.spb.resolve()), "--json", *command], env=env,
                               cwd=home, capture_output=True, text=True, timeout=60)
            if r.returncode:
                # Do not copy arbitrary tool output (or a credential) into the report.
                raise AssertionError(f"spb {' '.join(command)} exited {r.returncode}: "
                                     + (r.stderr + r.stdout).replace(key, "[redacted]")[:3000])
            return json.loads(r.stdout)

        try:
            sites = cli("site", "list")
            site = next(s for s in sites if s["id"] == "krill")
            site["id"] = "rline-test"
            site["lines"] = [l for l in site["lines"] if l["id"] in ("cn-fast", "overseas")]
            hits = []
            report["upstream"] = "configured Krill test relay via two loopback forwarders" if args.real else "two fake origins"
            for line in site["lines"]:
                server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
                server.line, server.hits = line["id"], hits
                server.upstream = test_env.get("SUPERBA_E2E_UPSTREAM")
                threading.Thread(target=server.serve_forever, daemon=True).start()
                stack.callback(server.server_close)
                stack.callback(server.shutdown)
                line["origin"] = f"http://127.0.0.1:{server.server_port}"
            private_json(home / "sites.json", [site])
            env["SUPERBA_TEST_SITES"] = str(home / "sites.json")
            cli("profile", "add", "rline", "--site", "rline-test", "--key-env", "RLINE_API_KEY",
                "--line", "cn-fast", "--line", "overseas", "--auth", "apikey", "--no-recommended-keys")
            profiles = json.loads((data / "profiles.json").read_text())
            profiles["settings"]["router"] = {"enabled": True, "members": ["rline"]}
            private_json(data / "profiles.json", profiles)
            with socket.socket() as sock:
                sock.bind(("127.0.0.1", 0))
                port = sock.getsockname()[1]
            token = "sbr_" + uuid.uuid4().hex + uuid.uuid4().hex
            lines = {l["id"]: l for l in site["lines"]}
            cfg = {"format": 1, "listen_port": port, "use_system_proxy": False,
                   "members": [{"id": "rline", "name": "rline", "kind": "krill", "websocket": False,
                                "lines": [{"id": id, "name": lines[id]["name"]["fallback"],
                                           "base_url": lines[id]["origin"] + "/codex/v1"}
                                          for id in ("cn-fast", "overseas")]}],
                   "tuning": {"active_probes": False}, "stats": {"record": True}}
            private_json(data / "router/config.json", cfg)
            private_json(data / "router/secrets.json", {"format": 1, "router_token": token,
                                                        "member_keys": {"rline": key}})
            router = subprocess.Popen([str(args.router.resolve()), "run", "--data-dir", str(data)],
                                      env=env, cwd=home, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            stack.callback(lambda: router.wait(timeout=15))
            stack.callback(router.terminate)
            runtime_path = data / "router/runtime.json"
            for _ in range(100):
                if runtime_path.exists():
                    break
                time.sleep(0.1)
            runtime = json.loads(runtime_path.read_text())

            def admin(path):
                req = urllib.request.Request(f"http://127.0.0.1:{port}/_superba/v1/{path}",
                                             headers={"authorization": "Bearer " + runtime["admin_token"]})
                with urllib.request.urlopen(req, timeout=10) as resp:
                    return json.load(resp)

            def request():
                old_ids = {r["id"] for r in admin("requests")}
                body = {"model": "gpt-6.1-sol", "input": "Reply OK.", "stream": True}
                req = urllib.request.Request(f"http://127.0.0.1:{port}/backend-api/codex/responses",
                                             data=json.dumps(body).encode(),
                                             headers={"authorization": "Bearer " + token,
                                                      "content-type": "application/json",
                                                      "session-id": "rline-same-conversation"})
                with urllib.request.urlopen(req, timeout=120) as resp:
                    resp.read()
                    check("request succeeded", resp.status == 200)
                for _ in range(100):
                    rows = admin("requests")
                    new = [r for r in rows if r["id"] not in old_ids]
                    if new:
                        row = new[-1]
                        report["routes"].append({k: row.get(k) for k in
                                                 ("member_id", "line_id", "status", "http_status", "failovers")})
                        return row["line_id"]
                    time.sleep(0.05)
                raise AssertionError("request log was not updated")

            if not args.real:
                check("initial request uses cn-fast", request() == "cn-fast")
            before = admin("status")
            cli("switch", "--profile", "rline", "--line", "overseas", "--yes", "--write-only",
                "--accept-unknown-version")
            saved = json.loads((data / "profiles.json").read_text())
            profile = next(p for p in saved["profiles"] if p["id"] == "rline")
            check("overseas saved as preferred", profile["line_id"] == "overseas")
            actual = request()
            after = admin("status")
            if args.expect_bug:
                check("reproduced: request still hits cn-fast", actual == "cn-fast")
                check("reproduced: live generation unchanged", before["config_generation"] == after["config_generation"])
            else:
                check("next request uses overseas", actual == "overseas")
                check("hot reload happened", after["config_generation"] > before["config_generation"])
                check("router process unchanged", router.poll() is None and before["instance_id"] == after["instance_id"])
                check("live status reports overseas", after["members"][0]["last_route"]["line_id"] == "overseas")
                if not args.real:
                    cli("router", "line", "rline", "cn-fast")
                    check("router line changes same conversation immediately", request() == "cn-fast")
                    cli("profile", "edit", "rline", "--line", "overseas", "--line", "cn-fast")
                    check("profile order reloads immediately", request() == "overseas")
                    check("both fake origins received requests", hits == ["cn-fast", "overseas", "cn-fast", "overseas"])
        finally:
            (args.out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(f"{len(report['checks'])} checks passed; report: {args.out / 'report.json'}")


if __name__ == "__main__":
    main()
