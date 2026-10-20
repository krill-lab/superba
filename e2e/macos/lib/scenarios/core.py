"""macOS equivalents of the Windows desktop, routing and restoration flows."""

import json
import time
import tomllib
from urllib.parse import urlsplit

from e2e.shared import desktop_flows

from ..runner import DATA, scenario
from ..ui import Ui
from ..vm import Blocked

GUI_CHECKS = {
    "M01": "First-run wizard AX detection, credential entry and Apply",
    "M02": "ChatGPT window, desktop hot-switch and renderer verification",
    "M03": "Router page and before/after UI banners",
    "M04": "Claude page before/after restoration",
    "M05": "Notification settings page",
    "M06": "Autostart window AX inspection (launchd/process checks still attempted)",
    "M07": "Data settings page after cold GUI restart",
    "M08": "App window after DMG replacement",
    "M09": "Restored configuration page before uninstall",
}


@scenario("M01", "DMG install and first-run wizard", "W01/W02/W04")
def install_wizard(t):
    t.install()
    if not t.gui:
        t.baseline()
        original = t.read("home/.codex/config.toml")
        name, base, key = t.provider()
        t.spb("switch", "--profile", name, "--yes", "--write-only")
        t.check(base in t.read("home/.codex/config.toml"), "CLI applies the chosen endpoint")
        t.check(key not in (t.read(DATA + "/profiles.json") or ""), "Profile metadata contains no raw key")
        t.spb("restore", "original", "--yes", "--write-only")
        t.check(t.read("home/.codex/config.toml") == original, "CLI restores the original configuration")
        t.shot("cli-install")
        return
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
    t.desktop_prerequisite()
    if not t.gui:
        t.check(t.rpc("desktop_installed"), "Verified official ChatGPT is installed")
        t.baseline()
        first, first_url, _ = t.provider("mac-a")
        second, second_url, _ = t.provider("mac-b", (t.start_fixture() + "/alternate/v1", "sk-e2e-macos-second"))
        t.spb("switch", "--profile", first, "--yes", "--write-only")
        t.spb("switch", "--profile", second, "--yes", "--write-only")
        config = tomllib.loads(t.read("home/.codex/config.toml"))
        t.check(config.get("openai_base_url") == second_url and first_url != second_url,
                "CLI line switch writes the second exact endpoint without logging in")
        t.shot("cli-desktop-switch")
        return
    if not t.rpc("desktop_installed"):
        t.launch()
        desktop_flows.wizard_detect(t.ui)
        t.shot("desktop-not-installed")
        raise Blocked("ChatGPT/Codex desktop app is not installed; no app was downloaded")
    if t.runner.opts.net == "real":
        first_url, second_url = pat_desktop_lines(t)
        switch = ("switch", "--profile", "mac-pat", "--line", "b")
    else:
        t.baseline()
        first, first_url, _ = t.provider("mac-a")
        second, second_url, _ = t.provider("mac-b", (t.start_fixture() + "/alternate/v1", "sk-e2e-macos-second"))
        t.runner.redactor.add("sk-e2e-macos-second")
        t.spb("switch", "--profile", first, "--yes", "--write-only")
        switch = ("switch", "--profile", second)
    desktop = t.rpc("desktop")
    t.save("desktop.json", desktop)
    desktop_ui = Ui(t, process_re="^(ChatGPT|Codex)$", pid=desktop["pid"])
    desktop_ui.wait_window()
    t.ui = desktop_ui
    t.shot("desktop-before-switch")
    if t.runner.opts.net == "real":
        desktop_onboarding(t, desktop_ui)
        t.shot("desktop-ready")
    ui = t.configured_ui()
    ui.page("providers")
    t.shot("before-switch")
    # A full relaunch through LaunchServices can discard an externally supplied HOME.
    # Never grant full-restart permission to this externally started, isolated app.
    preview = t.spb(*switch, "--dry-run", "--json", ok=False)
    plan = json.loads(preview["stdout"]) if preview["code"] == 0 else {}
    kind = plan.get("plan", {}).get("primary", {}).get("kind")
    if preview["code"] or kind != "app_server_hot":
        raise Blocked("The isolated desktop requires a full restart; this build cannot preserve its test HOME")
    result = t.spb(*switch, "--json", ok=False)
    if result["code"] != 0:
        raise Blocked("Desktop switch needs a managed, isolated hot-switch session; full restart was not authorized")
    outcome = json.loads(result["stdout"])["outcome"]
    t.check(outcome["status"] == "switched" and outcome["restart"]["kind"] == "app_server_hot",
            "Product reports an actual verified hot restart")
    checks = outcome.get("verification") or {}
    for check in ("process", "files", "renderer"):
        t.check(checks.get(check, {}).get("kind") == "passed", "Hot-switch verification: " + check)
    config = tomllib.loads(t.read("home/.codex/config.toml") or "")
    t.check(config.get("openai_base_url") == second_url and first_url != second_url,
            "Second line is the exact active Codex endpoint")
    processes = t.rpc("procs")
    t.check(any(row["pid"] == desktop["pid"] for row in processes), "Hot switch keeps the isolated desktop process")
    t.save("processes-after.json", processes)
    t.shot("after-switch")
    t.ui = desktop_ui
    t.shot("desktop-after-switch")
    if t.runner.opts.net == "real":
        t.save("line-connections.jsonl", t.read("home/line-connections.jsonl") or "")


def pat_desktop_lines(t):
    t.credentials_used = True
    upstream = t.runner.secrets.get("SUPERBA_E2E_UPSTREAM")
    key = t.runner.secrets.get("SUPERBA_E2E_KEY")
    if not upstream or not key:
        raise Blocked("PAT desktop mode requires the designated test relay and credential")
    t.runner.redactor.add(urlsplit(upstream).hostname)
    lines = t.rpc("line_proxy", upstream=upstream.rstrip("/"))
    t.rpc("env", values={"HTTPS_PROXY": lines["proxy"], "HTTP_PROXY": lines["proxy"],
                         "ALL_PROXY": lines["proxy"], "NO_PROXY": "localhost,127.0.0.1,::1",
                         "CODEX_AUTHAPI_BASE_URL": lines["origins"]["a"],
                         "SUPERBA_TEST_SITES": t.root + "/home/desktop-sites.json"})
    site = {"id": "mac-e2e", "kind": "krill", "name": {"key": "", "fallback": "Mac e2e"},
            "homepage": "https://example.invalid/", "default_line": "a",
            "lines": [{"id": name, "name": {"key": "", "fallback": "Mac line " + name},
                       "origin": origin, "source": "test"} for name, origin in lines["origins"].items()],
            "capabilities": {"auth_modes": ["pat", "apikey"],
                             "codex_paths": ["backend_api_codex", "codex_v1"],
                             "latency_probe": True, "key_check": True,
                             "account_summary": False, "model_catalog": True},
            "endpoints": {"models_unauth": "/backend-api/codex/models",
                          "whoami": "/v1/user-auth-credential/whoami",
                          "accounts_check": "/backend-api/wham/accounts/check",
                          "backend_codex_models": "/backend-api/codex/models",
                          "codex_v1_models": "/codex/v1/models"}}
    t.write("home/desktop-sites.json", json.dumps([site]))
    t.write("home/.codex/config.toml", "# e2e original\n")
    t.spb("profile", "add", "mac-pat", "--site", "mac-e2e", "--auth", "pat", "--line", "a",
          "--key-stdin", stdin=key + "\n")
    t.spb("switch", "--profile", "mac-pat", "--line", "a", "--yes", "--write-only")
    auth = json.loads(t.read("home/.codex/auth.json"))
    t.check("personal_access_token" in auth, "Desktop fixture uses PAT authentication in the temporary HOME")
    return tuple(lines["origins"][name] + "/backend-api/codex" for name in ("a", "b"))


def desktop_onboarding(t, ui):
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if (ui.exists(type="AXTextArea", enabled=True)
                and not ui.has_text(r"Which best describes your work|Skip setup\?")):
            t.check(True, "PAT desktop reaches an editable composer without personal-account login")
            return
        for label in ("Engineering", "Continue", "Skip", "Go to ChatGPT", "Got it", "Not now"):
            if ui.exists(name=label, type="Button", enabled=True):
                ui.click(name=label, type="Button")
                break
        time.sleep(1)
    t.check(False, "PAT desktop reaches an editable composer without personal-account login")


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
    t.check(service["code"] == 0 and service["owned"], "Owned LaunchAgent is loaded")
    t.save("router-service.json", service)
    ui = t.configured_ui()
    if ui:
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
    if ui:
        desktop_flows.router_page(ui)
    if ui and t.runner.opts.net == "real":
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
    if ui:
        desktop_flows.claude_page(ui)
    t.shot("claude-applied")
    t.spb("claude", "restore", "--yes")
    restored = json.loads(t.read("home/.claude/settings.json"))
    t.check(restored == original, "Original Claude settings restored without managed keys")
    t.rpc("stop")
    ui = t.configured_ui()
    if ui:
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
    if ui:
        desktop_flows.notification_page(ui)
    t.shot("notifications")


@scenario("M06", "GUI login LaunchAgent starts and is removed", "Login startup")
def autostart(t):
    if not t.local and not t.runner.opts.allow_reboot:
        raise Blocked("M06 needs --allow-reboot and a VM with GUI auto-login")
    if not t.local:
        t.rpc("reboot_ready")
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
    t.configured_ui()
    t.shot("before-login")
    before = t.rpc("service", label=label)["boot"]
    if t.local:
        t.rpc("stop")
        t.ui = None
        t.save("login-simulation.json", t.rpc("login_simulate"))
        after = t.rpc("service", label=label)["boot"]
        t.check(after == before, "Login was simulated without rebooting the runner")
    else:
        t.save("reboot-preflight.json", t.rpc("reboot_ready"))
        try:
            t.runner.vm.shell("sudo -n /sbin/reboot", check=False, timeout=15)
        except Exception:
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
    t.save("service-after-login.json", service)
    t.save("gui-after-login.json", processes)
    # An /usr/bin/open LaunchAgent exits after submitting the launch. Require the
    # actual GUI and its HOME, not a permanently running launcher process.
    t.check(service["code"] == 0 and service["owned"], "Product LaunchAgent was loaded at login")
    t.check(bool(processes) and all(process["isolated"] for process in processes),
            "Login started the Superba GUI with the temporary HOME and CODEX_HOME")
    if t.gui:
        t.ui = Ui(t, pid=processes[0]["pid"])
        t.ui.wait_window()
    t.shot("after-login")
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
    if ui:
        ui.superba("data")
    t.shot("data-moved")
    t.check("mac-test" in t.spb("profile", "list", "--json")["stdout"], "Fresh CLI reads the preserved providers")


@scenario("M08", "Overwrite DMG upgrade preserves configuration and data", "MSI upgrade")
def upgrade(t):
    reinstall = getattr(t.runner.opts, "reinstall", False)
    if not t.runner.opts.upgrade_dmg and not reinstall:
        raise Blocked("M08 requires --upgrade-dmg pointing to a second build")
    old = t.install()
    t.provider()
    t.write(DATA + "/e2e-preserved.txt", "preserve-on-upgrade\n")
    baseline = t.read(DATA + "/profiles.json")
    t.check(bool(baseline), "Provider configuration exists before the upgrade")
    t.rpc("stop")
    new = t.install(t.runner.opts.upgrade_dmg, upgrade=True)
    if reinstall:
        t.check(old["dmg_sha256"] == new["dmg_sha256"], "Overwrite reinstalls the same exact-source DMG")
        t.unavailable("Cross-version upgrade: this run verifies same-build overwrite/data preservation only")
    else:
        t.check(old["dmg_sha256"] != new["dmg_sha256"], "Upgrade uses two distinct DMG artifacts")
        t.check(old["superba_sha256"] != new["superba_sha256"], "Upgrade replaces the actual app executable")
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
    t.configured_ui()
    t.shot("restored-before-uninstall")
    t.rpc("stop")
    t.ui = None
    t.spb("uninstall", "--yes", "--write-only")
    t.check(t.rpc("service")["code"] != 0, "No router service remains")
    agents = t.rpc("list", path="home/Library/LaunchAgents")
    t.check(not agents, "No Superba LaunchAgent files remain in the test HOME")
    # The common finalizer removes the DMG/app/HOME and asserts all owned processes are gone.
