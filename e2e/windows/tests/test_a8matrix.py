"""Real modal scopes, operation completion, WSL files and release fixtures; no VM."""
import hashlib
import json
import re
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch
from urllib.error import HTTPError
from urllib.request import urlopen

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import flows
from scenarios.alpha7 import (confirm_or_observe_install, confirm_removal, tool_data_hashes,
                             verify_tool_version, wait_install, wait_tool_idle)
from scenarios.more import _Release
from strings import Strings
from ui import Ui, UiError


def checking_step():
    step = Mock()

    def check(ok, message, **kw):
        if not ok and not kw.get("soft"):
            raise AssertionError(message)
    step.check.side_effect = check
    return step


class HarnessTests(unittest.TestCase):
    def setUp(self):
        self.now = 0
        for name, fn in (("time.time", lambda: self.now), ("time.monotonic", lambda: self.now),
                         ("time.sleep", self.sleep)):
            p = patch(name, fn)
            p.start()
            self.addCleanup(p.stop)
        self.run = SimpleNamespace(agent=Mock(), vm=Mock(), guest={"DATA": r"C:\Fixture\data"},
                                   guest_read=Mock(), guest_ps=Mock(),
                                   strings={lang: Strings(lang) for lang in ("en", "zh-CN")},
                                   session_snapshot="wintest-wsl1")
        self.step = checking_step()

    def sleep(self, seconds):
        self.now += seconds

    def modal_ui(self, lang="en", removing=True, toggle="Off", job="Claude Code"):
        """A UIA tree with identical buttons behind and inside a structural modal."""
        u = Ui(self.run, lang)
        title = u.L("updates.list.confirm_remove" if removing else "updates.list.confirm")
        button = u.L("updates.list.remove" if removing else "updates.list.start")
        state = {"open": True, "clicks": [], "close_on_click": True, "inside": True}
        background = [dict(name=title, type="Text"), dict(name=button, type="Button"),
                      dict(name="Claude Code · 2.1.304 → —", type="Text")]
        children = [dict(name=job + " · 2.1.304 → —", type="Text")]
        if toggle is not None:
            children.append(dict(name=u.L("updates.list.remove_data"), type="CheckBox", toggle=toggle))

        def call(method, **sel):
            self.assertTrue(sel.get("raw"))
            within = sel.get("within")
            if within:
                self.assertEqual(within, dict(name=title, type="Window", visible=False))
                pool = (children + ([dict(name=button, type="Button")] if state["inside"] else [])
                        if state["open"] else [])
            else:
                pool = background[:]
                # The modal itself has no rectangle, unlike its child controls.
                if state["open"] and sel.get("visible") is False:
                    pool.append(dict(name=title, type="Window", rect=None))
            rows = [r for r in pool if (not sel.get("type") or r["type"] == sel["type"])
                    and ("name" not in sel or r["name"] == sel["name"])
                    and ("name_re" not in sel or re.search(sel["name_re"], r["name"]))]
            if method == "uia.act":
                if not rows:
                    raise UiError("button absent in modal")
                state["clicks"].append("modal" if within else "background")
                if within and state["close_on_click"]:
                    state["open"] = False
                return {}
            self.assertEqual(method, "uia.find")
            return rows

        self.run.agent.call.side_effect = call
        return u, state

    def test_removal_uses_only_the_real_modal_and_observes_its_close(self):
        for lang in ("en", "zh-CN"):
            with self.subTest(lang=lang):
                u, state = self.modal_ui(lang)
                confirm_removal(u, self.step, "Claude Code", offers_data_removal=True)
                self.assertEqual(state["clicks"], ["modal"])
                self.assertFalse(state["open"])

    def test_background_button_cannot_replace_missing_modal_button(self):
        u, state = self.modal_ui()
        state["inside"] = False
        with self.assertRaisesRegex(UiError, "absent in modal"):
            confirm_removal(u, self.step, "Claude Code")
        self.assertEqual(state["clicks"], [])

    def test_removal_rejects_a_dialog_that_stays_open(self):
        u, state = self.modal_ui()
        state["close_on_click"] = False
        with self.assertRaisesRegex(UiError, "still shown"):
            confirm_removal(u, self.step, "Claude Code", timeout=2)
        self.assertEqual(state["clicks"], ["modal"])

    def test_removal_must_name_the_requested_tool_inside_the_modal(self):
        u, state = self.modal_ui(job="opencode")
        with self.assertRaises(UiError):
            confirm_removal(u, self.step, "Claude Code", timeout=0)
        self.assertEqual(state["clicks"], [])

    def test_removal_never_accepts_enabled_or_unknown_data_removal(self):
        for toggle in ("On", "Indeterminate", 1, None):
            with self.subTest(toggle=toggle):
                u, state = self.modal_ui(toggle=toggle)
                with self.assertRaises(AssertionError):
                    confirm_removal(u, self.step, "Claude Code", offers_data_removal=True)
                self.assertEqual(state["clicks"], [])

    def test_tools_without_opt_in_data_removal_still_have_a_valid_review(self):
        u, state = self.modal_ui(toggle=None)
        confirm_removal(u, self.step, "Claude Code")
        self.assertEqual(state["clicks"], ["modal"])

    def test_install_review_also_scopes_its_job_and_button_to_the_modal(self):
        for job in ("Claude Code", "opencode"):
            with self.subTest(job=job):
                u, state = self.modal_ui(removing=False, job=job)
                if job == "Claude Code":
                    confirm_or_observe_install(u, self.step, job)
                    self.assertEqual(state["clicks"], ["modal"])
                else:
                    with self.assertRaises(UiError):
                        confirm_or_observe_install(u, self.step, "Claude Code", timeout=0)
                    self.assertEqual(state["clicks"], [])

    @staticmethod
    def operation(action="install", result="", stamp="new"):
        return f"{stamp} {action} pi (channel latest) on Windows x64\n" + result

    def poll(self, logs, inventories, installed=True, before_log="", timeout=90):
        self.run.guest_read.side_effect = logs
        with patch("scenarios.alpha7.tool_row", side_effect=inventories):
            return wait_install(self.run, self.step, "pi", installed,
                                before_log=before_log, timeout=timeout)

    def test_discovery_and_done_do_not_replace_the_async_version_probe(self):
        pending = {"installs": [{"version": None}]}
        ready = {"installs": [{"version": "1.3.0"}]}
        result = self.poll([self.operation(), self.operation(result="new done: 1.3.0\n"),
                            self.operation(result="new done: 1.3.0\n")], [ready, pending, ready])
        self.assertEqual(result, ready)
        self.assertEqual(self.now, 10)

    def test_empty_inventory_version_never_passes_even_when_operation_is_done(self):
        self.run.guest_read.return_value = self.operation(result="new done: 1.3.0\n")
        with patch("scenarios.alpha7.tool_row", return_value={"installs": [{"version": ""}]}):
            with self.assertRaisesRegex(AssertionError, "complete version probes"):
                wait_install(self.run, self.step, "pi", True, timeout=10)

    def test_failed_probe_stopped_and_refused_operations_cannot_pass(self):
        for terminal in ("failed: process error", "stopped: cancelled", "refused: blocked"):
            with self.subTest(terminal=terminal):
                with self.assertRaisesRegex(AssertionError, "no terminal failure"):
                    self.poll([self.operation(result="new --version: no answer\nnew " + terminal)],
                              [{"installs": [{"version": "1.3.0"}]}])

    def test_uninstall_requires_new_uninstall_log_not_an_old_install(self):
        before = self.operation(result="old done: 1.3.0", stamp="old")
        result = self.poll([before, self.operation("uninstall"),
                           self.operation("uninstall", "new done: removed")],
                          [{"installs": []}] * 3, installed=False, before_log=before)
        self.assertEqual(result, {"installs": []})
        self.assertEqual(self.now, 10)

    def test_appending_to_a_previous_operation_is_not_a_new_operation(self):
        before = self.operation("uninstall", stamp="old")
        self.run.guest_read.return_value = before + "old done: removed"
        with patch("scenarios.alpha7.tool_row", return_value={"installs": []}):
            with self.assertRaisesRegex(UiError, "no new uninstall operation"):
                wait_install(self.run, self.step, "pi", False, before_log=before, timeout=70)
        self.assertEqual(self.now, 60)

    def test_installed_files_without_completed_installer_never_pass(self):
        self.run.guest_read.return_value = self.operation(result="new start: installer /S")
        with patch("scenarios.alpha7.tool_row", return_value={"installs": [{"version": "5.8.3"}]}):
            with self.assertRaisesRegex(AssertionError, "successful install"):
                wait_install(self.run, self.step, "workbuddy", True, timeout=15)

    def test_gui_still_installing_waits_before_exposing_uninstall_controls(self):
        for lang in ("en", "zh-CN"):
            with self.subTest(lang=lang):
                u = Ui(self.run, lang)
                u.reveal = Mock()
                u.find = Mock(side_effect=[
                    [{"name": "WorkBuddy, " + u.L("updates.list.installing") + ", — → 5.8.3"}],
                    [{"name": "WorkBuddy AI, " + u.L("updates.list.current") + ", 5.8.4"}],
                    [{"name": "WorkBuddy, " + u.L("updates.list.current") + ", 5.8.3"}]])
                start = self.now
                wait_tool_idle(u, self.step, "WorkBuddy", True)
                self.assertEqual(self.now - start, 1)

    def test_gui_failure_and_unfinished_gui_are_not_ready(self):
        for state in ("failed", "installing", "missing"):
            with self.subTest(state=state):
                u = Ui(self.run)
                u.reveal = Mock()
                u.find = Mock(return_value=[{"name": "WorkBuddy, " + u.L("updates.list." + state) + ", —"}])
                with self.assertRaises(UiError):
                    wait_tool_idle(u, self.step, "WorkBuddy", True, timeout=1)

    def test_actual_cli_version_must_match_a_nonempty_inventory_version(self):
        row = {"installs": [{"path": r"C:\Fixture\pi.cmd", "version": "1.3.0"}]}
        self.run.agent.exec.return_value = dict(code=0, timed_out=False, stdout="pi 1.3.0", stderr="")
        verify_tool_version(self.run, self.step, "pi", row)
        for result in (dict(code=0, timed_out=False, stdout="pi 1.3.01", stderr=""),
                       dict(code=0, timed_out=False, stdout="pi 1.3.0-beta", stderr=""),
                       dict(code=1, timed_out=False, stdout="pi 1.3.0", stderr=""),
                       dict(code=0, timed_out=True, stdout="pi 1.3.0", stderr="")):
            self.run.agent.exec.return_value = result
            with self.assertRaises(AssertionError):
                verify_tool_version(self.run, self.step, "pi", row)
        row["installs"][0]["version"] = ""
        with self.assertRaisesRegex(AssertionError, "inventory contains"):
            verify_tool_version(self.run, self.step, "pi", row)

    def test_actual_pe_metadata_must_agree_with_inventory(self):
        row = {"installs": [{"path": r"C:\Fixture\WorkBuddy.exe", "version": "5.8.3"}]}
        self.run.guest_ps.return_value = json.dumps(dict(file_version="5.8.3.41334564", product_version=""))
        verify_tool_version(self.run, self.step, "workbuddy", row)
        self.run.guest_ps.return_value = json.dumps(dict(file_version="5.8.4.1", product_version=""))
        with self.assertRaisesRegex(AssertionError, "actual PE"):
            verify_tool_version(self.run, self.step, "workbuddy", row)

    def test_data_probe_keeps_missing_directory_states_and_named_file_hashes(self):
        data = {r"C:\Fixture\user's data": True, r"C:\Fixture\user's data\config": "abc",
                r"C:\Fixture\missing": False}
        self.run.guest_ps.return_value = json.dumps(data)
        self.assertEqual(tool_data_hashes(self.run, [r"C:\Fixture\user's data", r"C:\Fixture\missing"]), data)
        script = self.run.guest_ps.call_args.args[0]
        self.assertIn("user''s data", script)
        self.assertIn("Get-FileHash -LiteralPath", script)
        self.assertNotIn("Remove-Item", script)

    def test_late_wsl_router_confirmation_keeps_original_deadline_and_scope(self):
        for lang in ("en", "zh-CN"):
            with self.subTest(lang=lang):
                start = self.now
                u = Ui(self.run, lang)
                u.texts = Mock(side_effect=[
                    [u.L("banner.planning")]] * 20 +
                    [[u.L("confirm.title"), u.L("restart.reason.certificate_trust_changed"),
                      "WSL: Ubuntu-WSL1"], [u.L("banner.running.title", dest="router")], [], []])
                u.exists = Mock(return_value=True)
                u.click = Mock()
                phases, _ = flows.wait_switch_done(self.run, self.step, u, timeout=240, router_restart=True)
                self.assertIn("[full restart: router certificate]", phases)
                self.assertGreaterEqual(self.now - start, 40)
                self.assertLess(self.now - start, 240)
                u.click.assert_called_once_with(key="confirm.full_restart", type="Button",
                                                within=u.modal("confirm.title"))

    def test_router_confirmation_reason_outside_modal_is_rejected(self):
        u = Ui(self.run)
        u.texts = Mock(return_value=[u.L("confirm.title"), u.L("restart.reason.certificate_trust_changed")])
        u.exists = Mock(return_value=False)
        u.click = Mock()
        with self.assertRaisesRegex(AssertionError, "actual restart dialog"):
            flows.wait_switch_done(self.run, self.step, u, router_restart=True)
        u.click.assert_not_called()

    def test_slow_wsl_switch_does_not_gain_a_new_240_second_budget(self):
        u = Ui(self.run)
        u.texts = Mock(return_value=[u.L("banner.planning")])
        with self.assertRaisesRegex(UiError, "did not finish"):
            flows.wait_switch_done(self.run, self.step, u, timeout=240, router_restart=True)
        self.assertEqual(self.now, 240)

    def test_wsl_hashes_executes_the_script_directly_and_rejects_failure(self):
        output = "\n".join("missing /fixture" + p[1:] for p in flows.WSL_FILES.split())
        self.run.agent.exec.return_value = dict(code=0, timed_out=False, stdout=output)
        self.assertEqual(flows.wsl_hashes(self.run), output)
        self.run.agent.exec.assert_called_once_with("wsl.exe", ["-d", flows.WSL_DISTRO, "--exec", "sh",
                                                    "/mnt/c/Users/Public/wintest/wsl-hashes.sh"],
                                                    env={"WSL_UTF8": "1"}, timeout=120)
        for code, timed_out in ((1, False), (0, True)):
            self.run.agent.exec.return_value = dict(code=code, timed_out=timed_out, stdout=output)
            with self.assertRaisesRegex(UiError, "failed or timed out"):
                flows.wsl_hashes(self.run)


class LocalFixtureTests(unittest.TestCase):
    def test_wsl_script_hashes_actual_named_files_including_a_home_with_spaces(self):
        script = Path(__file__).resolve().parents[1] / "guest" / "wsl-hashes.sh"
        with tempfile.TemporaryDirectory(prefix="wsl fixture ") as directory:
            root = Path(directory)
            (root / ".codex").mkdir()
            files = {".codex/config.toml": b"configuration", ".codex/auth.json": b"{}", ".bashrc": b""}
            for name, data in files.items():
                (root / name).write_bytes(data)
            result = subprocess.run(["sh", str(script), directory], capture_output=True, text=True, check=True)
            expected = []
            for path in flows.WSL_FILES.split():
                name = path[2:]
                prefix = hashlib.sha256(files[name]).hexdigest() + "  " if name in files else "missing "
                expected.append(prefix + str(root / name))
            self.assertEqual(flows.checked_wsl_hashes(result.stdout), "\n".join(expected))
            (root / ".profile").mkdir()
            bad = subprocess.run(["sh", str(script), directory], capture_output=True, text=True)
            self.assertNotEqual(bad.returncode, 0)

    def test_wsl_stdin_empty_partial_duplicate_and_wrong_home_records_fail(self):
        good = ["missing /fixture" + p[1:] for p in flows.WSL_FILES.split()]
        invalid = ["", "\n".join([hashlib.sha256(b"").hexdigest() + "  -"] * 6),
                   "\n".join(good[:-1]), "\n".join([good[0]] * 6),
                   "\n".join(good[:-1] + [good[-1].replace("/fixture", "/other")])]
        for output in invalid:
            with self.subTest(output=output):
                with self.assertRaisesRegex(UiError, "six named files"):
                    flows.checked_wsl_hashes(output)

    def test_w53_release_by_tag_download_and_unknown_tags(self):
        with tempfile.TemporaryDirectory() as directory:
            msi = Path(directory) / "fixture.msi"
            msi.write_bytes(b"deliberately invalid MSI")
            server = ThreadingHTTPServer(("127.0.0.1", 0), _Release)
            server.tag, server.files, server.hits = "v9.9.9", [msi], []
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            base = f"http://127.0.0.1:{server.server_port}"
            try:
                for route in ("/repos/example/test/releases/tags/v9.9.9", "/releases/latest", "/releases"):
                    with urlopen(base + route, timeout=3) as response:
                        data = json.load(response)
                    if isinstance(data, list):
                        data = data[0]
                    self.assertEqual(data["tag_name"], server.tag)
                    self.assertEqual(data["assets"][0]["name"], msi.name)
                with urlopen(base + "/dl/v9.9.9/fixture.msi", timeout=3) as response:
                    self.assertEqual(response.read(), msi.read_bytes())
                for route in ("/releases/tags/v8.8.8", "/dl/v9.9.9/missing.msi"):
                    with self.assertRaises(HTTPError) as error:
                        urlopen(base + route, timeout=3)
                    self.assertEqual(error.exception.code, 404)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=3)


if __name__ == "__main__":
    unittest.main()
