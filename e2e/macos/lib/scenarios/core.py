"""macOS equivalents of the Windows desktop, routing and restoration flows."""

import json
import time
import tomllib
from urllib.parse import urlsplit

from e2e.shared import desktop_flows

from ..runner import DATA, scenario
from ..vm import Blocked


@scenario("M01", "DMG install and first-run wizard", "W01/W02/W04")
def install_wizard(t):
    t.install()
    ui = t.launch()
    desktop_flows.wizard_detect(ui)
    t.shot("first-run")
    if t.rpc("desktop_installed"):
        # The current product can start an installed app through LaunchServices
        # without carrying an externally supplied HOME. Don't expose real data.
        raise Blocked("Wizard Apply could launch an installed desktop app outside the test HOME")
    base, key = t.endpoint()
    desktop_flows.wizard_custom(ui, "Mac e2e", base, key)
    t.check(bool(t.read("home/.codex/config.toml")), "Wizard writes the temporary Codex configuration")
    t.check(base in t.read("home/.codex/config.toml"), "Wizard applies the chosen endpoint")
    t.check(key not in (t.read(DATA + "/profiles.json") or ""), "Profile metadata contains no raw key")
    t.shot("wizard-applied")
    t.spb("restore", "original", "--yes", "--write-only")
    t.check(base not in (t.read("home/.codex/config.toml") or ""), "Restore removes the test endpoint")


@scenario("M02", "Line switching with an installed ChatGPT/Codex desktop app", "W10/W11")
def desktop_switch(t):
    t.install()
    if not t.rpc("desktop_installed"):
        t.launch()
        desktop_flows.wizard_detect(t.ui)
        t.shot("desktop-not-installed")
        raise Blocked("ChatGPT/Codex desktop app is not installed; no app was downloaded")
    t.baseline()
    first, first_url, _ = t.provider("mac-a")
    second, second_url, _ = t.provider("mac-b", (t.start_fixture() + "/alternate/v1", "sk-e2e-macos-second"))
    t.runner.redactor.add("sk-e2e-macos-second")
    t.spb("switch", "--profile", first, "--yes", "--write-only")
    desktop = t.rpc("desktop")  # BLOCKED if absent; never installs a desktop app.
    t.save("desktop.json", desktop)
    ui = t.configured_ui()
    ui.page("providers")
    t.shot("before-switch")
    # A full relaunch through LaunchServices can discard an externally supplied HOME.
    # Never grant full-restart permission to this externally started, isolated app.
    preview = t.spb("switch", "--profile", second, "--dry-run", "--json", ok=False)
    plan = json.loads(preview["stdout"]) if preview["code"] == 0 else {}
    kind = plan.get("plan", {}).get("primary", {}).get("kind")
    if preview["code"] or kind != "app_server_hot":
        raise Blocked("The isolated desktop requires a full restart; this build cannot preserve its test HOME")
    result = t.spb("switch", "--profile", second, "--json", ok=False)
    if result["code"] != 0:
        raise Blocked("Desktop switch needs a managed, isolated hot-switch session; full restart was not authorized")
    outcome = json.loads(result["stdout"])["outcome"]
    t.check(outcome["status"] == "switched" and outcome["restart"]["kind"] == "app_server_hot",
            "Product reports an actual verified hot restart")
    checks = outcome.get("verification") or {}
    for check in ("process", "files", "renderer"):
        t.check(checks.get(check, {}).get("kind") == "passed", "Hot-switch verification: " + check)
    config = t.read("home/.codex/config.toml") or ""
    t.check(second_url in config and first_url != second_url, "Second line is written to Codex")
    processes = t.rpc("procs")
    t.check(any(row["pid"] == desktop["pid"] for row in processes), "Hot switch keeps the isolated desktop process")
    t.save("processes-after.json", processes)
    t.shot("after-switch")


@scenario("M03", "Router on, request, off and LaunchAgent removal", "W30/W33/W36")
def router_on_off(t):
    t.install()
    t.baseline()
    name, base, _ = t.provider()
    t.spb("switch", "--profile", name, "--yes", "--write-only")
    t.spb("router", "enable", "--member", name, "--yes", "--write-only")
    config = t.read("home/.codex/config.toml") or ""
    router = json.loads(t.read(DATA + "/router/config.json"))
    active = urlsplit(tomllib.loads(config).get("openai_base_url", ""))
    t.check(active.hostname == "127.0.0.1" and active.port == router["listen_port"],
            "Codex active endpoint uses the actual router listening port")
    service = t.rpc("service")
    t.check(service["code"] == 0 and t.root in service["stdout"], "Owned LaunchAgent is loaded")
    t.save("router-service.json", service)
    ui = t.configured_ui()
    desktop_flows.router_page(ui)
    t.shot("router-on")
    if t.runner.opts.net == "mock":
        result = t.rpc("router_request")
        t.check(result["id"] == "resp_macos_fixture", "A request travels through the real router to the fixture")
        t.save("router-request.json", result)
    t.spb("router", "disable", "--uninstall", "--yes", "--write-only")
    direct = tomllib.loads(t.read("home/.codex/config.toml") or "").get("openai_base_url")
    t.check(direct == base, "Router off restores the exact active direct endpoint")
    t.check(t.rpc("service")["code"] != 0, "LaunchAgent is unloaded")
    t.check(t.rpc("port_closed", port=router["listen_port"]), "Router listening port is closed")
    t.shot("router-off-before-restart")
    # Capture the externally changed configuration in a fresh GUI process.
    t.rpc("stop")
    ui = t.configured_ui()
    desktop_flows.router_page(ui)
    if t.runner.opts.net == "real":
        t.check(ui.exists(key="router.banner.off", timeout=15), "Cold-start GUI shows router off")
    else:
        t.save("ui-mode-limitation.txt", "This build identifies any HTTP 127.0.0.1 provider as router mode. "
               "The mock upstream is itself loopback, so its UI banner is ambiguous. "
               "The exact active endpoint, unloaded job and closed router port are asserted above. "
               "Use --net real for the GUI off-banner assertion.\n")
    t.shot("after-router-disable")


@scenario("M04", "Claude Code configuration apply and exact restore", "Claude harness")
def claude_apply_restore(t):
    t.install()
    original = {"env": {"ANTHROPIC_BASE_URL": "https://original.invalid", "E2E_KEEP": "present"},
                "permissions": {"allow": ["Read"]}}
    t.write("home/.claude/settings.json", json.dumps(original, indent=2) + "\n")
    name, base, key = t.provider()
    endpoint = base.removesuffix("/v1")
    t.spb("claude", "endpoint", endpoint, "--provider", name, "--auth", "bearer")
    t.spb("claude", "provider", name)
    t.spb("claude", "route", "direct")
    t.spb("claude", "slot", "sonnet", "e2e-model")
    t.spb("claude", "apply", "--yes")
    written = json.loads(t.read("home/.claude/settings.json"))
    env = written["env"]
    t.check(env.get("ANTHROPIC_BASE_URL") == endpoint, "Claude endpoint applied")
    t.check(env.get("ANTHROPIC_AUTH_TOKEN") == key, "Claude key applied in temporary HOME only")
    t.check(env.get("ANTHROPIC_DEFAULT_SONNET_MODEL") == "e2e-model", "Claude Sonnet slot applied")
    t.check(env.get("E2E_KEEP") == "present" and written["permissions"] == original["permissions"],
            "Unrelated Claude settings preserved")
    ui = t.configured_ui()
    desktop_flows.claude_page(ui)
    t.shot("claude-applied")
    t.spb("claude", "restore", "--yes")
    restored = json.loads(t.read("home/.claude/settings.json"))
    t.check(restored == original, "Original Claude settings restored without managed keys")
    t.rpc("stop")
    ui = t.configured_ui()
    desktop_flows.claude_page(ui)
    t.shot("claude-restored")


@scenario("M05", "Notification hooks, local delivery and removal", "GH#12")
def notifications(t):
    t.install()
    t.baseline()
    base = t.start_fixture()
    token = "123456:e2e-not-a-real-token"
    t.runner.redactor.add(token)
    t.spb("notify", "channels", "add", "telegram", "--name", "loopback", "--param", "chat_id=4242",
          "--param", "api_base=" + base, "--secret-env", "bot_token=E2E_NOTIFY_TOKEN",
          env={"E2E_NOTIFY_TOKEN": token})
    t.spb("notify", "set", "--enabled", "on")
    original = t.read("home/.codex/config.toml")
    t.spb("notify", "install", "codex")
    config = tomllib.loads(t.read("home/.codex/config.toml"))
    t.check("notify" in config and "spb" in config["notify"][0], "Codex hook invokes the installed spb")
    fragment = json.loads(t.spb("notify", "install", "claude-code", "--json")["stdout"])
    hook = fragment["hooks"]["Stop"][0]["hooks"][0]
    t.check(hook["async"] is True, "Claude Stop hook is asynchronous")
    payload = {"hook_event_name": "Stop", "session_id": "mac-e2e", "cwd": "/test/project",
               "stop_hook_active": False, "last_assistant_message": "do-not-forward-this"}
    t.spb("notify", "hook", "claude-code", stdin=json.dumps(payload))
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline and not t.read("notifications.jsonl"):
        time.sleep(0.3)
    delivered = t.read("notifications.jsonl") or ""
    t.check("Task finished" in delivered and "do-not-forward-this" not in delivered,
            "Hook delivers to the loopback sink with summaries disabled")
    t.spb("notify", "uninstall", "codex")
    t.check(tomllib.loads(t.read("home/.codex/config.toml")) == tomllib.loads(original), "Original Codex notify state restored")
    ui = t.configured_ui()
    desktop_flows.notification_page(ui)
    t.shot("notifications")


@scenario("M06", "GUI login LaunchAgent survives a real reboot and is removed", "Login startup")
def autostart(t):
    if not t.runner.opts.allow_reboot:
        raise Blocked("M06 needs --allow-reboot and a VM with GUI auto-login")
    t.install()
    if t.spb("autostart", "--help", ok=False)["code"]:
        raise Blocked("This DMG predates spb autostart; use an integrated build")
    # The GUI embeds the CLI; this makes the product's executable discovery
    # independent of where the DMG's standalone spb was copied.
    t.spb("autostart", "on", gui=True)
    label = "com.krill-code.superba.autostart"
    plist = "home/Library/LaunchAgents/" + label + ".plist"
    t.check(bool(t.read(plist)), "Product generated a LaunchAgent plist")
    item = t.rpc("login_item", path=plist)
    t.save("login-item.json", item)
    before = t.rpc("service", label=label)["boot"]
    # Reboot, not logout or a simulated process restart. The run marker survives.
    try:
        t.runner.vm.shell("sudo -n /sbin/reboot", check=False, timeout=15)
    except Exception:
        # ssh may be disconnected before sudo can return; a changed boot time is required below.
        pass
    after = t.runner.vm.wait_reboot(before)
    t.check(after != before, "Kernel boot time changed")
    deadline = time.monotonic() + 60
    service = t.rpc("service", label=label)
    processes = t.rpc("login_state")
    while time.monotonic() < deadline and not processes:
        time.sleep(2)
        service = t.rpc("service", label=label)
        processes = t.rpc("login_state")
    t.save("service-after-reboot.json", service)
    t.save("gui-after-reboot.json", processes)
    # An /usr/bin/open LaunchAgent exits after submitting the launch. Require the
    # actual GUI and its HOME, not a permanently running launcher process.
    t.check(service["code"] == 0 and t.root in service["stdout"], "Product LaunchAgent was loaded at login")
    t.check(bool(processes) and all(process["isolated"] for process in processes),
            "Real login started the Superba GUI with the temporary HOME and CODEX_HOME")
    t.spb("autostart", "off", gui=True)
    t.check(not t.read(plist), "Autostart off removes the product registration")


@scenario("M07", "Data-location move and cold restart (GH#15)", "GH#15")
def data_location(t):
    t.install()
    help_result = t.spb("data", "--help", ok=False)
    if help_result["code"]:
        raise Blocked("This DMG predates spb data (GH#15); use an integrated build")
    # Test the persisted pointer, not SUPERBA_DATA_DIR overriding every restart.
    t.rpc("env", values={"SUPERBA_DATA_DIR": None})
    t.provider()
    before = t.spb("data", "where", "--json")["stdout"]
    t.save("data-before.json", before)
    destination = t.root + "/home/moved-data"
    t.spb("data", "move", destination)
    location = t.spb("data", "where", "--json")["stdout"]
    t.check(destination in location, "Fresh CLI process follows persisted data pointer")
    t.check(bool(t.read("home/moved-data/profiles.json")), "Provider data exists at destination")
    t.rpc("stop")
    ui = t.configured_ui()
    ui.superba("data")
    t.shot("data-moved")
    t.check("mac-test" in t.spb("profile", "list", "--json")["stdout"], "Providers survive a cold GUI/CLI start")


@scenario("M08", "Overwrite DMG upgrade preserves configuration and data", "MSI upgrade")
def upgrade(t):
    if not t.runner.opts.upgrade_dmg:
        raise Blocked("M08 requires --upgrade-dmg pointing to a second build")
    old = t.install()
    t.provider()
    t.write(DATA + "/e2e-preserved.txt", "preserve-on-upgrade\n")
    baseline = t.read(DATA + "/profiles.json")
    t.rpc("stop")
    new = t.install(t.runner.opts.upgrade_dmg, upgrade=True)
    t.check(old["dmg_sha256"] != new["dmg_sha256"], "Upgrade uses two distinct DMG artifacts")
    t.check(t.read(DATA + "/profiles.json") == baseline, "Provider configuration survives app replacement")
    t.check(t.read(DATA + "/e2e-preserved.txt") == "preserve-on-upgrade\n", "Existing data survives app replacement")
    t.configured_ui()
    t.shot("after-upgrade")


@scenario("M09", "Restore and uninstall leave no managed configuration", "W40/W41/W42")
def uninstall(t):
    t.install()
    t.baseline()
    original = t.read("home/.codex/config.toml")
    name, _, _ = t.provider()
    t.spb("switch", "--profile", name, "--yes", "--write-only")
    t.spb("restore", "original", "--yes", "--write-only")
    t.check(t.read("home/.codex/config.toml") == original, "Original Codex config is byte-identical")
    t.spb("uninstall", "--yes", "--write-only")
    t.check(t.rpc("service")["code"] != 0, "No router service remains")
    agents = t.rpc("list", path="home/Library/LaunchAgents")
    t.check(not agents, "No Superba LaunchAgent files remain in the test HOME")
    # The common finalizer removes the DMG/app/HOME and asserts all owned processes are gone.
