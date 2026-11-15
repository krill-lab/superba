"""Scenario s24 of the Superba e2e run: forced composer predictions in the
desktop app opened by Superba, with an API-key sign-in (direct, then through
the local router). Registered into scenarios.SCENARIOS; the order is
scenarios.ORDER.

The app's own predictions need a ChatGPT sign-in (its main process asks
chatgpt.com for each one). Superba's page script answers the composer's request
itself with a `thread/sample` on the app-server (superba-appctl speedmenu,
`hook.js`), so the turn's model traffic goes through the run's lines like any
other: the forwarders see a Responses request whose last user message is the
prediction prompt. The scenario then checks the ghost text in the composer and
that Tab puts it into the composer as an editable draft.

Same rules as scenarios.py: real binaries, the real desktop app and Codex, an
isolated HOME, every process inside the run's namespace, no key or token
printed.
"""

import asyncio
import json
import time

import infra
from scenarios import Check, _with_cdp, acall, finish, onboarding, probe, scenario, shot, spb_stream, wait_turn
from scenarios_v2 import hot_switch_ok, router_status
from scenarios_v3 import app_servers, fmt, new_chat

# The start of the prediction prompt (superba-appctl speedmenu::PREDICTION_PROMPT).
PROMPT_HEAD = "This is an app-generated request for a suggested next user message."

# The visible composer: the app keeps a thread's composer mounted (hidden)
# while the home page shows its own.
EDITOR = ("[...document.querySelectorAll('.ProseMirror[contenteditable=\"true\"]')]"
          ".find((e) => e.getClientRects().length > 0)")
GHOST_JS = (f"(() => {{ const e = {EDITOR}; const g = e && e.querySelector('[data-cumberbatch] .cumberbatch-content');"
            " return g ? g.innerText : null; })()")
COMPOSER_JS = f"(() => {{ const e = {EDITOR}; return e ? e.innerText : null; }})()"
FOCUS_JS = (f"(() => {{ const e = {EDITOR}; if (!e) return false; e.focus();"
            " return document.activeElement === e || e.contains(document.activeElement); })()")
STATE_JS = "JSON.stringify(globalThis.__superbaSpeedMenu?.state ?? null)"
# The prediction modules of app 26.1103 / 26.1106 (speedmenu plan tests).
PREDICTION_MODULES = ("GaA7", "dlg1", "x2vX")


def page_state(run):
    raw = acall(_with_cdp(run, lambda c: c.eval(STATE_JS)))
    return json.loads(raw) if raw else {}


def sample_posts(run, since):
    out = []
    for line in run.d["ports"]:
        out += [e for e in infra.responses_posts(run, line, since) if (e.get("last_user") or "").startswith(PROMPT_HEAD)]
    return out


def send_visible(run, prompt):
    """Type `prompt` into the visible composer (any draft cleared) and press
    Enter, as cdp.Cdp.send_turn does for the first composer."""
    async def go(cc):
        if not await cc.eval(FOCUS_JS):
            raise RuntimeError("no visible composer")
        await cc.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", "key": "a", "code": "KeyA",
                                                 "windowsVirtualKeyCode": 65, "modifiers": 2, "commands": ["selectAll"]})
        await cc.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": "a", "code": "KeyA", "windowsVirtualKeyCode": 65, "modifiers": 2})
        await cc.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", "key": "Backspace", "code": "Backspace",
                                                 "windowsVirtualKeyCode": 8, "commands": ["deleteBackward"]})
        await cc.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": "Backspace", "code": "Backspace", "windowsVirtualKeyCode": 8})
        await cc.text(prompt)
        await asyncio.sleep(0.3)
        await cc.key("Enter")

    acall(_with_cdp(run, go))


def prediction_round(run, c, mode, marker):
    """A turn in a new chat, then the composer's prediction: the sample request
    on a line, the ghost text, and Tab accepting it."""
    new_chat(run)
    since = time.time()
    send_visible(run, f"{marker}: In one sentence, what does a binary search do?")
    line, e = wait_turn(run, marker, since)
    c.ok(e.get("status") == 200, f"{mode}: the turn went through line {line}: {fmt(e)}")
    try:
        sample = infra.wait_for(f"{mode}: the prediction request", lambda: sample_posts(run, since), 120)[-1]
    except TimeoutError:
        sample = None
    st = page_state(run).get("predictions") or {}
    c.ok(sample is not None and sample.get("status") == 200 and sample.get("originator") == "Codex Desktop",
         f"{mode}: the app-server sent the prediction request (thread/sample with the prediction prompt) through "
         f"line {sample and sample.get('line')}: {fmt(sample) if sample else None}; page state {json.dumps(st, ensure_ascii=False)}")

    def ghost():
        return acall(_with_cdp(run, lambda cc: cc.eval(GHOST_JS))) or None

    try:
        text = infra.wait_for(f"{mode}: the ghost text in the composer", ghost, 90)
    except TimeoutError:
        text = None
    st = page_state(run).get("predictions") or {}
    png = shot(run, f"s24-{mode}-ghost")
    c.ok(bool(text) and st.get("suggested", 0) >= 1 and page_state(run).get("forced", 0) > 0,
         f"{mode}: the composer shows the prediction {text!r} (Superba's service: {json.dumps(st, ensure_ascii=False)}) [{png}]")
    if not text:
        return
    acall(_with_cdp(run, lambda cc: cc.eval(FOCUS_JS)))
    time.sleep(0.3)
    acall(_with_cdp(run, lambda cc: cc.key("Tab")))
    time.sleep(1.0)
    draft = acall(_with_cdp(run, lambda cc: cc.eval(COMPOSER_JS)))
    left = acall(_with_cdp(run, lambda cc: cc.eval(GHOST_JS)))
    png = shot(run, f"s24-{mode}-accepted")
    c.ok((draft or "").strip() == text.strip() and not left,
         f"{mode}: Tab puts the prediction into the composer as a draft: {draft!r}, no ghost left [{png}]")
    # Leave no draft behind (it would hide the next chat's prediction).
    acall(_with_cdp(run, lambda cc: cc.eval(
        "(() => { const e = document.querySelector('.ProseMirror[contenteditable=\"true\"]'); if (!e) return false;"
        " e.focus(); document.execCommand('selectAll'); document.execCommand('delete'); return true; })()")))


@scenario("s24")
def s24(run):
    c = Check()
    try:
        _s24(run, c)
    finally:
        if router_status(run, "s24 status at the end").get("installed"):
            run.spb("router", "disable", "--countdown", "0", "--yes", "--force", label="s24 cleanup disable", timeout=300)
            run.spb("router", "uninstall", label="s24 cleanup uninstall")
    finish(run, "s24", c)


def _s24(run, c):
    # Key profiles authenticate with the API key (user bug 139).
    r = run.run([run.d["spb"], "profile", "add", "pred", "--name", "pred", "--site", "krill-e2e", "--line", "a",
                 "--line", "b", "--key-stdin"], stdin=run.key + "\n", label="s24 profile add pred")
    c.ok(r["rc"] == 0 or "exists" in (r["out"] + r["err"]), f"profile pred (krill-e2e, API key, lines a > b): exit {r['rc']}")
    res = spb_stream(run, "profile", "use", "pred", "--countdown", "0", "--yes", label="s24-use-pred")
    c.ok(res["rc"] == 0, f"spb profile use pred: exit {res['rc']} [{res['log']}]")
    main0, _ = app_servers(run)
    args = ["launch", "--restart", "--yes"] if main0 else ["launch"]
    res = spb_stream(run, *args, label="s24-launch")
    main1, servers = infra.wait_for("the app's app-server", lambda: (lambda m, s: (m, s) if m and s else None)(*app_servers(run)), 90)
    c.ok(res["rc"] == 0 and main1 != main0, f"spb {' '.join(args)}: the app opened by Superba in API-key mode (main {main1}) [{res['log']}]")
    onboarding(run, c)
    try:
        st = infra.wait_for("the prediction modules patched", lambda: (lambda s: s if all(m in (s.get("patched") or {}) for m in PREDICTION_MODULES) else None)(page_state(run)), 60)
    except TimeoutError:
        st = page_state(run)
    c.ok(all(m in (st.get("patched") or {}) for m in PREDICTION_MODULES) and (st.get("predictions") or {}).get("clients", 0) >= 1,
         f"page patches: {json.dumps(st.get('patched'))}, app-server clients registered: {(st.get('predictions') or {}).get('clients')}")
    prediction_round(run, c, "apikey", "E2E-PR1")

    r = spb_stream(run, "router", "install", "--member", "pred", label="s24-router-install")
    st = router_status(run, "s24 router status")
    c.ok(r["rc"] == 0 and st.get("running"), f"router installed with member pred, port {st.get('port')} [{r['log']}]")
    before = app_servers(run)
    res = spb_stream(run, "router", "enable", "--countdown", "0", "--yes", label="s24-router-enable")
    hot_switch_ok(c, res, before, app_servers(run), "spb router enable")
    st = router_status(run, "s24 status enabled")
    c.ok(st.get("codex_routed"), f"Codex routed through 127.0.0.1:{st.get('port')}")
    c.info("page state after the hot switch: " + json.dumps((page_state(run).get("predictions") or {}), ensure_ascii=False))
    prediction_round(run, c, "router", "E2E-PR2")
    c.info("screenshot: " + shot(run, "s24-end"))
    c.info(f"renderer at the end: {json.dumps({k: v for k, v in probe(run).items() if k in ('composer', 'stop', 'conversation')})}")
