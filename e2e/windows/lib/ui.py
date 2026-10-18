"""Driving the Superba GUI through UI Automation (egui -> AccessKit -> UIA).

Widgets are found by their accessible name, which is the label the user sees;
labels come from the locale files (Strings), so `ui.click(key="wizard.apply")`
works in English and Chinese. Buttons are pressed with the Invoke pattern (no
mouse needed); fields get their text through the Value pattern or by typing.
Coordinates are a fallback only (`click_at`).
"""

import re
import time

from agent import AgentError


class UiError(RuntimeError):
    pass


class Ui:
    def __init__(self, run, lang="en", process_re="^superba$"):
        self.run = run
        self.lang = lang
        self.win = {"process_re": process_re}

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

    # ------------------------------------------------------------- selectors
    def _sel(self, name=None, key=None, key_re=None, **sel):
        out = dict(self.win)
        out["raw"] = True  # popovers / menus are only in the raw view of AccessKit's tree
        if key:
            out["name"] = self.L(key)
        elif key_re:
            out["name_re"] = self.R(key_re)
        elif name is not None:
            out["name"] = name
        out.update(sel)
        return out

    def find(self, timeout=0, **kw):
        sel = self._sel(**kw)
        return self.agent.call("uia.find", timeout=timeout + 60, timeout_ms=int(timeout * 1000), **sel)

    def exists(self, timeout=0, **kw):
        sel = self._sel(**kw)
        idx = sel.get("index", 0)
        return len(self.find(timeout=timeout, **kw)) > idx

    def wait(self, timeout=20, **kw):
        sel = self._sel(**kw)
        idx = sel.get("index", 0)
        found = self.find(timeout=timeout, **kw)
        if len(found) <= idx:
            raise UiError(f"not found within {timeout} s: {self._describe(sel)}")
        return found[idx]

    def wait_gone(self, timeout=20, **kw):
        end = time.time() + timeout
        while time.time() < end:
            if not self.find(**kw):
                return True
            time.sleep(0.5)
        raise UiError(f"still shown after {timeout} s: {self._describe(self._sel(**kw))}")

    def act(self, action="invoke", timeout=20, settle=0.4, **kw):
        sel = self._sel(**kw)
        try:
            return self.agent.call("uia.act", timeout=timeout + 60, action=action, timeout_ms=int(timeout * 1000),
                                   settle_ms=int(settle * 1000), **sel)
        except AgentError as e:
            raise UiError(f"{action} {self._describe(sel)}: {e}") from None

    def click(self, name=None, key=None, type="Button", **kw):
        kw.setdefault("enabled", True)
        return self.act("invoke", name=name, key=key, type=type, **kw)

    def try_click(self, key=None, name=None, type="Button", **kw):
        """Press it if it is there right now (countdown buttons vanish on their own)."""
        try:
            return self.act("invoke", key=key, name=name, type=type, timeout=0, **kw)
        except UiError:
            return None

    def set_value(self, value, **kw):
        return self.act("set_value", value=value, **kw)

    def type_into(self, value, **kw):
        return self.act("type", value=value, **kw)

    def key(self, combo, repeat=1):
        self.agent.call("window", foreground=True, **self.win)
        return self.agent.call("key", combo=combo, repeat=repeat)

    def page(self, page):
        """Open a sidebar page: overview providers models router usage settings
        sessions hosts updates diagnostics preferences."""
        self.act("invoke", key=f"nav.{page}", type="TabItem")
        time.sleep(0.6)

    # ------------------------------------------------------------- reading
    def tree(self, **sel):
        s = dict(self.win, raw=True)
        s.update(sel)
        return self.agent.call("uia.tree", timeout=120, **s)

    def texts(self):
        return [e["name"] for e in self.tree(patterns=False) if e["name"]]

    def has_text(self, pattern, timeout=0):
        rx = re.compile(pattern)
        end = time.time() + timeout
        while True:
            for t in self.texts():
                if rx.search(t):
                    return t
            if time.time() >= end:
                return None
            time.sleep(0.7)

    def window(self):
        ws = [w for w in self.agent.windows(**self.win) if w["title"]]
        return ws[0] if ws else None

    def wait_window(self, timeout=60):
        w = self.agent.wait_window(timeout=timeout, **self.win)
        if not w:
            raise UiError(f"no Superba window within {timeout} s")
        return w

    def resize(self, w, h, x=40, y=40):
        return self.agent.call("window", x=x, y=y, w=w, h=h, restore=True, settle_ms=600, **self.win)

    def click_at(self, x, y, relative=True, **kw):
        return self.agent.call("click", x=x, y=y, relative=relative, **self.win, **kw)

    @staticmethod
    def _describe(sel):
        return ", ".join(f"{k}={v!r}" for k, v in sel.items() if k != "process_re")
