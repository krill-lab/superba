"""Codex inside WSL (user bug 45, MATRIX.md W80-W87), on snapshot `wintest-wsl1`: WSL 1,
distro `Ubuntu-WSL1` (default, user superba) with the app's Linux Codex in
~/.local/bin. WSL 2 cannot run in the pool VMs (no nested virtualization)."""

import json
import re
import time

import flows
from app import App
from runner import scenario
from ui import Ui

D = flows.WSL_DISTRO
HOST = f"wsl:{D}"
SNAP = "wintest-wsl1"
WSL_FILES = flows.WSL_FILES


def U(t, lang="en"):
    return Ui(t.run, lang)


def wsl_hashes(run):
    return flows.wsl_hashes(run)


def wsl_show(run):
    """The distribution's Codex files and Superba's env blocks, keys redacted."""
    script = ("cd ~; echo == config.toml; cat ~/.codex/config.toml 2>&1; echo; echo == auth.json keys;"
              " sed -E 's/: *[^,}]+/: <redacted>/g' ~/.codex/auth.json 2>&1; echo; stat -c '%a %n' ~/.codex/auth.json 2>&1;"
              # wsl.exe runs the command line through the login shell first: `\$f` reaches bash -lc as `$f`.
              " for f in .profile .bash_profile .bash_login .bashrc .zshenv; do grep -q superba \\$f 2>/dev/null && {"
              " echo == \\$f block; sed -n '/>>> superba/,/<<< superba/p' \\$f; }; done;"
              " echo == login bash sees CODEX_AUTHAPI_BASE_URL=$(bash -lc 'printf %s ${CODEX_AUTHAPI_BASE_URL-unset}')")
    return flows.wsl(run, script)["stdout"]


def wsl_config(run):
    return flows.wsl(run, "cat ~/.codex/config.toml 2>/dev/null")["stdout"]


def ensure_wsl_baseline(run, s):
    if "wsl_baseline" in run.state:
        return True
    if run.state.get("configured"):
        s.info("no first-sight baseline of the distribution (configured before the WSL scenarios ran)")
        return False
    run.state["wsl_baseline"] = wsl_hashes(run)
    s.save("wsl-baseline.sha256", run.state["wsl_baseline"])
    return True


def wsl_trust(run):
    """The harness CA for Codex inside the distribution (SSL_CERT_FILE)."""
    flows.wsl(run, "cat /etc/ssl/certs/ca-certificates.crt /mnt/c/Users/Public/wintest/tls/ca.pem > ~/.wintest-bundle.pem")
    return "SSL_CERT_FILE=$HOME/.wintest-bundle.pem"


@scenario("W80", "WSL: distributions and their Codex detected; listed as WSL hosts", area="wsl", on=SNAP,
          needs=("installed",), bugs=(45,), net="any")
def w80(t):
    run = t.run
    with t.step("The distribution as the snapshot has it") as s:
        v = flows.wsl(run, "codex --version; id -un; uname -r")
        s.save("wsl-codex-version.txt", v["stdout"] + v["stderr"])
        s.check(v["code"] == 0 and "codex" in v["stdout"], f"codex inside {D}: {v['stdout'].strip()[:80]!r}")
        ensure_wsl_baseline(run, s)
    with t.step("spb wsl list / spb remote list") as s:
        r = run.spb("wsl", "list", timeout=180, label="wsl-list")
        s.save("spb-wsl-list.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0 and D in r["stdout"], f"spb wsl list lists {D}")
        line = next((l for l in r["stdout"].splitlines() if D in l), "")
        s.check(re.search(r"\b1\b", line) is not None, f"WSL version 1 shown: {line!r}")
        s.check(re.search(r"(?i)yes|default|\*", line) is not None, f"default distribution marked: {line!r}", soft=True)
        r = run.spb("remote", "list", timeout=180, label="remote-list")
        s.save("spb-remote-list.txt", r["stdout"] + r["stderr"])
        s.check(f"WSL: {D}" in r["stdout"] or HOST in r["stdout"], "spb remote list shows the distribution as a WSL host")


@scenario("W82", "WSL: a switch writes the distribution's ~/.codex and env blocks; follows by default",
          area="wsl", on=SNAP, needs=("superba",), conflicts=("configured", "wizard_seen"), bugs=(45,))
def w82(t):
    run = t.run
    flows.wsl_keepalive(run)  # a stopped distribution is never started by a switch
    with t.step("Baseline of the distribution before Superba's first switch") as s:
        ensure_wsl_baseline(run, s)
    flows.wizard_krill(run, t)
    with t.step("The distribution followed the switch") as s:
        out = wsl_show(run)
        s.save("wsl-after-switch.txt", out)
        line = run.net.line_url(run.state.get("line", "a"))
        s.check(line in out, f"distribution config.toml points at {line}")
        s.check("600 " in out, "auth.json mode 0600")
        if run.state.get("auth_mode") == "pat":
            s.check(">>> superba" in out and f"CODEX_AUTHAPI_BASE_URL={line}" in out,
                    "PAT: env block in the bash login files and the login shell sees CODEX_AUTHAPI_BASE_URL")
        else:
            s.check("OPENAI_API_KEY" in out, "API key mode: auth.json has OPENAI_API_KEY")
        r = run.spb("remote", "list", timeout=180, label="remote-list-after")
        s.save("spb-remote-list.txt", r["stdout"])
    with t.step("GUI: Remote hosts shows the WSL host") as s:
        u = U(t)
        flows.start_superba(run, s)
        u.page("hosts")
        time.sleep(4)
        s.shot("hosts-wsl")
        s.ui_dump("hosts-wsl")
        s.check(bool(u.has_text(re.escape(D))), f"{D} on the Remote hosts page")
        s.manual("hosts-wsl.png: the distribution with a WSL badge, its Codex version, follows this computer")
    # Let the app finish its first-start sandbox setup first: a hot restart during it left the
    # app stuck on "Finishing Windows setup" twice (user bug 59).
    flows.app_ready(run, t)
    with t.step("A line switch updates the distribution too") as s:
        other = "b" if run.state.get("line", "a") == "a" else "a"
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", timeout=300,
                    label="switch-wsl")
        s.save("switch.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"switch exit {r['code']} [{r['transcript']}]")
        s.check(run.net.line_url(other) in wsl_config(run), f"distribution config.toml on line {other}")
        run.state["line"] = other


@scenario("W83", "WSL: sessions of the distribution, scan and migrate (helper hint on an MSI without it)",
          area="wsl", on=SNAP, net="mock", needs=("configured",), bugs=(45,))
def w83(t):
    run = t.run
    flows.wsl_keepalive(run)  # a stopped distribution is never started by a switch
    with t.step("Two sessions under the legacy provider krill inside the distribution") as s:
        wsl_trust(run)
        url = run.net.line_url("a") + "/backend-api/codex"
        script = "\n".join([
            "#!/bin/sh",
            "mkdir -p ~/work && cd ~/work",
            f"export SSL_CERT_FILE=$HOME/.wintest-bundle.pem SB_KEY='{run.net.key}'",
            "for i in 1 2; do",
            "  timeout 90 codex exec --skip-git-repo-check -s read-only -c model_provider=krill \\",
            f"    -c 'model_providers.krill={{name=\"krill\",base_url=\"{url}\",env_key=\"SB_KEY\",wire_api=\"responses\"}}' \\",
            "    \"legacy session $i: reply ok\" >/dev/null 2>&1",
            "  echo \"run $i: $?\"",
            "done",
            "grep -h -o '\"model_provider\":\"[a-z]*\"' ~/.codex/sessions/*/*/*/*.jsonl | sort | uniq -c",
            ""])
        # The script carries only the mock relay's fake key; it is deleted after the run.
        local = run.root / "work-w83-legacy.sh"
        local.write_text(script)
        run.vm.scp_to(local, "C:/Users/Public/wintest/legacy.sh")
        local.unlink()
        r = flows.wsl(run, "sh /mnt/c/Users/Public/wintest/legacy.sh; rm -f /mnt/c/Users/Public/wintest/legacy.sh", timeout=300)
        s.info(r["stdout"].strip()[-300:])
        s.check("krill" in r["stdout"], "rollouts under model_provider krill in the distribution")
    with t.step("spb sessions --host wsl:<distro>: list, then migrate") as s:
        r = run.spb("sessions", "--host", HOST, timeout=300, label="sessions-wsl")
        s.save("sessions-wsl.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0 and "krill" in r["stdout"], "the distribution's krill sessions are listed")
        r = run.spb("sessions", "--host", HOST, "migrate", "--from", "krill", "--yes", timeout=600, label="sessions-wsl-migrate")
        s.save("sessions-wsl-migrate.txt", r["stdout"] + r["stderr"])
        text = r["stdout"] + r["stderr"]
        if r["code"] == 0:
            s.check("migrated" in text.lower(), "migrated inside the distribution (Linux helper present)")
        else:
            s.check("helper" in text.lower(), "without the Linux helper in the MSI: a clear hint to run spb inside "
                                              "the distribution (expected until the MSI ships spb-linux)")
    with t.step("GUI: Sessions page, WSL host") as s:
        u = U(t)
        flows.start_superba(run, s)
        u.page("sessions")
        time.sleep(3)
        if u.exists(name_re=rf"^{re.escape(D)}"):
            u.act("invoke", name_re=rf"^{re.escape(D)}")
            time.sleep(3)
        s.shot("sessions-wsl")
        s.ui_dump("sessions-wsl")
        s.manual("sessions-wsl.png: the distribution's sessions; Migrate either runs inside the distribution or "
                 "says the Linux helper is missing (spb sessions migrate inside the distribution)")


@scenario("W84", "WSL: settings sync filtered by the distribution's Codex version", area="wsl", on=SNAP,
          needs=("configured",), bugs=(45,))
def w84(t):
    run = t.run
    flows.wsl_keepalive(run)  # a stopped distribution is never started by a switch
    with t.step("Settings preview for the WSL host") as s:
        r = run.spb("settings", "diff", timeout=180, label="settings-diff-wsl")
        s.save("settings-diff.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"spb settings diff exit {r['code']}")
        s.check(D in r["stdout"] or "WSL" in r["stdout"], "the diff covers the WSL host", soft=True)
        s.manual("settings-diff.txt: keys the distribution's Codex (0.163.0-alpha.10.2) does not know are skipped "
                 "with a reason; the app's newer Codex gets them")


@scenario("W85", "WSL 1: router mode reaches the router on 127.0.0.1; the request row names the distribution",
          area="wsl", on=SNAP, net="mock", needs=("router_on",), bugs=(45,))
def w85(t):
    run = t.run
    flows.wsl_keepalive(run)  # a stopped distribution is never started by a switch
    with t.step("The distribution points at the router with its own source token") as s:
        out = wsl_show(run)
        s.save("wsl-routed.txt", out)
        port = run.state.get("router_port")
        s.check(f"http://127.0.0.1:{port}/" in out, f"distribution config.toml on the router 127.0.0.1:{port}")
        tok = flows.wsl(run, "grep -o 'sbs_[a-z0-9]*_' ~/.codex/auth.json | head -1")["stdout"].strip()
        s.check(tok.startswith("sbs_"), f"auth.json holds a per-source router token (sbs_…, value not shown)")
        r = run.spb("wsl", "list", timeout=180, label="wsl-list-routed")
        s.save("spb-wsl-list.txt", r["stdout"])
        s.check("127.0.0.1" in r["stdout"], "router path 127.0.0.1 (WSL 1)", soft=True)
    with t.step("codex exec inside the distribution goes through the router") as s:
        since = time.time()
        env = wsl_trust(run)
        r = flows.wsl(run, f"mkdir -p ~/work && cd ~/work && {env} timeout 120 codex exec --skip-git-repo-check"
                           " -s read-only 'Reply with one word: pong' 2>&1 | tail -5; echo exit $?", timeout=200)
        s.save("codex-exec.txt", r["stdout"] + r["stderr"])
        posts = flows.wait_relay_turn(run, since, timeout=60)
        s.check(bool(posts), "the relay got the request")
        wsl_rows, rows = [], []
        for _ in range(8):  # the router writes its request log asynchronously
            rr = run.spb_json("usage", "log", timeout=120, label="usage-log-wsl")
            rows = (rr.get("json") or {}).get("rows") or []
            wsl_rows = [x for x in rows if D in json.dumps(x)]
            if wsl_rows:
                break
            time.sleep(5)
        s.save("usage-log.json", rows[:20])
        s.check(bool(wsl_rows), f"a request row names the distribution ({len(wsl_rows)} rows)")
    with t.step("Router off: the distribution is back on the direct line") as s:
        r = run.spb("router", "disable", "--yes", timeout=300, label="router-disable-wsl")
        s.check(r["code"] == 0, f"spb router disable exit {r['code']} [{r['transcript']}]")
        cfg = wsl_config(run)
        s.check("http://127.0.0.1" not in cfg and any(run.net.line_url(l) in cfg for l in run.net.ports),
                "distribution config.toml back on a test line")
        run.state.pop("router_on", None)


@scenario("W86", "WSL: follow off leaves the distribution alone; restore brings it back", area="wsl", on=SNAP,
          needs=("configured",), bugs=(45,))
def w86(t):
    run = t.run
    flows.wsl_keepalive(run)  # a stopped distribution is never started by a switch
    with t.step("follow off: a switch leaves the distribution untouched") as s:
        r = run.spb("remote", "follow", HOST, "off", timeout=120, label="follow-off")
        s.check(r["code"] == 0, f"spb remote follow {HOST} off: exit {r['code']}")
        before = wsl_hashes(run)
        other = "b" if run.state.get("line", "a") == "a" else "a"
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", timeout=300,
                    label="switch-follow-off")
        s.check(r["code"] == 0, f"switch exit {r['code']}")
        run.state["line"] = other
        after = wsl_hashes(run)
        s.check(before == after, "distribution files unchanged")
        r = run.spb("remote", "follow", HOST, "on", timeout=120, label="follow-on")
        s.check(r["code"] == 0, "follow back on")
    with t.step("spb restore original: the distribution back to its first sight") as s:
        r = run.spb("restore", "original", "--yes", timeout=600, label="restore-original-wsl")
        s.save("restore.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"spb restore original exit {r['code']} [{r['transcript']}]")
        now = wsl_hashes(run)
        s.save("wsl-after-restore.sha256", now)
        base = run.state.get("wsl_baseline")
        if base:
            s.check(now == base, "distribution files (config, auth, shell rc) identical to before Superba")
        else:
            s.info("no baseline in this session (run W80 or W82 first)")
        out = wsl_show(run)
        s.check(">>> superba" not in out, "no Superba env block left in the distribution's shell files")
        for k in ("configured", "app_ready", "turn", "router_on"):
            run.state.pop(k, None)


LEGACY_KEY = "runCodexInWindowsSubsystemForLinux"


def set_legacy_wsl(cfg, on=True):
    """config.toml text with [desktop] runCodexInWindowsSubsystemForLinux = true/false."""
    lines = [l for l in (cfg or "").splitlines() if not l.strip().startswith(LEGACY_KEY)]
    value = f"{LEGACY_KEY} = {'true' if on else 'false'}"
    for i, l in enumerate(lines):
        if l.strip() == "[desktop]":
            lines.insert(i + 1, value)
            break
    else:
        lines += ["", "[desktop]", value]
    return "\n".join(lines) + "\n"


def wsl_app_servers(run, a):
    main = a.main()
    if not main:
        return main, []
    return main, [p for p in run.agent.procs(r"^wsl\.exe$") if p["ppid"] == main["pid"]
                  and "app-server" in (p["cmdline"] or "")]


def legacy_mode_on(run, s, a):
    """Turn "Run Codex in WSL" on in config.toml with ChatGPT closed, then open ChatGPT through
    Superba: its app-server is a wsl.exe child running the app's Codex inside the distribution."""
    a.kill_all()
    time.sleep(3)
    path = run.guest["CODEX_HOME"] + r"\config.toml"
    cfg = run.guest_read(path) or ""
    local = run.root / "work-legacy-wsl-config.toml"
    local.write_bytes(set_legacy_wsl(cfg).encode("utf-8"))  # no BOM: the app ignores the key after a BOM
    run.vm.scp_to(local, path.replace("\\", "/"))
    r = run.spb("launch", timeout=240, label="launch-legacy-wsl")  # ChatGPT is closed: a plain launch
    s.check(r["code"] == 0, f"spb launch exit {r['code']} [{r['transcript']}]")
    m = a.wait_main(120)
    s.check(m is not None, "ChatGPT opened through Superba with a debugging port")
    end = time.time() + 120
    servers = []
    while time.time() < end and not servers:
        main, servers = wsl_app_servers(run, a)
        time.sleep(2)
    s.check(bool(servers), f"the app-server is a wsl.exe child of the app: {[(p['pid'], (p['cmdline'] or '')[:120]) for p in servers]}")
    # `[c]odex`: the pgrep's own command line does not match.
    inside = flows.wsl(run, "pgrep -af '[c]odex.*app-server' | head -3")["stdout"].strip()
    s.check("app-server" in inside, f"its Codex runs inside {D}: {inside[:160]!r}")
    steps, p = a.onboarding(120)
    s.check(not p["login"], "the app is usable (not on the sign-in page)")
    run.state["legacy_wsl"] = True


@scenario("W81", "WSL: the app's legacy 'Run Codex in WSL' mode — hot switch, WSLENV for PAT, restore",
          area="wsl", on=SNAP, needs=("app_ready",), bugs=(45, 54))
def w81(t):
    run = t.run
    a = App(run)
    flows.wsl_keepalive(run)
    with t.step("Turn the mode on in config.toml (ChatGPT closed) and open ChatGPT through Superba") as s:
        legacy_mode_on(run, s, a)
    with t.step("First switch with the mode on") as s:
        main0, sv0 = wsl_app_servers(run, a)
        other = "b" if run.state.get("line", "a") == "a" else "a"
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", "--force",
                    timeout=300, label="switch-legacy-wsl")
        s.save("switch.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"switch exit {r['code']} [{r['transcript']}]")
        if "Full restart" in r["stdout"]:
            # PAT: the app was opened before WSLENV forwarded CODEX_AUTHAPI_BASE_URL, so its WSL Codex lacks
            # the auth address; Superba says so and restarts the app once (it writes WSLENV first).
            s.info("planned as a full restart: " + next((l for l in r["stdout"].splitlines() if "auth API" in l), "")[:160])
            s.check(run.state.get("auth_mode") == "pat", "a full restart only in PAT mode (the auth address)")
        else:
            s.check("Hot restart" in r["stdout"], "planned as a hot restart")
        bad = [l for l in r["stdout"].splitlines() if "did not pass verification" in l or "Restart ChatGPT yourself" in l]
        s.check(not bad, f"no verification failure reported: {bad}", soft=True)
        m = a.wait_main(150, not_pid=None)
        end = time.time() + 150
        sv1 = []
        while time.time() < end:
            main1, sv1 = wsl_app_servers(run, a)
            if sv1 and not {p["pid"] for p in sv1} & {p["pid"] for p in sv0}:
                break
            time.sleep(3)
        s.check(bool(sv1) and not {p["pid"] for p in sv1} & {p["pid"] for p in sv0},
                f"a new WSL app-server: {[p['pid'] for p in sv0]} -> {[p['pid'] for p in sv1]}")
        run.state["line"] = other
    with t.step("The next switch is hot (the app's WSL Codex has everything it needs)") as s:
        main0, sv0 = wsl_app_servers(run, a)
        other = "b" if run.state.get("line", "a") == "a" else "a"
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", "--wait-turn",
                    timeout=300, label="switch-legacy-wsl-2")
        s.save("switch-2.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0 and "Hot restart" in r["stdout"], f"hot switch exit {r['code']} [{r['transcript']}]")
        main1, sv1 = wsl_app_servers(run, a)
        s.check(main1 and main0 and main1["pid"] == main0["pid"], "app main process kept")
        s.check(bool(sv1) and not {p["pid"] for p in sv1} & {p["pid"] for p in sv0},
                f"new WSL app-server: {[p['pid'] for p in sv0]} -> {[p['pid'] for p in sv1]}")
        run.state["line"] = other
        App(run).onboarding(90)
    flows.turn(run, t, "E2E-LEGACYWSL", expect_line=run.state["line"], new_chat=True)
    with t.step("WSLENV forwards CODEX_AUTHAPI_BASE_URL in PAT mode") as s:
        env = run.user_env()
        s.save("hkcu-env.json", {k: v for k, v in env.items() if k in ("WSLENV", "CODEX_AUTHAPI_BASE_URL")})
        if run.state.get("auth_mode") == "pat":
            s.check("CODEX_AUTHAPI_BASE_URL" in (env.get("WSLENV") or ""), f"WSLENV = {env.get('WSLENV')!r}")
        else:
            s.info("API-key mode (mock relay): no CODEX_AUTHAPI_BASE_URL to forward; run with --net real for the PAT part")
    with t.step("Restore original: the WSLENV entry goes with the value") as s:
        r = run.spb("restore", "original", "--countdown", "0", "--yes", "--force", timeout=600, label="restore-legacy-wsl")
        s.save("restore.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"spb restore original exit {r['code']} [{r['transcript']}]")
        env = run.user_env()
        s.check("CODEX_AUTHAPI_BASE_URL" not in env, "CODEX_AUTHAPI_BASE_URL removed")
        s.check("CODEX_AUTHAPI_BASE_URL" not in (env.get("WSLENV") or ""), f"WSLENV without it: {env.get('WSLENV')!r}")
        for k in ("configured", "app_ready", "turn", "router_on"):
            run.state.pop(k, None)


@scenario("W88", "WSL: a stopped distribution is not started by a switch; 'update pending' until spb wsl update",
          area="wsl", on=SNAP, needs=("configured",), bugs=(45, 54))
def w88(t):
    run = t.run
    other = "b" if run.state.get("line", "a") == "a" else "a"
    with t.step("Stop the distribution, then switch") as s:
        s.check(flows.wsl_stop(run), f"{D} stopped (wsl --terminate)")
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", timeout=300,
                    label="switch-stopped")
        s.save("switch.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"switch exit {r['code']} [{r['transcript']}]")
        s.check(not flows.wsl_running(run), "the switch did not start the distribution")
        run.state["line"] = other
        r = run.spb("wsl", "list", timeout=120, label="wsl-list-pending")
        s.save("wsl-list.txt", r["stdout"])
        s.check(re.search(r"(?i)update pending", r["stdout"]) is not None, "spb wsl list: stopped, update pending")
        s.check(not flows.wsl_running(run), "listing did not start it either")
    with t.step("GUI: Remote hosts says update pending") as s:
        u = U(t)
        flows.start_superba(run, s)
        u.page("hosts")
        time.sleep(4)
        s.shot("hosts-pending")
        s.ui_dump("hosts-pending")
        first = u.has_text(r"(?i)update pending|not updated by the switch")
        s.check(bool(first), f"the host tile says update pending right after the switch (Superba was open): {first!r}",
                soft=True)
        if not first and u.exists(name="Check again"):
            u.click(name="Check again")
            time.sleep(8)
            s.shot("hosts-pending-after-check")
            again = u.has_text(r"(?i)update pending|not updated by the switch")
            s.check(bool(again), f"after 'Check again': {again!r}", soft=True)
            s.check(not flows.wsl_running(run), "'Check again' did not start the distribution", soft=True)
    with t.step("spb wsl update applies the pending switch") as s:
        r = run.spb("wsl", "update", D, "--countdown", "0", "--yes", timeout=300, label="wsl-update")
        s.save("wsl-update.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"spb wsl update exit {r['code']} [{r['transcript']}]")
        s.check(run.net.line_url(other) in wsl_config(run), f"distribution config.toml now on line {other}")
        r = run.spb("wsl", "list", timeout=120, label="wsl-list-after-update")
        s.check(re.search(r"(?i)update pending", r["stdout"]) is None, "nothing pending any more")
        r = run.spb("wsl", "update", D, "--countdown", "0", "--yes", timeout=300, label="wsl-update-again")
        s.check("nothing pending" in (r["stdout"] + r["stderr"]).lower() or r["code"] == 0,
                f"a second update has nothing to do: exit {r['code']}", soft=True)
        flows.wsl_keepalive(run)


@scenario("W87", "WSL 2 behind NAT: router access listener + firewall rule (UAC)", area="wsl", manual_only=True,
          bugs=(45,))
def w87(t):
    t.skip("not runnable in the pool VMs: WSL 2 needs nested virtualization (see wsl-feasibility.md); run on "
           "real hardware: spb wsl router-access <distro>, approve UAC, codex exec in the distro, request row names it")


# --------------------------------------------------------------------------- the model kept (user bug 63)
def top_model(cfg):
    """The top-level `model` of a config.toml text (`None` = absent)."""
    head = re.split(r"(?m)^\s*\[", cfg or "", maxsplit=1)[0]
    m = re.search(r'(?m)^\s*model\s*=\s*"([^"]+)"', head)
    return m.group(1) if m else None


def wsl_exec_turn(run, s, marker, label):
    """`codex exec` inside the distribution (its own Codex and ~/.codex); the model the relay saw."""
    env = wsl_trust(run)
    since = time.time()
    r = flows.wsl(run, f"mkdir -p ~/work && cd ~/work && {env} timeout 150 codex exec --skip-git-repo-check"
                       f" -s read-only '{marker}: reply with exactly the text PONG' 2>&1 | tail -5; echo exit $?",
                  timeout=240)
    s.save(f"codex-exec-{label}.txt", r["stdout"] + r["stderr"])
    posts = flows.wait_relay_turn(run, since, marker, timeout=120)
    s.check(bool(posts), f"{marker}: the relay got the distribution's request")
    model = posts[-1].get("model") if posts else None
    s.info(f"{marker}: model {model!r}, line {posts[-1].get('line') if posts else None}")
    return model


def preview_row(text, title, pattern):
    """The line of the plan's diff preview under section `title:` that matches `pattern`."""
    section = None
    for line in text.splitlines():
        if re.fullmatch(r"\S.*:", line.strip()) and not re.search(r"\s(→|->)\s", line):
            section = line.strip()[:-1]
            continue
        if section == title and re.search(pattern, line.strip()):
            return line.strip()
    return None


def router_cli(run, s, enable, label, extra=()):
    if enable:
        r = run.spb("router", "install", "--member", "krill", timeout=300, label=f"router-install-{label}")
        s.check(r["code"] == 0, f"spb router install exit {r['code']} [{r['transcript']}]")
    r = run.spb("router", "enable" if enable else "disable", "--countdown", "0", "--yes", *extra, timeout=300,
                label=f"router-{'enable' if enable else 'disable'}-{label}")
    text = r["stdout"] + r["stderr"]
    s.save(f"router-{'enable' if enable else 'disable'}.txt", text)
    s.check(r["code"] == 0, f"spb router {'enable' if enable else 'disable'} exit {r['code']} [{r['transcript']}]")
    local = run.guest_read(run.guest["CODEX_HOME"] + r"\config.toml") or ""
    if enable:
        m = re.search(r'openai_base_url = "http://127\.0\.0\.1:(\d+)/backend-api/codex"', local)
        s.check(bool(m), "this computer's config.toml points Codex at the router")
        run.state["router_port"] = int(m.group(1)) if m else None
        run.state["router_on"] = True
    else:
        run.state.pop("router_on", None)
    return text, local


@scenario("W89", "WSL: router on/off keeps the model the distribution's Codex runs; the plan names it (bug 63)",
          area="wsl", on=SNAP, net="real", needs=("configured",), conflicts=("router_on",), bugs=(58, 63))
def w89(t):
    run = t.run
    flows.wsl_keepalive(run)  # a stopped distribution is never started by a switch
    title = f"WSL: {D}"
    with t.step("Before: the distribution on its PAT line, no model in its config.toml") as s:
        out = wsl_show(run)
        s.save("wsl-before.txt", out)
        s.check(run.state.get("auth_mode") == "pat", f"PAT applied (auth mode {run.state.get('auth_mode')!r})")
        s.check("personal_access_token" in out, "the distribution's auth.json holds the PAT")
        s.check(top_model(wsl_config(run)) is None, "no model in the distribution's config.toml")
        m0 = wsl_exec_turn(run, s, "W89-D0", "direct")
        s.check(bool(m0), f"the distribution's Codex ran {m0!r} (the default of the PAT's model list)")
        run.state["wsl_model"] = m0
    with t.step(f"spb router enable: the plan keeps {m0} for the distribution and says so") as s:
        text, local = router_cli(run, s, True, "w89")
        s.check(re.search(rf"{re.escape(title)}: Codex keeps {re.escape(m0)}, the model it runs with now", text),
                f"the plan names the model kept in {D}")
        row = preview_row(text, title, rf'^model: .*"{re.escape(m0)}" \(Keeps the model in use\)')
        s.check(bool(row), f"the preview row of {D}: {row!r}")
        cfg = wsl_config(run)
        s.save("wsl-config-routed.txt", cfg)
        s.check("http://127.0.0.1:" in cfg, "the distribution goes through the router (WSL 1: 127.0.0.1)")
        s.check(top_model(cfg) == m0, f"the distribution's config.toml keeps model = {m0!r} ({top_model(cfg)!r})")
        s.check(top_model(local) == m0, f"this computer keeps it too (bug 58): {top_model(local)!r}", soft=True)
        s.check(len(re.findall(r"Codex keeps", text)) >= 2, "named for this computer and for the distribution",
                soft=True)
    with t.step("codex exec in the distribution through the router runs the same model") as s:
        m1 = wsl_exec_turn(run, s, "W89-R1", "routed")
        s.check(m1 == m0, f"through the router: {m1!r} (direct: {m0!r})")
        rows = []
        for _ in range(8):  # the router writes its request log asynchronously
            rr = run.spb_json("usage", "log", timeout=120, label="usage-log-w89")
            rows = [x for x in ((rr.get("json") or {}).get("rows") or []) if D in json.dumps(x)]
            if rows:
                break
            time.sleep(5)
        s.save("usage-log-wsl.json", rows[:10])
        s.check(bool(rows), f"a request row names the distribution ({len(rows)} rows)", soft=True)
    with t.step("spb router disable: the distribution runs its PAT default again") as s:
        text, local = router_cli(run, s, False, "w89")
        cfg = wsl_config(run)
        s.save("wsl-config-direct.txt", cfg)
        s.check("http://127.0.0.1" not in cfg and any(run.net.line_url(l) in cfg for l in run.net.ports),
                "the distribution is back on a test line")
        if re.search(rf"{re.escape(title)}: Codex keeps", text):
            s.info("the PAT's default changed meanwhile: the model stays and the plan says so")
            s.check(top_model(cfg) == m0, f"model = {m0!r} kept")
        else:
            s.check(top_model(cfg) is None, f"the kept model is released (the PAT gives {m0!r} itself): "
                                            f"{top_model(cfg)!r}")
            row = preview_row(text, title, r'^model: "[^"]+" → - \(No longer managed')
            s.check(bool(row), f"the preview row of {D} releases it: {row!r}", soft=True)
        m2 = wsl_exec_turn(run, s, "W89-D1", "direct-again")
        s.check(m2 == m0, f"direct again: {m2!r} (before: {m0!r})")


@scenario("W90", "WSL: 'Run Codex in WSL' — router on/off keeps the model of the app's Codex; the plan names "
                 "the distribution (bug 63)",
          area="wsl", on=SNAP, net="real", needs=("app_ready",), conflicts=("router_on",), bugs=(54, 58, 63))
def w90(t):
    run = t.run
    a = App(run)
    flows.wsl_keepalive(run)
    with t.step("Turn the mode on in config.toml (ChatGPT closed) and open ChatGPT through Superba") as s:
        legacy_mode_on(run, s, a)
    with t.step("A line switch hands the app's WSL Codex the auth address (PAT: WSLENV, full restart)") as s:
        other = "b" if run.state.get("line", "a") == "a" else "a"
        r = run.spb("switch", "--line", run.net.line_id(other), "--countdown", "0", "--yes", "--force",
                    timeout=300, label="switch-legacy-w90")
        s.save("switch.txt", r["stdout"] + r["stderr"])
        s.check(r["code"] == 0, f"switch exit {r['code']} [{r['transcript']}]")
        run.state["line"] = other
        a.wait_main(150, not_pid=None)
        end = time.time() + 150
        servers = []
        while time.time() < end and not servers:
            main, servers = wsl_app_servers(run, a)
            time.sleep(3)
        s.check(bool(servers), "the app's app-server runs in WSL again")
        App(run).onboarding(120)
    flows.turn(run, t, "E2E-LW0", expect_line=run.state["line"], new_chat=True)
    m0 = (run.state.get("turn_models") or {}).get("E2E-LW0")
    with t.step(f"spb router enable: the plan says the app's Codex in {D} keeps {m0}") as s:
        s.check(bool(m0), f"the app's Codex in WSL ran {m0!r} with the PAT")
        text, local = router_cli(run, s, True, "w90", extra=("--wait-turn",))
        s.check(re.search(rf"runs this Codex inside WSL \({re.escape(D)},.*keeps {re.escape(m0)} there too", text),
                f"the plan names {D} and the model kept there")
        s.check(top_model(local) == m0, f"config.toml (read by the app's Codex in WSL) keeps {m0!r}: "
                                        f"{top_model(local)!r}")
        row = preview_row(text, "This computer", rf'^model: .*"{re.escape(m0)}" \(Keeps the model in use\)')
        s.check(bool(row), f"the preview row: {row!r}", soft=True)
        main, servers = wsl_app_servers(run, a)
        s.check(bool(servers), "the app's app-server runs in WSL", soft=True)
        App(run).onboarding(120)
    flows.turn(run, t, "E2E-LR1", new_chat=True)
    with t.step("The app's turn through the router ran the same model") as s:
        m1 = (run.state.get("turn_models") or {}).get("E2E-LR1")
        s.check(m1 == m0, f"through the router: {m1!r} (direct: {m0!r})")
    with t.step("spb router disable: back to the PAT, the same model") as s:
        text, local = router_cli(run, s, False, "w90", extra=("--wait-turn",))
        s.check("http://127.0.0.1" not in local, "this computer is back on a line")
        App(run).onboarding(120)
    flows.turn(run, t, "E2E-LD1", expect_line=run.state["line"], new_chat=True)
    with t.step("The app's turn after the router ran the same model") as s:
        m2 = (run.state.get("turn_models") or {}).get("E2E-LD1")
        s.check(m2 == m0, f"direct again: {m2!r} (before: {m0!r})")
