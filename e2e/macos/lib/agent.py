"""WtAgent command envelope over encrypted SSH stdin (one-shot MacAgent)."""

import json
import shlex
import time

from .vm import Blocked


class AgentError(RuntimeError):
    pass


class Agent:
    def __init__(self, vm):
        self.vm = vm
        self.executable = vm.root + "/agent/MacAgent"

    def call(self, cmd, timeout=120, **args):
        result = self.vm.shell(shlex.quote(self.executable),
                               stdin=json.dumps({"cmd": cmd, "args": args}) + "\n",
                               timeout=timeout, check=False)
        try:
            response = json.loads(result.stdout)
        except ValueError:
            raise AgentError("MacAgent did not return a JSON response") from None
        if not response.get("ok"):
            error = response.get("error", "MacAgent failed")
            if response.get("code", "").startswith("TCC_"):
                raise Blocked(f"{error}. Allow executable: {self.executable}")
            raise AgentError(error)
        return response.get("result")

    def preflight(self):
        result = self.call("permissions", prompt=True)
        if not result["accessibility"]:
            raise Blocked(f"TCC Accessibility permission required for {result['executable']}. "
                          + result["instructions"])
        if not result["screen_capture"]:
            raise Blocked(f"TCC Screen & System Audio Recording permission required for "
                          f"{result['executable']}. " + result["instructions"])
        return result

    def windows(self, **sel):
        return self.call("windows", **sel)

    def wait_window(self, timeout=60, **sel):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            windows = self.windows(**sel)
            if windows:
                return windows[0]
            time.sleep(0.5)
        return None

    def shot(self, guest_path, **sel):
        return self.call("shot", path=guest_path, **sel)
