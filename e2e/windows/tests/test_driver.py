"""Regression checks for UIA timing and navigation; no VM or user configuration."""
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE / "lib"))
import flows
from strings import Strings
from ui import Ui, UiError, SUPERBA_SECTIONS
from runner import T, Scenario, SkipScenario, status_of, PASS, WARN

spec = importlib.util.spec_from_file_location("windows_run", HERE / "run.py")
windows_run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(windows_run)


class DriverTests(unittest.TestCase):
    def ui(self, lang="en"):
        run = SimpleNamespace(strings={lang: Strings(lang)}, agent=Mock(), session_snapshot="wintest")
        return Ui(run, lang)

    @patch("ui.time.sleep")
    def test_home_selects_two_navigation_columns_even_if_tree_order_changes(self, _):
        u = self.ui()
        u.find = Mock(return_value=[{"rect": [240, 30, 164, 50]}, {"rect": [10, 600, 200, 32]}])
        u.act = Mock()
        u.superba("home")
        self.assertEqual([c.kwargs["index"] for c in u.act.call_args_list], [1, 0])

    def test_page_routes_maintenance_and_old_preferences(self):
        u = self.ui()
        u.superba = Mock()
        for name in ("updates", "diagnostics", "preferences", "superba:notify"):
            u.page(name)
        self.assertEqual([c.args[0] for c in u.superba.call_args_list],
                         ["updates", "diagnostics", "general", "notify"])

    def test_every_new_page_key_exists_in_both_languages(self):
        from scenarios.more import PAGES
        self.assertEqual(len(PAGES), 19)
        for lang in ("en", "zh-CN"):
            u = self.ui(lang)
            for section in SUPERBA_SECTIONS:
                self.assertTrue(u.L("superba.title" if section == "home" else f"superba.section.{section}"))
            self.assertTrue(u.L("superba.tools.title"))

    def test_new_scenarios_do_not_install_tools_before_integration(self):
        from scenarios.alpha7 import require_alpha7
        u = self.ui()
        u.s.table.pop("tools.apply", None)
        t = SimpleNamespace(run=u.run, skip=Mock(side_effect=SkipScenario("waiting for integration")))
        with patch("scenarios.alpha7.flows.ensure") as ensure:
            with self.assertRaises(SkipScenario):
                require_alpha7(t)
            ensure.assert_not_called()

    @patch("ui.time.sleep")
    def test_toast_after_empty_frames_is_polled(self, sleep):
        u = self.ui()
        u.texts = Mock(side_effect=[[], ["Overview"], ["Switch succeeded"]])
        self.assertEqual(u.poll_texts(["^Switch succeeded$"], timeout=1), ["Switch succeeded"])
        self.assertEqual(u.texts.call_count, 3)

    def test_toast_in_last_overlay_frame_is_preserved(self):
        u = self.ui()
        u.texts = Mock(return_value=[])
        self.assertEqual(u.poll_texts(["^done$"], initial=["done"]), ["done"])
        u.texts.assert_not_called()

    def test_still_running_is_named_immediately_when_force_quit_disallowed(self):
        for lang in ("en", "zh-CN"):
            u = self.ui(lang)
            title = u.L("decide.still_running.title")
            u.texts = Mock(return_value=[title])
            u.click = Mock()
            s = Mock()
            with self.assertRaisesRegex(UiError, title):
                flows.wait_switch_done(u.run, s, u, force_quit=False)
            u.click.assert_not_called()
            s.ui_dump.assert_called_once()

    def test_still_running_is_answered_once_and_repetition_fails_by_name(self):
        u = self.ui()
        title = u.L("decide.still_running.title")
        u.texts = Mock(return_value=[title])
        u.click = Mock()
        s = Mock()
        with self.assertRaisesRegex(UiError, "already attempted"):
            flows.wait_switch_done(u.run, s, u)
        u.click.assert_called_once_with(key="decide.still_running.end")
        self.assertTrue(s.check.call_args.kwargs["soft"])

    def test_wsl_timeout_survives_a_successful_scenario_retry(self):
        with tempfile.TemporaryDirectory() as folder:
            run = SimpleNamespace(root=Path(folder), wsl_switch_timeouts=[], wsl_retry_warnings={},
                                  vm=SimpleNamespace(renew=lambda: None), log=lambda _: None,
                                  opts=SimpleNamespace(include_manual=False), net_mode="mock", state={},
                                  session_snapshot="wintest-wsl1", dirty=False, ensure_session=lambda *a, **k: None)
            def first(t):
                t.run.wsl_switch_timeouts.append("transcripts/first-timeout.txt")
                with t.step("recovered switch") as s:
                    s.check(True, "second attempt completed")
            sc = Scenario("W85", "test", first)
            rec = windows_run.run_one(run, sc)
            self.assertEqual(rec["status"], WARN)
            sc.fn = lambda t: success(t)
            rec = windows_run.run_one(run, sc)
            self.assertEqual(rec["status"], WARN)
            self.assertIn("first-timeout.txt", str(rec["steps"]))

    def test_clean_run_is_not_warned(self):
        with tempfile.TemporaryDirectory() as folder:
            run = SimpleNamespace(root=Path(folder), log=lambda _: None)
            t = T(run, Scenario("W85", "test", None))
            success(t)
            self.assertEqual(status_of(t.steps), PASS)


def success(t):
    with t.step("completed") as s:
        s.check(True, "checks passed")


if __name__ == "__main__":
    unittest.main()
