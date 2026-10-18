"""The ChatGPT desktop app (MSIX `OpenAI.Codex`) in the guest: its processes,
the debugging port Superba opened, and the renderer through CDP (via the agent,
which connects to 127.0.0.1:<port> inside the guest).

The renderer helpers mirror e2e/lib/cdp.py (same DOM hooks as
superba-appctl's renderer checks): onboarding answers, typing a turn into the
composer, reading the sidebar and whether a turn is running.
"""

import base64
import json
import re
import time
from pathlib import Path

from agent import AgentError

AUMID = "OpenAI.Codex_2p2nqsd0c76g0!App"

PROBE_JS = r"""(() => {
  const all = (s) => { try { return [...document.querySelectorAll(s)]; } catch (e) { return []; } };
  const vis = (el) => !!(el && (el.offsetParent !== null || el.getClientRects().length > 0));
  const conv = all('[data-above-composer-conversation-id]')[0];
  const rows = all('[data-app-action-sidebar-thread-row]').map((r) => ({
    id: r.getAttribute('data-app-action-sidebar-thread-id'),
    title: r.getAttribute('data-app-action-sidebar-thread-title') || r.getAttribute('aria-label'),
    selected: r.getAttribute('data-app-action-sidebar-thread-selected') === 'true',
  }));
  const stop = ['button[aria-label="Stop"]', 'button[aria-label="Stop generating"]', '[data-testid="stop-button"]']
    .some((s) => all(s).some(vis));
  const text = (document.body && document.body.innerText) || '';
  return {
    url: String(location.href),
    conversation: conv ? conv.getAttribute('data-above-composer-conversation-id') : null,
    composer: all('.ProseMirror').length > 0,
    stop,
    login: /Sign in to ChatGPT|Continue to sign in|Log in to ChatGPT/.test(text),
    rows,
    text_len: text.length,
  };
})()"""


class AppError(RuntimeError):
    pass


class App:
    def __init__(self, run):
        self.run = run

    @property
    def agent(self):
        return self.run.agent

    # ------------------------------------------------------------- processes
    def procs(self):
        return self.agent.procs(r"^(ChatGPT|codex)\.exe$")

    def main(self):
        for p in self.procs():
            if p["name"].lower() == "chatgpt.exe" and "--type=" not in (p["cmdline"] or ""):
                return p
        return None

    def app_servers(self, main=None):
        main = main or self.main()
        if not main:
            return []
        return [p for p in self.procs() if p["name"].lower() == "codex.exe" and p["ppid"] == main["pid"]
                and " app-server" in (p["cmdline"] or "")]

    def cdp_port(self, main=None):
        main = main or self.main()
        m = re.search(r"--remote-debugging-port=(\d+)", (main or {}).get("cmdline") or "")
        return int(m.group(1)) if m else None

    def running(self):
        return self.main() is not None

    def wait_main(self, timeout=90, with_port=True, not_pid=None):
        end = time.time() + timeout
        while time.time() < end:
            m = self.main()
            if m and m["pid"] != not_pid and (not with_port or self.cdp_port(m)):
                return m
            time.sleep(1)
        return None

    def wait_app_server(self, timeout=90, exclude=()):
        end = time.time() + timeout
        while time.time() < end:
            s = [p for p in self.app_servers() if p["pid"] not in exclude]
            if s:
                return s
            time.sleep(1)
        return []

    def kill_all(self):
        for p in self.procs():
            if p["name"].lower() == "chatgpt.exe" and "--type=" not in (p["cmdline"] or ""):
                try:
                    self.agent.call("kill", pid=p["pid"], tree=True)
                except AgentError:
                    pass

    def activate(self):
        """Start the app the way the Start menu does (no Superba, no debugging port)."""
        return self.agent.start_proc("explorer.exe", [f"shell:AppsFolder\\{AUMID}"])

    # ------------------------------------------------------------- CDP
    def port(self):
        p = self.cdp_port()
        if not p:
            raise AppError("the app has no debugging port (not started by Superba?)")
        return p

    def cdp(self, method, params=None, timeout=30):
        return self.agent.call("cdp.call", timeout=timeout + 30, port=self.port(), method=method,
                               params=params or {}, timeout_ms=int(timeout * 1000))

    def eval(self, expr, timeout=30):
        r = self.cdp("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True}, timeout)
        if r and "exceptionDetails" in r:
            raise AppError("eval: " + json.dumps(r["exceptionDetails"])[:600])
        return ((r or {}).get("result") or {}).get("value")

    def probe(self, retries=20):
        last = None
        for _ in range(retries):
            try:
                return self.eval(PROBE_JS)
            except (AgentError, AppError) as e:
                last = e
                time.sleep(1.5)
        raise AppError(f"renderer probe failed: {last}")

    def body_text(self):
        return self.eval("document.body ? document.body.innerText : ''") or ""

    def click_text(self, labels):
        js = ("(() => { const want = %s;"
              " const els = [...document.querySelectorAll('button, a, [role=button], [role=radio], [role=option], label')];"
              " for (const w of want) { const el = els.find((e) => (e.innerText || '').trim() === w"
              " && (e.offsetParent !== null || e.getClientRects().length > 0) && !e.disabled);"
              " if (el) { el.click(); return w; } } return null; })()") % json.dumps(labels)
        return self.eval(js)

    def onboarding(self, timeout=120):
        """Answer the first-run onboarding (Engineering -> Continue -> Skip -> Go to
        ChatGPT) and dismiss announcement modals; returns (steps, probe)."""
        steps = []
        text = ""
        last_error = None
        end = time.time() + timeout
        while time.time() < end:
            try:
                p = self.probe(retries=3)
                text = self.body_text()
            except AppError as e:
                last_error = e
                time.sleep(2)
                continue
            if p["composer"] and not any(w in text for w in ("Which best describes your work", "Skip setup?")):
                clicked = self.click_text(["Continue with current model", "Not now", "Maybe later", "Got it"])
                if clicked:
                    steps.append(clicked)
                    time.sleep(1)
                    continue
                return steps, p
            for labels in (["Go to ChatGPT"], ["Skip"], ["Continue"], ["Engineering"], ["Try again"]):
                clicked = self.click_text(labels)
                if clicked:
                    steps.append(clicked)
                    break
            time.sleep(1.5)
        shown = " ".join((text or "").split())[:300]
        raise AppError(f"onboarding did not finish: {steps}; the app shows {shown!r}"
                       + (f"; last renderer error: {last_error}" if last_error and not shown else ""))

    def new_chat(self):
        return self.click_text(["New chat", "New thread"])

    def composer_ready(self, timeout=300):
        """Until the composer takes the focus (on a first start the Windows app keeps it
        disabled while "Finishing Windows setup" creates its sandbox, minutes)."""
        end = time.time() + timeout
        while True:
            ok = self.eval("(() => { const el = document.querySelector('.ProseMirror'); if (!el) return false;"
                           " el.focus(); return document.activeElement === el || el.contains(document.activeElement); })()")
            if ok:
                return True
            if time.time() >= end:
                return False
            time.sleep(3)

    def send_turn(self, prompt, wait_ready=300):
        if not self.composer_ready(wait_ready):
            text = self.body_text()
            raise AppError("composer not focusable" + (" (app still 'Finishing Windows setup')"
                                                        if "Finishing Windows setup" in text else ""))
        for ev in ({"type": "rawKeyDown", "key": "a", "code": "KeyA", "windowsVirtualKeyCode": 65, "modifiers": 2,
                    "commands": ["selectAll"]},
                   {"type": "keyUp", "key": "a", "code": "KeyA", "windowsVirtualKeyCode": 65, "modifiers": 2},
                   {"type": "rawKeyDown", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8,
                    "commands": ["deleteBackward"]},
                   {"type": "keyUp", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8}):
            self.cdp("Input.dispatchKeyEvent", ev)
        left = self.eval("(() => { const el = document.querySelector('.ProseMirror');"
                         " return el ? (el.innerText || '').trim().length : -1; })()")
        if left:
            raise AppError(f"the composer still holds {left} characters; nothing was sent")
        self.cdp("Input.insertText", {"text": prompt})
        time.sleep(0.3)
        for typ in ("rawKeyDown", "char", "keyUp"):
            ev = {"type": typ, "windowsVirtualKeyCode": 13, "key": "Enter", "code": "Enter"}
            if typ == "char":
                ev["text"] = "\r"
            self.cdp("Input.dispatchKeyEvent", ev)

    def wait_idle(self, timeout=120):
        end = time.time() + timeout
        while time.time() < end:
            try:
                p = self.probe(retries=2)
                if not p["stop"]:
                    return p
            except AppError:
                pass
            time.sleep(1)
        raise AppError("the turn did not finish in the renderer")

    def shot(self, host_path):
        r = self.cdp("Page.captureScreenshot", {"format": "png"}, timeout=30)
        Path(host_path).write_bytes(base64.b64decode(r["data"]))
        return host_path
