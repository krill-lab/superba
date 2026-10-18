"""Codex inside WSL (user bug 45, MATRIX.md W80-W87), on snapshot `wintest-wsl1`: WSL 1,
distro `Ubuntu-WSL1` (default, user superba) with the app's Linux Codex in
~/.local/bin. WSL 2 cannot run in the pool VMs (no nested virtualization)."""

import json
import re
import time

import flows
from runner import scenario
from ui import Ui

D = flows.WSL_DISTRO
HOST = f"wsl:{D}"
SNAP = "wintest-wsl1"
WSL_FILES = flows.WSL_FILES


def wsl_hashes(run):
    return flows.wsl_hashes(run)


def wsl_show(run):
    """The distribution's Codex files and Superba's env blocks, keys redacted."""
    script = ("cd ~; echo == config.toml; cat ~/.codex/config.toml 2>&1; echo; echo == auth.json keys;"
              " sed -E 's/: *[^,}]+/: <redacted>/g' ~/.codex/auth.json 2>&1; echo; stat -c '%a %n' ~/.codex/auth.json 2>&1;"
              " for f in .profile .bashrc .zshenv; do grep -q superba $f 2>/dev/null && { echo == $f block;"
              " sed -n '/>>> superba/,/<<< superba/p' $f; }; done;"
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


@scenario("W81", "WSL: the app's Codex in WSL (WSL connections / legacy setting), hot restart", area="wsl",
          on=SNAP, manual_only=True, bugs=(45,))
def w81(t):
    t.skip("not runnable here: the app makes WSL connections only when signed in (the VM cannot sign in: "
           "chatgpt.com is region-blocked), and its legacy 'run Codex in WSL' mode is not handled yet (bug 54b)")


@scenario("W87", "WSL 2 behind NAT: router access listener + firewall rule (UAC)", area="wsl", manual_only=True,
          bugs=(45,))
def w87(t):
    t.skip("not runnable in the pool VMs: WSL 2 needs nested virtualization (see wsl-feasibility.md); run on "
           "real hardware: spb wsl router-access <distro>, approve UAC, codex exec in the distro, request row names it")
