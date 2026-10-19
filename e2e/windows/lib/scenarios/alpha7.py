"""Alpha.7 tool and notification gates (W91-W101).

The VM snapshot owns all files touched here. New scenarios are registered now;
run them only after the tool editor, unified update list and installer are in
the MSI. A missing dependency is SKIP, never a successful feature check.
"""

import base64
import json
import re
import time

import flows
from runner import scenario
from ui import Ui
from vm import psq


def require_alpha7(t):
    u = Ui(t.run)
    missing = [key for key in ("tools.apply", "updates.list.all") if not u.s.has(key)]
    if missing:
        t.skip("waiting for M3, M4 and t1 integration; missing UI contracts: " + ", ".join(missing))
    flows.ensure(t.run, t, "installed")
    r = t.run.spb("tools", "--help", label="tools-dependency")
    if r["code"] != 0:
        t.skip("waiting for t1: the MSI has no spb tools command")
    return u


def write_guest(run, path, data):
    """Exact bytes, including CRLF, through the unelevated guest agent."""
    encoded = base64.b64encode(data).decode("ascii")
    run.guest_ps(f"$p={psq(path)}; New-Item -ItemType Directory -Force -Path (Split-Path $p) | Out-Null; "
                 f"[IO.File]::WriteAllBytes($p, [Convert]::FromBase64String('{encoded}'))")


def file_hashes(run, paths):
    expr = ",".join(psq(p) for p in paths)
    return json.loads(run.guest_ps(
        f"$o=@{{}}; foreach($p in @({expr})) {{ $o[$p] = if(Test-Path -LiteralPath $p) "
        "{ (Get-FileHash -LiteralPath $p -Algorithm SHA256).Hash } else { $null } }; "
        "$o | ConvertTo-Json -Compress"))


def harness_status(run, s, tool, label):
    r = run.spb_json("harness", "status", tool, label=label)
    s.check(r["code"] == 0 and isinstance(r.get("json"), dict), f"{tool}: status is readable")
    view = r["json"]
    s.save(f"{label}.json", view)  # the view contains paths and owned slot names, no key values
    return view


def select_tool(u, name):
    u.page("tools")
    u.act("invoke", name_re="^" + re.escape(name) + r",", type="ListItem")
    u.wait(key="tools.apply", type="Button", enabled=True, timeout=60)


def confirm_tool(u, restore=False, overwrite=False):
    u.click(key="tools.restore" if restore else "tools.apply")
    u.wait(key="tools.review_restore" if restore else "tools.review_apply", timeout=60)
    if overwrite:
        u.act("toggle", key="tools.take_over", type="CheckBox")
    u.click(key="tools.confirm")
    u.wait_gone(key="tools.confirm", type="Button", timeout=60)
    u.wait(key="tools.apply", type="Button", enabled=True, timeout=60)


def tool_config(t, tool, name):
    u = require_alpha7(t)
    run = t.run
    flows.ensure(run, t, "configured")
    with t.step(f"{name}: apply the active Krill provider from Tools") as s:
        before = harness_status(run, s, tool, "before")
        paths = [f["path"] for f in before["files"]]
        s.check(bool(paths), "configuration paths are listed")
        baseline = file_hashes(run, paths)
        s.save("baseline-hashes.json", baseline)
        select_tool(u, name)
        confirm_tool(u)
        after = harness_status(run, s, tool, "applied")
        s.check((after.get("applied") or {}).get("provider") == "krill", "GUI applied Krill")
        s.check(not (after.get("applied") or {}).get("drift"), "no drift after applying")
        s.check(file_hashes(run, paths) != baseline, "application wrote the tool's configuration")
        s.shot("tool-applied")
        s.ui_dump("tool-applied")
    with t.step(f"{name}: detect a manual change in CRLF configuration") as s:
        changed = []
        for f in after["files"]:
            content = run.guest_read(f["path"])
            if content and run.net.key in content:
                data = content.replace(run.net.key, "q1-manual-value").replace("\r\n", "\n").replace("\n", "\r\n").encode()
                write_guest(run, f["path"], data)
                changed.append(f["path"])
        s.check(bool(changed), "changed an owned key in the tool's files (values omitted)")
        drift = harness_status(run, s, tool, "drift")
        s.check(bool((drift.get("applied") or {}).get("drift")), "CLI detected the changed owned entry")
        u.click(key="tools.reload")
        s.check(bool(u.has_text(r"^" + re.escape(name) + r",.*" + re.escape(u.L("tools.changed")), timeout=60)),
                "the tool row reports File changed")
        s.shot("tool-drift")
        s.ui_dump("tool-drift")
        u.click(key="tools.apply")
        u.wait(key="tools.drift_hint", timeout=60)
        s.check(u.exists(key="tools.take_over", type="CheckBox"), "overwriting manual edits is an explicit choice")
        u.click(key="tools.cancel")
    with t.step(f"{name}: restore the original configuration") as s:
        confirm_tool(u, restore=True, overwrite=True)
        restored = harness_status(run, s, tool, "restored")
        s.check(not restored.get("applied"), "Superba no longer owns applied entries")
        s.check(file_hashes(run, paths) == baseline, "original file bytes / absence restored")
        s.shot("tool-restored")


for number, tool, name in ((91, "opencode", "opencode"), (92, "pi", "pi"),
                           (93, "zcode", "ZCode"), (94, "workbuddy", "WorkBuddy")):
    scenario(f"W{number}", f"Tools: {name} apply, detect drift and restore", area="tools", net="mock")(
        lambda t, tool=tool, name=name: tool_config(t, tool, name))


@scenario("W95", "Notifications: installed hook commands execute in Windows PowerShell, including paths with spaces",
          area="notify", net="mock")
def w95(t):
    u = require_alpha7(t)
    run = t.run
    flows.ensure(run, t, "configured")
    with t.step("Superba > Notifications exposes its settings") as s:
        u.superba("notify")
        s.check(u.exists(key="prefs.notify.enabled.title"), "notification switch is visible")
        s.shot("notifications")
        s.ui_dump("notifications")
    for folder in ("q1-hooks", "q1 hooks with spaces"):
        with t.step(f"PowerShell runs generated hooks from {folder}") as s:
            root = run.guest["LOCALAPPDATA"] + "\\" + folder
            program = root + r"\spb.exe"
            run.guest_ps(f"New-Item -ItemType Directory -Force -Path {psq(root)} | Out-Null; "
                         f"Copy-Item -LiteralPath {psq(run.spb_exe)} -Destination {psq(program)}")
            env = {"SUPERBA_NOTIFY_PROGRAM": program, "CODEX_HOME": root + r"\codex"}
            r = run.spb("notify", "install", "codex", "--approvals", env=env, label="install-test-hook")
            s.check(r["code"] == 0, "Codex approval hook installed in an isolated test directory")
            r = run.spb_json("notify", "status", env=env, label="hook-commands")
            s.check(r["code"] == 0 and r.get("json"), "generated hook commands are readable")
            hooks = r["json"]["hooks"]["hooks"]
            commands = [h["command"][0] for h in hooks if h.get("command") and len(h["command"]) == 1]
            s.check(len(commands) >= 2, "Codex approvals and Claude Code lifecycle commands returned")
            for command in commands:
                # Run exactly what Superba generated. Adding '&' here would mask a quoting bug.
                payload = json.dumps({"hook_event_name": "Stop", "session_id": "q1 quote test",
                                      "last_assistant_message": 'spaces, "quotes" and 中文'})
                script = ("$ErrorActionPreference='Stop'; " + psq(payload) + " | " + command +
                          "; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }")
                result = run.agent.exec("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", script],
                                        env=env, timeout=60)
                s.save("powershell-hook.json", {"command": command, **result})
                s.check(result["code"] == 0 and not result["timed_out"],
                        f"PowerShell executes generated command: {command}")


def tool_row(run, s, tool, label):
    r = run.spb_json("tools", "list", label=label, timeout=180)
    s.check(r["code"] == 0 and r.get("json"), "spb tools list is readable")
    row = next((r for r in r["json"]["rows"] if r["id"] == tool), None)
    s.check(row is not None, f"{tool} listed")
    s.save(f"{label}.json", row)
    return row


def wait_install(run, s, tool, installed, timeout=900):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        row = tool_row(run, s, tool, "inventory-poll")
        if bool(row["installs"]) == installed:
            return row
        time.sleep(5)
    s.check(False, f"{tool}: installed={installed} not reached within {timeout}s")


def tool_install(t, tool, name):
    u = require_alpha7(t)
    run = t.run
    flows.ensure(run, t, "configured")
    with t.step(f"{name}: install from Superba > Updates without elevation") as s:
        baseline = tool_row(run, s, tool, "before-install")
        s.check(not baseline["installs"], "test snapshot starts without this tool")
        u.superba("updates")
        action = f"{u.L('updates.list.install')} {name}"
        u.reveal(name=action, type="Button")
        u.click(name=action)
        u.wait(key="updates.list.confirm")
        s.shot("install-review")
        u.click(key="updates.list.start")
        installed = wait_install(run, s, tool, True)
        s.check(bool(installed["installs"][0].get("version")), "installed version matches the CLI inventory")
        u.reveal(name_re="^" + re.escape(name) + r",", type="Button")
        s.shot("tool-installed")
        s.ui_dump("tool-installed")
        s.manual("No UAC prompt; ZCode/WorkBuddy installer signature verified in the operation log. "
                 "For dsh, repeat on a snapshot with no Node and confirm the standalone variant.")
    with t.step(f"{name}: uninstall from the same update row, keeping user data") as s:
        row_selector = {"name_re": "^" + re.escape(name) + r",", "type": "Button"}
        u.reveal(**row_selector)
        u.act("expand", **row_selector)
        u.reveal(key="updates.list.remove", type="Button")
        u.click(key="updates.list.remove")
        u.wait(key="updates.list.confirm_remove")
        for checkbox in u.find(key="updates.list.remove_data", type="CheckBox"):
            s.check(checkbox.get("toggle") in ("Off", 0, False), "remove user data is off by default")
        s.shot("uninstall-review")
        u.click(key="updates.list.remove")
        wait_install(run, s, tool, False)
        s.shot("tool-uninstalled")
        s.ui_dump("tool-uninstalled")


for number, tool, name in ((96, "claude", "Claude Code"), (97, "opencode", "opencode"),
                           (98, "pi", "pi"), (99, "dsh", "dsh"),
                           (100, "zcode", "ZCode"), (101, "workbuddy", "WorkBuddy")):
    scenario(f"W{number}", f"Superba > Updates: install and uninstall {name}", area="tools")(
        lambda t, tool=tool, name=name: tool_install(t, tool, name))
