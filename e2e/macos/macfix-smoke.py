#!/usr/bin/env python3
"""Guest-side macOS lifecycle regression checks on a leased test machine.

Install the supplied bundle and standalone CLI before running. The caller owns
the VM lease and reboot tests. Every app start uses disposable Superba/Codex
directories; existing registrations are refused. No coding-tool config is read.
"""

import argparse
import json
import os
from pathlib import Path
import plistlib
import signal
import subprocess
import sys
import time

BUNDLE_ID = "com.krill-code.superba"
LABEL = BUNDLE_ID + ".autostart"


def wait_for(description, check, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.2)
    raise AssertionError("Timed out: " + description)


class Run:
    def __init__(self, args):
        self.app = Path(args.app).resolve()
        self.cli = Path(args.cli).resolve()
        self.out = Path(args.out).resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.data = self.out / "data"
        self.data.mkdir(exist_ok=True)
        self.exe = self.app / "Contents/MacOS/superba"
        self.env = dict(os.environ, SUPERBA_DATA_DIR=str(self.data),
                        CODEX_HOME=str(self.out / "codex"),
                        CLAUDE_CONFIG_DIR=str(self.out / "claude"),
                        SUPERBA_SECRET_BACKEND="file")
        self.env.pop("SUPERBA_GUI_EXE", None)
        self.entry = Path.home() / "Library/LaunchAgents" / (LABEL + ".plist")
        self.target = "gui/" + str(os.getuid()) + "/" + LABEL
        self.child = None
        self.owns_registration = False
        self.results = []
        self.log = (self.out / "commands.log").open("w")

    def command(self, *args, check=True):
        result = subprocess.run([str(a) for a in args], env=self.env,
                                capture_output=True, text=True, timeout=45)
        self.log.write(json.dumps({"argv": [str(a) for a in args],
                                   "exit": result.returncode,
                                   "stdout": result.stdout, "stderr": result.stderr}) + "\n")
        self.log.flush()
        if check and result.returncode:
            raise AssertionError("Command failed: " + str(args[0]) + ": " + result.stderr)
        return result

    def apple(self, body, *args):
        script = "on run argv\n" + body + "\nend run"
        return self.command("/usr/bin/osascript", "-e", script, *args).stdout.strip()

    def endpoint(self):
        try:
            return json.loads((self.data / "gui-instance.json").read_text())
        except (FileNotFoundError, ValueError):
            return None

    def start(self, tray=False):
        settings = {"auto_probe": False, "auto_update_check": False,
                    "start_in_tray": tray, "locale": "en"}
        profiles = self.data / "profiles.json"
        saved = json.loads(profiles.read_text()) if profiles.exists() else {}
        saved.setdefault("settings", {}).update(settings)
        profiles.write_text(json.dumps(saved))
        self.child = subprocess.Popen([str(self.exe), "--lang", "en"], env=self.env,
                                      stdout=self.log, stderr=self.log)
        endpoint = wait_for("IPC ready", self.endpoint)
        assert endpoint["pid"] == self.child.pid
        return endpoint["pid"]

    def stop(self):
        if self.child and self.child.poll() is None:
            self.child.send_signal(signal.SIGTERM)
            self.child.wait(timeout=15)
        self.child = None
        # SIGTERM may leave the endpoint; remove only this test's stale file.
        (self.data / "gui-instance.json").unlink(missing_ok=True)

    def windows(self, pid):
        return int(self.apple('tell application "System Events"\n'
                              'tell first process whose unix id is (item 1 of argv as integer)\n'
                              'return count of windows\nend tell\nend tell', str(pid)))

    def frontmost(self, pid):
        return self.apple('tell application "System Events"\n'
                          'return unix id of first process whose frontmost is true\n'
                          'end tell') == str(pid)

    def press(self, pid, label):
        return self.apple('set buttonLabel to item 2 of argv\n'
                          'tell application "System Events"\n'
                          'tell first process whose unix id is (item 1 of argv as integer)\n'
                          'set frontmost to true\n'
                          'set elements to entire contents of window 1\n'
                          'repeat with e in elements\nset node to contents of e\n'
                          'try\nif name of node is buttonLabel and '
                          '(role of node is "AXButton" or role of node is "AXRadioButton") then\n'
                          'perform action "AXPress" of node\nreturn "pressed"\nend if\n'
                          'end try\nend repeat\nend tell\nend tell\nreturn "missing"',
                          str(pid), label)

    def shot(self, name):
        self.command("/usr/sbin/screencapture", "-x", self.out / (name + ".png"))

    def passed(self, name):
        self.results.append(name)
        (self.out / "results.json").write_text(json.dumps(self.results, indent=2) + "\n")
        print("PASS", name, flush=True)

    def smoke(self):
        assert sys.platform == "darwin"
        assert self.exe.is_file() and self.cli.is_file()
        assert not self.entry.exists(), "Existing login registration: clean up the preceding test first"
        self.command(self.cli, "autostart", "on")
        self.owns_registration = True
        entry = plistlib.loads(self.entry.read_bytes())
        assert entry["ProgramArguments"] == [str(self.exe), "--autostart"]
        assert entry["AssociatedBundleIdentifiers"] == [BUNDLE_ID]
        self.command("/usr/bin/plutil", "-lint", self.entry)
        (self.out / "registered.plist").write_bytes(self.entry.read_bytes())
        self.passed("D1 standalone CLI finds the installed app; D2 direct program and bundle attribution")

        pid = self.start()
        wait_for("first window", lambda: self.windows(pid) == 1)
        wait_for("skip setup button", lambda: self.press(pid, "Skip setup") == "pressed")
        wait_for("persisted dismissal", lambda: json.loads((self.data / "profiles.json").read_text())
                 .get("settings", {}).get("wizard_dismissed"))
        self.shot("wizard-skipped")
        names = self.apple('tell application "System Events"\n'
                           'tell first process whose unix id is (item 1 of argv as integer)\n'
                           'return name of every menu item of menu 1 of menu bar item 2 of menu bar 1\n'
                           'end tell\nend tell', str(pid))
        assert "About Superba" in names and "Quit Superba" in names, names
        assert "About superba" not in names and "Quit superba" not in names
        self.passed("D5 application menu uses Superba")
        self.stop()

        pid = self.start()
        wait_for("restarted window", lambda: self.windows(pid) == 1)
        time.sleep(2)
        assert self.press(pid, "Skip setup") == "missing", "Wizard reappeared"
        self.shot("restart-without-wizard")
        self.passed("Skipped wizard stays dismissed after restarting")
        self.stop()

        for action in ["open", "reopen", "ipc"]:
            pid = self.start(tray=True)  # no --autostart: also the restored-start policy
            time.sleep(2)
            assert self.windows(pid) == 0, "Saved tray choice must apply without flags"
            self.command(self.exe, "--autostart")
            assert self.windows(pid) == 0, "Login IPC must stay hidden"
            if action == "open":
                self.command("/usr/bin/open", "-a", self.app)
            elif action == "reopen":
                self.apple('tell application id "' + BUNDLE_ID + '" to reopen')
            else:
                self.command(self.exe)
            wait_for(action + " shows window", lambda: self.windows(pid) == 1)
            wait_for(action + " focuses window", lambda: self.frontmost(pid))
            assert self.endpoint()["pid"] == pid, "Second launch replaced the instance"
            self.shot("D3-" + action)
            self.passed("D3 " + action + " shows and focuses the existing window")
            self.stop()

        pid = self.start(tray=True)
        # Test-only isolation for the process launchd will create. Product plist
        # arguments and bundle identity have already been checked above.
        entry["EnvironmentVariables"] = {k: self.env[k] for k in
                                         ["SUPERBA_DATA_DIR", "CODEX_HOME", "CLAUDE_CONFIG_DIR",
                                          "SUPERBA_SECRET_BACKEND"]}
        self.entry.write_bytes(plistlib.dumps(entry))
        self.command("/bin/launchctl", "bootstrap", "gui/" + str(os.getuid()), self.entry)
        self.command("/bin/launchctl", "print", self.target)
        time.sleep(2)
        assert self.windows(pid) == 0
        btm = self.command("/usr/bin/sudo", "/usr/bin/sfltool", "dumpbtm")
        (self.out / "login-items.txt").write_text(btm.stdout)
        self.command(self.cli, "autostart", "off")
        assert not self.entry.exists()
        assert self.command("/bin/launchctl", "print", self.target, check=False).returncode != 0
        self.command(self.cli, "autostart", "off")
        self.passed("D6 off removes the plist and loaded job; repeated off succeeds")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", required=True)
    parser.add_argument("--cli", required=True)
    parser.add_argument("--out", required=True)
    run = Run(parser.parse_args())
    try:
        run.smoke()
    finally:
        run.stop()
        # Never remove an entry that predates this invocation.
        if run.owns_registration:
            run.command(run.cli, "autostart", "off", check=False)
        run.log.close()


if __name__ == "__main__":
    main()
