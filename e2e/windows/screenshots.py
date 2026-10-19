#!/usr/bin/env python3
"""Capture the Superba tabs on Windows using an MSI and synthetic fixtures.

Acquires a disposable wintest VM, installs the supplied MSI, drives the real GUI
through Ui.superba, and always releases the VM. The source commit is supplied
explicitly because an MSI's version alone cannot identify its source. See README.
"""
import argparse
import hashlib
import importlib.util
import json
import sys
import time
from pathlib import Path
from types import SimpleNamespace

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "lib"))
from agent import Agent
from strings import Strings
from ui import Ui, SUPERBA_SECTIONS, SUPERBA_TABS, SETTINGS_ANCHORS
from vm import Vm, psq

REPO = HERE.parents[1]
DEMO = r"C:\Users\Public\SuperbaDemo"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--msi", type=Path, required=True)
    ap.add_argument("--source-commit", required=True)
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("--evidence", type=Path, required=True)
    ap.add_argument("--vm")
    args = ap.parse_args()
    if len(args.source_commit) != 40 or any(c not in "0123456789abcdef" for c in args.source_commit):
        ap.error("--source-commit must be the full source commit of the MSI")
    args.out.mkdir(parents=True, exist_ok=True)
    args.evidence.mkdir(parents=True, exist_ok=True)
    log_file = (args.evidence / "capture.log").open("a", buffering=1)

    def log(message):
        print(message, flush=True)
        log_file.write(message + "\n")

    vm = Vm(log=log)
    agent = None
    pid = None
    run = None
    manifest = {"source_commit": args.source_commit, "msi": args.msi.name,
                "msi_sha256": hashlib.sha256(args.msi.read_bytes()).hexdigest(),
                "completed": False, "screenshots": [], "narrow_checks": []}
    try:
        vm.acquire("wintest", vm_id=args.vm, label="q1b documentation screenshots", ttl="2h")
        manifest["vm"] = vm.id
        agent = Agent(vm, log=log)
        agent.start()
        vm.ps(f"New-Item -ItemType Directory -Force {psq(DEMO)}, {psq(DEMO + '/Home')}, "
              f"{psq(DEMO + '/Data')}, {psq(DEMO + '/Shots')} | Out-Null")
        vm.scp_to(args.msi, DEMO.replace("\\", "/") + "/superba.msi")
        result = agent.exec("msiexec.exe", ["/i", DEMO + r"\superba.msi", "/qn", "/norestart",
                            "INSTALLFOLDER=" + DEMO + r"\App", "/l*v", DEMO + r"\install.log"], timeout=600)
        vm.scp_from(DEMO.replace("\\", "/") + "/install.log", args.evidence / "install.log")
        if result["code"] != 0:
            raise RuntimeError(f"MSI install failed: {result['code']}")
        # The custom per-user install folder is recorded by the MSI.
        install = vm.ps(r"(Get-ItemProperty 'HKCU:\Software\Krill\Superba').InstallDir").stdout.strip()
        env = {"SUPERBA_DATA_DIR": DEMO + r"\Data", "CODEX_HOME": DEMO + r"\Home\.codex",
               "CODEX_SQLITE_HOME": DEMO + r"\Home\.codex",
               "CLAUDE_CONFIG_DIR": DEMO + r"\Home\.claude", "SUPERBA_SECRET_BACKEND": "file",
               "HOME": DEMO + r"\Home", "USERPROFILE": DEMO + r"\Home",
               "APPDATA": DEMO + r"\Home\AppData\Roaming", "LOCALAPPDATA": DEMO + r"\Home\AppData\Local",
               "PATH": install + r";C:\Windows\System32;C:\Windows", "NO_COLOR": "1"}
        vm.ps("; ".join(f"New-Item -ItemType Directory -Force {psq(p)} | Out-Null"
                        for k, p in env.items() if k in ("CODEX_HOME", "CLAUDE_CONFIG_DIR", "APPDATA", "LOCALAPPDATA")))

        def spb(*argv, stdin=None):
            result = agent.exec(install + r"\spb.exe", argv, env=env, cwd=DEMO, stdin=stdin)
            if result["code"] != 0:
                raise RuntimeError(f"spb {argv[:2]} failed: {result['stderr']}")
            return result["stdout"]

        manifest["version"] = spb("--version").strip()
        spb("site", "add", "example-api", "--name", "Example API", "--base-url", "https://api.example.invalid/v1")
        spb("profile", "add", "example-api", "--name", "Example API", "--site", "example-api",
            "--auth", "apikey", "--key-stdin", stdin="superba-screenshot-fixture-only\n")
        vm.ps(f"$p={psq(DEMO + '/Data/profiles.json')}; $j=Get-Content -Raw $p | ConvertFrom-Json; "
              "$j.settings.auto_update_check=$false; $j.settings.wizard_dismissed=$true; "
              "[IO.File]::WriteAllText($p, ($j | ConvertTo-Json -Depth 50), (New-Object Text.UTF8Encoding $false))")
        fixtures = REPO / "crates/superba-gui/tools/screenshots/fixtures.py"
        spec = importlib.util.spec_from_file_location("shot_fixtures", fixtures)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        module.main(["--data-dir", str(args.evidence / "fixture/data"),
                     "--codex-home", str(args.evidence / "fixture/codex"), "--upstream", "api.example.invalid:443"])
        vm.ps(f"New-Item -ItemType Directory -Force {psq(DEMO + '/Data/router')} | Out-Null")
        vm.scp_to(args.evidence / "fixture/data/router/usage.sqlite3", DEMO.replace("\\", "/") + "/Data/router/usage.sqlite3")
        vm.scp_to(args.evidence / "fixture/codex/state_5.sqlite", DEMO.replace("\\", "/") + "/Home/.codex/state_5.sqlite")
        run = SimpleNamespace(agent=agent, strings={lang: Strings(lang) for lang in ("en", "zh-CN")}, log=log)
        for theme, lang in (("dark", "en"), ("light", "en"), ("dark", "zh-CN"), ("light", "zh-CN")):
            pid = agent.start_proc(install + r"\superba.exe", ["--size", "1280x800", "--theme", theme,
                                   "--lang", lang, "--page", "superba"], env=env, cwd=DEMO)
            u = Ui(run, lang)
            u.wait_window()
            time.sleep(4)
            u.resize(1280, 800, x=40, y=40)
            for section in SUPERBA_SECTIONS:
                name = f"{'superba' if section == 'home' else section}-{theme}-{'zh' if lang == 'zh-CN' else 'en'}"
                try:
                    u.superba(section)
                    time.sleep(1)
                except Exception:
                    guest_png = DEMO + "\\Shots\\failure.png"
                    agent.shot(guest_png, **u.win)
                    vm.scp_from(guest_png.replace("\\", "/"), args.evidence / f"failure-{name}.png")
                    raise
                finally:
                    (args.evidence / f"{name}.json").write_text(json.dumps(u.tree(), ensure_ascii=False, indent=2))
                agent.call("hover", x=1800, y=1000)
                guest_png = DEMO + "\\Shots\\" + name + ".png"
                agent.shot(guest_png, **u.win)
                path = args.out / (name + ".png")
                vm.scp_from(guest_png.replace("\\", "/"), path)
                manifest["screenshots"].append({"file": path.name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()})
                log("captured " + name)
            # The same selectors must work when the jump row moves below search.
            u.resize(960, 640)
            for section in SETTINGS_ANCHORS:
                u.superba(section)
                left, top, width, height = u.window()["rect"]
                controls = [u.wait(key=key, type="TabItem", within=u.superba_tab_list()) for key in SUPERBA_TABS.values()]
                controls += [u.wait(key=f"superba.section.{a}", type="Button") for a in SETTINGS_ANCHORS]
                for control in controls:
                    x, y, w, h = control["rect"]
                    assert w > 0 and h > 0 and left <= x and top <= y and x + w <= left + width and y + h <= top + height, control
                (args.evidence / f"960x640-{section}-{theme}-{lang}.json").write_text(json.dumps(u.tree(), ensure_ascii=False, indent=2))
                manifest["narrow_checks"].append({"section": section, "theme": theme, "lang": lang})
            agent.call("kill", pid=pid, tree=True)
            pid = None
        manifest["completed"] = True
    finally:
        try:
            manifest["navigation_warnings"] = getattr(run, "navigation_warnings", [])
            (args.evidence / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        finally:
            try:
                if agent:
                    try:
                        if pid:
                            agent.call("kill", pid=pid, tree=True)
                    finally:
                        agent.stop()
            finally:
                try:
                    if vm.pool_held:
                        vm.release()
                finally:
                    log_file.close()


if __name__ == "__main__":
    main()
