import importlib.util
import io
import json
import os
import plistlib
import subprocess
import tarfile
import tempfile
import types
import unittest
from pathlib import Path
from unittest.mock import patch

from e2e.macos.build import extract_archive
from e2e.macos.lib.agent import Agent
from e2e.macos.lib.runner import Case, Runner, Scenario
from e2e.macos.lib.security import Redactor, load_env
from e2e.macos.lib.ui import Ui
from e2e.macos.lib.vm import Blocked, Lease, Vm, VmError
from e2e.shared import desktop_flows


class LeaseTests(unittest.TestCase):
    def test_held_lease_is_not_replaced_or_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "LOCK"
            path.write_text("someone else")
            with self.assertRaises(Blocked):
                with Lease(path):
                    pass
            self.assertEqual(path.read_text(), "someone else")

    def test_lease_released_on_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "LOCK"
            with self.assertRaises(ValueError):
                with Lease(path):
                    self.assertTrue(path.read_text().startswith("mx "))
                    raise ValueError("failure")
            self.assertFalse(path.exists())

    def test_new_lease_survives_old_owner_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "LOCK"
            with Lease(path):
                path.unlink()
                path.write_text("replacement owner")
            self.assertEqual(path.read_text(), "replacement owner")


class SecretTests(unittest.TestCase):
    def test_env_is_data_not_shell_code(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "test.env"
            path.write_text("export SUPERBA_E2E_KEY='$(touch unwanted)'\nURL=https://example.invalid # comment\n")
            result = load_env(path)
            self.assertEqual(result["SUPERBA_E2E_KEY"], "$(touch unwanted)")
            self.assertEqual(result["URL"], "https://example.invalid")
            self.assertEqual(list(Path(directory).iterdir()), [path])

    def test_raw_and_generated_keys_removed_before_json_persistence(self):
        redactor = Redactor(["secret-value-0123", "https://upstream.invalid"])
        text = redactor.json({"stdout": 'secret-value-0123 Bearer abcdef\n"router_token": "generated-token"',
                              "url": "https://upstream.invalid/v1", "token": "sbs_deadbeef_123abc"})
        for secret in ("secret-value-0123", "generated-token", "sbs_deadbeef_123abc", "abcdef", "upstream.invalid"):
            self.assertNotIn(secret, text)
        json.loads(text)

    def test_ssh_timeout_does_not_echo_secret_input(self):
        with tempfile.TemporaryDirectory() as directory:
            helper = Path(directory) / "ssh"
            helper.touch()
            vm = Vm(helper)
            with patch("subprocess.run", side_effect=subprocess.TimeoutExpired("command", 1, output="secret-input")):
                with self.assertRaises(VmError) as error:
                    vm.shell("command", stdin="secret-input")
            self.assertNotIn("secret-input", str(error.exception))


class ArchiveTests(unittest.TestCase):
    def extract(self, name, link=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "source.tar"
            with tarfile.open(archive, "w") as tar:
                member = tarfile.TarInfo(name)
                if link:
                    member.type = tarfile.SYMTYPE
                    member.linkname = link
                    tar.addfile(member)
                else:
                    member.size = 2
                    tar.addfile(member, io.BytesIO(b"ok"))
            destination = root / "source"
            destination.mkdir()
            extract_archive(archive, destination)
            return (destination / name).read_text()

    def test_valid_archive(self):
        self.assertEqual(self.extract("src/lib.rs"), "ok")

    def test_private_or_escaping_paths_rejected(self):
        for name in (str(Path(".secrets") / "token"), str(Path(".private") / "notes"),
                     "a/../../outside", "/absolute"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                self.extract(name)

    def test_escaping_link_rejected(self):
        with self.assertRaises(ValueError):
            self.extract("src/link", "../../outside")


class ProtocolTests(unittest.TestCase):
    def test_no_secret_on_ssh_command_line(self):
        calls = []

        def shell(command, **kwargs):
            calls.append((command, kwargs))
            return types.SimpleNamespace(stdout='{"ok":true,"result":true}', returncode=0)

        vm = types.SimpleNamespace(root="/Users/Shared/test", shell=shell)
        Agent(vm).call("uia.act", action="type", value="secret-value")
        self.assertNotIn("secret-value", calls[0][0])
        self.assertEqual(json.loads(calls[0][1]["stdin"])["args"]["value"], "secret-value")

    def test_tcc_denial_has_executable_and_does_not_retry_input(self):
        calls = []

        def shell(command, **kwargs):
            calls.append(command)
            return types.SimpleNamespace(stdout=json.dumps({"ok": True, "result": {
                "accessibility": False, "screen_capture": True, "executable": "/agent/MacAgent",
                "instructions": "Allow in Accessibility"}}), returncode=0)

        with self.assertRaisesRegex(Blocked, "/agent/MacAgent"):
            Agent(types.SimpleNamespace(root="/agent", shell=shell)).preflight()
        self.assertEqual(len(calls), 1)

    def test_ui_selection_by_name_role_and_pid(self):
        calls = []

        def call(cmd, **args):
            calls.append((cmd, args))
            return [{"name": "Apply"}]

        run = types.SimpleNamespace(agent=types.SimpleNamespace(call=call), strings={})
        ui = Ui(run, pid=42)
        ui.expect(name="Apply", type="Button", timeout=3)
        self.assertEqual(calls[0][0], "uia.find")
        self.assertEqual(calls[0][1]["pid"], 42)
        self.assertEqual(calls[0][1]["timeout_ms"], 3000)

    def test_shared_wizard_flow_uses_locale_keys(self):
        calls = []
        ui = types.SimpleNamespace(wait=lambda **kw: calls.append(("wait", kw)),
                                   click=lambda **kw: calls.append(("click", kw)),
                                   wait_gone=lambda **kw: calls.append(("gone", kw)))
        desktop_flows.wizard_skip(ui)
        self.assertEqual(calls[1], ("click", {"key": "wizard.skip"}))
        self.assertEqual(calls[-1], ("gone", {"key": "wizard.detect.title"}))


class CleanupTests(unittest.TestCase):
    def case(self, directory, fn, broken_log=False, cleanup_ok=True):
        calls = []

        def rpc(operation, **_):
            calls.append(operation)
            if operation == "create":
                return {"session": "abc", "root": "/Users/Shared/test/runs/abc"}
            if operation == "read":
                if broken_log:
                    raise VmError("log read failed")
                return "secret-fixture"
            if operation == "cleanup":
                return {"ok": cleanup_ok}
            raise AssertionError(operation)

        runner = types.SimpleNamespace(out=Path(directory), redactor=Redactor(["secret-fixture"]),
                                       vm=types.SimpleNamespace(rpc=rpc), log=lambda _: None)
        case = Case(runner, Scenario("M00", "failure injection", fn))
        return case, calls

    def test_failed_case_still_cleans_and_scrubs_log(self):
        def fail(_):
            raise ValueError("secret-fixture")

        with tempfile.TemporaryDirectory() as directory:
            case, calls = self.case(directory, fail)
            result = case.run()
            self.assertEqual(result["status"], "FAIL")
            self.assertEqual(calls[-1], "cleanup")
            for path in Path(directory).rglob("*"):
                if path.is_file():
                    self.assertNotIn("secret-fixture", path.read_text())

    def test_tcc_blocks_all_cases_before_install_or_secret_copy(self):
        from e2e.macos.lib import scenarios  # noqa: F401

        operations = []

        def rpc(op, **_):
            operations.append(op)
            self.assertEqual(op, "disk")
            return {"free_bytes": 17 * 1024**3}

        def denied():
            raise Blocked("Accessibility permission required for MacAgent")

        with tempfile.TemporaryDirectory() as directory:
            opts = types.SimpleNamespace(out=directory, env=None)
            vm = types.SimpleNamespace(root="/Users/Shared/test", prepare=lambda: None, rpc=rpc)
            runner = Runner(vm, opts)
            runner.agent = types.SimpleNamespace(preflight=denied)
            self.assertEqual(runner.run(["M01", "M02", "M03", "M04"]), 2)
            report = json.loads((Path(directory) / "report.json").read_text())
            self.assertEqual(report["counts"]["BLOCKED"], 4)
            self.assertEqual(report["counts"]["PASS"], 0)
            self.assertEqual(operations, ["disk"])
            disk = json.loads((Path(directory) / "disk-check.json").read_text())
            self.assertFalse(disk["development_build_allowed"])

    def test_log_read_error_does_not_skip_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            case, calls = self.case(directory, lambda _: None, broken_log=True)
            self.assertEqual(case.run()["cleanup"], {"ok": True})
            self.assertIn("cleanup", calls)

    def test_cleanup_failure_overrides_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            case, _ = self.case(directory, lambda _: None, cleanup_ok=False)
            self.assertEqual(case.run()["status"], "FAIL")

    def test_blocked_prerequisite_is_not_pass(self):
        def block(_):
            raise Blocked("app absent")

        with tempfile.TemporaryDirectory() as directory:
            case, calls = self.case(directory, block)
            self.assertEqual(case.run()["status"], "BLOCKED")
            self.assertEqual(calls[-1], "cleanup")


class GuestSafetyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = Path(__file__).resolve().parents[1] / "guest/guest.py"
        spec = importlib.util.spec_from_file_location("mac_guest", path)
        cls.guest = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.guest)

    def test_paths_cannot_escape_session(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "session"
            root.mkdir()
            for path in ("../other", "/etc/passwd"):
                with self.subTest(path=path), self.assertRaises(ValueError):
                    self.guest.inside(root, path)

    def test_symlink_cannot_escape_session(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "session"
            root.mkdir()
            (root / "outside").symlink_to(Path(directory), target_is_directory=True)
            with self.assertRaises(ValueError):
                self.guest.inside(root, "outside/other")

    def test_child_env_isolated_and_null_unsets_override(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.guest.save(root, {"env": {"SUPERBA_DATA_DIR": None}})
            with patch.dict(os.environ, {"ANTHROPIC_API_KEY": "real-user-key"}):
                env = self.guest.environment(root)
            self.assertNotIn("ANTHROPIC_API_KEY", env)
            self.assertNotIn("SUPERBA_DATA_DIR", env)
            for key in ("HOME", "CFFIXED_USER_HOME", "CODEX_HOME", "CLAUDE_CONFIG_DIR", "TMPDIR"):
                self.assertTrue(env[key].startswith(str(root) + "/"))

    def test_login_fixture_forwards_home_through_launchservices(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "session"
            root.mkdir()
            real_home = Path(directory) / "login-home"
            source = root / "autostart.plist"
            source.write_bytes(plistlib.dumps({"Label": "com.krill-code.superba.autostart",
                "ProgramArguments": ["/usr/bin/open", "-b", "com.krill-code.superba", "--args", "--autostart"]}))
            self.guest.save(root, {"env": {}, "login_plists": []})
            with patch.object(Path, "home", return_value=real_home):
                result = self.guest.login_item(root, "autostart.plist")
            plist = plistlib.loads(Path(result["path"]).read_bytes())
            argv = plist["ProgramArguments"]
            home_index = argv.index("HOME=" + str(root / "home"))
            self.assertEqual(argv[home_index - 1], "--env")
            self.assertLess(home_index, argv.index("--args"))
            self.assertEqual(argv[-1], "--autostart")


if __name__ == "__main__":
    unittest.main()
