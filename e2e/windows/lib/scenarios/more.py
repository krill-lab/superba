"""Scenarios beyond the core path (MATRIX.md). Most are `partial`: the harness does
the automatable part and lists what a person has to judge on the evidence."""

import json
import os
import random
import re
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import flows
from migration import (check_migrated, check_restored, rollout_hashes, scan_sessions,
                       wait_migration)
from app import App
from runner import scenario
from session import BASE_SNAPSHOT, GUEST_DL
from ui import Ui, SUPERBA_SECTIONS, SUPERBA_TABS, SETTINGS_ANCHORS
from vm import REPO, free_port, psq

PAGES = ["overview", "tools", "providers", "models", "router", "usage", "settings", "sessions", "hosts",
         "claude_code", "claude_settings"] + [f"superba:{section}" for section in SUPERBA_SECTIONS]


def U(t, lang="en"):
    return Ui(t.run, lang)


def page_tour(t, s, lang="en", tag=""):
    u = U(t, lang)
    for p in PAGES:
        u.page(p)
        u.wait_rendered()
        s.shot(f"{tag}{p.replace(':', '-')}")
    u.page("overview")


def restart_superba(t, s, **flags):
    flows.stop_superba(t.run)
    return flows.start_superba(t.run, s, via="exe", **flags)


# --------------------------------------------------------------------------- start-up
@scenario("W03", "Start-up robustness: renderer, failure message box, no console windows", area="install",
          needs=("installed",), bugs=(29, 43))
def w03(t):
    run = t.run
    with t.step("A start without a usable renderer shows a message box, never a silent exit") as s:
        flows.stop_superba(run)
        pid = run.agent.start_proc(run.superba_exe, [], env={"WGPU_BACKEND": "gl"})
        time.sleep(15)
        ws = run.agent.windows(pid=pid)
        s.save("windows.json", ws)
        s.shot("forced-renderer-failure", full=True)
        s.info(f"windows of pid {pid}: {[w['title'] for w in ws]}")
        s.manual("forced-renderer-failure.png: either Superba runs (wgpu ignored the variable) or a message box "
                 "'Superba could not start' names the reason and a report path; never nothing")
        try:
            run.agent.call("kill", pid=pid, tree=True)
        except Exception:
            pass
    def action():
        flows.stop_superba(run)
        run.agent.start_proc("explorer.exe", [run.guest["APPDATA"] + flows.START_MENU_LNK])
        time.sleep(10)
        run.spb("status", timeout=120, label="status-console-watch")
        run.spb("doctor", timeout=180, label="doctor-console-watch")

    console_watch(t, "Superba start + spb status + spb doctor", action)


def console_watch(t, what, action, secs=0):
    """Polls top-level windows while `action` runs; fails on any console window
    (conhost / Windows Terminal) whose process descends from a Superba program."""
    run = t.run
    with t.step(f"No console windows while: {what}") as s:
        seen, stop = [], threading.Event()

        def poll():
            while not stop.is_set():
                try:
                    for w in run.agent.windows(all=False):
                        if w["class"] in ("ConsoleWindowClass", "CASCADIA_HOSTING_WINDOW_CLASS", "PseudoConsoleWindow") \
                                and w["visible"] and w["rect"][2] > 50:
                            seen.append({k: w[k] for k in ("title", "class", "pid", "process", "rect")})
                except Exception:
                    pass
                time.sleep(0.5)

        th = threading.Thread(target=poll, daemon=True)
        th.start()
        try:
            action()
            if secs:
                time.sleep(secs)
        finally:
            stop.set()
            th.join(5)
        uniq = {json.dumps(x, sort_keys=True) for x in seen}
        s.save("console-windows.json", [json.loads(x) for x in uniq])
        s.check(not uniq, f"no console window appeared ({len(uniq)} seen)")


# --------------------------------------------------------------------------- wizard variants
@scenario("W05", "Wizard: line that blocks PAT -> API key with the reason", area="wizard", net="mock",
          needs=("configured",), bugs=(28,))
def w05(t):
    run = t.run
    with t.step("Automatic resolved to the API key on edge-blocked lines") as s:
        s.info(f"wizard review: {run.state.get('wizard_auth')}")
        s.check(run.state.get("auth_mode") == "apikey", "auth.json holds the API key")
        s.check("API key" in (run.state.get("wizard_auth") or ""), "the review said Automatic → API key")
        inv = flows.inventory(run)
        s.check("CODEX_AUTHAPI_BASE_URL" not in inv["user_env"], "no CODEX_AUTHAPI_BASE_URL written for API-key mode")
        r = run.spb("lines", "--probe", timeout=180, label="lines-probe")
        s.save("spb-lines-probe.txt", r["stdout"])
        s.check(r["code"] == 0, f"spb lines --probe exit {r['code']}")
        s.check("API key" in r["stdout"], "lines report API key only on the test lines", soft=True)


@scenario("W06", "Wizard: official ChatGPT login on a machine with no stashed login", area="wizard",
          conflicts=("configured", "wizard_seen"), needs=("superba",))
def w06(t):
    run = t.run
    u = U(t)
    with t.step("Official login preset") as s:
        run.state["wizard_seen"] = True
        u.click(key="wizard.continue")
        u.act("select", key="wizard.preset.official.title", type="RadioButton")
        u.click(key="wizard.continue")
        time.sleep(2)
        s.shot("official-credentials")
        s.ui_dump("official-credentials")
        while u.exists(key="wizard.continue", enabled=True):
            u.click(key="wizard.continue")
            time.sleep(2)
        s.shot("official-review")
        s.ui_dump("official-review")
        s.manual("official-*.png: explains that nothing is stashed on this machine and ChatGPT will ask to sign in; "
                 "Apply must not invent an auth.json")


@scenario("W07", "Wizard: custom OpenAI-compatible site", area="wizard", net="mock",
          conflicts=("configured", "wizard_seen"), needs=("superba",), bugs=(5,))
def w07(t):
    run = t.run
    u = U(t)
    with t.step("Custom site: URL, key, test, model") as s:
        run.state["wizard_seen"] = True
        u.click(key="wizard.continue")
        u.act("select", key="wizard.preset.custom.title", type="RadioButton")
        u.click(key="wizard.continue")
        time.sleep(1)
        s.ui_dump("custom-fields")
        # The fields are named by their labels (user bug 56).
        named = [k for k in ("wizard.custom.name", "wizard.custom.url", "wizard.custom.key")
                 if u.exists(key=k, type="Edit")]
        s.check(len(named) == 3, f"name, base URL and key fields by their labels: {named}")
        u.set_value("Mock site", key="wizard.custom.name", type="Edit")
        u.set_value("not a url", key="wizard.custom.url", type="Edit")
        time.sleep(0.5)
        bad = u.has_text(re.escape(u.L("wizard.custom.invalid_url")))
        s.check(bool(bad), "an invalid URL is refused inline", soft=True)
        u.set_value(run.net.line_url("b") + "/v1", key="wizard.custom.url", type="Edit")
        u.set_value(run.net.key, key="wizard.custom.key", type="Edit")
        u.click(key="wizard.continue")
        time.sleep(8)
        s.shot("custom-test")
        s.ui_dump("custom-test")
        if u.exists(key="wizard.continue", enabled=True):
            u.click(key="wizard.continue")
            time.sleep(2)
        s.shot("custom-review")
        s.ui_dump("custom-review")
        u.click(key="wizard.apply")
        time.sleep(3)
        if u.exists(key="confirm.full_restart"):
            u.click(key="confirm.full_restart")
        flows.wait_switch_done(run, s, u)
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.save("config.toml", cfg)
        s.check(run.net.line_url("b") in cfg, "config.toml points at the custom site")
        s.shot("custom-applied")
        run.state.update(configured=True, line="b")


def import_fixture_home(run):
    """A home with cc-switch / AI Toolbox fixture stores (fake keys): $SUPERBA_WINTEST_IMPORT_FIXTURES,
    or generated with superba-import's fixture_homes test into e2e/.work/windows/import-fixture-home."""
    env = os.environ.get("SUPERBA_WINTEST_IMPORT_FIXTURES")
    if env and Path(env, ".cc-switch", "cc-switch.db").is_file():
        return Path(env)
    home = REPO / "e2e" / ".work" / "windows" / "import-fixture-home"
    if (home / ".cc-switch" / "cc-switch.db").is_file():
        return home
    home.mkdir(parents=True, exist_ok=True)
    target = os.environ.get("CARGO_TARGET_DIR") or str(REPO / "target-wintest")
    run.log(f"building the import fixtures (cargo test -p superba-import --test fixture_homes, target {target})")
    r = subprocess.run(["cargo", "test", "-p", "superba-import", "--test", "fixture_homes", "--", "--ignored"],
                       cwd=REPO, env=dict(os.environ, SUPERBA_IMPORT_FIXTURE_HOME=str(home), CARGO_TARGET_DIR=target),
                       capture_output=True, text=True, timeout=3600)
    if r.returncode != 0:
        raise RuntimeError("fixture build failed: " + r.stderr[-800:])
    return home


@scenario("W08", "Import from cc-switch and AI Toolbox fixtures", area="wizard", net="mock",
          needs=("configured",), bugs=(37, 5))
def w08(t):
    run = t.run
    u = U(t)
    with t.step("Fixture stores where the tools keep them on Windows") as s:
        home = import_fixture_home(run)
        ccs = home / ".cc-switch"
        tb = home / ".local" / "share" / "com.ai-toolbox"
        run.vm.ps(f"New-Item -ItemType Directory -Force {psq(run.guest['USERPROFILE'] + chr(92) + '.cc-switch')},"
                  f" {psq(run.guest['APPDATA'] + chr(92) + 'com.ai-toolbox')} | Out-Null")
        for f in ccs.iterdir():
            run.vm.scp_to(f, (run.guest["USERPROFILE"] + "/.cc-switch/").replace("\\", "/"))
        for f in tb.iterdir():
            run.vm.scp_to(f, (run.guest["APPDATA"] + "/com.ai-toolbox/").replace("\\", "/"))
        hashes = run.vm.ps(f"Get-ChildItem {psq(run.guest['USERPROFILE'] + chr(92) + '.cc-switch')},"
                           f" {psq(run.guest['APPDATA'] + chr(92) + 'com.ai-toolbox')} -File | Get-FileHash |"
                           " ForEach-Object { $_.Hash + ' ' + $_.Path }").stdout
        s.save("stores-before.sha256", hashes)
        run.state["import_hashes"] = hashes
    with t.step("spb import --dry-run --json (preview only)") as s:
        for tool in ("ccs", "ai-toolbox"):
            r = run.spb_json("import", tool, "--dry-run", timeout=120, label=f"import-{tool}")
            s.save(f"import-{tool}.json", r.get("json") or r["stdout"])
            s.check(r["code"] == 0, f"spb import {tool} --dry-run exit {r['code']}")
    with t.step("GUI: Providers > Add > Import from other tools") as s:
        flows.start_superba(run, s)
        u.page("providers")
        time.sleep(1.5)
        s.ui_dump("providers")
        names = u.texts()
        add = next((n for n in names if n.lower().startswith("add")), None)
        if add:
            u.click(name=add)
            time.sleep(1.5)
        s.shot("add-dialog")
        s.ui_dump("add-dialog")
        imp = u.has_text(r"Import from other tools")
        if imp:
            u.click(name=imp)
            time.sleep(3)
        s.shot("import-preview")
        s.ui_dump("import-preview")
        s.manual("import-preview.png: providers of both tools with what they become, keys, conflicts (skip/rename/update)")
    with t.step("The other tools' files were only read") as s:
        after = run.vm.ps(f"Get-ChildItem {psq(run.guest['USERPROFILE'] + chr(92) + '.cc-switch')},"
                          f" {psq(run.guest['APPDATA'] + chr(92) + 'com.ai-toolbox')} -File | Get-FileHash |"
                          " ForEach-Object { $_.Hash + ' ' + $_.Path }").stdout
        s.check(after == run.state.get("import_hashes"), "cc-switch / AI Toolbox stores byte-identical")


# --------------------------------------------------------------------------- launch variants
@scenario("W14", "Quit / relaunch: tray-only app, Superba restarted while the app runs", area="launch",
          needs=("app_ready",), bugs=(40,))
def w14(t):
    run = t.run
    a = App(run)
    with t.step("ChatGPT window closed (still in the tray): a line switch still works") as s:
        m0 = a.main()
        w = run.agent.windows(pid=m0["pid"])
        for x in w:
            if x["title"]:
                run.agent.call("window", hwnd=x["hwnd"], close=True)
        time.sleep(3)
        s.check(a.main() is not None, "ChatGPT keeps running without a window (tray)")
        other = "b" if run.state.get("line", "a") == "a" else "a"
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", timeout=240, label="switch-tray")
        s.check(r["code"] == 0, f"switch exit {r['code']} [{r['transcript']}]")
        s.check("Hot restart" in r["stdout"], "still a hot restart", soft=True)
        run.state["line"] = other
    with t.step("Superba closed and reopened while ChatGPT runs: hot switching remembered") as s:
        flows.stop_superba(run)
        flows.start_superba(run, s)
        time.sleep(4)
        u = U(t)
        u.page("overview")
        time.sleep(3)
        tips = u.hover(key="overview.hero.this", type="Button")
        hot = next((x for x in tips if u.L("hot.ready") in x), None)
        s.check(bool(hot), f"the 'This computer' tooltip says hot switching is ready: {hot!r}", soft=True)
        r = run.spb("status", timeout=120, label="status-after-reopen")
        s.save("spb-status.txt", r["stdout"])
        s.check(str(a.cdp_port() or "") in r["stdout"], "spb status shows the app's debugging port", soft=True)


CHATGPT_LNK = r"\Microsoft\Windows\Start Menu\Programs\ChatGPT (Superba).lnk"


@scenario("W16", "Open ChatGPT through Superba: the ChatGPT (Superba) shortcut, the Overview's open / reopen (GH#7)",
          area="launch", needs=("app_ready",), bugs=(61, 66, 76))
def w16(t):
    run = t.run
    a = App(run)
    u = U(t)
    lnk = run.guest["APPDATA"] + CHATGPT_LNK

    def shortcut(s):
        run.agent.start_proc("explorer.exe", [lnk])
        s.info("started the ChatGPT (Superba) shortcut (explorer, unelevated)")

    def guis():
        return run.agent.procs(r"^superba\.exe$")

    def status(s, label):
        r = run.spb("status", timeout=120, label=label)
        s.save(f"{label}.txt", r["stdout"] + r["stderr"])
        return r["stdout"]

    def ready_status(s, label, timeout=150):
        """`spb status` once ChatGPT opened through Superba is ready: Superba records the
        launch when the app's renderer answers, which a slow VM takes tens of seconds for."""
        end = time.time() + timeout
        while True:
            out = status(s, label)
            if ("Launched by Superba: yes" in out and "Hot switch: Ready" in out) or time.time() >= end:
                return out
            time.sleep(10)

    def foreground(pid):
        return any(w.get("foreground") for w in run.agent.windows(pid=pid))

    with t.step("The MSI put ChatGPT (Superba) in the Start menu: superba.exe --launch-chatgpt") as s:
        r = run.vm.ps(f"$sc = (New-Object -ComObject WScript.Shell).CreateShortcut({psq(lnk)});"
                      " $sc.TargetPath + '|' + $sc.Arguments")
        target, _, args = r.stdout.strip().partition("|")
        s.check(target.lower().endswith("\\superba.exe") and args == "--launch-chatgpt",
                f"shortcut runs {target!r} {args!r}")
    with t.step("No Superba window, ChatGPT closed: the shortcut opens ChatGPT through Superba, no window") as s:
        flows.stop_superba(run)
        a.kill_all()
        time.sleep(3)
        t0 = time.time()
        shortcut(s)
        # The shortcut's process waits (invisibly) until ChatGPT's core is
        # ready, then exits; it never shows a window.
        seen, running = [], []
        while time.time() - t0 < 240:
            running = guis()
            if not running and time.time() - t0 > 5:
                break
            seen += [w["title"] for w in run.agent.windows(process_re="^superba$") if w["title"]]
            time.sleep(2)
        s.check(not running, f"the shortcut's Superba process exited after {time.time() - t0:.0f} s "
                             f"(left: {[p['pid'] for p in running]})")
        s.check(not seen, f"it showed no Superba window: {sorted(set(seen))}")
        m = a.wait_main(60)
        s.check(m is not None, f"ChatGPT running with a debugging port (pid {m and m['pid']})")
        w = run.agent.wait_window(timeout=90, pid=m["pid"]) if m else None
        s.check(w is not None, f"ChatGPT shows its window: {w and w['title']!r}", soft=True)
        out = status(s, "status-shortcut")
        s.check("Launched by Superba: yes" in out, "spb status: launched by Superba")
        s.check("Hot switch: Ready" in out, "spb status: hot switch ready")
        s.shot("shortcut-opened", full=True)
        run.state["main_pid"] = m and m["pid"]
    with t.step("Superba open, ChatGPT open through it: the shortcut brings ChatGPT forward, nothing restarts") as s:
        flows.start_superba(run, s)
        time.sleep(4)
        u.page("overview")
        time.sleep(2)
        s.check(not u.exists(key="action.open_app") and not u.exists(key="action.reopen_app"),
                "hot switching works: no open / reopen action on the Overview")
        m0 = a.main()
        shortcut(s)
        time.sleep(8)
        m1 = a.main()
        s.check(m0 and m1 and m0["pid"] == m1["pid"], f"same ChatGPT: {m0 and m0['pid']} -> {m1 and m1['pid']}")
        s.check(len(guis()) == 1, f"one Superba process: {[p['pid'] for p in guis()]}")
        s.check(m1 is not None and foreground(m1["pid"]), "ChatGPT is the foreground window", soft=True)
        s.shot("shortcut-activated", full=True)
    with t.step("ChatGPT started from its own tile: the Overview offers the reopen; the shortcut asks to confirm it") as s:
        a.kill_all()
        time.sleep(3)
        a.activate()
        m = a.wait_main(60, with_port=False)
        s.check(m is not None and not a.cdp_port(m), "ChatGPT started without Superba (no debugging port)")
        time.sleep(12)  # the Overview's next poll
        u.page("overview")
        s.check(bool(u.exists(timeout=20, key="action.reopen_app")),
                "the Overview shows " + repr(u.L("action.reopen_app")))
        s.shot("reopen-offered")
        shortcut(s)
        s.check(bool(u.exists(timeout=60, key="confirm.title_reopen")),
                "the open Superba window asks to confirm the reopen")
        s.check(len(guis()) == 1, f"one Superba process: {[p['pid'] for p in guis()]}")
        sw = run.agent.windows(process_re="^superba$")
        s.check(any(w.get("foreground") for w in sw), "the Superba window came to the front", soft=True)
        s.shot("reopen-confirm", full=True)
        # A ChatGPT Superba did not open cannot be asked to quit (no debugging port): the
        # confirmation says up front that it is force-quit, and confirming is the answer, no
        # graceful wait and no second question (user bug 76).
        s.check(bool(u.has_text(re.escape(u.L("switch.warn.windows_force_quit")))),
                "the confirmation says ChatGPT is force-quit (user bug 76)")
        u.click(key="confirm.reopen")
        forced, m1, t0 = False, None, time.time()
        quit_titles = (u.L("decide.force_quit.title_windows"), u.L("decide.force_quit.title"))
        while time.time() - t0 < 300:
            if not forced and any(x in u.texts() for x in quit_titles):
                s.shot("force-quit-question", full=True)
                u.click(key="decide.force")
                forced = True
                s.info("ChatGPT did not quit by itself: answered Force quit")
            m1 = a.main()
            if m1 and (not m or m1["pid"] != m["pid"]) and a.cdp_port(m1):
                break
            m1 = None
            time.sleep(2)
        s.check(m1 is not None, f"ChatGPT reopened with a debugging port (pid {m1 and m1['pid']})")
        s.check(not forced, "no force-quit question after the confirmation (user bug 76)")
        s.info(f"reopened {round(time.time() - t0)} s after the confirmation")
        # The reopen goes on verifying the new Codex core and window: status after it ends.
        phases, _ = flows.wait_switch_done(run, s, u, timeout=240)
        s.info(f"phases: {phases}")
        out = ready_status(s, "status-reopened")
        s.check("Launched by Superba: yes" in out and "Hot switch: Ready" in out,
                "spb status: launched by Superba, hot switch ready")
        run.state["main_pid"] = m1 and m1["pid"]
    with t.step("ChatGPT closed: the Overview's Open ChatGPT with Superba opens it") as s:
        a.kill_all()
        time.sleep(12)
        u.page("overview")
        s.check(bool(u.exists(timeout=30, key="action.open_app")),
                "the Overview shows " + repr(u.L("action.open_app")))
        s.shot("open-offered")
        u.click(key="action.open_app")
        m = a.wait_main(120)
        s.check(m is not None, f"ChatGPT running with a debugging port (pid {m and m['pid']})")
        out = ready_status(s, "status-open-button")
        s.check("Launched by Superba: yes" in out and "Hot switch: Ready" in out,
                "spb status: launched by Superba, hot switch ready")
        s.check(not u.exists(timeout=20, key="action.open_app"), "the action is gone once hot switching works",
                soft=True)
        s.shot("opened")
        run.state["main_pid"] = m and m["pid"]


# --------------------------------------------------------------------------- providers, settings, models
@scenario("W15", "Providers page: add, rename, context menu, delete confirmation, capability badges",
          area="providers", net="mock", needs=("configured",), bugs=(2, 3, 5, 6, 14))
def w15(t):
    run = t.run
    u = U(t)
    with t.step("Providers page and the per-line capabilities") as s:
        flows.start_superba(run, s)
        u.page("providers")
        time.sleep(2)
        s.shot("providers")
        s.ui_dump("providers")
        tabs = [e["name"] for e in u.tree(patterns=False) if e["type"] in ("TabItem", "RadioButton")]
        s.info(f"tabs: {tabs[:20]}")
    with t.step("Right-click a provider: context menu with Delete; delete asks first") as s:
        krill = u.find(name_re=r"^Krill", type="Button") or u.find(name_re=r"^Krill")
        if krill:
            u.act("right_click", name=krill[0]["name"], type=krill[0]["type"])
            time.sleep(1)
            s.shot("context-menu")
            s.ui_dump("context-menu")
            u.key("esc")
        s.manual("providers.png / context-menu.png: focus ring not clipped (bug 2), no separators between tiles "
                 "(bug 3), Test/Delete buttons in the detail, delete needs a confirmation (bug 6)")
    with t.step("Adding a provider does not restart anything (bug 5)") as s:
        m0 = App(run).main()
        r = run.spb("profile", "add", "krill2", "--site", "krill", "--line", run.net.line_id("b"), "--key-stdin",
                    stdin=run.net.key + "\n", timeout=120, label="profile-add")
        s.check(r["code"] == 0, f"spb profile add exit {r['code']}")
        s.check("restart" not in r["stdout"].lower(), "no restart offered")
        m1 = App(run).main()
        s.check(m0 and m1 and m0["pid"] == m1["pid"], "ChatGPT not restarted")


@scenario("W20", "Settings page: load, edit, apply, long values", area="settings", needs=("configured",),
          bugs=(15, 26, 41))
def w20(t):
    run = t.run
    u = U(t)
    with t.step("Settings page loads (Codex version known though codex.exe is under WindowsApps)") as s:
        flows.start_superba(run, s)
        u.page("settings")
        time.sleep(4)
        s.shot("settings")
        s.ui_dump("settings")
        err = u.has_text(r"(?i)can't load|could not load|failed")
        s.check(not err, f"no load error: {err!r}")
        r = run.spb("settings", "list", timeout=180, label="settings-list")
        s.save("settings-list.txt", r["stdout"])
        s.check(r["code"] == 0, f"spb settings list exit {r['code']}")
    with t.step("Long values: developer_instructions, notify array, [shell_environment_policy.set]") as s:
        long = "Always answer in English. " * 16
        r = run.spb("settings", "set", "developer_instructions", long.strip(), timeout=120, label="settings-set-long")
        s.check(r["code"] in (0, 3, 4), f"spb settings set developer_instructions exit {r['code']}")
        u.page("overview")
        u.page("settings")
        time.sleep(3)
        s.shot("settings-long-value")
        s.ui_dump("settings-long-value")
        s.manual("settings-long-value.png: value chip capped with an ellipsis, tooltip with the full text, detail "
                 "pane shows a wrapped preview and an Edit button (bug 15)")
        r = run.spb("settings", "diff", timeout=120, label="settings-diff")
        s.save("settings-diff.txt", r["stdout"])


@scenario("W21", "Models page: per-model context override, speed tiers", area="models", needs=("configured",),
          bugs=(7, 8, 25))
def w21(t):
    run = t.run
    u = U(t)
    with t.step("Models page and spb models") as s:
        flows.start_superba(run, s)
        u.page("models")
        time.sleep(4)
        s.shot("models")
        s.ui_dump("models")
        r = run.spb("models", timeout=180, label="models")
        s.save("spb-models.txt", r["stdout"])
        s.check(r["code"] == 0, f"spb models exit {r['code']}")
        r = run.spb("models", "--help", timeout=60, label="models-help")
        s.save("spb-models-help.txt", r["stdout"])
        note = u.has_text(r"(?i)desktop app.*(standard|tier)|API key")
        s.info(f"speed note: {note!r}")
        s.manual("models.png: each row shows the effective context window / auto-compact (default vs custom); the "
                 "speed selector's borders are not clipped (bug 8); in API-key mode a note says the desktop app "
                 "uses Standard")
    with t.step("Window at 960x640: Models columns fit (bug 25)") as s:
        u.resize(960, 640)
        time.sleep(2)
        s.shot("models-960")
        u.resize(1280, 800)


def legacy_sessions_tar(work, codex_home_win, n=600, kb=48, seed=7):
    """`n` rollout files under provider `krill` (Windows paths in the rows), as a tar
    to unpack into %USERPROFILE%\\.codex, plus the rows to insert into state_5.sqlite."""
    rng = random.Random(seed)
    root = work / "sessions"
    shutil.rmtree(root, ignore_errors=True)
    rows = []
    base = 1_758_000_000
    filler = json.dumps({"timestamp": "2025-09-16T00:00:00.000Z", "type": "response_item", "payload": {
        "type": "reasoning", "summary": [], "encrypted_content": "x" * 3800}}, separators=(",", ":"))
    for i in range(n):
        created = base + i * 1800
        tm = time.gmtime(created)
        uid = (f"{0x01992a00 + i:08x}-{rng.getrandbits(16):04x}-7{rng.getrandbits(12):03x}"
               f"-8{rng.getrandbits(12):03x}-{rng.getrandbits(48):012x}")
        stamp = time.strftime("%Y-%m-%dT%H-%M-%S", tm)
        iso = time.strftime("%Y-%m-%dT%H:%M:%S.000Z", tm)
        rel = Path("sessions") / time.strftime("%Y", tm) / time.strftime("%m", tm) / time.strftime("%d", tm) / f"rollout-{stamp}-{uid}.jsonl"
        p = work / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        meta = {"timestamp": iso, "type": "session_meta", "payload": {
            "session_id": uid, "id": uid, "timestamp": iso, "cwd": "C:\\Users\\superba\\project",
            "originator": "Codex Desktop", "cli_version": "0.162.0", "source": "vscode",
            "thread_source": "user", "model_provider": "krill", "history_mode": "paginated"}}
        lines = [json.dumps(meta, separators=(",", ":"))] + [filler] * max(1, (kb * 1024) // (len(filler) + 1))
        p.write_text("\n".join(lines) + "\n")
        title = f"Legacy session {i + 1}"
        rows.append({"id": uid, "rollout_path": codex_home_win + "\\" + str(rel).replace("/", "\\"),
                     "created_at": created, "updated_at": created + 600, "created_at_ms": created * 1000,
                     "updated_at_ms": (created + 600) * 1000, "source": "vscode", "model_provider": "krill",
                     "cwd": "C:\\Users\\superba\\project", "title": title, "first_user_message": title})
    tar = work / "sessions.tar"
    with tarfile.open(tar, "w") as tf:
        tf.add(root, arcname="sessions")
    return tar, rows


def w22_fixture(t):
    """The same real files/rows for full W22 and isolated migration retries."""
    run = t.run
    a = App(run)
    work = Path(run.root / "work-w22")
    work.mkdir(exist_ok=True)
    ch = run.guest["CODEX_HOME"]
    with t.step("600 rollout files under provider krill + their state_5.sqlite rows") as s:
        flows.stop_superba(run)
        a.kill_all()
        time.sleep(3)
        tar, rows = legacy_sessions_tar(work, ch)
        run.vm.scp_to(tar, "C:/Users/Public/wintest/sessions.tar")
        run.vm.ps(f"tar.exe -xf C:\\Users\\Public\\wintest\\sessions.tar -C {psq(ch)}; Remove-Item C:\\Users\\Public\\wintest\\sessions.tar")
        run.vm.scp_from((ch + "/state_5.sqlite").replace("\\", "/"), work / "state_5.sqlite")
        sys.path.insert(0, str(REPO / "crates" / "superba-gui" / "tools" / "screenshots"))
        import fixtures  # noqa: E402  (insert_threads: only the columns the table has)
        fixtures.insert_threads(work / "state_5.sqlite", rows)
        run.vm.scp_to(work / "state_5.sqlite", (ch + "/state_5.sqlite").replace("\\", "/"))
        run.vm.ps(f"Remove-Item -Force -ErrorAction SilentlyContinue {psq(ch + chr(92) + 'state_5.sqlite-wal')}, {psq(ch + chr(92) + 'state_5.sqlite-shm')}")
        hashes_before = rollout_hashes(run, s, "before")
        fixture_paths = {row["rollout_path"].casefold() for row in rows}
        s.check(fixture_paths <= hashes_before.keys(), "all 600 fixture rollout files are present")
        before = scan_sessions(run, s, "before")
    return before, hashes_before, fixture_paths


@scenario("W22", "Sessions: 600 legacy sessions, migration with progress and animation, undo", area="sessions",
          needs=("configured",), bugs=(31, 33, 16))
def w22(t):
    run = t.run
    u = U(t)
    before, hashes_before, fixture_paths = w22_fixture(t)
    with t.step("Sessions page: migrate with progress and the animation") as s:
        flows.start_superba(run, s)
        u.page("sessions")
        time.sleep(3)
        s.shot("sessions-before")
        s.ui_dump("sessions-before")
        u.click(name=u.L("sessions.migrate", to="openai"), type="Button")
        wait_migration(run, s, u)
        # The page must refresh by itself. Do not hide stale results with Rescan.
        s.check(u.exists(timeout=30, key="sessions.status.done", type="Text"), "completed migration appears in history")
        s.shot("sessions-after")
        s.ui_dump("sessions-after")
        after = scan_sessions(run, s, "after")
        hashes_after = rollout_hashes(run, s, "migrated")
        migration_id = check_migrated(s, before, after, hashes_before, hashes_after, fixture_paths)
        s.manual("migrating-*.png: carrying-boxes animation, intermediate migrated count, current file and Stop and undo (bug 33)")
    with t.step("Roll back the completed migration through the Sessions page") as s:
        u.click(key="sessions.roll_back", type="Button")
        wait_migration(run, s, u, restore=True)
        s.check(u.exists(timeout=30, key="sessions.rolled_back", type="Text"), "history shows Rolled back without Rescan")
        restored = scan_sessions(run, s, "rolled-back")
        check_restored(s, before, restored, hashes_before, rollout_hashes(run, s, "rolled-back"))
        s.check(any(x["id"] == migration_id and x["rolled_back"] for x in restored["history"]),
                "the same migration ledger is marked rolled back")
        s.shot("sessions-rolled-back")
        s.ui_dump("sessions-rolled-back")
    with t.step("Stop and undo during a second active migration") as s:
        u.click(name=u.L("sessions.migrate", to="openai"), type="Button")
        wait_migration(run, s, u, stop=True)
        stopped = scan_sessions(run, s, "stopped")
        check_restored(s, before, stopped, hashes_before, rollout_hashes(run, s, "stopped"))
        s.shot("sessions-stopped")
        s.ui_dump("sessions-stopped")


GLOBAL_STATE = {
    "codex-managed-remote-connections": [
        {"hostId": "remote-ssh-discovered:devbox", "displayName": "devbox", "source": "discovered",
         "alias": "devbox", "hostname": None, "sshPort": None, "identity": None}],
    "remote-projects": [
        {"id": "p1", "hostId": "remote-control:env_e_6a1b8b37df48832d9f0", "label": "site", "remotePath": "/Users/me/site"},
        {"id": "p2", "hostId": "remote-ssh-discovered:devbox", "remotePath": "/srv/app"}],
    "remote-connection-auto-connect-by-host-id": {"remote-control:env_e_6a1b8b37df48832d9f0": True,
                                                  "remote-ssh-discovered:devbox": True},
}


@scenario("W23", "Remote hosts: SSH host managed, remote-control / WSL read-only with friendly names",
          area="hosts", needs=("configured",), bugs=(17, 36, 16))
def w23(t):
    run = t.run
    u = U(t)
    a = App(run)
    with t.step("App global state with an SSH host and a remote-control environment") as s:
        a.kill_all()
        time.sleep(3)
        path = run.guest["CODEX_HOME"] + r"\.codex-global-state.json"
        cur = run.guest_json(path) or {}
        s.save("global-state-before.json", cur)
        cur.update(GLOBAL_STATE)
        local = run.root / "work-w23-global-state.json"
        local.write_text(json.dumps(cur))
        run.vm.scp_to(local, path.replace("\\", "/"))
    with t.step("Remote hosts page") as s:
        flows.stop_superba(run)
        flows.start_superba(run, s)
        u.page("hosts")
        time.sleep(4)
        s.shot("hosts")
        s.ui_dump("hosts")
        raw = u.has_text(r"env_e_6a1b|^FiE|remote-control:")
        s.check(not raw, f"no raw remote-control id or stray glyphs (bug 36): {raw!r}")
        s.check(bool(u.has_text(r"devbox")), "the SSH host devbox listed")
        u.page("sessions")
        time.sleep(3)
        s.shot("sessions-host-switcher")
        s.manual("hosts.png: tiles without separators (bug 17); remote-control entry read-only with a friendly "
                 "label; sessions-host-switcher.png: one segmented control (bug 16)")


# --------------------------------------------------------------------------- router extras
@scenario("W37", "Router under load: 150 parallel requests", area="router", net="mock", needs=("router_on",),
          bugs=(35,), manual_only=True)
def w37(t):
    t.skip("manual: see MATRIX.md W37 (needs the router token from the data folder; never print it)")


# --------------------------------------------------------------------------- updates
@scenario("W50", "Updates: ChatGPT app check / download / App Installer (official vs Store)", area="updates",
          needs=("configured",), bugs=(38,))
def w50(t):
    run = t.run
    u = U(t)
    with t.step("Updates page and spb app check") as s:
        flows.start_superba(run, s)
        u.superba("updates")
        time.sleep(5)
        u.update_row("updates.app.title")
        s.shot("updates")
        s.ui_dump("updates")
        r = run.spb("app", "--help", timeout=60, label="app-help")
        s.save("spb-app-help.txt", r["stdout"])
        r = run.spb_json("app", "check", timeout=180, label="app-check")
        s.save("spb-app-check.json", r.get("json") or (r["stdout"] + r["stderr"]))
        s.check(r["code"] in (0, 1), f"spb app check ran: exit {r['code']}")
        s.manual("updates.png: release + package version of ChatGPT, bundled Codex, source official vs Store; "
                 "for a newer build: Download with progress, then 'Open installer' launches App Installer "
                 "(not run by the harness: ~950 MB)")


@scenario("W51", "Updates: Codex CLI install / pin / unpin, pre-release badges", area="updates",
          needs=("configured",), bugs=(18, 20))
def w51(t):
    run = t.run
    u = U(t)
    with t.step("Codex versions in the GUI and the CLI") as s:
        flows.start_superba(run, s)
        u.superba("updates")
        time.sleep(4)
        u.update_row("updates.pin.title")
        s.shot("updates-codex")
        s.ui_dump("updates-pin")
        if u.s.has("updates.list.all"):
            u.update_row("updates.pin.title", expand=False)
            u.update_row("updates.list.cli")
            s.shot("updates-cli")
            s.ui_dump("updates-cli")
        r = run.spb("codex", "--help", timeout=60, label="codex-help")
        s.save("spb-codex-help.txt", r["stdout"])
        r = run.spb("codex", "list", timeout=180, label="codex-list")
        s.save("spb-codex-list.txt", r["stdout"] + r["stderr"])
        warn = u.has_text(r"(?i)instead of|rather than|而不是")
        s.check(not warn, f"no false 'the app runs another Codex' warning (bug 18): {warn!r}")
        s.manual("updates-codex.png: 'Include pre-releases' before the dropdown, pre-release badge, newest first "
                 "(bug 20). Install/pin/unpin per MATRIX.md W51 when internet time allows")


class _Release(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):
        srv = self.server
        release = {"tag_name": srv.tag, "draft": False, "prerelease": "-" in srv.tag, "body": "mock",
                   "published_at": "2026-10-18T00:00:00Z", "html_url": "https://example.invalid/r",
                   "assets": [{"name": p.name, "size": p.stat().st_size} for p in srv.files]}
        path = self.path.split("?")[0].rstrip("/")
        if path.endswith("/releases/latest"):
            body = json.dumps({**release, "prerelease": False}).encode()
            ctype = "application/json"
        elif "/releases/tags/" in path:
            # "Update now" installs the release it offered by its tag
            # (SelfUpdater::release_of: GET releases/tags/<tag>).
            if path.rsplit("/", 1)[-1] != srv.tag:
                self.send_error(404)
                return
            body = json.dumps(release).encode()
            ctype = "application/json"
        elif path.endswith("/releases"):
            # The Preview update channel (a pre-release build's default) reads the list.
            body = json.dumps([release]).encode()
            ctype = "application/json"
        elif self.path.startswith(f"/dl/{srv.tag}/"):
            name = self.path.split("/")[-1]
            f = next((p for p in srv.files if p.name == name), None)
            if not f:
                self.send_error(404)
                return
            body, ctype = f.read_bytes(), "application/octet-stream"
        else:
            self.send_error(404)
            return
        srv.hits.append(self.path)
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def wait_msi_version(run, version, timeout=420):
    """The MSI removes the old exe before copying the new one. Launch a stable
    PowerShell process for each probe so that gap is a retryable exit code."""
    command = ("$ErrorActionPreference='Stop'; "
               f"if (!(Test-Path -LiteralPath {psq(run.spb_exe)})) {{ exit 3 }}; "
               f"& {psq(run.spb_exe)} --version; exit $LASTEXITCODE")
    end = time.monotonic() + timeout
    last = {"code": None, "stdout": "", "stderr": "version probe did not run"}
    while time.monotonic() < end:
        time.sleep(10)
        last = run.agent.exec("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", command], timeout=60)
        if last["code"] == 0 and last["stdout"].strip() == f"spb {version}":
            break
    return last


# The self-update's failure notices (updates.*.toml problem_failed /
# problem_installed / failed, and the record's "still Superba <old>" reason).
UPDATE_FAILED_RX = (r"The update to Superba .* failed|was installed with a problem|Update failed"
                    r"|this is still Superba|更新到 Superba .*失败|已安装，但有问题|更新失败|仍是 Superba")


@scenario("W52", "Superba self-update via the MSI from an older version", area="updates",
          conflicts=("installed",), bugs=(30, 44))
def w52(t):
    import hashlib
    run = t.run
    if not run.old_msi:
        t.skip("needs --old-msi (the older MSI to start from); the MSI under test is the update")
    m_new = re.search(r"superba-(\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?)(?:-quick)?-windows", run.msi.name)
    tag = "v" + (m_new.group(1) if m_new else "9.0.0")
    new_msi, old_msi = run.msi, run.old_msi
    with t.step("Install the older MSI") as s:
        run.msi = old_msi
        try:
            flows.install(run, t, prefix="[old] ")
        finally:
            run.msi = new_msi
            # The report names the MSI under test, not the older bootstrap MSI.
            run.state["msi_sha256"] = hashlib.sha256(new_msi.read_bytes()).hexdigest()
        s.info(f"older Superba {run.state.get('version')} installed; update target {tag}")
    with t.step("Mock release (loopback) with the newer MSI and SHA256SUMS.txt") as s:
        rel = run.root / "work-w52"
        rel.mkdir(exist_ok=True)
        asset = rel / f"superba-{tag[1:]}-windows-x86_64.msi"
        shutil.copy2(new_msi, asset)
        (rel / "SHA256SUMS.txt").write_text(f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n")
        srv = ThreadingHTTPServer(("127.0.0.1", free_port()), _Release)
        srv.tag, srv.files, srv.hits = tag, [asset, rel / "SHA256SUMS.txt"], []
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        port = srv.server_address[1]
        tun = run.vm.tunnel(remote=[(port, port)])
        run.vm.ps(f"[Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_API', 'http://127.0.0.1:{port}/api', 'User');"
                  f" [Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_DOWNLOADS', 'http://127.0.0.1:{port}/dl', 'User')")
        run.agent.call("env.broadcast")
        s.info(f"mock release {tag} on guest 127.0.0.1:{port}")
    try:
        with t.step("Update from the GUI (Superba > Updates)") as s:
            u = U(t)
            flows.start_superba(run, s, fresh=True)
            if u.exists(key="wizard.skip"):
                u.click(key="wizard.skip")
            # The source MSI may predate the new navigation; the updated MSI must use it.
            if u.exists(key="superba.title", type="TabItem"):
                u.superba("updates")
                u.update_row("updates.superba.title")
            else:
                u.act("invoke", key="nav.updates", type="TabItem")
            time.sleep(4)
            s.shot("update-available")
            s.ui_dump("update-available")
            u.reveal(key="updates.superba.update_now", type="Button")
            u.click(key="updates.superba.update_now")
            # The CI fixture MSI is named 9.0.0; its programs report the
            # workspace version (flows.program_version).
            want = flows.program_version(tag[1:])
            probe = wait_msi_version(run, want)
            ver = probe["stdout"].strip()
            s.save("version-after-update.json", probe)
            s.check(probe["code"] == 0 and ver == f"spb {want}",
                    f"spb --version after the update: {ver!r} (exit {probe['code']}, want {want})")
            run.state["version"] = want
            s.info(f"mock release requests: {srv.hits}")
            inv = flows.inventory(run)
            s.save("inventory-after-update.json", inv)
            s.check(len(inv["msi_products"]) == 1 and tag[1:] in inv["msi_products"][0],
                    f"one Windows Installer product, the new version: {inv['msi_products']}")
            log = run.guest_read(run.guest["DATA"] + r"\logs\selfupdate.log")
            s.check(log is not None, "selfupdate.log written (bug 44)", soft=True)
            if log:
                s.save("selfupdate.log", log)
            w = U(t).wait_window(120)
            s.check(w is not None, "the new Superba started")
            if u.exists(key="wizard.skip"):
                u.click(key="wizard.skip")
            u.superba("updates")
            s.shot("after-update")
            # alpha.9: spb --version alone missed a GUI that relaunched with
            # "The installer reported success, but this is still Superba <old>".
            bad = u.has_text(UPDATE_FAILED_RX, timeout=5)
            s.check(bad is None, f"no 'update failed' notice after the relaunch: {bad!r}")
    finally:
        srv.shutdown()
        tun.terminate()
        run.vm.ps("[Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_API', $null, 'User');"
                  " [Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_DOWNLOADS', $null, 'User')", check=False)


# --------------------------------------------------------------------------- interface
@scenario("W60", "Language zh-CN / en on every page and in the stats line", area="ui", needs=("configured",),
          bugs=(1, 23))
def w60(t):
    run = t.run
    with t.step("Every page in Chinese") as s:
        restart_superba(t, s, lang="zh-CN", size="1280x800")
        page_tour(t, s, "zh-CN", "zh-")
        s.manual("zh-*.png: no tofu boxes (bug 1), Chinese fallback font, labels not clipped")
    with t.step("Every page in English") as s:
        restart_superba(t, s, lang="en", size="1280x800")
        page_tour(t, s, "en", "en-")


@scenario("W61", "Themes light / dark and backgrounds; reduce motion", area="ui", needs=("configured",),
          bugs=(24, 27, 33))
def w61(t):
    run = t.run
    for theme in ("light", "dark"):
        with t.step(f"Theme {theme}: every page") as s:
            flows.stop_superba(run)
            run.agent.start_proc(run.superba_exe, ["--theme", theme, "--size", "1280x800"])
            U(t).wait_window(60)
            page_tour(t, s, "en", f"{theme}-")
    with t.step("Backgrounds (Superba > Appearance)") as s:
        u = U(t)
        u.superba("appearance")
        time.sleep(2)
        for key in ("neutral", "mint", "sky", "sand", "lavender"):
            label = u.L(f"settings.background.{key}")
            u.reveal(name=label, type="RadioButton")
            u.act("invoke", name=label, type="RadioButton")
            s.check(u.wait(name=label, type="RadioButton").get("selected"), f"background {key} selected")
            u.page("overview")
            time.sleep(1.5)
            s.shot(f"background-{key}")
            u.superba("appearance")
        u.reveal(key="prefs.motion.title", type="Button")
        motion = u.wait(key="prefs.motion.title", type="Button")
        if motion.get("toggle") != "On":
            u.act("toggle", key="prefs.motion.title", type="Button")
        s.check(u.wait(key="prefs.motion.title", type="Button").get("toggle") == "On", "reduce motion enabled")
        s.manual("background-*.png and theme tours: soft background, readable contrast in both themes (bugs 24/27)")


@scenario("W62", "Window sizes 960x640 / 1280x800 / 1440x900: every page", area="ui", needs=("configured",),
          bugs=(2, 8, 9, 10, 19, 25))
def w62(t):
    run = t.run
    for w, h in ((960, 640), (1280, 800), (1440, 900)):
        with t.step(f"{w}x{h}") as s:
            restart_superba(t, s, size=f"{w}x{h}")
            got = U(t).window()
            s.info(f"window {got and got['rect']}")
            page_tour(t, s, "en", f"{w}x{h}-")
            if (w, h) == (960, 640):
                u = U(t)
                u.superba("home")
                left, top, width, height = u.window()["rect"]
                def visible(elements, label):
                    s.check(len(elements) == 1, f"{label}: exactly one navigation control")
                    for element in elements:
                        x, y, tw, th = element["rect"]
                        s.check(tw > 0 and th > 0 and left <= x and top <= y
                                and x + tw <= left + width and y + th <= top + height,
                                f"{label} visible without scrolling: {element['rect']}")
                visible(u.find(key="superba.title", type="TabItem", visible=False), "sidebar Superba")
                for tab, key in SUPERBA_TABS.items():
                    visible(u.find(key=key, type="TabItem", within=u.superba_tab_list(), visible=False), tab)
                s.shot("960x640-superba-navigation")
                s.ui_dump("960x640-superba-navigation")
                for section in SETTINGS_ANCHORS:
                    u.superba(section)
                    for anchor in SETTINGS_ANCHORS:
                        visible(u.find(key=f"superba.section.{anchor}", type="TabItem", visible=False), anchor)
                    selected = u.wait(key=f"superba.section.{section}", type="TabItem").get("selected")
                    s.check(selected is True, f"{section} selected after its heading settled: {selected}",
                            soft=selected is None)
                    s.shot(f"960x640-settings-{section}")
                    s.ui_dump(f"960x640-settings-{section}")
    with t.step("Below the minimum (programmatic resize): the layout survives") as s:
        u = U(t)
        try:
            u.resize(700, 500)
            time.sleep(1)
            got = u.window()
            # The minimum (960x640, design tokens size::MIN_WINDOW) is the
            # window's minimum tracking size: Windows applies it to sizing by
            # the user, not to SetWindowPos, so the agent's resize may go
            # below it (alpha.8 D9). Only record what happened.
            s.info(f"after SetWindowPos 700x500: {got['rect']}")
            s.shot("minimum-size-attempt")
            s.ui_dump("minimum-size-attempt")
        finally:
            # The programmatic resize can leave the window below its normal
            # minimum. Keep later scenarios independent of this stress check.
            u.resize(1280, 800)
        s.manual("every page at each size: no clipped borders/focus rings (bugs 2, 8, 9), detail cards fit "
                 "(bug 10), label+control rows aligned (bug 19), Models columns fit at 960 (bug 25)")


@scenario("W63", "No console windows; quiet while minimised", area="ui", needs=("app_ready",), bugs=(29, 34))
def w63(t):
    run = t.run
    u = U(t)
    def action():
        run.spb("status", timeout=120, label="status-console-watch")
        other = "b" if run.state.get("line", "a") == "a" else "a"
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", timeout=240,
                    label="switch-console-watch")
        if r["code"] == 0:
            run.state["line"] = other

    console_watch(t, "60 s on the Overview + a hot switch + spb status", action, secs=60)
    with t.step("Minimised: CPU and child processes") as s:
        procs = run.agent.procs(r"^superba\.exe$")
        if not procs:
            flows.start_superba(run, s)
            procs = run.agent.procs(r"^superba\.exe$")
        pid = procs[0]["pid"]
        run.agent.call("window", minimize=True, **u.win)

        def sample():
            r = run.vm.ps(f"$p = Get-Process -Id {pid}; '{{0}} {{1}}' -f $p.CPU, (Get-CimInstance Win32_Process -Filter \"ParentProcessId={pid}\" | Measure-Object).Count")
            cpu, kids = r.stdout.split()
            return float(cpu), int(kids)
        c0, k0 = sample()
        time.sleep(120)
        c1, k1 = sample()
        s.save("cpu.json", {"cpu_s_before": c0, "cpu_s_after": c1, "children_before": k0, "children_after": k1})
        s.check(c1 - c0 < 2.0, f"CPU while minimised: {c1 - c0:.2f} s in 120 s")
        s.check(k1 <= k0, f"no new child processes: {k0} -> {k1}")
        run.agent.call("window", restore=True, **u.win)


# --------------------------------------------------------------------------- accounts / platforms
@scenario("W70", "Standard (non-admin) user account", area="accounts", manual_only=True)
def w70(t):
    t.skip("manual: follow MATRIX.md W70 (creates a user, changes the auto-logon, reboots)")


def elevated_notice(u):
    return u.has_text(re.escape(u.L("notice.elevated_title")), timeout=20)


def integrity(run, pid):
    return run.agent.call("proc.integrity", pid=pid)


@scenario("W71", "UAC off: an elevated Superba works (notice once), hot switch works", area="accounts",
          snapshot="clean", bugs=(44,))
def w71(t):
    """The `clean` image has UAC off: every process of the admin account is elevated."""
    run = t.run
    with t.step("The image's old Superba removed; the desktop token is elevated (UAC off)") as s:
        out = flows.uninstall_all_superba(run)
        s.info(out.strip() or "no old Superba product")
        info = run.agent.call("ping")
        s.check(info["admin_token"] is True, "the agent (and what it starts) has an elevated token")
    flows.install(run, t)
    flows.wizard_krill(run, t)
    u = U(t)
    with t.step("Elevated Superba: no block, the administrator notice once") as s:
        p = (run.agent.procs(r"^superba\.exe$") or [{}])[0]
        lvl = integrity(run, p["pid"]) if p else {}
        s.check(lvl.get("elevated") is True, f"superba.exe runs elevated: {lvl}")
        s.check(run.state.get("configured"), "the wizard's switch was not blocked")
        notice = elevated_notice(u)
        s.check(bool(notice), f"one-time notice: {notice!r}")
        s.shot("elevated-notice")
        flows.stop_superba(run)
        flows.start_superba(run, s)
        time.sleep(5)
        again = u.has_text(re.escape(u.L("notice.elevated_title")))
        s.check(not again, "not shown again after a restart")
    flows.app_ready(run, t)
    with t.step("Hot line switch from the elevated Superba") as s:
        a = App(run)
        m0 = a.main()
        lvl = integrity(run, m0["pid"])
        s.info(f"ChatGPT main process: {lvl}")
        other = run.net.line_id("b")
        r = run.spb("switch", "--line", other, "--countdown", "0", "--yes", timeout=240, label="switch-uacoff")
        s.check(r["code"] == 0 and "Hot restart" in r["stdout"], f"hot switch exit {r['code']} [{r['transcript']}]")
        m1 = a.main()
        s.check(m1 and m1["pid"] == m0["pid"], "main process kept")
    flows.turn(run, t, "E2E-UACOFF", expect_line="b")


@scenario("W73", "UAC on, Superba run as administrator: ChatGPT still medium integrity, hot switch works",
          area="accounts", conflicts=("configured", "wizard_seen"), needs=("installed",), bugs=(44,))
def w73(t):
    run = t.run
    u = U(t)
    with t.step("Start Superba elevated (an agent with the Highest run level)") as s:
        run.start_agent(run_level="Highest")
        run.dirty = True  # the next scenario starts from the base again
        info = run.agent.call("ping")
        s.check(info["admin_token"] is True, "elevated token")
        flows.stop_superba(run)
        # Directly, not through the Start-menu shortcut: Explorer would start it unelevated.
        flows.start_superba(run, s, via="exe")
        p = run.agent.procs(r"^superba\.exe$")[0]
        lvl = integrity(run, p["pid"])
        s.check(lvl["level"] == "high" and lvl["elevated"], f"superba.exe: {lvl}")
    flows.wizard_krill(run, t)
    with t.step("ChatGPT opened by the elevated Superba runs unelevated") as s:
        a = App(run)
        m = a.main()
        lvl = integrity(run, m["pid"])
        s.check(lvl["level"] == "medium" and not lvl["elevated"],
                f"ChatGPT main process integrity: {lvl} (an elevated app would be a security problem)")
        for sv in a.wait_app_server(60):
            l2 = integrity(run, sv["pid"])
            s.check(l2["level"] == "medium", f"app-server {sv['pid']}: {l2}")
        notice = elevated_notice(u)
        s.check(bool(notice), f"administrator notice once: {notice!r}")
    flows.app_ready(run, t)
    with t.step("Hot line switch from the elevated Superba (GUI)") as s:
        a = App(run)
        target = "b" if run.state.get("line", "a") == "a" else "a"
        before = flows.applied(run, "status-before-switch")
        s.info(f"applied before: {before}")
        m0 = a.main()
        sv0 = [p["pid"] for p in a.app_servers(m0)]
        u.page("overview")
        u.click(name="Switch provider or line")
        time.sleep(1)
        u.act("invoke", name_re=rf"^Test {run.net.line_id(target)}\b", type="Button")
        phases, _ = flows.wait_switch_done(run, s, u, timeout=120)
        s.info(f"phases: {phases}")
        # The switch must have completed (a new generation on the target line) before an
        # unchanged main process means "hot restart": with no switch at all it is unchanged too.
        done, after, pause = flows.wait_applied(run, before, run.net.line_id(target), timeout=90, u=u)
        if not done:
            s.ui_dump("switch-not-applied")
        s.check(done, f"the switch completed: generation {before.get('generation')} -> {after.get('generation')},"
                      f" line {before.get('line_id')} -> {after.get('line_id')}"
                      + (f"; the GUI showed {pause}" if pause else ""))
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.check(run.net.line_url(target) in cfg, f"config.toml on line {target}")
        m1 = a.main()
        sv1 = [p["pid"] for p in a.wait_app_server(60, exclude=sv0)]
        s.check(m1 and m0 and m1["pid"] == m0["pid"], f"main process kept (hot restart): {m0 and m0['pid']} -> {m1 and m1['pid']}")
        s.check(sv1 and not set(sv1) & set(sv0), f"the Codex core restarted: app-server {sv0} -> {sv1}")
        run.state["line"] = target
    flows.turn(run, t, "E2E-ELEV", expect_line=target)


SYSTEM_TASK_PS = r"""
$ErrorActionPreference = 'Stop'
$name = 'superba-wintest-system'
$a = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument ('/d /c ""' + $args[0] + '" ' + $args[1] + ' > "' + $args[2] + '" 2>&1"')
Register-ScheduledTask -TaskName $name -Action $a -User 'NT AUTHORITY\SYSTEM' -RunLevel Highest -Force | Out-Null
Start-ScheduledTask -TaskName $name
$deadline = (Get-Date).AddSeconds(90)
Start-Sleep 2
while ((Get-ScheduledTask -TaskName $name).State -eq 'Running' -and (Get-Date) -lt $deadline) { Start-Sleep 1 }
$r = (Get-ScheduledTaskInfo -TaskName $name).LastTaskResult
Unregister-ScheduledTask -TaskName $name -Confirm:$false
'result ' + $r
"""


@scenario("W74", "Superba started as SYSTEM (scheduled task): refused, nothing written", area="accounts",
          needs=("installed",), bugs=(44,))
def w74(t):
    run = t.run
    sysprof = r"C:\Windows\System32\config\systemprofile"
    probe = (f"@(Test-Path '{sysprof}\\AppData\\Local\\krill-code'), (Test-Path '{sysprof}\\AppData\\Roaming\\krill-code'),"
             f" (Test-Path '{sysprof}\\.codex\\config.toml'), (Test-Path '{sysprof}\\.codex\\auth.json') -join ','")
    before = run.vm.ps(probe).stdout.strip()
    for exe, args, label in ((run.superba_exe, "", "gui"), (run.spb_exe, "status", "spb-status"),
                             (run.spb_exe, "profile add wintest-system --site krill", "spb-profile-add")):
        with t.step(f"{label} as SYSTEM") as s:
            out = rf"C:\Users\Public\wintest\system-{label}.txt"
            script = SYSTEM_TASK_PS.replace("$args[0]", psq(exe)).replace("$args[1]", psq(args)).replace("$args[2]", psq(out))
            r = run.vm.ps(script, timeout=180)
            m = re.search(r"result (\d+)", r.stdout)
            code = int(m.group(1)) if m else None
            text = run.guest_read(out) or ""
            s.save(f"system-{label}.txt", text)
            s.info(f"exit {code}: {text.strip()[:300]!r}")
            if label == "spb-status":
                s.check(code in (0, 3), f"spb status as SYSTEM ran (read-only): exit {code}", soft=True)
            else:
                s.check(code == 3, f"refused with exit 3: {code}")
                s.check("SYSTEM" in text or "wrong user profile" in text, "the reason names the other account")
            if label == "gui":
                box = run.agent.windows(title_re=re.escape(U(t).L("startup.failed_title")))
                s.check(bool(box), f"message box on the signed-in user's desktop: {[w['title'] for w in box]}", soft=True)
                s.shot("system-refusal", full=True)
                for w in box:
                    run.agent.call("window", hwnd=w["hwnd"], close=True)
    with t.step("Nothing written to the SYSTEM profile") as s:
        after = run.vm.ps(probe).stdout.strip()
        s.check(before == after, f"SYSTEM profile unchanged (krill-code dirs, .codex files): {before} -> {after}")


@scenario("W53", "Self-update failure: toast, Updates page, spb self-update check, selfupdate.log", area="updates",
          needs=("installed",), bugs=(44,))
def w53(t):
    run = t.run
    u = U(t)
    tag = "v9.9.9"
    rel = run.root / "work-w53"
    rel.mkdir(exist_ok=True)
    import hashlib
    asset = rel / f"superba-{tag[1:]}-windows-x86_64.msi"
    asset.write_bytes(os.urandom(256 * 1024))  # not an MSI: msiexec fails (1620)
    (rel / "SHA256SUMS.txt").write_text(f"{hashlib.sha256(asset.read_bytes()).hexdigest()}  {asset.name}\n")
    srv = ThreadingHTTPServer(("127.0.0.1", free_port()), _Release)
    srv.tag, srv.files, srv.hits = tag, [asset, rel / "SHA256SUMS.txt"], []
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    port = srv.server_address[1]
    tun = run.vm.tunnel(remote=[(port, port)])
    run.vm.ps(f"[Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_API', 'http://127.0.0.1:{port}/api', 'User');"
              f" [Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_DOWNLOADS', 'http://127.0.0.1:{port}/dl', 'User')")
    run.agent.call("env.broadcast")
    try:
        with t.step("Update now to a release whose MSI is broken") as s:
            flows.start_superba(run, s, fresh=True)
            if u.exists(timeout=5, key="wizard.skip"):
                u.click(key="wizard.skip")
            u.superba("updates")
            time.sleep(4)
            u.update_row("updates.superba.title")
            r = run.spb("self-update", "check", timeout=120, label="self-update-check-before")
            s.save("check-before.txt", r["stdout"])
            s.check("9.9.9" in r["stdout"], "the mock release 9.9.9 is offered")
            s.shot("update-available")
            u.click(key="updates.superba.update_now", timeout=30)
            end = time.time() + 300
            log = None
            while time.time() < end:
                time.sleep(10)
                log = run.guest_read(run.guest["DATA"] + r"\logs\selfupdate.log")
                if log and "msiexec" in log and u.window():
                    break
            s.check(log is not None, "selfupdate.log written")
            if log:
                s.save("selfupdate.log", log)
                s.check(re.search(r"(?i)msiexec.*(1620|exit|code)", log) is not None, "the log names the msiexec result")
            s.info(f"mock release requests: {srv.hits}")
        with t.step("The failure is shown: toast, Updates page, spb self-update check") as s:
            w = u.wait_window(120)
            s.check(w is not None, "Superba running again (the old version)")
            if u.exists(timeout=5, key="wizard.skip"):
                # Notices wait for the main window: the setup wizard of an unconfigured install covers it.
                s.info("the relaunched Superba shows the setup wizard (never configured): Skip setup")
                u.click(key="wizard.skip")
            toast = u.has_text(u.R("updates.superba.problem_failed"), timeout=30)
            s.check(bool(toast), f"failure shown: {toast!r}")
            s.shot("failure-toast")
            u.superba("updates")
            time.sleep(3)
            u.update_row("updates.superba.title")
            s.shot("updates-problem")
            s.ui_dump("updates-problem")
            s.check(u.exists(key="updates.superba.open_log"), "'Open the log' on the Updates page", soft=True)
            r = run.spb("self-update", "check", timeout=120, label="self-update-check-after")
            s.save("check-after.txt", r["stdout"])
            s.check("failed" in r["stdout"].lower() and "selfupdate.log" in r["stdout"],
                    "spb self-update check reports the failed update and the log")
            r = run.agent.exec(run.spb_exe, ["--version"], timeout=60)
            s.check(run.state.get("version", "") in r["stdout"], f"still the installed version: {r['stdout'].strip()}")
    finally:
        srv.shutdown()
        tun.terminate()
        run.vm.ps("[Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_API', $null, 'User');"
                  " [Environment]::SetEnvironmentVariable('SUPERBA_SELF_UPDATE_DOWNLOADS', $null, 'User')", check=False)


@scenario("W72", "Windows on ARM64 (not testable here)", area="accounts", manual_only=True)
def w72(t):
    t.skip("manual notes only: MATRIX.md W72")


