"""Host-side network fixtures for the Windows VM.

- A throwaway CA + 127.0.0.1 server certificate (e2e/lib/mkcert.sh); the CA is
  imported into the guest's LocalMachine\\Root, which Superba (rustls platform
  verifier), the desktop app (Chromium) and Codex use on Windows.
- `mock`: crates/superba-gui/tools/screenshots/mockrelay.py on host loopback
  (whoami, accounts/check, models, billing, usage, Responses that answer "pong"),
  with a random fake key.
- `real`: logging line forwarders (e2e/lib/linefwd.py, needs aiohttp) in front of
  the test relay named by SUPERBA_E2E_ENV (values never printed).
- Every host port is reverse-forwarded into the guest at the same port number
  (`ssh -R 127.0.0.1:P:127.0.0.1:P`), so the guest reaches the lines as
  https://127.0.0.1:P like the Linux harness's forwarders.
- The built-in Krill production origins (superba-core catalog.rs) are pointed at
  0.0.0.0 in the guest's hosts file, so a test key never reaches a production line;
  the test lines are added to the Krill preset with SUPERBA_TEST_LINES.
"""

import json
import os
import re
import secrets
import subprocess
import sys
import time
from pathlib import Path

from vm import REPO, free_port, psq

MKCERT = REPO / "e2e" / "lib" / "mkcert.sh"
LINEFWD = REPO / "e2e" / "lib" / "linefwd.py"
MOCKRELAY = REPO / "crates" / "superba-gui" / "tools" / "screenshots" / "mockrelay.py"
CATALOG_RS = REPO / "crates" / "superba-core" / "src" / "catalog.rs"
GUEST_TLS = r"C:\Users\Public\wintest\tls"


def read_env_file(path):
    """Only the three keys e2e/README.md documents; values stay in memory."""
    keys = {"SUPERBA_E2E_UPSTREAM", "SUPERBA_E2E_KEY", "SUPERBA_E2E_APP_DIR"}
    out = {}
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        k, v = line.split("=", 1)
        if k.strip() in keys:
            out[k.strip()] = v.strip().strip('"').strip("'")
    return out


def _pid_alive(pid):
    if not pid:
        return False
    try:
        os.kill(pid, 0)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def production_hosts():
    hosts = []
    for m in re.finditer(r'KRILL_ORIGIN_\w+:\s*&str\s*=\s*"https://([^"/]+)', CATALOG_RS.read_text()):
        hosts.append(m.group(1))
    return sorted(set(hosts))


class Net:
    """mode: 'mock' or 'real'. Lines are named a, b, c (mock) / a, b (real)."""

    def __init__(self, run, mode):
        self.run = run
        self.mode = mode
        self.dir = run.root / "net"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.procs = []
        self.ports = {}
        self.tunnel = None
        self.key = None
        self.env = {}
        self.pids = []          # detached processes of an attached session
        self.tunnel_pid = None

    # ------------------------------------------------------------- host side
    def start(self):
        tls = self.dir / "tls"
        subprocess.run(["bash", str(MKCERT), str(tls)], check=True)
        self.tls = tls
        if self.mode == "mock":
            self._start_mock()
        else:
            self._start_real()
        self.tunnel = self.run.vm.tunnel(remote=[(p, p) for p in self.ports.values()], detach=True)
        self.run.log(f"net {self.mode}: lines {self.ports} reverse-forwarded into the guest")

    def _start_mock(self):
        self.key = "nb_mock_" + secrets.token_hex(12)
        ports_file = self.dir / "mock-ports.json"
        ports_file.unlink(missing_ok=True)
        self.faults = self.dir / "mock-faults.json"
        self.faults.write_text("{}")
        log = open(self.dir / "mockrelay.out", "w")
        p = subprocess.Popen([sys.executable, "-I", str(MOCKRELAY), "--lines", "a,b,c",
                              "--ports-file", str(ports_file), "--cert", str(self.tls / "server.pem"),
                              "--key", str(self.tls / "server.key"), "--api-key", self.key,
                              "--latency", "a=30,b=60,c=90",
                              # every mock line blocks the PAT endpoints like an edge would, so
                              # Automatic resolves to the API key (the desktop app cannot finish a
                              # PAT sign-in against the mock; PAT runs use --net real)
                              "--edge-block", "a", "--edge-block", "b", "--edge-block", "c",
                              "--log", str(self.dir / "mockrelay.jsonl"), "--fault-file", str(self.faults)],
                             stdout=log, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                             start_new_session=True)
        self.procs.append(p)
        for _ in range(100):
            if ports_file.exists():
                self.ports = {k: int(v) for k, v in json.loads(ports_file.read_text()).items()}
                return
            if p.poll() is not None:
                break
            time.sleep(0.1)
        raise RuntimeError(f"mock relay did not start (see {self.dir / 'mockrelay.out'})")

    def _start_real(self):
        envf = os.environ.get("SUPERBA_E2E_ENV")
        if not envf:
            raise RuntimeError("--net real needs SUPERBA_E2E_ENV (see e2e/README.md)")
        e = read_env_file(envf)
        if not e.get("SUPERBA_E2E_UPSTREAM") or not e.get("SUPERBA_E2E_KEY"):
            raise RuntimeError("SUPERBA_E2E_UPSTREAM / SUPERBA_E2E_KEY missing in SUPERBA_E2E_ENV")
        try:
            import aiohttp  # noqa: F401  (the forwarder's dependency, like e2e/)
        except ImportError:
            raise RuntimeError("--net real: the line forwarders need python3 aiohttp (pip install aiohttp zstandard)")
        self.key = e["SUPERBA_E2E_KEY"]
        upstream = e["SUPERBA_E2E_UPSTREAM"].rstrip("/")
        for line in ("a", "b"):
            port = free_port()
            fault = self.dir / f"fault-{line}"
            fault.write_text("")
            out = open(self.dir / f"linefwd-{line}.out", "w")
            # The upstream origin goes to the forwarder through its argv only
            # (as in e2e/lib/infra.py); it is never logged by the harness.
            p = subprocess.Popen([sys.executable, str(LINEFWD), line, str(port), upstream,
                                  str(self.dir / f"line-{line}.jsonl"), "--tls", str(self.tls / "server.pem"),
                                  str(self.tls / "server.key"), "--fault-file", str(fault)],
                                 stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                 start_new_session=True)
            self.procs.append(p)
            self.ports[line] = port
        time.sleep(1.5)
        for p in self.procs:
            if p.poll() is not None:
                raise RuntimeError(f"a line forwarder exited (see {self.dir}/linefwd-*.out)")

    def retunnel(self):
        for p in ([self.tunnel] if self.tunnel else []):
            p.terminate()
        if self.tunnel_pid:
            try:
                os.kill(self.tunnel_pid, 15)
            except ProcessLookupError:
                pass
        self.tunnel_pid = None
        self.tunnel = self.run.vm.tunnel(remote=[(p, p) for p in self.ports.values()])

    def stop(self):
        pids = [p.pid for p in self.procs] + ([self.tunnel.pid] if self.tunnel else []) + self.pids
        for pid in pids:
            try:
                os.kill(pid, 15)
            except ProcessLookupError:
                pass
        for p in self.procs + ([self.tunnel] if self.tunnel else []):
            try:
                p.wait(5)
            except subprocess.TimeoutExpired:
                p.kill()
        self.procs, self.pids = [], []
        self.tunnel = None

    def alive(self):
        procs = all(p.poll() is None for p in self.procs) and all(_pid_alive(p) for p in self.pids)
        tun = (self.tunnel.poll() is None) if self.tunnel else _pid_alive(self.tunnel_pid)
        return procs and tun

    # ------------------------------------------------------------- attach (run.py session)
    def to_state(self):
        return {"mode": self.mode, "ports": self.ports, "pids": [p.pid for p in self.procs] + self.pids,
                "tunnel_pid": self.tunnel.pid if self.tunnel else self.tunnel_pid,
                "mock_key": self.key if self.mode == "mock" else None}

    @classmethod
    def from_state(cls, run, st):
        n = cls(run, st["mode"])
        n.ports = {k: int(v) for k, v in st["ports"].items()}
        n.pids = list(st["pids"])
        n.tunnel_pid = st.get("tunnel_pid")
        n.tls = n.dir.parent.parent / "dummy"
        if st["mode"] == "mock":
            n.key = st["mock_key"]
        else:
            n.key = read_env_file(os.environ["SUPERBA_E2E_ENV"])["SUPERBA_E2E_KEY"]
        if not _pid_alive(n.tunnel_pid):
            n.tunnel = run.vm.tunnel(remote=[(p, p) for p in n.ports.values()])
            n.tunnel_pid = None
        return n

    # ------------------------------------------------------------- guest side
    def line_url(self, line):
        return f"https://127.0.0.1:{self.ports[line]}"

    def line_id(self, line):
        return f"e2e-{line}"

    def test_lines_env(self):
        return ",".join(f"{self.line_id(l)}={self.line_url(l)}" for l in sorted(self.ports))

    def prepare_guest(self):
        """CA into LocalMachine\\Root, production Krill hosts blocked, harness
        variables in the user environment (so Start-menu launches see them too)."""
        vm = self.run.vm
        vm.ps(f"New-Item -ItemType Directory -Force {psq(GUEST_TLS)} | Out-Null")
        vm.scp_to(self.tls / "ca.pem", GUEST_TLS.replace("\\", "/") + "/ca.pem")
        hosts = production_hosts()
        lines = "\n".join(f"0.0.0.0 {h}" for h in hosts)
        vm.ps(f"""
$c = Import-Certificate -FilePath {psq(GUEST_TLS + chr(92) + 'ca.pem')} -CertStoreLocation Cert:\\LocalMachine\\Root
'CA ' + $c.Thumbprint
$h = "$env:SystemRoot\\System32\\drivers\\etc\\hosts"
$t = Get-Content -Raw $h
if ($t -notmatch 'superba-wintest') {{ Add-Content -Path $h -Value "`r`n# superba-wintest: Krill production lines blocked for the test run`r`n{lines.replace(chr(10), '`r`n')}" }}
ipconfig /flushdns | Out-Null
[Environment]::SetEnvironmentVariable('SUPERBA_TEST_LINES', {psq(self.test_lines_env())}, 'User')
""")
        self.run.harness_env["SUPERBA_TEST_LINES"] = self.test_lines_env()
        self.run.log(f"guest: CA imported, {len(hosts)} production Krill hosts blocked, SUPERBA_TEST_LINES set")

    # ------------------------------------------------------------- request logs
    def entries(self, since=0.0):
        """Requests seen by the relay side, newest last: dicts with line, method,
        path, status (+ forwarder fields in real mode)."""
        out = []
        if self.mode == "mock":
            f = self.dir / "mockrelay.jsonl"
            if f.exists():
                for raw in f.read_text().splitlines():
                    try:
                        e = json.loads(raw)
                    except ValueError:
                        continue
                    ts = e.get("ts") or e.get("time") or 0
                    if isinstance(ts, str):
                        ts = 0
                    if ts and ts < since:
                        continue
                    out.append(e)
        else:
            for line in self.ports:
                f = self.dir / f"line-{line}.jsonl"
                if not f.exists():
                    continue
                for raw in f.read_text().splitlines():
                    try:
                        e = json.loads(raw)
                    except ValueError:
                        continue
                    if e.get("ts", 0) < since:
                        continue
                    e.setdefault("line", line)
                    out.append(e)
            out.sort(key=lambda e: e.get("ts", 0))
        return out

    def responses_posts(self, since=0.0):
        return [e for e in self.entries(since)
                if e.get("method") == "POST" and "/responses" in e.get("path", "")]

    def set_fault(self, line, status=None):
        if self.mode == "real":
            (self.dir / f"fault-{line}").write_text(str(status) if status else "")
        else:
            raise NotImplementedError("mock line faults: use mock-faults.json per path")
