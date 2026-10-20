"""SSH transport and exclusive local lease. No connection details in the repo."""

import hashlib
import json
import os
import shlex
import subprocess
import tempfile
import time
import uuid
from datetime import datetime, timezone
from pathlib import Path


class VmError(RuntimeError):
    pass


class Blocked(RuntimeError):
    pass


class Lease:
    def __init__(self, path, label="mx", wait=0):
        self.path = Path(path)
        self.wait = wait
        self.label = label
        self.owner = None
        self.inode = None

    def __enter__(self):
        deadline = time.monotonic() + self.wait
        self.path.parent.mkdir(parents=True, exist_ok=True)
        while True:
            try:
                with self.path.open("x") as stream:
                    self.owner = f"{self.label} {datetime.now(timezone.utc).isoformat()} {uuid.uuid4().hex}\n"
                    stream.write(self.owner)
                    self.inode = os.fstat(stream.fileno()).st_ino
                return self
            except FileExistsError:
                if time.monotonic() >= deadline:
                    raise Blocked("VM lease is held; wait for its owner to release it") from None
                time.sleep(min(5, max(0.1, deadline - time.monotonic())))

    def __exit__(self, *_):
        # Never remove a lease that has been replaced by another worker.
        if self.inode is not None and self.path.exists():
            if self.path.stat().st_ino == self.inode and self.path.read_text() == self.owner:
                self.path.unlink()


class Vm:
    def __init__(self, helper, root="/Users/Shared/superba-e2e"):
        self.helper = str(Path(helper).resolve())
        if not Path(self.helper).is_file():
            raise VmError("SSH helper not found")
        if not root.startswith("/Users/Shared/") or ".." in Path(root).parts:
            raise VmError("Guest root must be an absolute directory under /Users/Shared")
        self.root = root.rstrip("/")

    def shell(self, command, *, stdin=None, timeout=120, check=True):
        try:
            result = subprocess.run([self.helper, command], input=stdin, text=True,
                                    capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            # TimeoutExpired includes the command/input; never propagate it into reports.
            raise VmError("SSH command timed out") from None
        if check and result.returncode:
            raise VmError(f"SSH command exited {result.returncode}")
        return result

    def rpc(self, operation, **args):
        timeout = args.pop("rpc_timeout", 180)
        command = shlex.join(["/usr/bin/python3", self.root + "/guest.py"])
        result = self.shell(command, stdin=json.dumps({"op": operation, "args": args}),
                            timeout=timeout, check=False)
        if result.returncode == 255:
            # The request may have reached the guest. Retrying a mutation could
            # install, switch or reboot twice; recover the recorded session first.
            raise VmError("SSH connection failed (exit 255); guest completion is unknown. "
                          "Restore connectivity and clean up the recorded session before retrying")
        try:
            response = json.loads(result.stdout)
        except ValueError:
            raise VmError(f"Guest helper returned invalid JSON (exit {result.returncode})") from None
        if not response.get("ok"):
            if response.get("code") == "BLOCKED":
                raise Blocked(response.get("error", "Guest prerequisite is missing"))
            raise VmError(response.get("error", "Guest operation failed"))
        return response["result"]

    def put(self, source, destination):
        # A helper needs no scp identity or destination parser; bytes travel on stdin.
        code = ("import sys,pathlib,os; p=pathlib.Path(sys.argv[1]); "
                "p.parent.mkdir(parents=True,exist_ok=True); "
                "p.write_bytes(sys.stdin.buffer.read()); os.chmod(p,0o600)")
        with Path(source).open("rb") as stream:
            result = subprocess.run([self.helper, shlex.join(["/usr/bin/python3", "-c", code, destination])],
                                    stdin=stream, capture_output=True, timeout=900)
        if result.returncode:
            raise VmError("Upload failed")

    def get(self, source, destination):
        # Stream binary artifacts instead of buffering base64 in both processes.
        # Large DMGs over SSH can take longer than a UI command's timeout.
        destination = Path(destination)
        temporary = None
        try:
            with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
                temporary = Path(stream.name)
                result = subprocess.run([self.helper, shlex.join(["/bin/cat", source])],
                                        stdout=stream, stderr=subprocess.PIPE, timeout=900)
            if result.returncode:
                raise VmError("Download failed")
            temporary.replace(destination)
        except subprocess.TimeoutExpired:
            raise VmError("Download timed out") from None
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)

    def cache_dmg(self, source, remote_source=None):
        """Reuse an immutable DMG across fresh test HOMEs, checking its digest."""
        with Path(source).open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        cache = self.root + "/artifacts/" + digest + ".dmg"

        def valid(path):
            result = self.shell(shlex.join(["/usr/bin/shasum", "-a", "256", path]), check=False)
            return result.returncode == 0 and result.stdout.split(maxsplit=1)[:1] == [digest]

        if not valid(cache):
            partial = cache + ".part"
            if remote_source is not None:
                if not valid(remote_source):
                    raise VmError("Guest DMG digest does not match the downloaded artifact")
                self.shell("mkdir -p " + shlex.quote(self.root + "/artifacts"))
                self.shell(shlex.join(["/bin/cp", remote_source, partial]))
            else:
                self.put(source, partial)
            if not valid(partial):
                self.shell(shlex.join(["/bin/rm", "-f", partial]))
                raise VmError("Uploaded DMG digest mismatch")
            self.shell(shlex.join(["/bin/mv", "-f", partial, cache]))
        return cache

    def wait_reboot(self, before, timeout=240):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                result = self.shell("/usr/sbin/sysctl -n kern.boottime", timeout=10, check=False)
                if result.returncode == 0 and result.stdout.strip() != before:
                    # Wait for auto-login, not just sshd.
                    console = self.shell("/usr/bin/stat -f %Su /dev/console", timeout=10).stdout.strip()
                    if console not in ("root", "loginwindow", ""):
                        return result.stdout.strip()
            except (VmError, OSError):
                pass
            time.sleep(5)
        raise VmError("Reboot or GUI auto-login did not complete within the deadline")

    def prepare(self):
        here = Path(__file__).resolve().parents[1]
        self.put(here / "guest" / "guest.py", self.root + "/guest.py")
        self.put(here / "guest" / "fixture.py", self.root + "/fixture.py")
        self.put(here / "guest" / "line_proxy.py", self.root + "/line_proxy.py")
        self.put(here / "guest" / "desktop.py", self.root + "/desktop.py")
        self.put(here / "agent" / "MacAgent.swift", self.root + "/agent/MacAgent.swift")
        root = shlex.quote(self.root + "/agent")
        # Preserve the existing executable when unchanged: replacing it can invalidate TCC consent.
        command = f"cd {root} && /bin/bash -s"
        script = '''set -euo pipefail
digest=$(/usr/bin/shasum -a 256 MacAgent.swift | /usr/bin/cut -d ' ' -f 1)
if [[ ! -x MacAgent || ! -f source.sha256 || $(cat source.sha256) != "$digest" ]]; then
    /usr/bin/xcrun swiftc -parse-as-library -swift-version 5 -O MacAgent.swift -o MacAgent.new \\
        -framework AppKit -framework ApplicationServices -framework ScreenCaptureKit
    /usr/bin/codesign --force --sign - --identifier com.superba.e2e.MacAgent MacAgent.new
    mv MacAgent.new MacAgent
    printf '%s' "$digest" > source.sha256
fi
'''
        result = self.shell(command, stdin=script, timeout=300, check=False)
        if result.returncode:
            # Compiler diagnostics only; this command never has secrets in scope.
            raise VmError("MacAgent compilation failed:\n" + result.stderr[-8000:])
        return {"source_sha256": hashlib.sha256((here / "agent/MacAgent.swift").read_bytes()).hexdigest(),
                "executable": self.root + "/agent/MacAgent", "exit": result.returncode,
                "diagnostics": result.stderr}


def configured_vm():
    helper = os.environ.get("SUPERBA_MACVM_SSH")
    lock = os.environ.get("SUPERBA_MACVM_LOCK")
    if not helper or not lock:
        raise VmError("Set SUPERBA_MACVM_SSH and SUPERBA_MACVM_LOCK")
    return Vm(helper, os.environ.get("SUPERBA_MACVM_ROOT", "/Users/Shared/superba-e2e")), Lease(lock)
