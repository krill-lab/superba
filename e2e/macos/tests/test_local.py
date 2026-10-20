import hashlib
import json
import os
import plistlib
import subprocess
import sys
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from e2e.macos.guest import desktop, guest
from e2e.macos.lib import scenarios  # noqa: F401
from e2e.macos.lib.local import Local, LocalLease
from e2e.macos.lib.runner import Case, REGISTRY, Runner, Scenario
from e2e.macos.lib.vm import Blocked, VmError
from e2e.macos.package import metadata, record, stamp


class LocalTransportTests(unittest.TestCase):
    def test_local_execution_uses_stdin_without_inheriting_secrets_or_shell_startup(self):
        with tempfile.TemporaryDirectory() as directory:
            local = Local(Path(directory) / "owned")
            with patch.dict(os.environ, {"BASH_ENV": "/must/not/read", "GITHUB_TOKEN": "private-value"}):
                result = local.shell('read -r value; printf "%s/%s" "$value" "${GITHUB_TOKEN-unset}"',
                                     stdin="protocol-input\n")
            self.assertEqual(result.stdout, "protocol-input/unset")
            self.assertFalse(hasattr(local, "helper"))

    def test_root_and_transfer_ownership(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            foreign = base / "foreign"
            foreign.mkdir()
            with self.assertRaises(VmError):
                Local(foreign)
            local = Local(base / "owned")
            source = base / "source"
            source.write_text("bytes")
            local.put(source, local.root + "/copy")
            self.assertEqual(Path(local.root, "copy").read_text(), "bytes")
            for destination in (str(foreign / "copy"), local.root + "/../outside"):
                with self.assertRaises(VmError):
                    local.put(source, destination)
            Path(local.root, "link").symlink_to(foreign, target_is_directory=True)
            with self.assertRaises(VmError):
                local.put(source, local.root + "/link/copy")

    def test_timeout_does_not_echo_input_and_reboot_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            local = Local(Path(directory) / "owned")
            with patch("subprocess.run", side_effect=subprocess.TimeoutExpired("secret-command", 1)):
                with self.assertRaisesRegex(VmError, "Local command timed out") as error:
                    local.shell("secret-command", stdin="private-input")
            self.assertNotIn("secret", str(error.exception))
            with self.assertRaisesRegex(VmError, "never reboots"):
                local.wait_reboot("before")

    @unittest.skipUnless(os.name == "posix", "local macOS locks use POSIX flock")
    def test_local_lease_blocks_other_holders_and_recovers_after_process_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "LOCK"
            with LocalLease(path):
                with self.assertRaises(Blocked):
                    with LocalLease(path):
                        self.fail("Two holders acquired the same local lock")
            # Simulate interruption without __exit__: the persistent file must
            # not prevent the always-run cleanup process acquiring the lock.
            child = subprocess.run([sys.executable, "-c",
                                    "import os,sys; from e2e.macos.lib.local import LocalLease; "
                                    "lock=LocalLease(sys.argv[1]); lock.__enter__(); os._exit(17)",
                                    str(path)], timeout=10, capture_output=True)
            self.assertEqual(child.returncode, 17)
            with LocalLease(path):
                self.assertTrue(path.is_file())

    @unittest.skipUnless(os.name == "posix", "local macOS locks use POSIX flock")
    def test_local_lease_does_not_follow_a_replaced_lock_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "LOCK"
            foreign = Path(directory) / "foreign"
            foreign.write_text("unmodified")
            path.symlink_to(foreign)
            with self.assertRaises(OSError):
                with LocalLease(path):
                    self.fail("Followed a lock symlink")
            self.assertEqual(foreign.read_text(), "unmodified")


class ScreenshotSecretTests(unittest.TestCase):
    def test_real_credentials_prevent_both_window_and_full_screen_capture(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = types.SimpleNamespace(out=Path(directory), opts=types.SimpleNamespace(net="real"))
            case = Case(runner, Scenario("M02", "Secret screenshot", None))
            case.credentials_used = True
            # No transport or AX object is provided: neither capture may reach
            # a tool once credentials could be visible in image pixels.
            case.shot("window")
            case.fullshot("screen")
            self.assertEqual(len(case.report["unavailable"]), 2)
            self.assertEqual(list(case.evdir.iterdir()), [])
            case.credentials_used = False
            self.assertTrue(case.capture_allowed("before-credentials"))
            case.credentials_used = True
            runner.opts.net = "mock"
            self.assertTrue(case.capture_allowed("mock"))


class FallbackTests(unittest.TestCase):
    def test_setup_failure_keeps_a_scrubbed_report_and_cannot_become_a_partial_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            opts = types.SimpleNamespace(out=directory, env=None, net="real")
            vm = types.SimpleNamespace(local=True, root=directory)

            def fail():
                raise VmError("setup error private-test-key")

            vm.prepare = fail
            with patch.dict(os.environ, {"SUPERBA_E2E_KEY": "private-test-key",
                                        "SUPERBA_E2E_UPSTREAM": "https://relay.invalid"}):
                runner = Runner(vm, opts)
            self.assertEqual(runner.run(["M01", "M02"]), 1)
            report = json.loads(Path(directory, "report.json").read_text())
            self.assertEqual(report["counts"]["FAIL"], 2)
            for path in Path(directory).iterdir():
                self.assertNotIn("private-test-key", path.read_text())

    def run_case(self, directory, cli_code=0, cleanup_ok=True):
        operations = []

        def rpc(op, **_):
            operations.append(op)
            return {"disk": {"free_bytes": 16 * 1024**3},
                    "create": {"session": "a" * 32, "root": directory + "/session"},
                    "spb": {"code": cli_code, "stdout": "spb version", "stderr": ""},
                    "read": None, "cleanup": {"ok": cleanup_ok}}[op]

        vm = types.SimpleNamespace(local=True, root=directory, prepare=lambda: {}, rpc=rpc,
                                   shell=lambda *a, **kw: types.SimpleNamespace(returncode=1, stderr="no display"))
        opts = types.SimpleNamespace(out=directory + "/out", env=None, net="real")
        with patch.dict(os.environ, {"SUPERBA_E2E_KEY": "", "SUPERBA_E2E_UPSTREAM": ""}):
            runner = Runner(vm, opts)
        runner.agent = types.SimpleNamespace(probe=lambda: {"accessibility": False, "screen_capture": False})

        def run(t):
            self.assertFalse(t.gui)
            t.spb("--version")
            t.shot("fallback")

        with patch.dict(REGISTRY, {"M01": Scenario("M01", "CLI fallback", run)}):
            code = runner.run(["M01"])
        return code, json.loads(Path(opts.out, "report.json").read_text()), operations

    def test_denied_ax_runs_cli_and_records_each_missing_check_without_a_false_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            code, report, operations = self.run_case(directory)
        self.assertEqual(code, 2)
        self.assertEqual(report["network"], "mock")
        self.assertEqual(report["counts"]["PASS"], 0)
        self.assertEqual(report["counts"]["PARTIAL"], 1)
        self.assertIn("spb", operations)
        self.assertEqual(operations[-1], "cleanup")
        unavailable = report["results"][0]["unavailable"]
        self.assertTrue(any("wizard" in item for item in unavailable))
        self.assertTrue(any("Full-screen capture" in item for item in unavailable))

    def test_cli_and_cleanup_failures_stay_failures_in_fallback_mode(self):
        for cli_code, cleanup_ok in ((1, True), (0, False)):
            with self.subTest(cli_code=cli_code), tempfile.TemporaryDirectory() as directory:
                code, report, _ = self.run_case(directory, cli_code, cleanup_ok)
            self.assertEqual(code, 1)
            self.assertEqual(report["counts"]["FAIL"], 1)


class PackageTests(unittest.TestCase):
    def stage(self, root, version="0.1.1-alpha.8"):
        stage = root / "stage"
        app = stage / "Superba.app/Contents"
        (app / "MacOS").mkdir(parents=True)
        (app / "Resources").mkdir()
        (app / "Info.plist").write_bytes(plistlib.dumps({"CFBundleVersion": "0.1.1",
                                                       "CFBundleShortVersionString": "0.1.1"}))
        notices = "Superba third-party notices\nUsed by:\n" + "License text\n" * 100
        for path in (stage / "THIRD-PARTY-NOTICES.txt", app / "Resources/THIRD-PARTY-NOTICES.txt"):
            path.write_text(notices)
        for name in ("superba", "superba-router"):
            (app / "MacOS" / name).write_bytes(name.encode())
        (stage / "spb").write_bytes(b"cli")
        build = {"source_sha": "a" * 40, "version": version, "bundle_version": "0.1.1"}
        return stage, build

    def test_exact_prerelease_and_source_are_embedded_and_all_bytes_are_recorded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            stage, build = self.stage(root)
            stamped = stamp(stage, build)
            info = plistlib.loads((stage / "Superba.app/Contents/Info.plist").read_bytes())
            self.assertEqual(info["SuperbaVersion"], "0.1.1-alpha.8")
            self.assertEqual(info["SuperbaSourceSHA"], "a" * 40)
            dmg = root / "current.dmg"
            dmg.write_bytes(b"current-source-image")
            manifest = record(stage, dmg, root / "package.json")
            self.assertEqual(manifest["build"], stamped)
            self.assertEqual(manifest["dmg_sha256"], hashlib.sha256(dmg.read_bytes()).hexdigest())
            self.assertEqual(set(manifest["executables"]), {"superba", "superba-router", "spb"})

    def test_placeholder_notices_and_incorrect_bundle_versions_are_rejected(self):
        for mutation in ("notices", "version"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                stage, build = self.stage(Path(directory))
                if mutation == "notices":
                    (stage / "Superba.app/Contents/Resources/THIRD-PARTY-NOTICES.txt").write_text("placeholder")
                else:
                    build["bundle_version"] = "0.1.0"
                with self.assertRaises(ValueError):
                    stamp(stage, build)

    def test_wrong_checkout_or_dirty_tracked_source_is_rejected(self):
        with patch("subprocess.check_output", return_value="b" * 40 + "\n"):
            with self.assertRaisesRegex(ValueError, "dispatched commit"):
                metadata("a" * 40)
        with patch("subprocess.check_output", return_value="a" * 40 + "\n"), \
                patch("subprocess.run", side_effect=subprocess.CalledProcessError(1, "git diff")):
            with self.assertRaises(subprocess.CalledProcessError):
                metadata("a" * 40)

    def test_wrong_dmg_is_rejected_before_mount_or_install(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dmg = root / "old.dmg"
            dmg.write_bytes(b"wrong-old-package")
            guest.save(root, {"installed": False})
            with patch.object(guest, "command") as command:
                with self.assertRaisesRegex(RuntimeError, "digest differs"):
                    guest.install(root, dmg, expected={"dmg_sha256": "0" * 64})
            command.assert_not_called()


class DesktopVerificationTests(unittest.TestCase):
    def test_desktop_launch_allocates_a_local_port_and_uses_only_the_test_home(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app = root / "Applications/ChatGPT.app"
            (app / "Contents").mkdir(parents=True)
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleExecutable": "ChatGPT"}))
            guest.save(root, {"env": {}, "pids": []})

            def test_path(path):
                return root / path.lstrip("/") if path.startswith("/Applications/") else Path(path)

            with patch.object(guest, "session", return_value=root), \
                    patch.object(guest, "Path", side_effect=test_path), \
                    patch.object(guest, "desktop_user_data", return_value=[]), \
                    patch.object(guest, "start", return_value={"pid": 42}) as start:
                result = guest.dispatch("desktop", {"session": "fixture"})
            self.assertGreater(result["debug_port"], 0)
            self.assertEqual(start.call_args.args[1], app / "Contents/MacOS/ChatGPT")
            self.assertIn("--user-data-dir=" + str(root / "home/desktop"), start.call_args.args[2])
            self.assertEqual(guest.state(root)["desktop_owned_app"], str(app))

    def test_download_redirects_cannot_leave_the_official_https_origin(self):
        self.assertEqual(desktop.official_url(desktop.DOWNLOAD), desktop.DOWNLOAD)
        for url in ("http://persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg",
                    "https://persistent.oaistatic.com.attacker.invalid/codex-app-prod/ChatGPT.dmg",
                    "https://persistent.oaistatic.com/sidekick/ChatGPT.dmg",
                    "https://secret@persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                desktop.official_url(url)

    def verify(self, root, failure=None):
        calls = []
        slot = root / "applications/ChatGPT.app"
        state = {"mounts": []}

        def command(argv, **_):
            argv = [str(arg) for arg in argv]
            calls.append(argv)
            output = ""
            name = Path(argv[0]).name
            if name == "hdiutil" and "attach" in argv:
                app = root / "chatgpt-mount/ChatGPT.app/Contents"
                app.mkdir(parents=True)
                (app / "Info.plist").write_bytes(plistlib.dumps({"CFBundleIdentifier": "com.openai.codex",
                                                                "CFBundleShortVersionString": "test"}))
            if name == "codesign" and "-dv" in argv:
                output = "Authority=Developer ID Application: OpenAI OpCo, LLC (2DC432GLL2)\nTeamIdentifier=2DC432GLL2\n"
            if name == "spctl":
                output = "accepted\nsource=Notarized Developer ID\n"
            failed = failure == name or (failure == "identity" and "-dv" in argv)
            return types.SimpleNamespace(returncode=int(failed), stdout="", stderr=output)

        fake = types.SimpleNamespace(Path=lambda name: slot if name == "/Applications/ChatGPT.app" else root / "absent",
                                     Blocked=guest.Blocked, desktop_user_data=lambda: [], command=command,
                                     state=lambda _: state, save=lambda _, value: state.update(value))
        with patch.object(desktop, "download", return_value={"sha256": "fixture"}):
            if failure:
                with self.assertRaises(RuntimeError):
                    desktop.install(root, fake)
            else:
                desktop.install(root, fake)
        return calls, json.loads((root / "desktop-verification.json").read_text()), state

    def test_signature_notarization_and_ticket_must_pass_before_install(self):
        for failure in ("identity", "codesign", "spctl", "xcrun"):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                calls, report, state = self.verify(Path(directory), failure)
            self.assertFalse(any(Path(call[0]).name == "ditto" for call in calls))
            self.assertNotIn("desktop_installed_by_session", state)
            self.assertEqual(report["status"], "FAIL")
            self.assertTrue(any(check["code"] for check in report["checks"]))

    def test_verified_app_is_claimed_after_all_checks_without_login(self):
        with tempfile.TemporaryDirectory() as directory:
            calls, report, state = self.verify(Path(directory))
        names = [Path(call[0]).name for call in calls]
        self.assertLess(names.index("xcrun"), names.index("ditto"))
        self.assertEqual(report["status"], "VERIFIED")
        self.assertFalse(report["account_login"])
        self.assertIn("desktop_installed_by_session", state)
        self.assertEqual(state["mounts"], [])


class LoginSimulationTests(unittest.TestCase):
    def test_bootstrap_and_kickstart_only_use_the_owned_product_plist(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plist = root / "login.plist"
            plist.write_bytes(plistlib.dumps({"Label": guest.GUI_LABEL, "EnvironmentVariables": {"HOME": str(root)}}))
            guest.save(root, {"login_plists": [str(plist)]})
            with patch.object(guest, "session", return_value=root), \
                    patch.object(guest, "command", return_value=types.SimpleNamespace(returncode=1)), \
                    patch.object(guest, "checked", return_value="") as checked:
                result = guest.dispatch("login_simulate", {"session": "fixture"})
            self.assertFalse(result["rebooted"])
            self.assertEqual([call.args[0][1] for call in checked.call_args_list], ["bootstrap", "kickstart"])
            self.assertEqual(checked.call_args_list[0].args[0][-1], str(plist))

    def test_foreign_launchd_job_is_not_unloaded(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plist = root / "login.plist"
            plist.write_text(str(root))
            guest.save(root, {"login_plists": [str(plist)]})
            with patch.object(guest, "session", return_value=root), \
                    patch.object(guest, "command", return_value=types.SimpleNamespace(returncode=0, stdout="someone-else")), \
                    patch.object(guest, "checked") as checked:
                with self.assertRaises(guest.Blocked):
                    guest.dispatch("login_simulate", {"session": "fixture"})
            checked.assert_not_called()

    def test_service_evidence_contains_no_raw_launchd_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = str(root) + "\nenvironment = { SECRET = private-input }"
            with patch.object(guest, "session", return_value=root), \
                    patch.object(guest, "command", return_value=types.SimpleNamespace(returncode=0, stdout=raw)), \
                    patch.object(guest, "checked", return_value="boot"):
                result = guest.dispatch("service", {"session": "fixture"})
            self.assertTrue(result["owned"])
            self.assertNotIn("private-input", json.dumps(result))
            self.assertNotIn("stdout", result)


if __name__ == "__main__":
    unittest.main()
