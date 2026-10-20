"""Regression checks for UIA timing and navigation; no VM or user configuration."""
import importlib.util
import re
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
from ui import Ui, UiError, SUPERBA_SECTIONS, SUPERBA_TABS, SETTINGS_ANCHORS
from runner import T, Scenario, SkipScenario, status_of, PASS, WARN

spec = importlib.util.spec_from_file_location("windows_run", HERE / "run.py")
windows_run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(windows_run)


class DriverTests(unittest.TestCase):
    def ui(self, lang="en"):
        run = SimpleNamespace(strings={lang: Strings(lang)}, agent=Mock(), session_snapshot="wintest")
        return Ui(run, lang)

    def test_superba_scopes_tabs_away_from_same_named_sidebar_items(self):
        for lang in ("en", "zh-CN"):
            for section in (*SUPERBA_SECTIONS, "superba"):
                with self.subTest(lang=lang, section=section):
                    u = self.ui(lang)
                    u.act = Mock()
                    u.wait_selected = Mock()
                    u.wait_settings_anchor = Mock()
                    u.superba(section)
                    tab = "settings" if section in SETTINGS_ANCHORS else {"superba": "home"}.get(section, section)
                    calls = u.act.call_args_list
                    self.assertEqual(calls[0].kwargs, {"key": "superba.title", "type": "TabItem"})
                    self.assertEqual(calls[1].kwargs, {
                        "key": SUPERBA_TABS[tab], "type": "TabItem",
                        "within": {"name": "Superba", "type": "Tab", "visible": False}})
                    u.wait_selected.assert_called_once_with(**calls[1].kwargs)
                    if section in SETTINGS_ANCHORS:
                        self.assertEqual(calls[2].kwargs["type"], "Button")
                        self.assertEqual(calls[2].kwargs["key"], f"superba.section.{section}")
                        u.wait_settings_anchor.assert_called_once_with(section)
                    else:
                        self.assertEqual(len(calls), 2)
                        u.wait_settings_anchor.assert_not_called()

    def test_unknown_superba_section_does_not_navigate(self):
        u = self.ui()
        with self.assertRaises(ValueError):
            u.superba("missing")
        u.agent.call.assert_not_called()

    @patch("ui.time.sleep")
    def test_tab_waits_until_selected(self, sleep):
        u = self.ui()
        u.find = Mock(side_effect=[[], [{"selected": False}], [{"selected": True}]])
        self.assertTrue(u.wait_selected(key="superba.tab.home", type="TabItem")["selected"])
        self.assertEqual(u.find.call_count, 3)

    @patch("ui.time.sleep")
    @patch("ui.time.monotonic", side_effect=[0, 1, 2])
    def test_tab_that_never_selects_fails(self, clock, sleep):
        u = self.ui()
        u.find = Mock(return_value=[{"selected": False}])
        with self.assertRaisesRegex(UiError, "not selected"):
            u.wait_selected(timeout=2, key="superba.tab.home", type="TabItem")

    @patch("ui.time.sleep")
    @patch("ui.time.monotonic", side_effect=[0, 0.2, 0.4, 0.6, 0.8, 1.0, 1.2, 1.4, 1.6])
    def test_anchor_waits_for_selection_viewport_and_stable_heading(self, clock, sleep):
        u = self.ui()
        u.window = Mock(return_value={"rect": [0, 0, 960, 640]})
        button = {"selected": True, "rect": [600, 200, 80, 30]}
        def heading(y):
            return [{"rect": [220, y, 120, 28], "offscreen": False}]
        u.find = Mock(side_effect=[
            [{**button, "selected": False}], heading(240),
            [button], heading(150),  # above the content viewport
            [button], heading(700),  # below the window
            [button], heading(300),  # animation still moving
            [button], heading(240),
            [button], heading(240),
            [button], heading(240),
            [button], heading(240),
        ])
        self.assertEqual(u.wait_settings_anchor("network"), (220, 240, 120, 28))
        self.assertGreaterEqual(u.find.call_count, 14)
        self.assertEqual(u.find.call_args.kwargs["within"], {
            "name": u.L("superba.tab.settings"), "type": "Pane", "visible": False})

    @patch("ui.time.sleep")
    @patch("ui.time.monotonic", side_effect=[0, 0, 0.2, 0.5])
    def test_missing_button_selection_is_recorded_without_inventing_state(self, clock, sleep):
        u = self.ui()
        u.window = Mock(return_value={"rect": [0, 0, 960, 640]})
        button = {"rect": [600, 200, 80, 30]}
        u.find = Mock(side_effect=lambda **kw: [button] if kw["type"] == "TabItem"
                      else [{"rect": [220, 240, 120, 28]}])
        self.assertEqual(u.wait_settings_anchor("notify"), (220, 240, 120, 28))
        self.assertIn("no selected state", u.run.navigation_warnings[0])
        self.assertNotIn("selected", button)

    @patch("ui.time.sleep")
    @patch("ui.time.monotonic", side_effect=[0, 0, 1])
    def test_anchor_without_visible_heading_fails(self, clock, sleep):
        u = self.ui()
        u.window = Mock(return_value={"rect": [0, 0, 960, 640]})
        u.find = Mock(side_effect=lambda **kw: [{"selected": True, "rect": [600, 200, 80, 30]}]
                      if kw["type"] == "Button" else [])
        with self.assertRaisesRegex(UiError, "anchor did not settle: notify"):
            u.wait_settings_anchor("notify", timeout=1)

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
            for key in SUPERBA_TABS.values():
                self.assertTrue(u.L(key))
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

    def install_ui(self, frames, lang="en"):
        """Filter successive actual UIA frames, including unrelated controls."""
        u = self.ui(lang)
        frame = {"index": 0}

        def find(**selector):
            rows = frames[min(frame["index"], len(frames) - 1)]
            name = u.L(selector["key"]) if "key" in selector else None
            return [row for row in rows if row["type"] == selector["type"]
                    and (row["name"] == name if name else re.search(selector["name_re"], row["name"]))]

        u.find = Mock(side_effect=find)
        u.click = Mock()
        return u, frame

    def test_tool_install_direct_start_requires_its_localized_installing_row(self):
        from scenarios.alpha7 import confirm_or_observe_install
        for lang in ("en", "zh-CN"):
            with self.subTest(lang=lang):
                row = {"name": f"Claude Code, {Strings(lang).t('updates.list.installing')}, 2.1.304",
                       "type": "Button", "rect": [20, 30, 800, 50]}
                u, _ = self.install_ui([[row]], lang)
                s = Mock()
                confirm_or_observe_install(u, s, "Claude Code")
                u.click.assert_not_called()
                s.save.assert_called_once_with("install-started.json", [row])
                s.shot.assert_called_once_with("install-started")

    def test_tool_install_waits_for_actual_review_and_records_it_before_start(self):
        from scenarios.alpha7 import confirm_or_observe_install
        for lang in ("en", "zh-CN"):
            with self.subTest(lang=lang):
                strings = Strings(lang)
                u, frame = self.install_ui([[], [
                    {"name": strings.t("updates.list.confirm"), "type": "Text"},
                    {"name": "Claude Code · — → 2.1.304", "type": "Text"},
                ]], lang)
                events = []
                s = Mock()
                s.shot.side_effect = lambda name: events.append(name)
                u.click.side_effect = lambda **kw: events.append(kw["key"])
                with patch("scenarios.alpha7.time.sleep", side_effect=lambda _: frame.update(index=1)):
                    confirm_or_observe_install(u, s, "Claude Code")
                self.assertEqual(events, ["install-review", "updates.list.start"])
                u.click.assert_called_once_with(key="updates.list.start")
                self.assertEqual(s.save.call_args.args[1]["job"]["name"], "Claude Code · — → 2.1.304")

    def test_tool_install_missing_or_unrelated_state_never_skips_review(self):
        from scenarios.alpha7 import confirm_or_observe_install
        for rows in ([], [{"name": "opencode, Installing, 2.0.32", "type": "Button"}],
                     [{"name": "Claude Code, Not installed, Installing is only a description", "type": "Button"}],
                     [{"name": "Claude Code, Installing, 2.1.304", "type": "Text"}],
                     [{"name": "Claude Code, Update failed, 2.1.304", "type": "Button"}],
                     [{"name": "Full restart", "type": "Button"}]):
            with self.subTest(rows=rows):
                u, _ = self.install_ui([rows])
                s = Mock()
                with self.assertRaisesRegex(UiError, "neither its install review nor Installing row"):
                    confirm_or_observe_install(u, s, "Claude Code", timeout=0)
                u.click.assert_not_called()
                s.shot.assert_not_called()

    def test_tool_install_review_must_name_the_requested_tool(self):
        from scenarios.alpha7 import confirm_or_observe_install
        u, _ = self.install_ui([[
            {"name": "Review updates", "type": "Text"},
            {"name": "opencode · — → 2.0.32", "type": "Text"},
        ]])
        with self.assertRaises(UiError):
            confirm_or_observe_install(u, Mock(), "Claude Code", timeout=0)
        u.click.assert_not_called()

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

    @patch("flows.time.sleep")
    @patch("flows.time.time", side_effect=range(100))
    def test_toast_seen_while_overlay_closes_survives_next_tree_query(self, clock, sleep):
        u = self.ui()
        toast = u.L("shell.switched", line="Test")
        u.texts = Mock(side_effect=[[u.L("banner.planning"), toast], ["Overview"]])
        _, names = flows.wait_switch_done(u.run, Mock(), u)
        self.assertEqual(flows.switch_result(u, names), [toast])
        self.assertEqual(u.texts.call_count, 2)

    def test_rendered_switch_results_in_both_languages(self):
        cases = [("shell.switched", {"line": "Test"}),
                 ("shell.reapplied", {"dest": "Test"}),
                 ("shell.router_on", {}), ("shell.router_off", {}),
                 ("shell.host_synced", {"host": "Ubuntu-WSL1"}),
                 ("toast.written_only", {}), ("toast.written_not_running", {})]
        for lang in ("en", "zh-CN"):
            for key, args in cases:
                with self.subTest(lang=lang, key=key):
                    u = self.ui(lang)
                    title = u.L(key, **args)
                    u.texts = Mock(side_effect=[[], [title]])
                    with patch("ui.time.sleep"):
                        self.assertEqual(flows.switch_result(u), [title])

    @patch("scenarios.more.time.sleep")
    def test_msi_version_probe_waits_for_the_replacement_executable(self, sleep):
        from scenarios.more import wait_msi_version
        run = SimpleNamespace(spb_exe=r"C:\Program Files\Superba\spb.exe", agent=Mock())
        missing = {"code": 3, "stdout": "", "stderr": ""}
        installed = {"code": 0, "stdout": "spb 9.0.0\r\n", "stderr": ""}
        run.agent.exec.side_effect = [missing, installed]
        self.assertEqual(wait_msi_version(run, "9.0.0"), installed)
        self.assertEqual(run.agent.exec.call_count, 2)
        self.assertTrue(all(call.args[0] == "powershell.exe" for call in run.agent.exec.call_args_list))

    @patch("scenarios.more.time.sleep")
    def test_msi_version_probe_does_not_hide_a_disconnected_agent(self, sleep):
        from agent import AgentError
        from scenarios.more import wait_msi_version
        run = SimpleNamespace(spb_exe=r"C:\Superba\spb.exe", agent=Mock())
        run.agent.exec.side_effect = AgentError("agent gone")
        with self.assertRaisesRegex(AgentError, "agent gone"):
            wait_msi_version(run, "9.0.0")
        self.assertEqual(run.agent.exec.call_count, 1)

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

    def test_navigation_warning_reaches_report_without_leaking_to_next_scenario(self):
        with tempfile.TemporaryDirectory() as folder:
            run = SimpleNamespace(root=Path(folder), wsl_switch_timeouts=[], wsl_retry_warnings={},
                                  vm=SimpleNamespace(renew=lambda: None), log=lambda _: None,
                                  opts=SimpleNamespace(include_manual=False), net_mode="mock", state={},
                                  session_snapshot="wintest", dirty=False, ensure_session=lambda *a, **k: None)
            warning = "Superba network: UIA Button has no selected state"

            def navigate(t):
                t.run.navigation_warnings.append(warning)
                success(t)

            rec = windows_run.run_one(run, Scenario("W62", "navigation", navigate))
            self.assertEqual(rec["status"], WARN)
            self.assertIn(warning, str(rec["steps"]))
            rec = windows_run.run_one(run, Scenario("W63", "next scenario", success))
            self.assertEqual(rec["status"], PASS)
            self.assertNotIn(warning, str(rec["steps"]))

            def fails(t):
                t.run.navigation_warnings.append(warning)
                raise RuntimeError("navigation stopped")

            rec = windows_run.run_one(run, Scenario("W62", "failed navigation", fails))
            self.assertEqual(rec["status"], windows_run.FAIL)
            self.assertIn("navigation stopped", rec["reason"])


def success(t):
    with t.step("completed") as s:
        s.check(True, "checks passed")


if __name__ == "__main__":
    unittest.main()
