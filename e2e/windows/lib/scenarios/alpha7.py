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
from ui import Ui, UiError
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


def tool_log(run, tool):
    return run.guest_read(run.guest["DATA"] + "\\logs\\tools\\" + tool + ".log") or ""


def wait_install(run, s, tool, installed, timeout=900, before_log=""):
    """File discovery can precede both --version and the operation's result."""
    end = time.monotonic() + timeout
    operation = "install" if installed else "uninstall"
    started = False
    start_deadline = min(end, time.monotonic() + 60)
    before_header = before_log.splitlines()[:1]
    last_log = None
    while time.monotonic() < end:
        log = tool_log(run, tool)
        if log != last_log:
            s.save(f"{tool}-{operation}-operation.log", log)
            last_log = log
        current = (log.splitlines()[:1] != before_header
                   and re.match(r"^\S+ " + operation + r" .+ \(channel ", log))
        if current:
            if not started:
                s.save(f"{tool}-{operation}-started.log", log)
                started = True
            s.check(not re.search(r"(?m)^\S+ (?:failed|refused|stopped):", log),
                    f"{tool}: {operation} has no terminal failure (see operation log)")
        if not started and time.monotonic() >= start_deadline:
            raise UiError(f"{tool}: no new {operation} operation started within 60s")
        row = tool_row(run, s, tool, "inventory-poll")
        ready = bool(row["installs"]) == installed
        if installed:
            ready = ready and all(str(i.get("version") or "").strip() for i in row["installs"])
        if ready and current and re.search(r"(?m)^\S+ done:", log):
            return row
        time.sleep(5)
    s.check(False, f"{tool}: installed={installed} with complete version probes and a successful "
                  f"{operation} not reached within {timeout}s")


def wait_tool_idle(u, s, name, installed, timeout=60):
    """The GUI can still be finishing an installer after CLI discovery succeeds."""
    end = time.monotonic() + timeout
    selector = {"name_re": "^" + re.escape(name) + r",", "type": "Button"}
    u.reveal(**selector)
    states = ("current", "available", "vendor", "unknown") if installed else ("missing",)
    ready = re.compile("^" + re.escape(name) + r", (?:" +
                       "|".join(re.escape(u.L("updates.list." + k)) for k in states) + r")(?:,|$)")
    failed = name + ", " + u.L("updates.list.failed") + ","
    while True:
        rows = u.find(**selector)
        if any(row["name"].startswith(failed) for row in rows):
            s.save("tool-row-failed.json", rows)
            raise UiError(f"{name}: GUI reports an operation failure")
        if len(rows) == 1 and ready.search(rows[0]["name"]):
            s.save("tool-row-ready.json", rows[0])
            return
        if time.monotonic() >= end:
            s.save("tool-row-not-ready.json", rows)
            s.ui_dump("tool-row-not-ready")
            raise UiError(f"{name}: GUI did not finish the operation within {timeout}s")
        time.sleep(0.5)


def verify_tool_version(run, s, tool, row):
    """Compare a new execution (or PE metadata) with the detected version."""
    for index, item in enumerate(row["installs"]):
        version = str(item.get("version") or "").removeprefix("v")
        s.check(bool(version), f"{tool}: inventory contains the installed version")
        path = item["path"]
        if tool in ("claude", "opencode", "pi", "dsh"):
            command = "& " + psq(path) + " --version; exit $LASTEXITCODE"
            result = run.agent.exec("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", command],
                                    timeout=120)
            s.save(f"{tool}-actual-version-{index}.json", result)
            s.check(result["code"] == 0 and not result["timed_out"],
                    f"{tool}: actual installed program returns its version")
            pattern = r"(?<![\w.+-])v?" + re.escape(version) + r"(?![\w.+-])"
            s.check(re.search(pattern, result["stdout"] + "\n" + result["stderr"]) is not None,
                    f"{tool}: executed version agrees with detected version {version}")
        else:
            data = json.loads(run.guest_ps(
                "$f=Get-Item -LiteralPath " + psq(path) + "; "
                "@{file_version=$f.VersionInfo.FileVersion;product_version=$f.VersionInfo.ProductVersion} "
                "| ConvertTo-Json -Compress"))
            s.save(f"{tool}-actual-version-{index}.json", data)
            expected = re.findall(r"\d+", version)
            s.check(bool(expected) and any(re.findall(r"\d+", str(v or ""))[:len(expected)] == expected
                                          for v in data.values()),
                    f"{tool}: actual PE file/product version agrees with detected version")


def confirm_or_observe_install(u, s, name, timeout=20):
    """Uncautioned installs start directly; review only an actual install dialog."""
    end = time.monotonic() + timeout
    installing = "^" + re.escape(f"{name}, {u.L('updates.list.installing')}") + r"(?:,|$)"
    while True:
        modal = u.modal("updates.list.confirm")
        review = u.find(**modal)
        if review:
            job = u.wait(name_re="^" + re.escape(name + " · "), type="Text",
                         within=modal, timeout=max(0, end - time.monotonic()))
            s.save("install-review.json", {"title": review, "job": job})
            s.shot("install-review")
            u.click(key="updates.list.start", within=modal)
            u.wait_gone(**modal, timeout=timeout)
            return
        rows = u.find(name_re=installing, type="Button")
        if rows:
            s.save("install-started.json", rows)
            s.shot("install-started")
            return
        if time.monotonic() >= end:
            raise UiError(f"{name}: neither its install review nor Installing row appeared within {timeout}s")
        time.sleep(0.25)


def confirm_removal(u, s, name, offers_data_removal=False, timeout=20):
    modal = u.modal("updates.list.confirm_remove")
    title = u.wait(**modal, timeout=timeout)
    job = u.wait(name_re="^" + re.escape(name + " · "), type="Text", within=modal, timeout=timeout)
    checkboxes = u.find(key="updates.list.remove_data", type="CheckBox", within=modal)
    s.check(not offers_data_removal or bool(checkboxes), "removable user data has an explicit removal choice")
    for checkbox in checkboxes:
        s.check(checkbox.get("toggle") in ("Off", 0, False), "remove user data is off by default")
    s.save("uninstall-review.json", {"title": title, "job": job, "remove_data": checkboxes})
    s.shot("uninstall-review")
    s.ui_dump("uninstall-review")
    # The expanded update row behind the modal has the very same button name.
    u.click(key="updates.list.remove", within=modal)
    u.wait_gone(**modal, timeout=timeout)


def tool_data_hashes(run, folders):
    """Observe the existing opt-in removal directories without changing them."""
    if not folders:
        return {}
    paths = ",".join(psq(p) for p in folders)
    return json.loads(run.guest_ps(
        "$ErrorActionPreference='Stop'; $o=@{}; foreach($p in @(" + paths + ")) { "
        "$o[$p]=[bool](Test-Path -LiteralPath $p); "
        "if ($o[$p]) { Get-ChildItem -LiteralPath $p -Recurse -File -Force | ForEach-Object { "
        "$o[$_.FullName]=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash } } }; "
        "$o | ConvertTo-Json -Compress"))


def tool_install(t, tool, name):
    u = require_alpha7(t)
    run = t.run
    flows.ensure(run, t, "configured")
    with t.step(f"{name}: install from Superba > Updates without elevation") as s:
        if tool == "dsh":
            node = run.agent.exec("where.exe", ["node"], timeout=30)
            s.check(node["code"] != 0, "dsh standalone case: Node is absent from the snapshot PATH")
        baseline = tool_row(run, s, tool, "before-install")
        s.check(not baseline["installs"], "test snapshot starts without this tool")
        u.superba("updates")
        action = f"{u.L('updates.list.install')} {name}"
        u.reveal(name=action, type="Button")
        before_log = tool_log(run, tool)
        u.click(name=action)
        confirm_or_observe_install(u, s, name)
        installed = wait_install(run, s, tool, True, before_log=before_log)
        s.check(bool(installed["installs"][0].get("version")), "installed version matches the CLI inventory")
        verify_tool_version(run, s, tool, installed)
        wait_tool_idle(u, s, name, True)
        s.shot("tool-installed")
        s.ui_dump("tool-installed")
        s.manual("No UAC prompt; ZCode/WorkBuddy installer signature verified in the operation log.")
    with t.step(f"{name}: uninstall from the same update row, keeping user data") as s:
        data_dirs = installed.get("data_dirs", [])
        before_data = tool_data_hashes(run, data_dirs)
        s.save("user-data-before.json", before_data)
        row_selector = {"name_re": "^" + re.escape(name) + r",", "type": "Button"}
        u.reveal(**row_selector)
        u.act("expand", **row_selector)
        u.reveal(key="updates.list.remove", type="Button")
        before_log = tool_log(run, tool)
        u.click(key="updates.list.remove")
        confirm_removal(u, s, name, offers_data_removal=tool == "opencode" and bool(data_dirs))
        wait_install(run, s, tool, False, before_log=before_log)
        wait_tool_idle(u, s, name, False)
        after_data = tool_data_hashes(run, data_dirs)
        s.save("user-data-after.json", after_data)
        s.check(before_data == after_data, "existing user data is unchanged after uninstall")
        s.shot("tool-uninstalled")
        s.ui_dump("tool-uninstalled")


for number, tool, name in ((96, "claude", "Claude Code"), (97, "opencode", "opencode"),
                           (98, "pi", "pi"), (99, "dsh", "dsh"),
                           (100, "zcode", "ZCode"), (101, "workbuddy", "WorkBuddy")):
    scenario(f"W{number}", f"Superba > Updates: install and uninstall {name}", area="tools")(
        lambda t, tool=tool, name=name: tool_install(t, tool, name))
