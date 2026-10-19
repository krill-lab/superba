"""AccessKit -> AX adapter, matching the Windows Ui selectors and actions."""

import importlib.util
import re
import time
from pathlib import Path

from .agent import AgentError

# Reuse the locale reader without importing Windows' top-level `agent`/`vm` modules.
_source = Path(__file__).resolve().parents[2] / "windows/lib/strings.py"
_spec = importlib.util.spec_from_file_location("superba_e2e_strings", _source)
_strings = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_strings)
Strings = _strings.Strings


class UiError(RuntimeError):
    pass


class Ui:
    def __init__(self, run, lang="en", process_re="^superba$", pid=None):
        self.run, self.lang = run, lang
        self.win = {"process_re": process_re}
        if pid:
            self.win["pid"] = pid

    @property
    def agent(self):
        return self.run.agent

    @property
    def s(self):
        return self.run.strings[self.lang]

    def L(self, key, **kw):
        return self.s.t(key, **kw)

    def R(self, key):
        return self.s.re(key)

    def _sel(self, name=None, key=None, key_re=None, **sel):
        result = dict(self.win)
        if key:
            result["name"] = self.L(key)
        elif key_re:
            result["name_re"] = self.R(key_re)
        elif name is not None:
            result["name"] = name
        result.update(sel)
        return result

    def find(self, timeout=0, **kw):
        return self.agent.call("uia.find", timeout=timeout + 30, timeout_ms=int(timeout * 1000),
                               **self._sel(**kw))

    def exists(self, timeout=0, **kw):
        return len(self.find(timeout=timeout, **kw)) > kw.get("index", 0)

    def wait(self, timeout=20, **kw):
        found = self.find(timeout=timeout, **kw)
        if len(found) <= kw.get("index", 0):
            # A selector can contain input values. Don't include it in the error.
            raise UiError(f"Element not found within {timeout} seconds")
        return found[kw.get("index", 0)]

    def expect(self, timeout=20, **kw):
        return self.wait(timeout=timeout, **kw)

    def wait_gone(self, timeout=20, **kw):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if not self.exists(**kw):
                return True
            time.sleep(0.3)
        raise UiError("Element is still visible")

    def act(self, action="invoke", timeout=20, settle=0.4, **kw):
        try:
            return self.agent.call("uia.act", timeout=timeout + 30, action=action,
                                   timeout_ms=int(timeout * 1000), settle_ms=int(settle * 1000),
                                   **self._sel(**kw))
        except AgentError as error:
            raise UiError(str(error)) from None

    def click(self, name=None, key=None, type="Button", **kw):
        kw.setdefault("enabled", True)
        return self.act(name=name, key=key, type=type, **kw)

    def try_click(self, **kw):
        try:
            return self.click(timeout=0, **kw)
        except UiError:
            return None

    def set_value(self, value, **kw):
        return self.act("set_value", value=value, **kw)

    def type_into(self, value, **kw):
        return self.act("type", value=value, **kw)

    def value(self, **kw):
        return self.act("value", **kw)

    def key(self, combo, repeat=1):
        return self.agent.call("key", combo=combo, repeat=repeat, **self.win)

    def page(self, page):
        self.act("invoke", key=f"nav.{page.replace('-', '_')}", type="TabItem")

    def superba(self, section):
        self.page("superba")
        self.act("invoke", key=f"superba.section.{section}")

    def tree(self, **sel):
        return self.agent.call("uia.tree", **dict(self.win, **sel))

    def texts(self):
        return [element["name"] for element in self.tree() if element["name"]]

    def has_text(self, pattern, timeout=0):
        end = time.monotonic() + timeout
        while True:
            found = next((text for text in self.texts() if re.search(pattern, text)), None)
            if found or time.monotonic() >= end:
                return found
            time.sleep(0.4)

    def wait_window(self, timeout=60):
        window = self.agent.wait_window(timeout=timeout, **self.win)
        if not window:
            raise UiError("Superba window did not appear")
        return window

    def resize(self, w, h):
        return self.agent.call("window", w=w, h=h, **self.win)
