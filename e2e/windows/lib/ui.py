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

SUPERBA_SECTIONS = ("home", "updates", "diagnostics", "data", "general", "appearance", "network", "notify")
SETTINGS_ANCHORS = ("general", "appearance", "network", "notify")
SUPERBA_TABS = {
    "home": "superba.tab.home",
    "updates": "superba.section.updates",
    "settings": "superba.tab.settings",
    "diagnostics": "superba.section.diagnostics",
    "data": "superba.tab.data",
}


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
        """Open a main page or superba:<section>. Maintenance and preferences
        live in Superba; legacy preferences callers go to General."""
        if page.startswith("superba:"):
            return self.superba(page.split(":", 1)[1])
        if page in SUPERBA_SECTIONS or page in ("superba", "preferences"):
            return self.superba({"superba": "home", "preferences": "general"}.get(page, page))
        key = "superba.tools.title" if page == "tools" else f"nav.{page.replace('-', '_')}"
        self.act("invoke", key=key, type="TabItem")
        time.sleep(0.6)

    def superba(self, section="home"):
        """Open a Superba tab, then a settings anchor when requested.

        Overview and Settings also occur in the main sidebar. Scope tab lookup
        to the Superba TabList, then wait for the selected anchor's heading to
        stop moving inside the content viewport.
        """
        section = "home" if section == "superba" else section
        if section not in SUPERBA_SECTIONS:
            raise ValueError(f"unknown Superba section: {section}")
        self.act("invoke", key="superba.title", type="TabItem")
        tab = "settings" if section in SETTINGS_ANCHORS else section
        sel = dict(key=SUPERBA_TABS[tab], type="TabItem", within=self.superba_tab_list())
        self.act("invoke", **sel)
        self.wait_selected(**sel)
        if section in SETTINGS_ANCHORS:
            self.click(key=f"superba.section.{section}")
            self.wait_settings_anchor(section)

    def superba_tab_list(self):
        # AccessKit's structural containers have no bounding rectangle. Only
        # their interactive children must pass the visibility filter.
        return {"name": self.L("superba.title"), "type": "Tab", "visible": False}

    def wait_selected(self, timeout=20, **sel):
        end = time.monotonic() + timeout
        while True:
            found = self.find(**sel)
            if found and found[0].get("selected") is True:
                return found[0]
            if time.monotonic() >= end:
                raise UiError(f"not selected within {timeout} s: {self._describe(self._sel(**sel))}")
            time.sleep(0.2)

    def wait_settings_anchor(self, section, timeout=20):
        key = f"superba.section.{section}"
        end = time.monotonic() + timeout
        previous, stable_since = None, None
        while True:
            buttons = self.find(key=key, type="Button")
            headings = self.find(key=key, type="Text",
                                 within={"name": self.L("superba.tab.settings"), "type": "Pane", "visible": False})
            window = self.window()
            rect = None
            selected = buttons[0].get("selected") if buttons else False
            if buttons and selected is not False and window:
                bx, by, bw, bh = buttons[0]["rect"]
                wx, wy, ww, wh = window["rect"]
                for heading in headings:
                    r = heading.get("rect")
                    # AccessKit exposes the section's label as UIA Text. The
                    # fixed jump row is above the scrolling content viewport.
                    if r and not heading.get("offscreen", False):
                        x, y, w, h = r
                        if (w > 0 and h > 0 and wx <= x and by + bh <= y <= by + bh + 32
                                and x + w <= wx + ww and y + h <= wy + wh):
                            rect = tuple(r)
                            break
            now = time.monotonic()
            if rect is not None and rect == previous:
                if stable_since is not None and now - stable_since >= 0.4:
                    if selected is None:
                        # AccessKit Windows 0.35 does not expose SelectionItem
                        # for Button. Preserve that missing assertion as WARN;
                        # never manufacture a selected=true result from bounds.
                        warning = f"Superba {section}: UIA Button has no selected state; verified heading at viewport top"
                        warnings = getattr(self.run, "navigation_warnings", None)
                        if warnings is None:
                            warnings = self.run.navigation_warnings = []
                        if warning not in warnings:
                            warnings.append(warning)
                            if hasattr(self.run, "log"):
                                self.run.log("WARN: " + warning)
                    return rect
            else:
                stable_since = now if rect is not None else None
            previous = rect
            if now >= end:
                raise UiError(f"Superba settings anchor did not settle: {section}")
            time.sleep(0.2)

    def expand(self, row_key=None, **kw):
        """Expand an update row by its localized accessible name."""
        return self.act("expand", key=row_key, type="Button", **kw)

    def collapse(self, row_key=None, **kw):
        """Collapse an update row through UIA's ExpandCollapse pattern."""
        return self.act("collapse", key=row_key, type="Button", **kw)

    def scroll_detail(self, notches=-4):
        """Scroll the content pane, below the fixed tabs and settings jump row."""
        w = self.wait_window()
        return self.agent.call("scroll", x=int(w["rect"][2] * 0.8), y=int(w["rect"][3] * 0.65),
                               relative=True, notches=notches, settle_ms=450, **self.win)

    def reveal(self, attempts=12, **sel):
        """Find a control while scrolling only the detail pane, from its top."""
        if self.exists(**sel):
            return self.wait(**sel)
        self.scroll_detail(80)
        for _ in range(attempts):
            if self.exists(**sel):
                return self.wait(**sel)
            self.scroll_detail(-4)
        raise UiError(f"control not found in the detail pane: {self._describe(self._sel(**sel))}")

    def update_row(self, title_key, expand=True):
        """M3 update row names include status/version after the localized title.
        M2 has an ungrouped body, so no row action is needed there."""
        if not self.s.has("updates.list.all"):
            return None
        sel = {"name_re": "^" + re.escape(self.L(title_key)) + r"(?:,|，)", "type": "Button"}
        row = self.reveal(**sel)
        self.act("expand" if expand else "collapse", **sel)
        return row

    def poll_texts(self, patterns, timeout=8, initial=()):
        """Read transient result/toast text until it arrives; include the last
        overlay snapshot so a toast that already appeared is not lost."""
        regexes = [re.compile(p) for p in patterns]
        end = time.monotonic() + timeout
        names = initial
        while True:
            matches = [n for n in names if any(rx.search(n) for rx in regexes)]
            if matches:
                return matches
            names = self.texts()
            matches = [n for n in names if any(rx.search(n) for rx in regexes)]
            if matches or time.monotonic() >= end:
                return matches
            time.sleep(0.25)

    # ------------------------------------------------------------- reading
    def tree(self, **sel):
        s = dict(self.win, raw=True)
        s.update(sel)
        return self.agent.call("uia.tree", timeout=120, **s)

    def texts(self):
        return [e["name"] for e in self.tree(patterns=False) if e["name"]]

    def snapshot(self):
        """Cached names and button bounds for observing a short-lived flow."""
        return self.agent.call("uia.snapshot", timeout=30, **self.win)

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

    def hover(self, settle=1.5, **sel):
        """Point at a widget (found like act()) so its tooltip shows; returns the texts then."""
        el = self.wait(**sel)
        r = el["rect"]
        self.agent.call("hover", x=r[0] + r[2] // 2, y=r[1] + r[3] // 2, settle_ms=int(settle * 1000), **self.win)
        return self.texts()

    def click_at(self, x, y, relative=True, **kw):
        return self.agent.call("click", x=x, y=y, relative=relative, **self.win, **kw)

    @staticmethod
    def _describe(sel):
        return ", ".join(f"{k}={v!r}" for k, v in sel.items() if k != "process_re")
