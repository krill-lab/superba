"""Host-side control of the local Windows test VMs (stdlib only).

The VMs form a pool (`.private/winvm/pool.sh`, override the directory with
SUPERBA_WINVM_HELPERS): ids 1-3, each with its own container, disk, ports and
lock. A run takes one with `pool.sh acquire` (lock held by a detached holder
whose owner is this process, so it is freed when the harness exits) and gives it
back with `pool.sh release` (restores `clean`, leaves it stopped). Inside the run
the harness restores its own snapshots with snapshot.sh / winvm.sh `--vm <id>`.
SSH settings (port, key, user, known_hosts) are read from common.sh for the VM, so
nothing is duplicated here; secrets are never printed.
"""

import base64
import os
import shlex
import socket
import subprocess
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


class VmError(RuntimeError):
    pass


def helpers_dir():
    return Path(os.environ.get("SUPERBA_WINVM_HELPERS", REPO / ".private" / "winvm"))


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def encode_ps(script):
    return base64.b64encode(script.encode("utf-16-le")).decode()


SHELLS = {"bash", "zsh", "sh", "dash", "ksh", "mksh", "fish", "tcsh", "csh", "env", "timeout", "nohup", "setsid",
          "flock", "sudo", "su", "script", "xargs", "time", "nice", "ionice", "stdbuf", "tee", "watch"}


def caller_owner():
    """The nearest ancestor of this process that is not a shell (for an agent: its claude
    process), like pool.sh's default owner: a lock it owns outlives this command but not
    the agent session."""
    pid = os.getppid()
    while pid > 1:
        try:
            comm = Path(f"/proc/{pid}/comm").read_text().strip()
            stat = Path(f"/proc/{pid}/stat").read_text()
        except OSError:
            break
        if comm not in SHELLS:
            return pid
        pid = int(stat.rsplit(")", 1)[1].split()[1])
    return None


def psq(s):
    """A PowerShell single-quoted literal."""
    return "'" + str(s).replace("'", "''") + "'"


class Vm:
    def __init__(self, vm_id=None, log=None):
        self.helpers = helpers_dir()
        if not (self.helpers / "common.sh").is_file():
            raise VmError(f"VM helper scripts not found in {self.helpers} (set SUPERBA_WINVM_HELPERS)")
        self.log = log or (lambda msg: None)
        self.pool_held = False
        self.select(vm_id or "1")

    def select(self, vm_id):
        self.id = str(vm_id)
        vals = self._common(["WINVM_ID", "WINVM_NAME", "WINVM_SSH_PORT", "WINVM_KEY", "WINVM_USER",
                             "WINVM_SNAPSHOTS", "WINVM_STORAGE", "WINVM_LOCK"])
        self.name = vals["WINVM_NAME"]
        self.ssh_port = int(vals["WINVM_SSH_PORT"])
        self.key = vals["WINVM_KEY"]
        self.user = vals["WINVM_USER"]
        self.snapshots = Path(vals["WINVM_SNAPSHOTS"])
        self.lock_file = vals["WINVM_LOCK"]
        self.known_hosts = str(self.helpers / "known_hosts")

    # ------------------------------------------------------------- helpers
    def _env(self):
        env = dict(os.environ)
        env.pop("WINVM_INSTANCE", None)
        env["WINVM_VM"] = self.id
        return env

    def _common(self, names):
        script = f"WINVM_NO_ARGS=1; source {shlex.quote(str(self.helpers / 'common.sh'))}; " + "; ".join(
            f'printf "%s=%s\\n" {n} "${n}"' for n in names)
        out = subprocess.run(["bash", "-c", script], env=self._env(), capture_output=True,
                             text=True, check=True).stdout
        return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)

    def ssh_opts(self, batch=True):
        return ["-p", str(self.ssh_port), "-i", self.key, "-o", f"BatchMode={'yes' if batch else 'no'}",
                "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=accept-new",
                "-o", f"UserKnownHostsFile={self.known_hosts}", "-o", "LogLevel=ERROR",
                "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=8"]

    def scp_opts(self):
        o = self.ssh_opts()
        o[0] = "-P"
        return ["-O", *o]  # legacy protocol: SFTP downloads were truncated (README)

    @property
    def target(self):
        return f"{self.user}@127.0.0.1"

    def helper(self, script, *args, timeout=900, check=True):
        cmd = [str(self.helpers / script), "--vm", self.id, *map(str, args)]
        self.log(f"$ {' '.join(cmd)}")
        r = subprocess.run(cmd, env=self._env(), capture_output=True, text=True, timeout=timeout)
        self.log((r.stdout + r.stderr).strip())
        if check and r.returncode != 0:
            raise VmError(f"{script} {' '.join(map(str, args))} failed ({r.returncode}): {(r.stdout + r.stderr).strip()[-800:]}")
        return r

    # ------------------------------------------------------------- the pool
    def acquire(self, snapshot, vm_id=None, wait_s=4 * 3600, label="superba-wintest", owner=None, ttl="2h"):
        """Lock a VM of the pool (any free one unless vm_id) restored to `snapshot`."""
        cmd = [str(self.helpers / "pool.sh"), "acquire", "--snapshot", snapshot, "--wait", str(wait_s),
               "--owner", str(owner or os.getpid()), "--ttl", ttl, "--label", label]
        if vm_id:
            cmd += ["--vm", str(vm_id)]
        self.log("$ " + " ".join(cmd))
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=wait_s + 1800)
        self.log(r.stderr.strip())
        if r.returncode != 0:
            raise VmError(f"pool.sh acquire failed ({r.returncode}): {r.stderr.strip()[-600:]}")
        got = dict(l.split("=", 1) for l in r.stdout.splitlines() if "=" in l)
        self.select(got["VM"])
        self.pool_held = True
        return self.id

    def renew(self):
        """Extend the lease (pool default, 2 h from now); a long run renews before each scenario."""
        if not self.pool_held:
            return
        r = subprocess.run([str(self.helpers / "pool.sh"), "renew", self.id], capture_output=True, text=True, timeout=120)
        if r.returncode != 0:
            self.log(f"pool.sh renew {self.id}: {(r.stdout + r.stderr).strip()[-300:]}")

    def release(self):
        """Restore `clean` (left stopped) and free the lock."""
        r = subprocess.run([str(self.helpers / "pool.sh"), "release", self.id], capture_output=True,
                           text=True, timeout=1800)
        self.log((r.stdout + r.stderr).strip())
        self.pool_held = False
        if r.returncode != 0:
            raise VmError(f"pool.sh release {self.id} failed: {r.stderr.strip()[-600:]}")

    def pool_status(self):
        r = subprocess.run([str(self.helpers / "pool.sh"), "status"], capture_output=True, text=True, timeout=60)
        return r.stdout

    # ------------------------------------------------------------- lifecycle
    def running(self):
        r = subprocess.run(["docker", "inspect", "-f", "{{.State.Running}}", self.name],
                           capture_output=True, text=True)
        return r.stdout.strip() == "true"

    def snapshot_exists(self, name):
        return (self.snapshots / name / "data.qcow2").is_file()

    def snapshot_created(self, name):
        info = self.snapshots / name / "info.txt"
        if not info.is_file():
            return None
        for line in info.read_text(errors="replace").splitlines():
            if line.startswith("created: "):
                return line.split(": ", 1)[1].strip()
        return None

    def restore(self, name):
        if not self.snapshot_exists(name):
            raise VmError(f"snapshot {name!r} missing (see `run.py prep-snapshot`)")
        self.helper("snapshot.sh", "restore", name, timeout=1800)

    def start(self):
        self.helper("winvm.sh", "start", timeout=600)
        self.wait_ssh(300)
        self.wait_desktop(240)

    def stop(self):
        self.helper("winvm.sh", "stop", timeout=300)

    def create_snapshot(self, name):
        self.helper("snapshot.sh", "create", name, timeout=1800)

    def wait_ssh(self, timeout=300):
        end = time.time() + timeout
        while time.time() < end:
            r = subprocess.run(["ssh", *self.ssh_opts(), "-o", "ConnectTimeout=5", self.target, "exit 0"],
                               capture_output=True, timeout=30)
            if r.returncode == 0:
                return
            time.sleep(4)
        raise VmError("guest SSH not reachable")

    def wait_desktop(self, timeout=180):
        """Until explorer runs in an active console session (auto-logon done)."""
        end = time.time() + timeout
        while time.time() < end:
            r = self.ps("$e = Get-Process explorer -ErrorAction SilentlyContinue | Select-Object -First 1;"
                        " if ($e) { $e.SessionId } else { 'none' }", check=False, timeout=60)
            if r.returncode == 0 and r.stdout.strip().isdigit():
                return int(r.stdout.strip())
            time.sleep(4)
        raise VmError("no interactive desktop session (auto-logon did not finish)")

    def reboot(self, timeout=420):
        self.ps("shutdown.exe /r /t 2 /f", check=False)
        time.sleep(25)
        self.wait_ssh(timeout)
        return self.wait_desktop(timeout)

    # ------------------------------------------------------------- commands
    def ps(self, script, timeout=300, check=True, user=None):
        """Run a PowerShell script in the guest over SSH (service session, the
        admin token of the VM user). Long scripts go through a temporary file."""
        full = "$ProgressPreference = 'SilentlyContinue'\n" + script
        enc = encode_ps(full)
        target = f"{user}@127.0.0.1" if user else self.target
        if len(enc) < 7000:
            cmd = ["ssh", *self.ssh_opts(), target,
                   f"powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand {enc}"]
            r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout,
                               encoding="utf-8", errors="replace")
        else:
            tmp = REPO / "e2e" / ".work" / "windows"
            tmp.mkdir(parents=True, exist_ok=True)
            local = tmp / f"ps-{os.getpid()}-{time.time_ns()}.ps1"
            local.write_text("\ufeff" + full, encoding="utf-8")
            try:
                remote = f"C:/Users/Public/wintest-{local.name}"
                self.scp_to(local, remote, user=user)
                r = subprocess.run(["ssh", *self.ssh_opts(), target,
                                    f"powershell.exe -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File {remote};"
                                    f" $c = $LASTEXITCODE; Remove-Item -Force {remote}; exit $c"],
                                   capture_output=True, text=True, timeout=timeout, encoding="utf-8", errors="replace")
            finally:
                local.unlink(missing_ok=True)
        if check and r.returncode != 0:
            raise VmError(f"guest PowerShell failed ({r.returncode}): {(r.stdout + r.stderr).strip()[-1500:]}")
        return r

    def scp_to(self, local, remote, user=None):
        target = f"{user}@127.0.0.1" if user else self.target
        subprocess.run(["scp", *self.scp_opts(), "-r", str(local), f"{target}:{remote}"],
                       check=True, capture_output=True, timeout=1800)

    def scp_from(self, remote, local):
        Path(local).parent.mkdir(parents=True, exist_ok=True)
        r = subprocess.run(["scp", *self.scp_opts(), "-r", f"{self.target}:{remote}", str(local)],
                           capture_output=True, text=True, timeout=1800)
        if r.returncode != 0:
            raise VmError(f"scp from {remote} failed: {r.stderr.strip()[-400:]}")

    def tunnel(self, local=(), remote=(), detach=True):
        """One `ssh -N` carrying local forwards [(host_port, guest_port)] and
        remote forwards [(guest_port, host_port)]; returns the Popen. Its stderr
        goes to a file, so it survives the CLI that started it (run.py session)."""
        args = ["ssh", *self.ssh_opts(), "-N", "-o", "ExitOnForwardFailure=yes"]
        for hp, gp in local:
            args += ["-L", f"127.0.0.1:{hp}:127.0.0.1:{gp}"]
        for gp, hp in remote:
            args += ["-R", f"127.0.0.1:{gp}:127.0.0.1:{hp}"]
        args.append(self.target)
        work = REPO / "e2e" / ".work" / "windows"
        work.mkdir(parents=True, exist_ok=True)
        errf = work / f"tunnel-{time.time_ns()}.log"
        with open(errf, "w") as err:
            p = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=err, start_new_session=detach)
        time.sleep(1.5)
        if p.poll() is not None:
            raise VmError(f"ssh tunnel failed: {errf.read_text(errors='replace')[-400:]}")
        return p

    def screen(self, out):
        """Raw framebuffer through the QEMU monitor (works without the agent)."""
        r = self.helper("winvm.sh", "screen", out, check=False, timeout=60)
        return out if r.returncode == 0 else None
