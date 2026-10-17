"""Minimal Chrome DevTools Protocol client for the desktop app's main renderer.

Library use (from driver.py) or CLI:

    python3 cdp.py PORT eval EXPR | shot FILE | text STR | key NAME | probe | targets

Only the main renderer (`app://-/index.html`, not the avatar overlay or the
quick-chat prewarm page) is driven. Nothing here writes to the app's files; input
goes through `Input.*` like a user would type it.
"""

import asyncio
import base64
import json
import sys
import time
import urllib.request

import websockets

KEYS = {"Enter": (13, "\r"), "Escape": (27, ""), "Tab": (9, "\t")}

# Read-only DOM probe mirroring superba-appctl's renderer hooks (renderer.rs).
PROBE_JS = r"""(() => {
  const all = (s) => { try { return [...document.querySelectorAll(s)]; } catch (e) { return []; } };
  const vis = (el) => !!(el && (el.offsetParent !== null || el.getClientRects().length > 0));
  const conv = all('[data-above-composer-conversation-id]')[0];
  const rows = all('[data-app-action-sidebar-thread-row]').map((r) => ({
    id: r.getAttribute('data-app-action-sidebar-thread-id'),
    host: r.getAttribute('data-app-action-sidebar-thread-host-id'),
    title: r.getAttribute('data-app-action-sidebar-thread-title') || r.getAttribute('aria-label'),
    selected: r.getAttribute('data-app-action-sidebar-thread-selected') === 'true',
    busy: !!r.querySelector('[role="status"][aria-label="Working"], [class*="animate-spin"]'),
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


def targets(port):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/list", timeout=5) as r:
        return json.load(r)


def main_target(port):
    for t in targets(port):
        u = t.get("url", "")
        if (
            t.get("type") == "page"
            and u.startswith("app://-/index.html")
            and "avatar-overlay" not in u
            and "quick-chat" not in u
            and "prewarm" not in u
        ):
            return t
    raise RuntimeError("main renderer target not found")


class Cdp:
    """One CDP session on the main renderer."""

    def __init__(self, port):
        self.port = port
        self._id = 0

    async def __aenter__(self):
        t = main_target(self.port)
        self.ws = await websockets.connect(t["webSocketDebuggerUrl"], max_size=None, open_timeout=10)
        return self

    async def __aexit__(self, *exc):
        await self.ws.close()

    async def call(self, method, params=None, timeout=30):
        self._id += 1
        i = self._id
        await self.ws.send(json.dumps({"id": i, "method": method, "params": params or {}}))
        end = time.time() + timeout
        while True:
            left = end - time.time()
            if left <= 0:
                raise TimeoutError(method)
            m = json.loads(await asyncio.wait_for(self.ws.recv(), left))
            if m.get("id") == i:
                if "error" in m:
                    raise RuntimeError(f"{method}: {m['error']}")
                return m.get("result", {})

    async def eval(self, expr, timeout=30):
        r = await self.call(
            "Runtime.evaluate",
            {"expression": expr, "awaitPromise": True, "returnByValue": True},
            timeout,
        )
        if "exceptionDetails" in r:
            raise RuntimeError("eval: " + json.dumps(r["exceptionDetails"])[:800])
        return r.get("result", {}).get("value")

    async def shot(self, path):
        r = await self.call("Page.captureScreenshot", {"format": "png"})
        with open(path, "wb") as f:
            f.write(base64.b64decode(r["data"]))
        return path

    async def text(self, s):
        await self.call("Input.insertText", {"text": s})

    async def key(self, name):
        code, txt = KEYS[name]
        for typ in ("rawKeyDown", "char", "keyUp"):
            p = {"type": typ, "windowsVirtualKeyCode": code, "key": name, "code": name}
            if typ == "char":
                if not txt:
                    continue
                p["text"] = txt
            await self.call("Input.dispatchKeyEvent", p)

    async def probe(self):
        return await self.eval(PROBE_JS)

    async def click_text(self, labels):
        """Click the first visible button/link whose trimmed text equals one of
        `labels`; returns the label clicked or None."""
        js = (
            "(() => { const want = %s;"
            " const els = [...document.querySelectorAll('button, a, [role=button], [role=radio], [role=option], label')];"
            " for (const w of want) { const el = els.find((e) => (e.innerText || '').trim() === w"
            " && (e.offsetParent !== null || e.getClientRects().length > 0) && !e.disabled);"
            " if (el) { el.click(); return w; } } return null; })()"
        ) % json.dumps(labels)
        return await self.eval(js)

    async def click_xy(self, x, y):
        """A real mouse click at (x, y) in CSS pixels (menus that ignore
        synthetic `element.click()`, e.g. Radix popovers, need pointer events)."""
        for typ in ("mouseMoved", "mousePressed", "mouseReleased"):
            p = {"type": typ, "x": x, "y": y, "button": "left", "clickCount": 1}
            if typ == "mouseMoved":
                p.pop("clickCount")
                p["button"] = "none"
            await self.call("Input.dispatchMouseEvent", p)

    async def rect_of_text(self, text, exact=True):
        """Centre of the last visible element whose own text is `text` (or
        starts with it), or None."""
        js = (
            "(() => { const want = %s, exact = %s; const els = [...document.querySelectorAll('*')].filter((e) => {"
            " const t = (e.innerText || '').trim(); if (!(exact ? t === want : t.startsWith(want))) return false;"
            " const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; });"
            " const e = els[els.length - 1]; if (!e) return null; const r = e.getBoundingClientRect();"
            " return [r.x + r.width / 2, r.y + r.height / 2]; })()"
        ) % (json.dumps(text), "true" if exact else "false")
        return await self.eval(js)

    async def click_on_text(self, text, exact=True):
        """Mouse click on the element showing `text`; returns its centre or None."""
        pos = await self.rect_of_text(text, exact)
        if pos:
            await self.click_xy(*pos)
        return pos

    async def body_text(self):
        return await self.eval("document.body ? document.body.innerText : ''")

    async def send_turn(self, prompt):
        """Type `prompt` into the composer and press Enter (like a user)."""
        ok = await self.eval(
            "(() => { const el = document.querySelector('.ProseMirror'); if (!el) return false;"
            " el.focus(); return document.activeElement === el || el.contains(document.activeElement); })()"
        )
        if not ok:
            raise RuntimeError("composer not found")
        # A draft already in the composer (from anything) is discarded, never
        # sent along with the prompt.
        await self.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", "key": "a", "code": "KeyA",
                                                   "windowsVirtualKeyCode": 65, "modifiers": 2, "commands": ["selectAll"]})
        await self.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": "a", "code": "KeyA",
                                                   "windowsVirtualKeyCode": 65, "modifiers": 2})
        await self.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", "key": "Backspace", "code": "Backspace",
                                                   "windowsVirtualKeyCode": 8, "commands": ["deleteBackward"]})
        await self.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": "Backspace", "code": "Backspace",
                                                   "windowsVirtualKeyCode": 8})
        left = await self.eval("(() => { const el = document.querySelector('.ProseMirror');"
                               " return el ? (el.innerText || '').trim().length : -1; })()")
        if left:
            raise RuntimeError(f"the composer still holds {left} characters; nothing was sent")
        await self.text(prompt)
        await asyncio.sleep(0.3)
        await self.key("Enter")


def run_cli(argv):
    port, cmd = argv[1], argv[2]
    arg = argv[3] if len(argv) > 3 else ""

    async def go():
        if cmd == "targets":
            print(json.dumps(targets(port), indent=1))
            return
        async with Cdp(port) as c:
            if cmd == "eval":
                v = await c.eval(arg)
                print(v if isinstance(v, str) else json.dumps(v, ensure_ascii=False)[:20000])
            elif cmd == "shot":
                print("saved", await c.shot(arg))
            elif cmd == "text":
                await c.text(arg)
            elif cmd == "key":
                await c.key(arg)
            elif cmd == "probe":
                print(json.dumps(await c.probe(), ensure_ascii=False, indent=1))
            elif cmd == "click":
                print(await c.click_text(arg.split("|")))
            elif cmd == "clicktext":
                print(await c.click_on_text(arg))
            else:
                sys.exit(__doc__)

    asyncio.run(go())


if __name__ == "__main__":
    run_cli(sys.argv)
