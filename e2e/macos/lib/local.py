"""Same guest protocol on a disposable local Mac; never invokes SSH."""

import os
import platform
import shutil
import subprocess
import time
from pathlib import Path

from .vm import Blocked, Vm, VmError


class LocalLease:
    """The kernel releases this lock even when a hosted job is interrupted."""

    def __init__(self, path, wait=0):
        self.path, self.wait = Path(path), wait
        self.stream = None

    def __enter__(self):
        import fcntl

        deadline = time.monotonic() + self.wait
        # Never follow a replacement symlink or delete another holder's lock.
        descriptor = os.open(self.path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
        self.stream = os.fdopen(descriptor, "r+")
        try:
            while True:
                try:
                    fcntl.flock(self.stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    return self
                except BlockingIOError:
                    if time.monotonic() >= deadline:
                        raise Blocked("Local e2e lease is held; wait for its owner to exit") from None
                    time.sleep(min(1, max(0.1, deadline - time.monotonic())))
        except BaseException:
            self.stream.close()
            self.stream = None
            raise

    def __exit__(self, *_):
        if self.stream is not None:
            self.stream.close()
            self.stream = None


class Local(Vm):
    local = True

    def __init__(self, root):
        path = Path(root).absolute()
        if path != path.resolve() or path in (Path("/"), Path.home()) or len(path.parts) < 3:
            raise VmError("Local root must be a dedicated absolute directory without symlinks")
        marker = path / ".superba-e2e-local"
        if path.exists() and not marker.is_file():
            raise VmError("Local root already exists without an ownership marker")
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        marker.touch(mode=0o600)
        (path / "tmp").mkdir(exist_ok=True, mode=0o700)
        self.root = str(path)

    def shell(self, command, *, stdin=None, timeout=120, check=True):
        # Do not forward CI tokens, BASH_ENV, proxy credentials or a caller's rc
        # files. guest.environment supplies a separate HOME to every test app.
        env = {"HOME": str(Path.home()), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
               "TMPDIR": self.root + "/tmp", "LANG": "en_US.UTF-8"}
        try:
            result = subprocess.run(["/bin/bash", "--noprofile", "--norc", "-c", command],
                                    input=stdin, text=True, capture_output=True,
                                    env=env, timeout=timeout)
        except subprocess.TimeoutExpired:
            raise VmError("Local command timed out") from None
        if check and result.returncode:
            raise VmError(f"Local command exited {result.returncode}")
        return result

    def owned(self, path):
        path = Path(path)
        if not path.resolve().is_relative_to(Path(self.root)):
            raise VmError("Path escapes the local test root")
        return path

    def put(self, source, destination):
        destination = self.owned(destination)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(0o600)

    def get(self, source, destination):
        shutil.copyfile(self.owned(source), destination)

    def wait_reboot(self, *_args, **_kwargs):
        raise VmError("Local mode never reboots; use the simulated login scenario")


def configured_local(root):
    if platform.system() != "Darwin":
        raise VmError("Local e2e execution requires macOS")
    if not root or not Path(root).is_absolute():
        raise VmError("--local-root must name a dedicated absolute directory")
    return Local(root), LocalLease(Path(root) / "LOCK")
