"""PID 1 of the e2e run's user + PID namespace.

    nsinit.py -- CMD [ARG...]     run CMD, reap every orphan, exit with CMD's code
    nsinit.py --hold READY_FILE   stay up (step mode) until SIGTERM; writes READY_FILE

The harness runs the desktop app, `superba`, `spb`, the router and the forwarders
inside an unprivileged user + PID namespace (`unshare --user --map-current-user
--pid --fork --mount-proc`): desktop apps and Codex processes of other sessions on
this machine are invisible there, so Superba never lists, probes or signals them,
and every process the run started dies with this init. The uid stays the same
(`--map-current-user`, not `--map-root-user`): Superba never gives a root-owned app
a debugging port, so a mapped root would disable hot switching.

As PID 1 this process must reap orphans (the app and the router supervisor are
detached from their launchers); otherwise exited processes would stay zombies and
look alive to Superba.
"""

import os
import signal
import subprocess
import sys
import time


def main(argv):
    stop = []
    signal.signal(signal.SIGTERM, lambda *_: stop.append(1))
    signal.signal(signal.SIGINT, lambda *_: stop.append(1))
    if len(argv) == 3 and argv[1] == "--hold":
        with open(argv[2], "w") as f:
            f.write(str(os.getpid()))
        while not stop:
            reap(block=False)
            time.sleep(0.2)
        return 0
    if len(argv) < 3 or argv[1] != "--":
        sys.exit(__doc__)
    child = subprocess.Popen(argv[2:])
    code = None
    while code is None:
        if stop:
            child.send_signal(signal.SIGTERM)
            stop.clear()
        for pid, status in reap(block=True):
            if pid == child.pid:
                code = os.waitstatus_to_exitcode(status)
    return code if code >= 0 else 128 - code


def reap(block):
    """Reap exited children; returns [(pid, status)]."""
    out = []
    flags = 0 if block else os.WNOHANG
    while True:
        try:
            pid, status = os.waitpid(-1, flags)
        except ChildProcessError:
            if block:
                time.sleep(0.2)
            return out
        except InterruptedError:
            continue
        if pid == 0:
            return out
        out.append((pid, status))
        flags = os.WNOHANG


if __name__ == "__main__":
    sys.exit(main(sys.argv))
