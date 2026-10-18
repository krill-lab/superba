"""Reusable flows (install, wizard, open ChatGPT, turns, router...) with their checks.

Scenarios call these directly; `ensure(run, t, need)` runs one as a prerequisite
when the current VM state does not have it yet (run.state records what was done
since the last snapshot restore). Every flow records its own steps, so a
prerequisite shows up in the report like any other step.
"""

import hashlib
import json
import re
import time
from pathlib import Path

from agent import AgentError
from app import App, AppError
from session import BASE_SNAPSHOT, GUEST_DL, HARNESS_ENV
from ui import Ui, UiError
from vm import REPO, psq

GUEST_MSI = GUEST_DL + r"\superba-under-test.msi"
START_MENU_LNK = r"\Microsoft\Windows\Start Menu\Programs\Superba.lnk"
ROUTER_RUN_VALUE = "SuperbaRouter"


def ui(run, lang="en"):
    return Ui(run, lang=lang)


def app(run):
    return App(run)


# --------------------------------------------------------------------------- base snapshot
def prep_snapshot(run, rebuild=False):
    """Build `wintest` from `clean` (guest/prep-base.ps1): UAC on, Superba removed."""
    vm = run.vm
    if vm.snapshot_exists(BASE_SNAPSHOT) and not rebuild:
        return
    if not vm.pool_held and not getattr(run.opts, "held", False):
        vm.acquire("clean", vm_id=run.opts.vm or None, label="superba-wintest prep")
    else:
        vm.restore("clean")
        vm.start()
    run.log("running guest/prep-base.ps1 on clean")
    r = vm.ps((REPO / "e2e" / "windows" / "guest" / "prep-base.ps1").read_text(), timeout=600)
    run.log(r.stdout)
    vm.stop()
    if vm.snapshot_exists(BASE_SNAPSHOT):
        vm.helper("snapshot.sh", "delete", BASE_SNAPSHOT)
    notes = vm.helpers / "snapshot-notes" / f"{BASE_SNAPSHOT}.txt"
    notes.parent.mkdir(exist_ok=True)
    notes.write_text("wintest = clean + e2e/windows/guest/prep-base.ps1 (run.py prep-snapshot): UAC on with\n"
                     "Windows defaults, Superba (0.1.1-alpha.1) uninstalled; nothing else.\n")
    vm.create_snapshot(BASE_SNAPSHOT)
    run.log(f"snapshot {BASE_SNAPSHOT} created")


def uninstall_all_superba(run):
    """Every Superba MSI product of the desktop user (e.g. the `clean` snapshot's alpha)."""
    return run.uninstall_all_superba()


# --------------------------------------------------------------------------- prerequisites
def ensure(run, t, need):
    if run.state.get(need):
        return
    fn = NEEDS.get(need)
    if not fn:
        raise KeyError(f"unknown prerequisite {need!r}")
    fn(run, t)


def need_installed(run, t):
    install(run, t, prefix="[setup] ")


def need_superba(run, t):
    ensure(run, t, "installed")
    with t.step("[setup] Superba window open") as s:
        start_superba(run, s)


def need_configured(run, t):
    ensure(run, t, "superba")
    wizard_krill(run, t, prefix="[setup] ")


def need_app_ready(run, t):
    ensure(run, t, "configured")
    app_ready(run, t, prefix="[setup] ")


def need_turn(run, t):
    ensure(run, t, "app_ready")
    turn(run, t, "E2E-T1", prefix="[setup] ")


def need_router_on(run, t):
    ensure(run, t, "turn")
    router_on(run, t, prefix="[setup] ")


NEEDS = {
    "installed": need_installed,
    "superba": need_superba,
    "configured": need_configured,
    "app_ready": need_app_ready,
    "turn": need_turn,
    "router_on": need_router_on,
}


# --------------------------------------------------------------------------- inventory
INVENTORY_PS = r"""
$o = [ordered]@{}
$o.install_dir = Test-Path "$env:LOCALAPPDATA\Programs\Superba"
$o.install_files = @(if ($o.install_dir) { Get-ChildItem "$env:LOCALAPPDATA\Programs\Superba" -Recurse -File | ForEach-Object { $_.FullName.Substring($env:LOCALAPPDATA.Length) } })
$o.data_dir = Test-Path "$env:LOCALAPPDATA\krill-code"
$o.data_files = @(if ($o.data_dir) { Get-ChildItem "$env:LOCALAPPDATA\krill-code" -Recurse -File | ForEach-Object { $_.FullName.Substring($env:LOCALAPPDATA.Length) } })
$o.roaming_dir = Test-Path "$env:APPDATA\krill-code"
$o.user_path = [Environment]::GetEnvironmentVariable('PATH', 'User')
$o.user_env = @{}
$k = Get-Item 'HKCU:\Environment'
foreach ($n in $k.GetValueNames()) { $o.user_env[$n] = [string]$k.GetValue($n, $null, 'DoNotExpandEnvironmentNames') }
$run = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -ErrorAction SilentlyContinue
$o.run_key = @{}
if ($run) { foreach ($p in $run.PSObject.Properties) { if ($p.Name -notlike 'PS*') { $o.run_key[$p.Name] = [string]$p.Value } } }
$o.krill_reg = Test-Path 'HKCU:\Software\Krill'
$o.krill_reg_values = @(if ($o.krill_reg) { Get-ChildItem 'HKCU:\Software\Krill' -Recurse | ForEach-Object { $_.Name } })
$o.start_menu = Test-Path "$env:APPDATA\Microsoft\Windows\Start Menu\Programs\Superba.lnk"
$o.desktop_lnk = Test-Path "$env:USERPROFILE\Desktop\Superba.lnk"
$installer = New-Object -ComObject WindowsInstaller.Installer
$prods = $installer.GetType().InvokeMember('ProductsEx', 'GetProperty', $null, $installer, @('', '', 7))
$o.msi_products = @(foreach ($p in $prods) {
  $n = $p.GetType().InvokeMember('InstallProperty', 'GetProperty', $null, $p, @('ProductName'))
  if ($n -like 'Superba*') { $n + ' ' + $p.GetType().InvokeMember('ProductCode', 'GetProperty', $null, $p, $null) + ' ' + $p.GetType().InvokeMember('InstallProperty', 'GetProperty', $null, $p, @('VersionString')) } })
$o.processes = @(Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^(superba|spb|superba-router)\.exe$' } | ForEach-Object { "$($_.Name) $($_.ProcessId)" })
$o.codex_files = @(if (Test-Path "$env:USERPROFILE\.codex") { Get-ChildItem "$env:USERPROFILE\.codex" -File | ForEach-Object { $_.Name + ' ' + $_.Length } })
$o.config_sha = if (Test-Path "$env:USERPROFILE\.codex\config.toml") { (Get-FileHash "$env:USERPROFILE\.codex\config.toml").Hash } else { $null }
$o.auth_sha = if (Test-Path "$env:USERPROFILE\.codex\auth.json") { (Get-FileHash "$env:USERPROFILE\.codex\auth.json").Hash } else { $null }
$o.scheduled_tasks = @(Get-ScheduledTask -ErrorAction SilentlyContinue | Where-Object { $_.TaskName -match 'superba' -and $_.TaskName -notmatch 'wintest' } | ForEach-Object { $_.TaskName })
$o.cred_targets = @(cmdkey /list | Select-String -Pattern 'superba|krill' | ForEach-Object { $_.Line.Trim() })
$o | ConvertTo-Json -Depth 4 -Compress
"""


OWNED_KEYS = ("model_provider", "openai_base_url", "chatgpt_base_url", "model", "service_tier",
              "model_catalog_json")


def owned_keys(config_text):
    """Top-level values of the config.toml keys Superba owns (before the first table)."""
    out = {}
    for line in (config_text or "").splitlines():
        if line.lstrip().startswith("["):
            break
        m = re.match(r"\s*([A-Za-z0-9_]+)\s*=\s*(.+?)\s*$", line)
        if m and m.group(1) in OWNED_KEYS:
            out[m.group(1)] = m.group(2)
    return out


def inventory(run):
    """What Superba can leave behind, from the user's point of view (as the VM user)."""
    out = run.guest_ps(INVENTORY_PS, timeout=180)
    d = json.loads(out.strip().splitlines()[-1])
    for k in list(d.get("user_env", {})):
        if k in HARNESS_ENV:
            d["user_env"].pop(k)
    return d


# --------------------------------------------------------------------------- install
def msi_version(run):
    m = re.search(r"superba-(\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.\d+)?)(?:-quick)?-windows", run.msi.name) if run.msi else None
    return m.group(1) if m else None


def install(run, t, prefix="", passive=True):
    with t.step(prefix + "MSI installs per-user without elevation") as s:
        if "baseline" not in run.state:
            run.state["baseline"] = inventory(run)
            run.state["baseline_config"] = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml")
            s.save("inventory-before.json", run.state["baseline"])
            if run.state["baseline_config"] is not None:
                s.save("config-before.toml", run.state["baseline_config"])
            if "wsl" in (run.session_snapshot or "") and "wsl_baseline" not in run.state:
                run.state["wsl_baseline"] = wsl_hashes(run)
                s.save("wsl-baseline.sha256", run.state["wsl_baseline"])
                wsl_prepare(run)
                s.info(f"WSL: harness CA in the distribution's store; keep-alive pid {wsl_keepalive(run)}")
        run.state["msi_sha256"] = hashlib.sha256(run.msi.read_bytes()).hexdigest()
        s.info(f"MSI {run.msi.name} sha256 {run.state['msi_sha256'][:16]}…")
        run.vm.scp_to(run.msi, GUEST_MSI.replace("\\", "/"))
        log = r"C:\Users\Public\wintest\msi-install.log"
        r = run.agent.exec("msiexec.exe", ["/i", GUEST_MSI, "/passive" if passive else "/qn", "/norestart", "/l*v", log],
                           timeout=600)
        s.check(r["code"] == 0, f"msiexec /i exit {r['code']} in {r['ms']} ms (unelevated agent token, {'passive UI' if passive else 'silent'})")
        try:
            run.vm.scp_from(log.replace("\\", "/"), t.evdir / "msi-install.log")
            s.add_evidence(t.evdir / "msi-install.log")
        except Exception:
            pass
        inv = inventory(run)
        s.save("inventory-after-install.json", inv)
        files = {Path(f.replace("\\", "/")).name for f in inv["install_files"]}
        for f in ("superba.exe", "spb.exe", "superba-router.exe", "superba-msi.marker"):
            s.check(f in files, f"{f} in %LOCALAPPDATA%\\Programs\\Superba")
        s.check("Superba" in (inv["user_path"] or ""), "install folder on the user PATH")
        s.check(inv["start_menu"], "Start-menu shortcut Superba.lnk")
        s.check(not inv["desktop_lnk"], "no desktop shortcut (off by default)", soft=True)
        s.check(len(inv["msi_products"]) == 1, f"one Windows Installer product: {inv['msi_products']}")
        s.check(inv["krill_reg"], "HKCU\\Software\\Krill\\Superba written (InstallDir, Version)")
        r = run.vm.ps("Test-Path C:\\Windows\\System32\\vcruntime140.dll")
        s.check(r.stdout.strip() == "False", "still no VC++ runtime in System32 (the programs must not need it)")
        run.state["installed"] = True
    with t.step(prefix + "spb starts from a new terminal (PATH)") as s:
        r = run.agent.exec("cmd.exe", ["/d", "/c", "spb --version"], timeout=60)
        s.check(r["code"] == 0 and "spb" in r["stdout"], f"`spb --version` through PATH: {r['stdout'].strip()!r} exit {r['code']}")
        run.state["version"] = r["stdout"].strip().split()[-1] if r["stdout"].strip() else None
        want = msi_version(run)
        if want and run.state["version"]:
            s.check(run.state["version"] == want, f"spb version {run.state['version']} = MSI version {want}", soft=True)
        r = run.agent.exec(run.guest["INSTALL"] + r"\superba-router.exe", ["--version"], timeout=60)
        s.check(r["code"] == 0, f"superba-router --version exit {r['code']}: {(r['stdout'] + r['stderr']).strip()[:80]!r}")


def start_superba(run, s, via="shortcut", lang=None, size=None, page=None, fresh=False):
    """Start the GUI (Start-menu shortcut like a user, or the exe with flags)."""
    u = ui(run)
    if fresh:
        stop_superba(run)
    if u.window():
        return u.window()
    if via == "shortcut" and not (lang or size or page):
        run.agent.start_proc("explorer.exe", [run.guest["APPDATA"] + START_MENU_LNK])
        s.info("started from the Start-menu shortcut (explorer)")
    else:
        args = []
        if lang:
            args += ["--lang", lang]
        if size:
            args += ["--size", size]
        if page:
            args += ["--page", page]
        run.agent.start_proc(run.superba_exe, args)
        s.info(f"started superba.exe {' '.join(args)}")
    w = u.wait_window(90)
    s.check(w is not None, f"Superba window '{w['title']}' {w['rect'][2]}x{w['rect'][3]}")
    run.state["superba"] = True
    return w


def stop_superba(run):
    for p in run.agent.procs(r"^superba\.exe$"):
        try:
            run.agent.call("kill", pid=p["pid"], tree=False)
        except AgentError:
            pass
    run.state.pop("superba", None)
    time.sleep(1)


def gui_log(run):
    return run.guest_read(run.guest["DATA"] + r"\logs\superba-gui.log") or ""


# --------------------------------------------------------------------------- wizard / switching
# How much longer a switch may stay paused while ChatGPT finishes its first-start Windows
# setup (it creates its sandbox, minutes on a fresh VM; the switch goes on by itself after).
APP_SETUP_EXTRA_S = 480


def wait_switch_done(run, s, u, timeout=240, force_quit=True):
    """Follows the switch overlay until it is gone; answers the force-quit question
    (Windows: ChatGPT not opened by Superba) when force_quit. Returns the phases seen.

    A switch paused for running turns ("N conversations are running; switch paused") is
    still busy: it goes on by itself once they finish. While the pause names ChatGPT's
    first-start Windows setup the wait gets APP_SETUP_EXTRA_S more; a pause that outlasts
    the wait fails with its text (user bug 78)."""
    seen = []
    t0 = time.time()
    end = t0 + timeout
    busy = [re.compile(u.R(k)) for k in ("banner.running.title", "banner.armed.title", "banner.reapply.title",
                                         "banner.planning")]
    paused = re.compile(u.R("banner.waiting.title"))
    app_setup = u.L("banner.waiting.app_setup")
    here = u.L("banner.waiting.this_computer")
    pause_seen = None
    extended = False
    names = []

    def pause_lines(names):
        return [n for n in names if paused.match(n) or n == app_setup or n.startswith(here + " ·")]

    while time.time() < end:
        names = u.texts()
        lines = pause_lines(names)
        if any(paused.match(n) for n in lines):
            if lines != pause_seen:
                s.info(f"switch paused for running turns: {lines}")
                pause_seen = lines
                if "[paused]" not in seen:
                    seen.append("[paused]")
            if app_setup in lines and not extended:
                extended = True
                end = max(end, time.time() + APP_SETUP_EXTRA_S)
                s.info(f"ChatGPT is finishing its first-start Windows setup: waiting up to {APP_SETUP_EXTRA_S} s more")
            time.sleep(2)
            continue
        for n in names:
            if n not in seen and (n.startswith("→") or n in [u.L(f"step.{k}") for k in
                                  ("quit", "launch", "restart_core", "verify_auth", "verify_line", "verify_files",
                                   "verify_process", "verify_renderer", "write_config", "write_env", "migrate")]):
                seen.append(n)
        if force_quit and (u.L("decide.force_quit.title_windows") in names or u.L("decide.force_quit.title") in names):
            s.info("ChatGPT did not quit by itself: answering Force quit")
            u.click(key="decide.force")
            seen.append("[force quit]")
            continue
        if u.L("decide.remote.title") in names:
            # A WSL distribution's turn check timed out on a slow VM: nothing runs there in the harness.
            s.info("running turns on a remote host could not be checked: answering Continue")
            u.click(key="decide.continue")
            seen.append("[continue: remote turns unknown]")
            continue
        if time.time() - t0 > 3 and not any(rx.match(n) for rx in busy for n in names) \
                and not any(n == u.L("confirm.title") for n in names):
            return seen, names
        time.sleep(2)
    lines = pause_lines(names)
    if lines:
        raise UiError(f"the switch stayed paused for running turns ({round(time.time() - t0)} s): {lines}; "
                      f"phases seen {seen}")
    raise UiError(f"the switch did not finish within {timeout} s; phases seen {seen}")


def wizard_krill(run, t, prefix="", line="a", lang="en"):
    u = ui(run, lang)
    net = run.net
    with t.step(prefix + "Setup wizard: Krill with the test key") as s:
        run.state["wizard_seen"] = True
        start_superba(run, s)
        s.check(u.exists(timeout=20, key="wizard.detect.title"), "wizard opens on the first start ('What Superba found')")
        s.ui_dump("wizard-detect")
        s.shot("wizard-detect")
        app_text = u.has_text(r"\(package \d")
        s.check(bool(app_text), f"desktop app detected with its package version: {app_text!r}")
        u.click(key="wizard.continue")
        s.check(u.exists(timeout=10, key="wizard.provider.title"), "provider step")
        sel = u.wait(key="wizard.preset.krill.title", type="RadioButton")
        s.check(sel.get("selected") is True, "Krill pre-selected")
        u.click(key="wizard.continue")
        s.check(u.exists(timeout=10, key_re="wizard.credentials.key.title"), "key step")
        # By its name, the step's title (user bug 56); the key goes through the agent socket only, never logged.
        u.set_value(net.key, key_re="wizard.credentials.key.title", type="Edit")
        u.click(key="wizard.continue")
        s.check(u.exists(timeout=60, key="wizard.line.title"), "line & model step")
        time.sleep(4)  # latency and key checks
        s.ui_dump("wizard-lines")
        s.shot("wizard-lines")
        tile = u.wait(name=f"Test {net.line_id(line)}", type="RadioButton")
        if not tile.get("selected"):
            u.act("select", name=f"Test {net.line_id(line)}", type="RadioButton")
        u.click(key="wizard.continue")
        s.check(u.exists(timeout=10, key="wizard.review.title"), "confirm step")
        s.ui_dump("wizard-confirm")
        s.shot("wizard-confirm")
        auth = u.has_text(u.R("wizard.review.auth"))
        s.info(f"review: {auth}")
        run.state["wizard_auth"] = auth
        u.click(key="wizard.apply")
        time.sleep(3)
    with t.step(prefix + "First switch: full restart of ChatGPT (not opened by Superba)") as s:
        # Planning ("Checking lines, your key and running conversations…") can take a while
        # (it also probes remote and WSL hosts): wait for the confirmation or the switch itself.
        t0 = time.time()
        planning = re.compile(u.R("banner.planning"))
        while time.time() - t0 < 180:
            names = u.texts()
            if not any(planning.match(n) for n in names):
                break
            time.sleep(2)
        s.info(f"planning took {round(time.time() - t0)} s")
        names = u.texts()
        if u.L("confirm.title") in names:
            s.shot("full-restart-confirm")
            s.ui_dump("full-restart-confirm")
            reason = u.has_text(r"not opened from Superba")
            s.check(bool(reason), f"reason shown: {reason!r}", soft=True)
            u.click(key="confirm.full_restart")
        else:
            s.info("no full-restart confirmation (ChatGPT not running?)")
        phases, names = wait_switch_done(run, s, u)
        s.info(f"phases: {phases}")
        s.shot("after-first-switch")
        toast = [n for n in names if re.match(u.R("toast.switched"), n) or "failed" in n.lower()]
        s.info(f"toast/result: {toast}")
        a = app(run)
        m = a.wait_main(90)
        s.check(m is not None, "ChatGPT relaunched by Superba with a debugging port")
        port = a.cdp_port(m)
        s.check(f"--remote-allow-origins=http://127.0.0.1:{port}" in (m["cmdline"] or ""),
                f"--remote-debugging-port={port} with --remote-allow-origins=http://127.0.0.1:{port} only")
        r = run.vm.ps(f"(Get-NetTCPConnection -State Listen -LocalPort {port} -ErrorAction SilentlyContinue |"
                      " Select-Object -First 1 | ForEach-Object { $_.LocalAddress + ' ' + $_.OwningProcess })")
        addr = r.stdout.strip().split()
        s.check(len(addr) == 2 and addr[0] in ("127.0.0.1", "::1") and int(addr[1]) == m["pid"],
                f"debugging port listens on loopback in the app's main process: {r.stdout.strip()!r} (main {m['pid']})")
        env = run.user_env().get("CODEX_AUTHAPI_BASE_URL", "")
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.save("config.toml", cfg)
        s.check(f'openai_base_url = "{net.line_url(line)}/backend-api/codex"' in cfg,
                f"config.toml openai_base_url -> line {line}")
        auth = run.guest_json(run.guest["CODEX_HOME"] + r"\auth.json") or {}
        shape = sorted(auth.keys())
        s.info(f"auth.json keys: {shape} (values not read)")
        run.state["auth_mode"] = "pat" if "personal_access_token" in auth else ("apikey" if "OPENAI_API_KEY" in auth else "?")
        if run.state["auth_mode"] == "pat":
            s.check(env == net.line_url(line), f"HKCU CODEX_AUTHAPI_BASE_URL = line {line} (PAT)")
        run.state.update(configured=True, line=line, main_pid=m["pid"])


def app_ready(run, t, prefix=""):
    a = app(run)
    with t.step(prefix + "ChatGPT shows the composer (onboarding answered)") as s:
        p = a.probe()
        if p["login"] and run.state.get("auth_mode") == "pat":
            s.shot("app-login-page", process_re="^ChatGPT$")
            s.check(False, "ChatGPT shows its sign-in page after the PAT switch (PAT login did not take)")
        # A fresh ChatGPT first finishes its Windows setup (blank window / "Finishing Windows
        # setup", minutes on a new VM): that time is not part of the onboarding budget.
        try:
            steps, p = a.onboarding(150, setup_timeout=420)
        except Exception:
            s.save("app-diagnose.json", a.diagnose())
            raise
        if a.setup_waited:
            s.info(f"waited {a.setup_waited} s for ChatGPT's first-start setup")
        s.info(f"onboarding answered: {steps}")
        s.check(p["composer"] and not p["login"], "composer shown, not the sign-in page")
        ready = a.composer_ready(420)
        if not ready:
            s.check(False, "composer usable (the app's first-start 'Finishing Windows setup' did not finish in 7 min)")
        a.shot(t.evdir / f"{t.seq():02d}-app-ready.png")
        s.add_evidence(t.evdir / f"{t._seq:02d}-app-ready.png")
        run.state["app_ready"] = True
    set_model(run, t, prefix)


def set_model(run, t, prefix=""):
    """--model: the Krill profile's model, applied with a hot switch once the app is
    ready (a restart of the core during the app's first-start sandbox setup leaves it
    stuck on "Finishing Windows setup")."""
    model = getattr(run.opts, "model", None)
    if not model or run.state.get("model_set") == model:
        return
    with t.step(prefix + f"Default model {model} for the profile (applied hot)") as s:
        r = run.spb("profile", "edit", "krill", "--model", model, timeout=120, label="profile-model")
        s.check(r["code"] == 0, f"spb profile edit krill --model {model}: exit {r['code']}")
        r = run.spb("switch", "--profile", "krill", "--countdown", "0", "--yes", "--wait-turn", timeout=300,
                    label="apply-model")
        s.check(r["code"] == 0, f"applied: exit {r['code']} [{r['transcript']}]")
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        s.check(f'model = "{model}"' in cfg, f"config.toml model = {model}")
        s.check(App(run).composer_ready(120), "composer usable after the restart")
        run.state["model_set"] = model


def wait_relay_turn(run, since, marker=None, timeout=240):
    end = time.time() + timeout
    while time.time() < end:
        posts = [e for e in run.net.responses_posts(since) if e.get("status") == 200]
        if marker and run.net.mode == "real":
            posts = [e for e in posts if (e.get("last_user") or "").startswith(marker)]
        if posts:
            return posts
        time.sleep(2)
    return []


def turn(run, t, marker, prefix="", expect_line=None, new_chat=False):
    a = app(run)
    with t.step(prefix + f"Turn {marker} reaches the line") as s:
        if new_chat:
            a.new_chat()
            time.sleep(1)
        since = time.time()
        a.send_turn(f"{marker}: reply with exactly the text PONG")
        posts = wait_relay_turn(run, since, marker)
        s.check(bool(posts), f"POST …/responses seen by the relay: {[(e.get('line'), e.get('path'), e.get('status')) for e in posts][:3]}")
        a.wait_idle(180)
        time.sleep(2)
        p = a.probe()
        line = posts[-1].get("line") if posts else None
        if expect_line:
            s.check(line == expect_line, f"answered on line {line} (expected {expect_line})")
        if run.net.mode == "real" and posts:
            e = posts[-1]
            s.info(f"auth on the wire: {e.get('auth_kind')}, input items {e.get('n_input')}, model {e.get('model')}")
            run.state.setdefault("turn_inputs", {})[marker] = e.get("n_input")
            run.state.setdefault("turn_models", {})[marker] = e.get("model")
        s.check(bool(p["conversation"]) or bool(p["rows"]), f"thread {p['conversation']} in the sidebar")
        run.state["thread"] = p["conversation"]
        a.shot(t.evdir / f"{t.seq():02d}-turn-{marker}.png")
        s.add_evidence(t.evdir / f"{t._seq:02d}-turn-{marker}.png")
        run.state["turn"] = True
        return posts


# --------------------------------------------------------------------------- router
def router_on(run, t, prefix=""):
    u = ui(run)
    with t.step(prefix + "Router page: member gate, add Krill, turn on") as s:
        start_superba(run, s)
        u.page("router")
        time.sleep(1.5)
        s.shot("router-before")
        s.ui_dump("router-before")
        gate = u.find(key="router.banner.turn_on", type="Button")
        if gate:
            s.check(gate[0]["enabled"] is False, "'Turn on the router…' disabled while there is no member (bug 28/39)")
            s.check(bool(u.has_text(re.escape(u.L("router.eligibility.none")))), "the reason is shown inline")
            u.click(name=u.L("router.eligibility.add", name="Krill"))
            time.sleep(2)
        s.check(bool(u.has_text(r"^Krill")), "Krill listed as a member")
        u.click(key="router.banner.turn_on")
        time.sleep(2)
        if not getattr(run.opts, "model", None) and run.state.get("turn_models"):
            # User bug 58: a profile without a model keeps the model the PAT gave. Read
            # before the dump: the plan's countdown (3 s) may end while it is written.
            s.check(bool(u.has_text(u.R("switch.models.keep_api_key"))),
                    "the plan says Codex keeps the model the PAT gave (user bug 58)", soft=True)
        s.ui_dump("router-plan")
        u.try_click(key="banner.switch_now")  # else the countdown runs out by itself
        phases, names = wait_switch_done(run, s, u)
        s.info(f"phases: {phases}")
        s.shot("router-on")
        cfg = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
        m = re.search(r'openai_base_url = "http://127\.0\.0\.1:(\d+)/backend-api/codex"', cfg)
        s.check(bool(m), "config.toml points Codex at http://127.0.0.1:<port>/backend-api/codex")
        run.state["router_port"] = int(m.group(1)) if m else None
        direct = (run.state.get("turn_models") or {}).get("E2E-T1")
        if direct and not getattr(run.opts, "model", None):
            s.check(f'model = "{direct}"' in cfg,
                    f"config.toml keeps the model the PAT gave ({direct}; user bug 58)")
        inv = inventory(run)
        s.check(ROUTER_RUN_VALUE in inv["run_key"], f"HKCU Run value {ROUTER_RUN_VALUE}: {inv['run_key'].get(ROUTER_RUN_VALUE, '')[:120]!r}")
        s.check(any(p.startswith("superba-router.exe") for p in inv["processes"]), f"superba-router.exe running: {inv['processes']}")
        run.state["router_on"] = True


# --------------------------------------------------------------------------- WSL (snapshot wintest-wsl1)
WSL_DISTRO = "Ubuntu-WSL1"


def wsl(run, script, user=None, timeout=120, distro=WSL_DISTRO):
    """Run `bash -lc script` in the WSL distro as the desktop user's wsl.exe (no `"` in
    the script: Windows argument quoting); returns the agent's exec result."""
    if '"' in script:
        raise ValueError("no double quotes in WSL scripts (use single quotes or a file)")
    args = ["-d", distro]
    if user:
        args += ["-u", user]
    args += ["--", "bash", "-lc", script]
    return run.agent.exec("wsl.exe", args, env={"WSL_UTF8": "1"}, timeout=timeout)


WSL_FILES = "~/.codex/config.toml ~/.codex/auth.json ~/.profile ~/.bashrc ~/.zshenv ~/.bash_profile"


def wsl_hashes(run, distro=WSL_DISTRO):
    """sha256 of the distribution files Superba may touch (or `missing <f>`)."""
    r = wsl(run, f"for f in {WSL_FILES}; do [ -f $f ] && sha256sum $f || echo missing $f; done", distro=distro)
    return r["stdout"].strip()


def wsl_tree_hashes(run, path="~/.codex ~/.bashrc ~/.profile", distro=WSL_DISTRO):
    """sha256 of every file under the given paths inside the distro (W86: unchanged)."""
    r = wsl(run, f"for p in {path}; do [ -e $p ] && find $p -type f -print0 | xargs -0 -r sha256sum; done | sort -k2",
            distro=distro)
    return r["stdout"]


def wsl_running(run, distro=WSL_DISTRO):
    """Whether the distribution runs now (`wsl --list --running --quiet` starts nothing)."""
    r = run.agent.exec("wsl.exe", ["--list", "--running", "--quiet"], env={"WSL_UTF8": "1"}, timeout=60)
    names = [l.strip().strip("\x00") for l in r["stdout"].replace("\x00", "").splitlines()]
    return distro in names


def wsl_keepalive(run, distro=WSL_DISTRO):
    """A WSL 1 distribution stops ~30 s after it goes idle and a switch never starts a stopped
    one (it records "update pending"): keep it running with a long `sleep` while switching."""
    pid = run.state.get("wsl_keepalive")
    if pid and any(p["pid"] == pid for p in run.agent.procs(r"^wsl\.exe$")):
        return pid
    pid = run.agent.start_proc("wsl.exe", ["-d", distro, "--exec", "sleep", "3600"])
    run.state["wsl_keepalive"] = pid
    end = time.time() + 30
    while time.time() < end and not wsl_running(run, distro):
        time.sleep(1)
    return pid


def wsl_stop(run, distro=WSL_DISTRO):
    """End the keep-alive and terminate the distribution."""
    pid = run.state.pop("wsl_keepalive", None)
    if pid:
        try:
            run.agent.call("kill", pid=pid, tree=True)
        except AgentError:
            pass
    run.agent.exec("wsl.exe", ["--terminate", distro], timeout=60)
    end = time.time() + 30
    while time.time() < end and wsl_running(run, distro):
        time.sleep(1)
    return not wsl_running(run, distro)


def wsl_prepare(run, distro=WSL_DISTRO):
    """The harness CA in the distribution's system store (Codex inside it talks to the
    test lines over https)."""
    return wsl(run, "cp /mnt/c/Users/Public/wintest/tls/ca.pem /usr/local/share/ca-certificates/superba-wintest.crt"
                    " && update-ca-certificates >/dev/null 2>&1; echo ca $?", user="root", distro=distro)
