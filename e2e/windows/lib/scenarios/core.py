"""Core path: install -> wizard -> ChatGPT through Superba -> hot switch -> router ->
stats line -> usage -> restore -> uninstall (MATRIX.md W01-W43)."""

import json
import re
import time

import flows
from app import App
from runner import scenario
from ui import Ui


def U(t, lang="en"):
    return Ui(t.run, lang)


# --------------------------------------------------------------------------- install
@scenario("W01", "Fresh install from the MSI on a clean Windows (no VC++, no GPU)", area="install",
          conflicts=("installed",), bugs=(30, 43), tags=("core",))
def w01(t):
    run = t.run
    flows.install(run, t)
    with t.step("Superba starts from the Start menu and opens the setup wizard") as s:
        flows.start_superba(run, s, via="shortcut")
        u = U(t)
        s.check(u.exists(timeout=30, key="wizard.detect.title"), "setup wizard shown on the first start")
        s.shot("first-start-wizard")
        s.ui_dump("first-start-wizard")
        log = flows.gui_log(run)
        s.save("superba-gui.log", log)
        m = re.search(r"renderer: (.+)", log)
        s.check(bool(m), f"GUI log names the renderer: {m.group(1) if m else None}")
        if m:
            s.info("no GPU in the VM: Direct3D 12 on WARP (Microsoft Basic Render Driver) is expected")
        p = u.has_text(r"^\\\\\?\\")
        s.check(not p, f"no \\\\?\\ prefix in shown paths (bug 43c): {p!r}")
        app_txt = u.has_text(r"\(package \d")
        s.check(bool(app_txt), f"ChatGPT shown with release and package version (bug 43c): {app_txt!r}")
        s.manual("first-start-wizard.png: wizard readable, no tofu boxes (bug 1), fonts crisp on WARP")


@scenario("W02", "Skip setup writes nothing", area="wizard", conflicts=("configured", "wizard_seen"),
          needs=("installed",))
def w02(t):
    from e2e.shared.desktop_flows import wizard_skip

    run = t.run
    u = U(t)
    with t.step("Skip setup leaves Codex untouched") as s:
        run.state["wizard_seen"] = True
        before = flows.inventory(run)
        flows.start_superba(run, s)
        s.check(u.exists(timeout=30, key="wizard.skip"), "wizard with 'Skip setup'")
        wizard_skip(u)
        s.check(not u.exists(key="wizard.detect.title"), "wizard closed")
        after = flows.inventory(run)
        s.check(before["config_sha"] == after["config_sha"], "config.toml unchanged")
        s.check(before["auth_sha"] == after["auth_sha"], "auth.json unchanged")
        s.check(before["user_env"] == after["user_env"], "user environment unchanged")
        s.shot("after-skip")


# --------------------------------------------------------------------------- wizard + first switch
@scenario("W04", "Wizard: Krill key (Automatic auth), first switch = full restart of ChatGPT", area="wizard",
          conflicts=("configured", "wizard_seen"), needs=("superba",), bugs=(4, 5, 40, 43), tags=("core",))
def w04(t):
    run = t.run
    flows.wizard_krill(run, t)
    with t.step("Authentication resolved as the net mode expects") as s:
        want = "pat" if run.net.mode == "real" else "apikey"
        s.check(run.state.get("auth_mode") == want,
                f"auth.json shape {run.state.get('auth_mode')} (expected {want}: "
                f"{'https test lines' if want == 'pat' else 'mock lines block the PAT endpoints'})")
        s.info(f"wizard review line: {run.state.get('wizard_auth')}")
        prof = run.guest_json(run.guest["DATA"] + r"\profiles.json") or {}
        s.save("profiles.json", json.dumps(prof, indent=1).replace(run.net.key, "<key>"))
        # `original` and `official` are Superba's built-in entries (restore targets).
        profs = [p for p in prof.get("profiles", []) if p.get("id") not in ("original", "official")]
        s.check(len(profs) == 1, f"one provider profile created: {[p.get('id') for p in profs]}")
    flows.app_ready(run, t)
    flows.turn(run, t, "E2E-T1", expect_line="a")


@scenario("W10", "Open ChatGPT with Superba: MSIX activation and the debugging port", area="launch",
          needs=("app_ready",), bugs=(40,), tags=("core",))
def w10(t):
    run = t.run
    a = App(run)
    u = U(t)
    with t.step("Quit ChatGPT (Superba opened it: graceful through the debugging port)") as s:
        m0 = a.main()
        flows.start_superba(run, s)
        r = run.spb("launch", "--restart", "--yes", timeout=240, label="launch-restart")
        s.save("spb-launch-restart.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"spb launch --restart exit {r['code']} [{r['transcript']}]")
        s.check("force" not in (r["stdout"] + r["stderr"]).lower(), "no force quit needed (graceful quit worked)", soft=True)
        m1 = a.wait_main(90, not_pid=m0["pid"] if m0 else None)
        s.check(m1 is not None and (not m0 or m1["pid"] != m0["pid"]), f"ChatGPT restarted: {m0 and m0['pid']} -> {m1 and m1['pid']}")
        port = a.cdp_port(m1)
        s.check(bool(port), f"new debugging port {port}")
        sv = a.wait_app_server(90)
        s.check(bool(sv), f"app-server started: {[p['pid'] for p in sv]}")
        p = a.probe()
        s.check(not p["login"], "renderer not on the sign-in page")
        run.state["main_pid"] = m1["pid"]
    with t.step("ChatGPT started outside Superba: Superba offers a full restart") as s:
        a.kill_all()
        time.sleep(3)
        a.activate()
        m = a.wait_main(60, with_port=False)
        s.check(m is not None and not a.cdp_port(m), "ChatGPT started from the Start menu (no debugging port)")
        time.sleep(5)
        u.page("overview")
        time.sleep(2)
        time.sleep(8)  # the overview's next poll
        tips = u.hover(key="overview.hero.this", type="Button")
        hot = next((x for x in tips if re.search(r"(?i)not opened from Superba|full restart", x)), None)
        s.check(bool(hot), f"the 'This computer' tooltip says hot switching needs a full restart: {hot!r} "
                           "(the only place on the Overview)", soft=True)
        s.shot("app-not-from-superba")
        r = run.spb("status", timeout=120, label="status-outside")
        s.save("spb-status.txt", r["stdout"])
        s.check(r["code"] == 0, "spb status exit 0")
        s.check("not opened from Superba" in r["stdout"], "spb status says the app was not opened from Superba")
    with t.step("Fully restart the outside-started ChatGPT through Superba") as s:
        r = run.spb("launch", "--restart", "--yes", timeout=120, label="launch-outside-no-force")
        s.check(r["code"] != 0 and "does not close" in r["stderr"] + r["stdout"],
                f"without --force spb does not kill an app it cannot ask to quit: exit {r['code']} [{r['transcript']}]")
        s.check(a.main() is not None, "ChatGPT still running after the refused restart")
        r = run.spb("launch", "--restart", "--yes", "--force", timeout=240, label="launch-outside-force")
        s.check(r["code"] == 0, f"spb launch --restart --yes --force exit {r['code']} [{r['transcript']}]")
        m = a.wait_main(120)
        s.check(m is not None, f"ChatGPT running with a debugging port again (pid {m and m['pid']})")
        steps, p = a.onboarding(120)
        s.check(not p["login"], "renderer usable after the relaunch")
        run.state["main_pid"] = m["pid"]


@scenario("W11", "Hot line switch keeps the window and the thread", area="switch", needs=("turn",),
          tags=("core",))
def w11(t):
    run = t.run
    a = App(run)
    u = U(t)
    target = "b" if run.state.get("line", "a") == "a" else "a"
    if not a.probe()["conversation"]:
        # The app was relaunched since the last turn (e.g. W10): open a thread in this window first.
        flows.turn(run, t, "E2E-T1")
    before = (run.state.get("turn_inputs") or {}).get("E2E-T1")
    with t.step(f"Switch to line {target.upper()} from the Overview (hero provider menu)") as s:
        flows.start_superba(run, s)
        u.page("overview")
        time.sleep(1)
        m0 = a.main()
        sv0 = [p["pid"] for p in a.app_servers(m0)]
        thread0 = a.probe()["conversation"]
        u.click(name="Switch provider or line")
        time.sleep(1)
        s.shot("line-menu")
        t0 = time.time()
        u.act("invoke", name_re=rf"^Test {run.net.line_id(target)}\b", type="Button")
        phases, names = flows.wait_switch_done(run, s, u, timeout=120)
        secs = round(time.time() - t0, 1)
        s.info(f"switch took {secs} s including the countdown; phases {phases}")
        m1 = a.main()
        sv1 = [p["pid"] for p in a.wait_app_server(60, exclude=sv0)]
        s.check(m1 and m0 and m1["pid"] == m0["pid"], f"main process kept: {m0['pid']} -> {m1 and m1['pid']}")
        s.check(sv1 and not set(sv1) & set(sv0), f"app-server replaced: {sv0} -> {sv1}")
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.check(run.net.line_url(target) in cfg, f"config.toml now on line {target}")
        p = a.probe()
        s.check(bool(thread0) and p["conversation"] == thread0 and not p["login"], f"window kept the thread {thread0}")
        s.shot("after-hot-switch")
        run.state["line"] = target
    flows.turn(run, t, "E2E-T2", expect_line=target)
    if run.net.mode == "real":
        with t.step("Turn 2 carries the history") as s:
            n2 = run.state.get("turn_inputs", {}).get("E2E-T2")
            s.check(before is not None and n2 is not None and n2 > before, f"input items {before} -> {n2}")


@scenario("W12", "PAT <-> API key switch (hot)", area="switch", net="real", needs=("turn",))
def w12(t):
    run = t.run
    a = App(run)
    for mode, label in (("apikey", "API key"), ("pat", "PAT")):
        with t.step(f"spb switch --auth {mode}") as s:
            m0 = a.main()
            r = run.spb("switch", "--auth", mode, "--countdown", "0", "--yes", timeout=240, label=f"auth-{mode}")
            s.check(r["code"] == 0, f"exit {r['code']} [{r['transcript']}]")
            s.check("Hot restart" in r["stdout"], "planned as a hot restart")
            m1 = a.main()
            s.check(m1 and m0 and m1["pid"] == m0["pid"], "main process kept")
            auth = run.guest_json(run.guest["CODEX_HOME"] + r"\auth.json") or {}
            key = "OPENAI_API_KEY" if mode == "apikey" else "personal_access_token"
            s.check(key in auth, f"auth.json now has {key}")
        posts = flows.turn(run, t, f"E2E-AUTH-{mode.upper()}")
        with t.step(f"turn in {label} mode on the wire") as s:
            e = next((x for x in posts if x.get("kind") == "http"), posts[-1] if posts else {})
            want = "bearer" if mode == "apikey" else "pat"
            s.check(e.get("auth_kind") == want, f"forwarder saw auth {e.get('auth_kind')} (expected {want}; the PAT "
                                                f"header chatgpt-account-id only in PAT mode)")


@scenario("W13", "Busy turn: the switch waits, --wait-turn, --force", area="switch", net="real", needs=("turn",))
def w13(t):
    run = t.run
    a = App(run)
    other = "a" if run.state.get("line", "a") == "b" else "b"
    with t.step("A held turn (forwarder keeps response.completed for 40 s) blocks a switch") as s:
        since = time.time()
        a.send_turn("E2E-LONG-1: reply with exactly the text LONG")
        posts = flows.wait_relay_turn(run, since, "E2E-LONG-1", timeout=60)
        s.check(bool(posts), "long turn reached the forwarder")
        time.sleep(3)
        cfg0 = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", timeout=120, label="busy-plain")
        s.check(r["code"] in (3, 4, 5), f"switch without flags does not run while the turn is busy: exit {r['code']} [{r['transcript']}]")
        cfg1 = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.check(cfg0 == cfg1, "nothing written")
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", "--wait-turn", timeout=180,
                    label="busy-wait")
        s.check(r["code"] == 0, f"--wait-turn switched after the turn finished: exit {r['code']}")
        p = a.wait_idle(60)
        s.check(not p["stop"], "turn finished before the restart")
        run.state["line"] = other
    with t.step("--force interrupts a running turn") as s:
        other = "a" if run.state.get("line") == "b" else "b"
        since = time.time()
        a.send_turn("E2E-LONG-2: reply with exactly the text LONG")
        flows.wait_relay_turn(run, since, "E2E-LONG-2", timeout=60)
        time.sleep(3)
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", "--force", timeout=180,
                    label="busy-force")
        s.check(r["code"] == 0, f"--force switched at once: exit {r['code']}")
        p = a.probe()
        s.check(not p["login"], "window still usable after the forced restart")
        s.shot("after-force")
        run.state["line"] = other


# --------------------------------------------------------------------------- router
def same_model(t, first, later, how):
    """User bug 58: a turn after turning the router on or off runs the model the
    first turn ran (the forwarders log each request's model; real network only)."""
    models = t.run.state.get("turn_models") or {}
    if first not in models or later not in models:
        return
    with t.step(f"Turn {later} runs the model of {first} ({how})") as s:
        s.check(models[later] == models[first],
                f"{later}: {models[later]!r}, {first}: {models[first]!r} (user bug 58)")


@scenario("W30", "Router: member gate, add member, turn on, requests through the router", area="router",
          needs=("turn",), bugs=(28, 39, 35, 58), tags=("core",))
def w30(t):
    run = t.run
    flows.router_on(run, t)
    flows.turn(run, t, "E2E-R1", new_chat=True)
    same_model(t, "E2E-T1", "E2E-R1", "through the router")
    with t.step("The router recorded the request") as s:
        r = run.spb_json("usage", "log", timeout=120, label="usage-log")
        rows = (r.get("json") or {}).get("rows") or []
        s.save("usage-log.json", rows[:50])
        s.check(r["code"] == 0, f"spb usage log exit {r['code']}")
        routed = [x for x in rows if x.get("source") == "router"]
        s.check(bool(routed), f"router rows: {[(x.get('model'), (x.get('status') or {}).get('kind')) for x in routed[:4]]}")
        r = run.spb("router", "status", timeout=60, label="router-status")
        s.save("router-status.txt", r["stdout"])
        s.check(r["code"] == 0, "spb router status exit 0")
        s.check(run.net.key not in r["stdout"], "no key in the status output")


@scenario("W31", "Stats line under the reply (router on, default template follows the UI language)",
          area="router", needs=("router_on",), bugs=(23,), tags=("core",))
def w31(t):
    run = t.run
    u = U(t)
    a = App(run)
    with t.step("Turn the stats line on (Router page)") as s:
        flows.start_superba(run, s)
        u.page("router")
        time.sleep(1)
        # 「高级」 is a plain section since user bug 116 (no disclosure to open):
        # scroll its stats line switch into view.
        u.reveal(key="stats_line.title", type="Button")
        el = u.wait(key="stats_line.title", type="Button")
        if el.get("toggle") != "On":
            u.act("toggle", key="stats_line.title", type="Button")
            time.sleep(2)
        el = u.wait(key="stats_line.title", type="Button")
        s.check(el.get("toggle") == "On", "stats line switch on")
        s.shot("stats-line-on")
    with t.step("The reply ends with the stats line") as s:
        since = time.time()
        a.send_turn("E2E-S1: reply with exactly the text PONG")
        flows.wait_relay_turn(run, since, "E2E-S1")
        a.wait_idle(120)
        time.sleep(3)
        text = a.body_text()
        line = next((l for l in text.splitlines() if "tok/s" in l), None)
        s.check(bool(line), f"stats line in the reply: {line!r}")
        s.check(line is None or "first token" in line, "English template (UI language en)", soft=True)
        a.shot(t.evdir / f"{t.seq():02d}-stats-line.png")
        s.add_evidence(t.evdir / f"{t._seq:02d}-stats-line.png")
        s.manual("stats-line.png: one quote line under the answer, no tofu box before the numbers (bug 1)")
    if run.net.mode == "real":
        with t.step("The next request carries no stats-line marker upstream") as s:
            since = time.time()
            a.send_turn("E2E-S2: reply with exactly the text PONG")
            posts = flows.wait_relay_turn(run, since, "E2E-S2")
            a.wait_idle(120)
            e = next((x for x in posts if x.get("kind") == "http"), posts[-1] if posts else {})
            s.check(e.get("markers") == 0, f"stats-line markers in the next request's input: {e.get('markers')}")


def usage_rows(run):
    r = run.spb_json("usage", "log", timeout=120, label="usage-log")
    if r["code"] != 0:
        raise RuntimeError(f"spb usage log failed: exit {r['code']}")
    return (r.get("json") or {}).get("rows") or []


def routed_usage_turn(t, timeout=60):
    """Each W32 invocation makes its own request, including after a snapshot reset."""
    run = t.run
    before = {x["id"] for x in usage_rows(run) if x.get("source") == "router"}
    flows.turn(run, t, "E2E-W32", new_chat=True)
    with t.step("The independent W32 turn completed and was recorded by the router") as s:
        thread = run.state.get("thread")
        s.check(bool(thread), "the new turn has an actual conversation id")
        end = time.monotonic() + timeout
        while True:
            rows = usage_rows(run)
            ok = [x for x in rows if x.get("source") == "router" and x.get("id") not in before
                  and x.get("thread_id") == thread and (x.get("status") or {}).get("kind") == "ok"]
            if ok or time.monotonic() >= end:
                s.save("w32-independent-request.json", {"before_ids": sorted(before), "thread": thread,
                                                       "new_successful_rows": ok, "rows": rows[:50]})
                s.check(bool(ok), "a new successful router row belongs to W32's own conversation")
                return
            time.sleep(2)


def usage_provider_group(u, s):
    """AccessKit ComboBox has no Invoke; click its actual UIA bounds and choice."""
    combo = u.wait(key="requests.stats.group", type="ComboBox", enabled=True)
    rect = combo.get("rect")
    s.check(bool(rect), "Group by ComboBox has visible bounds")
    u.click_at(rect[0] + rect[2] / 2, rect[1] + rect[3] / 2, relative=False)
    choices = u.find(timeout=10, key="requests.stats.by.provider", enabled=True)
    choices = [x for x in choices if x["type"] in ("ListItem", "RadioButton", "Button") and x.get("rect")]
    s.check(len(choices) == 1, "the provider grouping choice is uniquely identified")
    rect = choices[0]["rect"]
    u.click_at(rect[0] + rect[2] / 2, rect[1] + rect[3] / 2, relative=False)
    end = time.monotonic() + 10
    while True:
        selected = u.wait(key="requests.stats.group", type="ComboBox")
        if selected.get("value") == u.L("requests.stats.by.provider") or time.monotonic() >= end:
            s.check(selected.get("value") == u.L("requests.stats.by.provider"), "Group by actually displays Provider")
            break
        time.sleep(0.2)


def usage_show_window(u, s):
    """The completed ChatGPT turn must not cover Usage's clicks or screenshots."""
    windows = u.agent.call("windows", process_re="^ChatGPT$")
    minimized = []
    for window in windows:
        if window.get("visible") and not window.get("minimized"):
            result = u.agent.call("window", hwnd=window["hwnd"], minimize=True)
            minimized.append(result)
            s.check(bool(result and result.get("minimized")),
                    "the completed ChatGPT window was minimized before inspecting Usage")
    target = u.agent.call("window", foreground=True, **u.win)
    s.save("usage-window-state.json", {"chatgpt_before": windows, "minimized": minimized,
                                       "superba": target})
    s.check(bool(target and target.get("foreground") and not target.get("minimized")),
            "Superba is active for Usage clicks and screenshots")


def usage_open_detail(u, s, row):
    # A row exposes selection and invocation patterns; those need not open its
    # detail. Use a real click on this named row, then require both detail tabs.
    action = u.act("click", name=row["name"], type="DataItem", enabled=True)
    s.save("usage-row-action.json", action)
    for key in ("requests.detail.general", "requests.detail.timing"):
        u.wait(key=key, type="TabItem")


@scenario("W32", "Usage page: router rows, detail General/Timing; router off reads session files",
          area="router", needs=("router_on",), bugs=(10, 11, 12, 13, 21, 22), tags=("core",))
def w32(t):
    run = t.run
    u = U(t)
    routed_usage_turn(t)
    with t.step("Usage page lists the routed requests") as s:
        r = run.spb_json("usage", "log", timeout=120, label="usage-log")
        rows = (r.get("json") or {}).get("rows") or []
        s.save("usage-log.json", rows[:50])
        routed = [x for x in rows if x.get("source") == "router"]
        s.check(bool(routed), f"router rows in `spb usage log`: {len(routed)} of {len(rows)}")
        ok = [x for x in routed if (x.get("status") or {}).get("kind") == "ok"]
        s.check(bool(ok), f"a successful routed request: {[(x.get('model'), x.get('first_token_ms'), x.get('duration_ms')) for x in ok[:3]]}")
        bad = [x for x in rows if (x.get("first_token_ms") or 0) < 0 or (x.get("duration_ms") or 0) < 0]
        s.check(not bad, f"no negative timings (bug 12): {bad[:2]}")
        flows.start_superba(run, s)
        usage_show_window(u, s)
        u.page("usage")
        time.sleep(3)
        s.shot("usage-router-on")
        s.ui_dump("usage-router-on")
        s.check(not u.has_text(r"^(Request headers|Response headers|Request data|Response data)$"),
                "no request-content tabs (bug 22)")
        s.manual("usage-router-on.png: banner 'Router on (127.0.0.1:<port>)…', rows with session names and "
                 "'Krill · <line>' provider, filter dropdown borders not clipped (bug 9)")
    with t.step("Open a request: General and Timing tabs") as s:
        # Rows are DataItems named by their summary (time, session, status, model, cost), clicked by
        # name; the detail tabs are TabItems named General / Timing (user bug 56).
        tree = u.tree(patterns=False)
        rows = [e for e in tree if e["type"] == "DataItem" and e["rect"] and e["rect"][3] == 26
                and e["rect"][2] > 600 and e["name"]]
        s.check(bool(rows), f"{len(rows)} named table rows: {[e['name'] for e in rows[:3]]}")
        s.check(not [e for e in tree if e["type"] == "Custom" and not e["name"] and e["rect"]
                     and e["rect"][3] == 26 and e["rect"][2] > 600],
                "no unnamed row left (bug 56)")
        if rows:
            usage_open_detail(u, s, rows[0])
        s.shot("usage-detail-general")
        s.ui_dump("usage-detail-general")
        tabs = [e for e in u.tree(patterns=False) if e["type"] == "TabItem"
                and e["name"] in (u.L("requests.detail.general"), u.L("requests.detail.timing"))]
        s.check(len(tabs) == 2, f"detail tabs by name: {[e['name'] for e in tabs]} (General and Timing; "
                "bug 22: no content tabs)")
        if len(tabs) == 2:
            u.click(key="requests.detail.timing", type="TabItem")
            time.sleep(1.5)
            s.shot("usage-detail-timing")
            s.ui_dump("usage-detail-timing")
        neg = u.has_text(r"-\d+ ms|∞")
        s.check(not neg, f"no negative or infinite timing values (bug 12): {neg!r}")
        s.manual("usage-detail-*.png: cards not clipped at 1280x800 (bug 10); timing segments in order, 0 ms allowed")
    with t.step("Statistics tab") as s:
        u.act("select", key="usage.tab.stats", type="RadioButton")
        usage_provider_group(u, s)
        s.shot("usage-stats-provider")
        s.ui_dump("usage-stats-provider")
        u.act("select", key="requests.stats.metric.cost", type="RadioButton")
        u.wait_selected(key="requests.stats.metric.cost", type="RadioButton")
        s.shot("usage-stats-cost")
        s.ui_dump("usage-stats-cost")
        s.manual("usage-stats-*.png: provider grouping (bug 11), cost estimate labelled (bug 13)")


@scenario("W33", "Router off: Codex back to the direct line", area="router", needs=("router_on",), bugs=(58,),
          tags=("core",))
def w33(t):
    run = t.run
    u = U(t)
    with t.step("Turn off from the Router page") as s:
        flows.start_superba(run, s)
        u.page("router")
        time.sleep(1)
        a = App(run)
        before_main = a.main()
        u.click(key="router.banner.turn_off")
        time.sleep(2)
        u.try_click(key="banner.switch_now")
        # Turning off also changes the router certificate settings: the product
        # asks for a full restart of ChatGPT (expected, alpha.8 D8).
        phases, _ = flows.wait_switch_done(run, s, u, timeout=240, router_restart=True)
        s.info(f"phases: {phases}")
        if "[full restart: router certificate]" in phases:
            after_main = a.wait_main(90, not_pid=(before_main or {}).get("pid"))
            s.check(after_main is not None, "Full restart relaunched the app with a new main process")
            s.check(a.composer_ready(300), f"composer usable after the full restart: {a.composer_error}")
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.check("http://127.0.0.1:%d/backend-api/codex" % (run.state.get("router_port") or 0) not in cfg,
                "config.toml no longer points at the router")
        s.check(any(run.net.line_url(l) in cfg for l in run.net.ports), "config.toml back on a test line")
        s.shot("router-off")
        run.state.pop("router_on", None)
    flows.turn(run, t, "E2E-D1", new_chat=True)
    same_model(t, "E2E-T1", "E2E-D1", "direct again")
    with t.step("Usage page without the router reads Codex's session files") as s:
        r = run.spb_json("usage", "log", timeout=120, label="usage-log-off")
        rows = (r.get("json") or {}).get("rows") or []
        s.check(any(x.get("source") == "rollout" for x in rows), "rows from the session files (source rollout)")
        u.page("usage")
        time.sleep(3)
        s.shot("usage-router-off")
        s.manual("usage-router-off.png: banner 'Router off: requests come from Codex's session files…', the direct "
                 "turn listed with '—' for first token / TPS")


@scenario("W34", "Watchdog: router killed -> Superba switches Codex back to direct", area="router",
          needs=("router_on",))
def w34(t):
    run = t.run
    with t.step("Kill superba-router.exe and wait for the fallback (20 s grace)") as s:
        flows.start_superba(run, s)
        procs = run.agent.procs(r"^superba-router\.exe$")
        s.check(bool(procs), f"router running: {[p['pid'] for p in procs]}")
        for p in procs:
            try:
                run.agent.call("kill", pid=p["pid"], tree=True)
            except Exception:
                pass
        t0 = time.time()
        cfg = ""
        while time.time() - t0 < 120:
            cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
            if "http://127.0.0.1" not in cfg:
                break
            if run.agent.procs(r"^superba-router\.exe$") and time.time() - t0 > 60:
                break  # restarted by its supervisor instead
            time.sleep(5)
        secs = round(time.time() - t0)
        back = run.agent.procs(r"^superba-router\.exe$")
        s.info(f"after {secs} s: router processes {[p['pid'] for p in back]}")
        s.check("http://127.0.0.1" not in cfg or bool(back),
                f"either Codex is back on a direct line or the router runs again ({secs} s)")
        s.shot("watchdog")
        u = U(t)
        msg = u.has_text(u.R("router.watch.fell_back_title"), timeout=5)
        s.info(f"GUI notice: {msg!r}")
        if "http://127.0.0.1" not in cfg:
            run.state.pop("router_on", None)


@scenario("W35", "Router after a reboot (HKCU Run key)", area="router", needs=("router_on",), bugs=(39,))
def w35(t):
    run = t.run
    with t.step("Reboot: the Run key starts superba-router at logon") as s:
        flows.stop_superba(run)
        run.vm.reboot()
        run.reconnect()
        time.sleep(20)
        procs = run.agent.procs(r"^superba-router\.exe$")
        s.check(bool(procs), f"superba-router.exe running after logon: {[(p['pid'], p['path']) for p in procs]}")
        s.check(all("Programs\\Superba" in (p["path"] or "") for p in procs), "started from the installed folder")
        r = run.spb("router", "status", timeout=60, label="router-status-after-reboot")
        s.save("router-status.txt", r["stdout"])
        s.check(r["code"] == 0 and "Running: yes" in r["stdout"], "spb router status: running")
        for k in ("superba", "app_ready", "turn"):
            run.state.pop(k, None)


@scenario("W36", "spb router doctor and the Diagnostics page", area="diag", needs=("configured",), bugs=(39,))
def w36(t):
    run = t.run
    u = U(t)
    with t.step("spb doctor / spb router doctor") as s:
        for args, label in ((("doctor",), "doctor"), (("router", "doctor"), "router-doctor")):
            r = run.spb(*args, timeout=180, label=label)
            s.save(f"spb-{label}.txt", r["stdout"] + "\n--- stderr\n" + r["stderr"])
            s.check(r["code"] in (0, 1, 3), f"spb {' '.join(args)} ran: exit {r['code']}")
            s.check(run.net.key not in r["stdout"] + r["stderr"], "no key in the output")
    with t.step("Diagnostics page loads") as s:
        flows.start_superba(run, s)
        u.superba("diagnostics")
        time.sleep(4)
        s.shot("diagnostics")
        s.ui_dump("diagnostics")
        s.manual("diagnostics.png: checks listed with reasons, no raw errors; the coding-tools table has one row per installed tool")


# --------------------------------------------------------------------------- restore / uninstall
@scenario("W40", "Restore the original configuration (Overview -> Actions)", area="restore", needs=("turn",),
          tags=("core",))
def w40(t):
    run = t.run
    u = U(t)
    base = run.state.get("baseline") or {}
    with t.step("Restore original from the GUI") as s:
        flows.start_superba(run, s)
        u.page("overview")
        time.sleep(1)
        u.click(key="overview.menu.title")
        time.sleep(1)
        u.act("invoke", key="overview.menu.restore_original")
        time.sleep(2)
        s.ui_dump("restore-choice")
        s.shot("restore-choice")
        for k in ("banner.restore.keep", "banner.restore.exact", "banner.restore.reverse"):
            if u.exists(key=k):
                s.info(f"choice offered: {u.L(k)}")
        if u.exists(key="banner.restore.exact"):
            u.try_click(key="banner.restore.exact", type=None)
        u.try_click(key="banner.switch_now")
        u.try_click(key="confirm.full_restart")
        flows.wait_switch_done(run, s, u, timeout=240)
        s.shot("after-restore")
    with t.step("Codex files and environment are back to the baseline") as s:
        inv = flows.inventory(run)
        s.save("inventory-after-restore.json", inv)
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml")
        s.save("config-after-restore.toml", cfg or "<none>")
        was, now = flows.owned_keys(run.state.get("baseline_config")), flows.owned_keys(cfg)
        s.check(was == now, f"the keys Superba owns are back to the original: {now} (original {was})")
        s.info("config.toml " + ("byte-identical to before Superba" if inv["config_sha"] == base.get("config_sha") else
                                 "differs from before Superba only by keys the app itself added meanwhile (they stay)"))
        s.check(inv["auth_sha"] == base.get("auth_sha"), "auth.json back to the original (none on a new machine)")
        s.check("CODEX_AUTHAPI_BASE_URL" not in inv["user_env"], "CODEX_AUTHAPI_BASE_URL removed from HKCU\\Environment")
    for k in ("configured", "app_ready", "turn", "router_on"):
        run.state.pop(k, None)


def leftovers(run, s, inv, base, expect_installed=False):
    s.save("inventory.json", inv)
    if not expect_installed:
        s.check(not inv["install_dir"], f"install folder gone (left: {inv['install_files'][:5]})")
        s.check("Programs\\Superba" not in (inv["user_path"] or ""), f"user PATH without Superba: {inv['user_path']!r}")
        s.check(not inv["start_menu"], "Start-menu shortcut gone")
        s.check(not inv["msi_products"], f"no Windows Installer product: {inv['msi_products']}")
        s.check(not inv["krill_reg"], f"HKCU\\Software\\Krill gone: {inv['krill_reg_values']}")
    s.check(flows.ROUTER_RUN_VALUE not in inv["run_key"], f"no {flows.ROUTER_RUN_VALUE} Run value")
    s.check(not any(p.startswith("superba-router") for p in inv["processes"]), f"no router process: {inv['processes']}")
    s.check("CODEX_AUTHAPI_BASE_URL" not in inv["user_env"], "no CODEX_AUTHAPI_BASE_URL in HKCU\\Environment")
    extra = sorted(set(inv["user_env"]) - set((base or {}).get("user_env", {})))
    s.check(not extra, f"no other new user variables: {extra}")
    tasks = sorted(set(inv["scheduled_tasks"]) - set((base or {}).get("scheduled_tasks", [])))
    s.check(not tasks, f"no new scheduled tasks: {tasks}")  # the VM helpers' own superba-* tasks predate the run
    if inv["data_dir"]:
        s.info(f"Superba data folder kept ({len(inv['data_files'])} files under %LOCALAPPDATA%\\krill-code): user data, see MATRIX.md")


def product_code(inv):
    prods = inv.get("msi_products") or []
    return next((w for w in (prods[0].split() if prods else []) if w.startswith("{")), None)


@scenario("W41", "spb uninstall restores and removes everything Superba added", area="restore",
          conflicts=("uninstalled",), needs=("router_on",), bugs=(30,))
def w41(t):
    run = t.run
    with t.step("spb uninstall --yes") as s:
        flows.stop_superba(run)
        r = run.spb("uninstall", "--yes", timeout=300, label="uninstall")
        s.save("spb-uninstall.txt", r["stdout"] + "\n--- stderr\n" + r["stderr"])
        s.check(r["code"] == 0, f"exit {r['code']} [{r['transcript']}]")
    with t.step("Nothing of Superba's changes is left (the program itself stays until the MSI is removed)") as s:
        inv = flows.inventory(run)
        base = run.state.get("baseline") or {}
        leftovers(run, s, inv, base, expect_installed=True)
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml")
        s.save("config-after-uninstall.toml", cfg or "<none>")
        was, now = flows.owned_keys(run.state.get("baseline_config")), flows.owned_keys(cfg)
        s.check(was == now, f"the keys Superba owns are back to the original: {now} (original {was})")
        s.check(inv["auth_sha"] == base.get("auth_sha"), "auth.json back to the original")
        for k in ("configured", "app_ready", "turn", "router_on"):
            run.state.pop(k, None)


@scenario("W42", "MSI uninstall leaves nothing (files, PATH, Run key, env)", area="restore",
          needs=("installed",), bugs=(30,), tags=("core",))
def w42(t):
    run = t.run
    with t.step("msiexec /x (as Settings > Apps does)") as s:
        flows.stop_superba(run)
        inv = flows.inventory(run)
        s.check(len(inv["msi_products"]) == 1, f"installed product: {inv['msi_products']}")
        code = product_code(inv)
        log = r"C:\Users\Public\wintest\msi-uninstall.log"
        r = run.agent.exec("msiexec.exe", ["/x", code, "/passive", "/norestart", "/l*v", log], timeout=600)
        s.check(r["code"] == 0, f"msiexec /x exit {r['code']} in {r['ms']} ms")
        try:
            run.vm.scp_from(log.replace("\\", "/"), t.evdir / "msi-uninstall.log")
            s.add_evidence(t.evdir / "msi-uninstall.log")
        except Exception:
            pass
    with t.step("Leftovers") as s:
        inv = flows.inventory(run)
        leftovers(run, s, inv, run.state.get("baseline"))
        run.state = {k: v for k, v in run.state.items() if k in ("baseline", "msi_sha256", "version")}
        run.state["uninstalled"] = True


@scenario("W43", "MSI uninstall while Codex is routed", area="restore", conflicts=("uninstalled",),
          needs=("router_on",), bugs=(30,))
def w43(t):
    run = t.run
    with t.step("msiexec /x with the router on") as s:
        flows.stop_superba(run)
        code = product_code(flows.inventory(run))
        r = run.agent.exec("msiexec.exe", ["/x", code, "/passive", "/norestart"], timeout=600)
        s.check(r["code"] == 0, f"msiexec /x exit {r['code']}")
    with t.step("Codex is back on a direct line and nothing is left") as s:
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.check("http://127.0.0.1" not in cfg, "config.toml no longer points at the removed router")
        inv = flows.inventory(run)
        leftovers(run, s, inv, run.state.get("baseline"))
        run.state = {k: v for k, v in run.state.items() if k in ("baseline", "msi_sha256", "version")}
        run.state["uninstalled"] = True
