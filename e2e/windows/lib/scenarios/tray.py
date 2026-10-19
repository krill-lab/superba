"""The tray icon and "close to tray" (item 70, GH#10; MATRIX.md W64)."""

import json
import time

import flows
from agent import AgentError
from runner import scenario
from ui import Ui

# The icon is named by its tooltip ("Superba · <provider> · <line>"); the
# taskbar button of the window is "Superba - 1 running window".
TRAY_NAME_RE = r"^Superba · "


def taskbar(run):
    """The taskbar window (Shell_TrayWnd) of Explorer."""
    for w in run.agent.windows(process_re="^explorer$", all=True):
        if w["class"] == "Shell_TrayWnd":
            return w
    return None


def promote_icon(run):
    """Show Superba's icon on the taskbar instead of the overflow (what Settings >
    Personalization > Taskbar > Other system tray icons does)."""
    return run.guest_ps(r"""
$n = 0
Get-ChildItem 'HKCU:\Control Panel\NotifyIconSettings' -ErrorAction SilentlyContinue | ForEach-Object {
  $p = Get-ItemProperty $_.PSPath
  if ($p.ExecutablePath -like '*\superba.exe') {
    Set-ItemProperty $_.PSPath -Name IsPromoted -Value 1 -Type DWord; $n++
  }
}
"promoted=$n"
""").strip()


def tray_icon(run, timeout=20):
    """The icon's UIA element in the notification area (taskbar or overflow)."""
    end = time.time() + timeout
    while time.time() < end:
        bar = taskbar(run)
        if bar:
            try:
                found = run.agent.call("uia.find", hwnd=bar["hwnd"], name_re=TRAY_NAME_RE, raw=True)
            except AgentError:
                found = []
            if found:
                return bar, found[0]
        time.sleep(1)
    return None, None


def menu_window(run, timeout=10):
    """The open tray menu (a Win32 popup menu, class #32768)."""
    end = time.time() + timeout
    while time.time() < end:
        ws = [w for w in run.agent.windows(all=True) if w["class"] == "#32768" and w["visible"]]
        if ws:
            return ws[0]
        time.sleep(0.3)
    return None


def open_menu(run, s):
    bar, icon = tray_icon(run)
    s.check(icon is not None, f"tray icon found: {icon and icon['name']!r}")
    run.agent.call("uia.act", hwnd=bar["hwnd"], name=icon["name"], raw=True, action="right_click")
    m = menu_window(run)
    s.check(m is not None, "a right click opens the tray menu")
    return m


def menu_items(run, m):
    items = run.agent.call("uia.find", hwnd=m["hwnd"], type="MenuItem", raw=True)
    return [(i["name"], i["enabled"]) for i in items]


def superba_visible(run):
    return any(w["visible"] and w["title"] for w in run.agent.windows(process_re="^superba$", all=True))


def wait(cond, timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        if cond():
            return True
        time.sleep(0.5)
    return False


@scenario("W64", "Tray icon: close to tray (asked once), the tray menu, a second start, quit from the tray",
          area="gui", needs=("configured",), bugs=(70,))
def w64(t):
    run = t.run
    u = Ui(run)
    with t.step("Superba shows its icon in the notification area") as s:
        flows.start_superba(run, s)
        time.sleep(2)
        s.info(promote_icon(run))
        time.sleep(2)
        bar, icon = tray_icon(run)
        s.check(icon is not None, f"tray icon in the notification area: {icon and icon['name']!r}")
        s.info(f"tooltip / name: {icon['name']!r}")
        s.shot("taskbar", full=True)
        s.ui_dump("taskbar", hwnd=bar["hwnd"])
        u.superba("general")
        u.reveal(key="tray.prefs.close.title", type=None)
        s.shot("general-close-behavior")
        s.ui_dump("general-close-behavior")
        # The router runs as its own service (HKCU Run) whatever the window does.
        r = run.spb("router", "install", "--member", "krill", timeout=180, label="router-install")
        s.info(f"spb router install rc={r.get('code')}")
        s.check(any(p["name"].lower().startswith("superba-router")
                    for p in run.agent.procs(r"^superba-router\.exe$")), "superba-router.exe runs")

    with t.step("Closing the window asks once; 'Keep running in the tray' hides it") as s:
        run.agent.call("window", process_re="^superba$", close=True)
        s.check(u.exists(timeout=10, key="tray.prompt.title"), "the first close asks")
        s.shot("first-close")
        s.ui_dump("first-close")
        u.click(key="tray.prompt.keep")
        s.check(wait(lambda: not superba_visible(run)), "the window is hidden")
        s.check(bool(run.agent.procs(r"^superba\.exe$")), "superba.exe keeps running")
        prof = run.guest_json(run.guest["DATA"] + r"\profiles.json") or {}
        s.check((prof.get("settings") or {}).get("close_action") == "tray", "close_action = tray saved")
        s.shot("hidden", full=True)

    with t.step("The tray menu (UI Automation on the popup menu)") as s:
        m = open_menu(run, s)
        items = menu_items(run, m)
        s.save("tray-menu.json", items)
        names = [n for n, _ in items]
        for key in ("tray.menu.open", "tray.menu.switch", "tray.menu.quit"):
            s.check(u.L(key) in names, f"menu item {u.L(key)!r}")
        s.check(any(n.startswith("Router: ") for n in names), "the router's state is listed")
        s.shot("tray-menu", full=True)
        run.agent.call("uia.act", hwnd=m["hwnd"], name=u.L("tray.menu.open"), type="MenuItem", raw=True)
        s.check(wait(lambda: superba_visible(run)), "'Open Superba' shows the window")
        s.shot("shown-from-menu")

    with t.step("Closing again hides at once; a click on the icon and a second start show it") as s:
        run.agent.call("window", process_re="^superba$", close=True)
        s.check(wait(lambda: not superba_visible(run)), "hidden without asking again")
        s.check(not u.exists(timeout=2, key="tray.prompt.title"), "no question the second time")
        bar, icon = tray_icon(run)
        run.agent.call("uia.act", hwnd=bar["hwnd"], name=icon["name"], raw=True, action="click")
        s.check(wait(lambda: superba_visible(run)), "a left click on the icon shows the window")
        run.agent.call("window", process_re="^superba$", close=True)
        s.check(wait(lambda: not superba_visible(run)), "hidden again")
        before = [p["pid"] for p in run.agent.procs(r"^superba\.exe$")]
        r = run.agent.exec(run.superba_exe, [], timeout=60)
        s.info(f"second start exit {r.get('code')} in {r.get('ms')} ms")
        s.check(wait(lambda: superba_visible(run)), "a second start shows the running window")
        after = [p["pid"] for p in run.agent.procs(r"^superba\.exe$")]
        s.check(after == before, f"still one Superba process ({before} -> {after})")

    with t.step("Quit Superba from the tray: Superba exits, the router keeps running") as s:
        run.agent.call("window", process_re="^superba$", close=True)
        wait(lambda: not superba_visible(run))
        m = open_menu(run, s)
        run.agent.call("uia.act", hwnd=m["hwnd"], name=u.L("tray.menu.quit"), type="MenuItem", raw=True)
        s.check(wait(lambda: not run.agent.procs(r"^superba\.exe$"), timeout=20), "superba.exe exited")
        s.check(bool(run.agent.procs(r"^superba-router\.exe$")), "superba-router.exe still runs")
        bar, icon = tray_icon(run, timeout=3)
        s.check(icon is None, "the tray icon is gone")
        s.shot("after-quit", full=True)
        run.state.pop("superba", None)
        s.save("procs.json", json.dumps(run.agent.procs(r"^superba"), indent=1))
