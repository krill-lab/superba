"""Client of the desktop agent (guest/WtAgent.cs) and its lifecycle.

The agent runs in the interactive desktop session of the guest (UI Automation,
input, screenshots, processes started with the user's unelevated token) and
listens on the guest's 127.0.0.1; the host reaches it through an SSH local
forward. Every request carries a random token; nothing of a request is logged
by the agent except the command name.
"""

import json
import os
import secrets
import socket
import subprocess
import time
from pathlib import Path

from vm import Vm, VmError, free_port, psq

HERE = Path(__file__).resolve().parent
GUEST_SRC = HERE.parent / "guest"
GUEST_ROOT = r"C:\Users\Public\wintest"
AGENT_DIR = GUEST_ROOT + r"\agent"


class AgentError(RuntimeError):
    pass


def list2cmdline(args):
    return subprocess.list2cmdline([str(a) for a in args])


class Agent:
    def __init__(self, vm: Vm, log=None, user=None, run_level="Limited"):
        self.vm = vm
        self.log = log or (lambda m: None)
        self.user = user  # None: the VM's own user; else a local account name
        self.run_level = run_level
        self.token = secrets.token_hex(16)
        self.guest_port = 47000 + secrets.randbelow(900)
        self.host_port = None
        self.tunnel = None
        self.tunnel_pid = None

    # ------------------------------------------------------------- lifecycle
    def start(self):
        vm = self.vm
        # A previous agent (same task name) would make the new start a no-op.
        vm.ps("Stop-ScheduledTask -TaskName superba-wintest-agent -ErrorAction SilentlyContinue;"
              " Unregister-ScheduledTask -TaskName superba-wintest-agent -Confirm:$false -ErrorAction SilentlyContinue;"
              " Get-CimInstance Win32_Process -Filter \"Name='powershell.exe'\" | Where-Object { $_.CommandLine -like '*wintest*agent.ps1*' }"
              " | ForEach-Object { Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue };"
              f" New-Item -ItemType Directory -Force {psq(AGENT_DIR)} | Out-Null;"
              f" Get-ChildItem {psq(AGENT_DIR)} | Remove-Item -Recurse -Force")
        for name in ("WtAgent.cs", "agent.ps1", "start-agent.ps1"):
            vm.scp_to(GUEST_SRC / name, AGENT_DIR.replace("\\", "/") + "/" + name)
        vm.ps(f"Set-Content -NoNewline -Encoding ascii -Path {psq(AGENT_DIR + chr(92) + 'agent.token')} -Value {psq(self.token)}")
        user = f"{self.user}" if self.user else "$env:COMPUTERNAME\\$env:USERNAME"
        if self.user and "\\" not in self.user:
            user = f"$env:COMPUTERNAME\\{self.user}"
        r = vm.ps(f"& {psq(AGENT_DIR + chr(92) + 'start-agent.ps1')} -Port {self.guest_port} -Root {psq(AGENT_DIR)}"
                  f" -User \"{user}\" -RunLevel {self.run_level}", timeout=180, check=False)
        if r.returncode != 0 or "agent pid" not in r.stdout:
            raise AgentError(f"desktop agent did not start: {(r.stdout + r.stderr).strip()[-1200:]}")
        self.host_port = free_port()
        self.tunnel = vm.tunnel(local=[(self.host_port, self.guest_port)])
        info = self.call("ping", timeout=30)
        self.log(f"agent up: {info}")
        return info

    def to_state(self):
        return {"token": self.token, "guest_port": self.guest_port, "host_port": self.host_port,
                "tunnel_pid": self.tunnel.pid if self.tunnel else self.tunnel_pid,
                "user": self.user, "run_level": self.run_level}

    @classmethod
    def from_state(cls, vm, st, log=None):
        a = cls(vm, log=log, user=st.get("user"), run_level=st.get("run_level", "Limited"))
        a.token, a.guest_port, a.host_port = st["token"], st["guest_port"], st["host_port"]
        a.tunnel_pid = st.get("tunnel_pid")
        if not a.alive():
            a.tunnel = vm.tunnel(local=[(a.host_port, a.guest_port)])
            a.tunnel_pid = None
        return a

    def alive(self):
        try:
            self.call("ping", timeout=10)
            return True
        except Exception:
            return False

    def stop(self, remove=True):
        try:
            if self.host_port:
                self.call("quit", timeout=10)
        except Exception:
            pass
        if self.tunnel:
            self.tunnel.terminate()
            try:
                self.tunnel.wait(5)
            except subprocess.TimeoutExpired:
                self.tunnel.kill()
            self.tunnel = None
        if self.tunnel_pid:
            try:
                os.kill(self.tunnel_pid, 15)
            except ProcessLookupError:
                pass
            self.tunnel_pid = None
        if remove:
            try:
                self.vm.ps("Unregister-ScheduledTask -TaskName superba-wintest-agent -Confirm:$false -ErrorAction SilentlyContinue;"
                           f" Start-Sleep 1; Remove-Item -Recurse -Force -ErrorAction SilentlyContinue {psq(AGENT_DIR)}",
                           check=False, timeout=60)
            except Exception:
                pass

    def fetch_log(self, dest):
        try:
            self.vm.scp_from(AGENT_DIR.replace("\\", "/") + "/agent.log", dest)
        except VmError:
            pass

    # ------------------------------------------------------------- protocol
    def call(self, cmd, timeout=120, **args):
        req = json.dumps({"token": self.token, "cmd": cmd, "args": args}) + "\n"
        try:
            with socket.create_connection(("127.0.0.1", self.host_port), timeout=timeout) as s:
                s.settimeout(timeout)
                s.sendall(req.encode())
                buf = bytearray()
                while not buf.endswith(b"\n"):
                    d = s.recv(1 << 16)
                    if not d:
                        break
                    buf += d
        except (OSError, socket.timeout) as e:
            raise AgentError(f"agent {cmd}: {type(e).__name__}: {e}") from None
        if not buf:
            raise AgentError(f"agent {cmd}: no answer (agent gone?)")
        resp = json.loads(buf.decode("utf-8"))
        if not resp.get("ok"):
            raise AgentError(f"agent {cmd}: {resp.get('error')}")
        return resp.get("result")

    # ------------------------------------------------------------- conveniences
    def exec(self, file, args=(), env=None, unset=(), cwd=None, timeout=120, stdin=None, fresh_env=True):
        a = {"file": file, "args": list2cmdline(args) if not isinstance(args, str) else args,
             "timeout_ms": int(timeout * 1000), "fresh_env": fresh_env, "unset": list(unset)}
        if env:
            a["env"] = env
        if cwd:
            a["cwd"] = cwd
        if stdin is not None:
            a["stdin"] = stdin
        return self.call("exec", timeout=timeout + 30, **a)

    def start_proc(self, file, args=(), env=None, unset=(), cwd=None, log=None, fresh_env=True):
        a = {"file": file, "args": list2cmdline(args) if not isinstance(args, str) else args,
             "fresh_env": fresh_env, "unset": list(unset)}
        if env:
            a["env"] = env
        if cwd:
            a["cwd"] = cwd
        if log:
            a["log"] = log
        return self.call("start", timeout=60, **a)["pid"]

    def procs(self, name_re=None):
        return self.call("procs", timeout=60, **({"name_re": name_re} if name_re else {}))

    def windows(self, **sel):
        return self.call("windows", **sel)

    def wait_window(self, timeout=60, **sel):
        end = time.time() + timeout
        while time.time() < end:
            ws = self.windows(**sel)
            ws = [w for w in ws if w["title"] and w["rect"][2] > 200]
            if ws:
                return ws[0]
            time.sleep(0.5)
        return None

    def shot(self, guest_path, **sel):
        return self.call("shot", path=guest_path, **sel)
