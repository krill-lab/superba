"""Startup waits retry transient failures without turning a failed probe into a pass."""

import subprocess
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
from agent import AgentError
from app import App
from vm import Vm, VmError


class Clock:
    def __init__(self):
        self.now = 0

    def time(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class ReadinessTests(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        for name, replacement in (("time.time", self.clock.time), ("time.sleep", self.clock.sleep)):
            patcher = patch(name, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)
        self.messages = []

    def app(self):
        return App(SimpleNamespace(log=self.messages.append, agent=SimpleNamespace(procs=Mock(return_value=[]))))

    def vm(self):
        vm = object.__new__(Vm)
        vm.user = "test-guest"
        vm.ssh_opts = Mock(return_value=[])
        vm.log = self.messages.append
        return vm

    def test_composer_retries_renderer_timeout_and_unfocusable_composer(self):
        app = self.app()
        app.eval = Mock(side_effect=[AgentError("renderer timed out"), False, False, True])
        app.main = Mock(return_value={"pid": 1})
        self.assertTrue(app.composer_ready(timeout=20))
        self.assertEqual(app.eval.call_count, 4)
        self.assertEqual(self.clock.now, 6)

    def test_composer_waits_for_restarted_main(self):
        app = self.app()
        app.eval = Mock(side_effect=[AgentError("renderer gone"), True])
        app.main = Mock(return_value=None)
        app.wait_main = Mock(return_value={"pid": 2})
        self.assertTrue(app.composer_ready(timeout=20))
        app.wait_main.assert_called_once()

    def test_composer_errors_expire_with_a_reason(self):
        app = self.app()
        app.eval = Mock(side_effect=AgentError("renderer timed out"))
        app.main = Mock(return_value={"pid": 1})
        self.assertFalse(app.composer_ready(timeout=6))
        self.assertIn("renderer timed out", app.composer_error)
        self.assertLessEqual(self.clock.now, 9)

    def test_model_introduction_can_appear_after_loading_then_focus_is_rechecked(self):
        app = self.app()
        actions = []

        def evaluate(expr, timeout):
            actions.append(expr)
            if expr == App.FOCUS_JS:
                return self.clock.now >= 6
            self.assertEqual(expr, App.KEEP_MODEL_JS)
            return self.clock.now >= 3

        app.eval = Mock(side_effect=evaluate)
        self.assertTrue(app.composer_ready(timeout=10))
        self.assertEqual(actions, [App.FOCUS_JS, App.KEEP_MODEL_JS, App.FOCUS_JS,
                                  App.KEEP_MODEL_JS, App.FOCUS_JS])
        self.assertEqual(self.clock.now, 6)
        self.assertTrue(any("Continue with current model" in m for m in self.messages))

    def test_answering_model_introduction_does_not_extend_deadline_or_prove_readiness(self):
        app = self.app()
        app.eval = Mock(side_effect=lambda expr, timeout: expr == App.KEEP_MODEL_JS)
        self.assertFalse(app.composer_ready(timeout=7))
        self.assertEqual(self.clock.now, 7)
        self.assertIn("not focusable", app.composer_error)
        self.assertLessEqual(app.eval.call_args.kwargs["timeout"], 1)

    def test_finished_sandbox_does_not_require_composer_before_onboarding(self):
        app = self.app()
        app.setup_procs = Mock(side_effect=lambda: [{"pid": 10}] if self.clock.now < 3 else [])
        app.app_servers = Mock(side_effect=lambda: [{"pid": 1 if self.clock.now < 3 else 2}])
        app.composer_ready = Mock(side_effect=AssertionError("onboarding must run first"))
        result = app.wait_first_start_setup(timeout=30, settle=6)
        self.assertTrue(result["ok"])
        self.assertTrue(result["restarted"])
        self.assertIn(10, result["setup"])
        app.composer_ready.assert_not_called()

    def test_absent_core_is_not_mistaken_for_finished_setup(self):
        app = self.app()
        app.setup_procs = Mock(return_value=[])
        app.app_servers = Mock(return_value=[])
        result = app.wait_first_start_setup(timeout=9, settle=3)
        self.assertFalse(result["ok"])

    def test_wait_ssh_retries_timeout(self):
        vm = self.vm()
        with patch("vm.subprocess.run", side_effect=[subprocess.TimeoutExpired("ssh", 30),
                   subprocess.CompletedProcess([], 0, "", "")]) as probe:
            vm.wait_ssh(timeout=60)
        self.assertEqual(probe.call_count, 2)
        self.assertTrue(any("no answer" in message for message in self.messages))

    def test_wait_ssh_stops_at_deadline(self):
        vm = self.vm()

        def timeout(*args, **kwargs):
            self.clock.sleep(kwargs["timeout"])
            raise subprocess.TimeoutExpired("ssh", kwargs["timeout"])

        with patch("vm.subprocess.run", side_effect=timeout) as probe:
            with self.assertRaisesRegex(VmError, "not reachable"):
                vm.wait_ssh(timeout=40)
        self.assertEqual(probe.call_count, 2)
        self.assertEqual(self.clock.now, 40)

    def test_ssh_never_replays_a_command_after_session_disconnect(self):
        vm = self.vm()
        result = subprocess.CompletedProcess([], 255, "", "client_loop: Connection reset by 127.0.0.1 port 22")
        with patch("vm.subprocess.run", return_value=result) as probe:
            self.assertIs(vm._ssh_run(["ssh"], 30, "probe", text=True), result)
        probe.assert_called_once()


if __name__ == "__main__":
    unittest.main()
