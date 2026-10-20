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


# How long ChatGPT's first start may take to become usable ("Finishing Windows setup":
# it creates its sandbox; minutes on a fresh VM): the onboarding's extra budget and the
# composer's.
FIRST_START_S = 420

# The desktop app's first-start sandbox setup helper: while one runs, a switch pauses
# ("ChatGPT is finishing its Windows setup (creating its sandbox)"); Superba detects it by
# this executable name (superba-appctl procs::app_setup_pids / SANDBOX_SETUP_EXE).
SANDBOX_SETUP_RE = r"^codex-windows-sandbox-setup\.exe$"


class App:
    def __init__(self, run):
        self.run = run
        self.setup_waited = 0
        self.composer_error = None

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

    def onboarding(self, timeout=120, setup_timeout=0):
        """Answer the first-run onboarding (Engineering -> Continue -> Skip -> Go to
        ChatGPT) and dismiss announcement modals; returns (steps, probe).

        setup_timeout: while ChatGPT is still in its first-start setup (a blank window, or
        "Finishing Windows setup", before any onboarding page) the budget grows, up to this
        many seconds in all (self.setup_waited says how long that took)."""
        steps = []
        text = ""
        last_error = None
        t0 = time.time()
        end = t0 + timeout
        hard_end = end + setup_timeout
        setup_since = None
        self.setup_waited = 0
        while time.time() < end:
            try:
                p = self.probe(retries=3)
                text = self.body_text()
            except AppError as e:
                last_error = e
                time.sleep(2)
                continue
            if setup_timeout and not steps and self.first_start_setup(p, text):
                setup_since = setup_since or time.time()
                end = min(hard_end, max(end, time.time() + 30))
                time.sleep(3)
                continue
            if setup_since:
                self.setup_waited += round(time.time() - setup_since)
                setup_since = None
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
        if setup_since:
            self.setup_waited += round(time.time() - setup_since)
        shown = " ".join((text or "").split())[:300]
        raise AppError(f"onboarding did not finish: {steps}"
                       + (f" (first-start setup for {self.setup_waited} s)" if self.setup_waited else "")
                       + f"; the app shows {shown!r}"
                       + (f"; last renderer error: {last_error}" if last_error and not shown else ""))

    SETUP_TEXTS = ("Finishing Windows setup", "Setting up", "Getting things ready")

    DIAG_JS = r"""(() => ({
      url: String(location.href), ready: document.readyState, title: document.title,
      html: document.body ? document.body.innerHTML.length : -1,
      root: (() => { const r = document.getElementById('root'); return r ? r.childElementCount : -1; })(),
      visibility: document.visibilityState, focus: document.hasFocus(),
    }))()"""

    def diagnose(self):
        """What the renderer is doing when it shows nothing usable: its pages (CDP
        targets), the main page's URL / ready state / DOM size, ChatGPT's processes."""
        out = {}
        try:
            port = self.cdp_port()
            out["port"] = port
            out["targets"] = [{k: t.get(k) for k in ("type", "url", "title")}
                              for t in (self.agent.call("cdp.list", port=port) or [])]
        except Exception as e:
            out["targets_error"] = str(e)
        try:
            out["page"] = self.eval(self.DIAG_JS)
        except Exception as e:
            out["page_error"] = str(e)
        try:
            out["procs"] = [{k: p.get(k) for k in ("pid", "name", "cmdline")} for p in self.procs()]
        except Exception as e:
            out["procs_error"] = str(e)
        return out

    @classmethod
    def first_start_setup(cls, probe, text):
        """ChatGPT is still in its first-start setup: no composer, nothing to answer, and
        a blank window or one of its setup texts."""
        if probe.get("composer") or probe.get("login"):
            return False
        shown = " ".join((text or "").split())
        return not shown or any(t in shown for t in cls.SETUP_TEXTS)

    def new_chat(self):
        return self.click_text(["New chat", "New thread"])

    FOCUS_JS = ("(() => { const el = document.querySelector('.ProseMirror'); if (!el) return false;"
                " el.focus(); return document.activeElement === el || el.contains(document.activeElement); })()")

    KEEP_MODEL_JS = r"""(() => {
      const visible = e => !!e && (e.offsetParent !== null || e.getClientRects().length > 0);
      const dialogs = [...document.querySelectorAll('[role=dialog],dialog,[aria-modal=true]')];
      for (const dialog of dialogs) {
        if (!visible(dialog) || !/^Introducing\s+\S+/.test((dialog.innerText || '').trim())) continue;
        const buttons = [...dialog.querySelectorAll('button,[role=button]')].filter(e =>
          visible(e) && !e.disabled && e.getAttribute('aria-disabled') !== 'true' &&
          (e.innerText || '').trim() === 'Continue with current model');
        if (buttons.length === 1) { buttons[0].click(); return true; }
      }
      return false;
    })()"""

    def composer_ready(self, timeout=FIRST_START_S, call_s=30):
        """Until the composer takes the focus (on a first start the Windows app keeps it
        disabled while "Finishing Windows setup" creates its sandbox, minutes).

        The renderer may not answer at all meanwhile (a Runtime.evaluate over 30 s during the
        first-start setup, the app restarting its core or itself): such a try is retried with
        a new CDP connection (the agent connects per call and finds the debugging port and
        the page again), waiting for a new main process when the old one is gone, until
        `timeout` in all. False when it never became usable (self.composer_error = why)."""
        t0 = time.time()
        end = t0 + timeout
        tries = errors = 0
        self.composer_error = None
        while True:
            tries += 1
            try:
                remaining = end - time.time()
                if remaining <= 0:
                    return False
                if self.eval(self.FOCUS_JS, timeout=min(call_s, remaining)):
                    self.composer_error = None
                    if errors:
                        self.run.log(f"  composer usable after {tries} tries / {time.time() - t0:.0f} s"
                                     f" ({errors} renderer errors)")
                    return True
                self.composer_error = "the composer is not focusable"
                remaining = end - time.time()
                if remaining > 0 and self.eval(self.KEEP_MODEL_JS, timeout=min(call_s, remaining)):
                    self.run.log("  answered the visible model introduction: Continue with current model")
                    # A click is not readiness. Recheck focus inside the same deadline.
            except (AgentError, AppError) as e:
                errors += 1
                self.composer_error = f"renderer did not answer: {e}"
                self.run.log(f"  composer check {tries} ({time.time() - t0:.0f} s): {str(e)[:300]}; trying again")
                try:
                    if not self.main():
                        self.run.log("  ChatGPT's main process is gone: waiting for it to come back")
                        self.wait_main(max(1, min(90, end - time.time())))
                except AgentError as e2:
                    self.run.log(f"  process list failed: {e2}")
            if time.time() >= end:
                return False
            time.sleep(min(3, max(0, end - time.time())))

    # ------------------------------------------------------------- first-start setup
    def setup_procs(self):
        """The running sandbox setup helpers (any session: the app's sandbox service may run
        it elevated)."""
        return self.agent.procs(SANDBOX_SETUP_RE)

    def wait_first_start_setup(self, timeout=900, settle=45, log=None):
        """Until ChatGPT's first-start Windows setup has finished, the way Superba sees it: no
        codex-windows-sandbox-setup.exe runs (a switch meanwhile pauses, "ChatGPT is finishing
        its Windows setup"). When one ran, also until the app restarted its Codex core at its
        end (a new app-server; at most 120 s). The composer is checked by app_ready
        after answering onboarding: after the wizard's first switch, the onboarding
        may still hide it even though the sandbox is ready.

        "No setup" must hold for `settle` s with the same app-servers: a core that just started
        may not have spawned the setup yet. Returns a dict: ok, secs, setup (pid -> started,
        parent, session, cmdline), app-servers during / after, consent (UAC prompts seen: the
        harness answers none) and, when not ok, why."""
        log = log or self.run.log
        t0 = time.time()
        end = t0 + timeout
        seen = {}
        consent = set()
        during = set()
        setup_end = None
        stable, stable_since = None, None
        info = {"ok": False, "setup": seen}
        last_error = None
        while True:
            now = time.time()
            try:
                procs = self.setup_procs()
                servers = frozenset(p["pid"] for p in self.app_servers())
                uac = [p["pid"] for p in self.agent.procs(r"^consent\.exe$")]
            except (AgentError, AppError) as e:
                last_error = e
                log(f"  first-start setup check failed: {e}; trying again")
                procs, servers, uac = None, None, []
            for p in procs or ():
                if p["pid"] not in seen:
                    seen[p["pid"]] = {"started": p.get("started"), "parent": p.get("ppid"),
                                      "session": p.get("session"), "cmdline": (p.get("cmdline") or "")[:300]}
                    log(f"  ChatGPT's first-start Windows setup runs: codex-windows-sandbox-setup.exe pid {p['pid']}"
                        f" (parent {p.get('ppid')}, session {p.get('session')}, started {p.get('started')})")
            for pid in uac:
                if pid not in consent:
                    consent.add(pid)
                    log(f"  a UAC prompt is open (consent.exe pid {pid}); the harness answers none")
            info.update(consent=sorted(consent), during=sorted(during))
            if procs:
                during |= servers
                setup_end, stable, stable_since = None, None, None
            elif procs is not None:
                if seen and setup_end is None:
                    setup_end = now
                    log(f"  the first-start setup ended after ~{now - t0:.0f} s of waiting; app-servers {sorted(servers)}")
                if servers != stable:
                    stable, stable_since = servers, now
                restarted = bool(servers - during) or not during
                settled = bool(servers) and now - stable_since >= settle
                if settled and (not seen or restarted or now - setup_end >= 120):
                    info.update(servers=sorted(servers), restarted=restarted if seen else None)
                    if not seen:
                        info.update(ok=True, secs=round(now - t0))
                        return info
                    if not restarted:
                        log(f"  no new app-server {now - setup_end:.0f} s after the setup ended (still {sorted(servers)})")
                    info.update(ok=True, secs=round(time.time() - t0))
                    return info
            if now >= end:
                running = sorted(p["pid"] for p in procs or ())
                info.update(secs=round(now - t0), why=(
                    f"codex-windows-sandbox-setup.exe still running after {now - t0:.0f} s: pids {running}" if running
                    else f"the app did not settle (app-servers {sorted(servers or ())})"
                    + (f"; last error: {last_error}" if last_error else "")))
                return info
            time.sleep(3)

    def send_turn(self, prompt, wait_ready=300):
        if not self.composer_ready(wait_ready):
            try:
                text = self.body_text()
            except (AgentError, AppError):
                text = ""
            raise AppError(f"composer not usable: {self.composer_error}" + (" (app still 'Finishing Windows setup')"
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
        # Right after a turn the composer sometimes keeps the text on the first Enter (the
        # turn never leaves the app): press it again while the prompt is still there.
        for attempt in range(3):
            for typ in ("rawKeyDown", "char", "keyUp"):
                ev = {"type": typ, "windowsVirtualKeyCode": 13, "key": "Enter", "code": "Enter"}
                if typ == "char":
                    ev["text"] = "\r"
                self.cdp("Input.dispatchKeyEvent", ev)
            end = time.time() + 5
            left = -1
            while time.time() < end:
                left = self.eval("(() => { const el = document.querySelector('.ProseMirror');"
                                 " return el ? (el.innerText || '').trim().length : -1; })()")
                if left is None or left <= 0:
                    return
                time.sleep(0.5)
            self.run.log(f"  the composer still holds {left} characters after Enter (try {attempt + 1}); pressing it again")

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
