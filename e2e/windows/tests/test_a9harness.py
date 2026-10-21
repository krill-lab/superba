"""alpha.9 Windows acceptance harness fixes: unique output folders, --net real
preflight, router on/off waits, settings anchors as tabs, render waits."""
import argparse
import sys
import tempfile
import unittest
from datetime import datetime
from pathlib import Path

HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(HERE / 'lib'))
sys.path.insert(0, str(HERE))
import flows
import ui
from session import unique_out_dir
from net import check_real_env


class OutDirTests(unittest.TestCase):
    def test_same_second_runs_get_their_own_folder(self):
        with tempfile.TemporaryDirectory() as d:
            now = datetime(2026, 10, 21, 6, 51, 25)
            a, b, c = (unique_out_dir(d, now) for _ in range(3))
            self.assertEqual([a.name, b.name, c.name],
                             ["windows-20261021-065125", "windows-20261021-065125-2", "windows-20261021-065125-3"])


class RealNetPreflightTests(unittest.TestCase):
    def test_missing_env_fails_before_a_vm(self):
        import run
        with self.assertRaises(SystemExit) as e:
            run.preflight_net(argparse.Namespace(net="real"), environ={})
        self.assertIn("no VM was acquired", str(e.exception))
        run.preflight_net(argparse.Namespace(net="mock"), environ={})  # mock needs nothing

    def test_env_file_must_name_relay_and_key(self):
        with tempfile.TemporaryDirectory() as d:
            f = Path(d) / "e2e.env"
            f.write_text("SUPERBA_E2E_UPSTREAM=https://relay.invalid\n")
            with self.assertRaises(RuntimeError):
                check_real_env({"SUPERBA_E2E_ENV": str(f)})
            with self.assertRaises(RuntimeError):
                check_real_env({"SUPERBA_E2E_ENV": str(Path(d) / "missing.env")})
            f.write_text("SUPERBA_E2E_UPSTREAM=https://relay.invalid\nSUPERBA_E2E_KEY=k\n")
            self.assertEqual(check_real_env({"SUPERBA_E2E_ENV": str(f)})["SUPERBA_E2E_KEY"], "k")


class RouterWaitTests(unittest.TestCase):
    def test_turn_off_waits_for_the_full_restart_question(self):
        on = lambda k: k == "router.banner.turn_off"  # still on while planning
        self.assertTrue(flows.router_wait_more("off", False, [], on))
        # answered, but the banner has not flipped yet
        self.assertTrue(flows.router_wait_more("off", True, [], on))
        self.assertFalse(flows.router_wait_more("off", True, [], lambda k: k == "router.banner.turn_on"))

    def test_turn_on_keeps_its_meaning(self):
        self.assertTrue(flows.router_wait_more(True, False, [], lambda k: True))
        self.assertFalse(flows.router_wait_more(True, True, [], lambda k: k == "router.banner.turn_off"))
        self.assertFalse(flows.router_wait_more(True, False, ["Router on"], lambda k: k == "router.banner.turn_off"))


class AnchorAndRenderTests(unittest.TestCase):
    def ui_class(self):
        for v in vars(ui).values():
            if isinstance(v, type) and hasattr(v, "superba") and hasattr(v, "wait_rendered"):
                return v
        self.fail("no UI driver class with superba()/wait_rendered()")

    def test_settings_anchor_is_clicked_as_a_tab(self):
        cls = self.ui_class()
        calls = []
        u = cls.__new__(cls)
        u.act = lambda *a, **kw: calls.append(("act", a, kw))
        u.click = lambda **kw: calls.append(("click", kw))
        u.wait_selected = lambda **kw: None
        u.wait_settings_anchor = lambda section: None
        u.wait_rendered = lambda: calls.append(("rendered",))
        u.superba_tab_list = lambda: {}
        u.L = lambda k: k
        u.superba("general")
        clicks = [c for c in calls if c[0] == "click"]
        self.assertEqual(clicks, [("click", {"key": "superba.section.general", "type": "TabItem"})])
        self.assertEqual(calls[-1], ("rendered",))

    def test_wait_rendered_returns_once_names_settle(self):
        cls = self.ui_class()
        u = cls.__new__(cls)
        frames = iter([["a"], ["a", "b"], ["a", "b", "c"], ["a", "b", "c"], ["a", "b", "c"]])
        u.texts = lambda: next(frames)
        self.assertEqual(u.wait_rendered(timeout=5, poll=0), ["a", "b", "c"])


if __name__ == "__main__":
    unittest.main()


class ListAndW52Tests(unittest.TestCase):
    def test_list_sorts_ids_of_any_prefix(self):
        import run as runpy
        from types import SimpleNamespace as N
        ids = ["W52", "MX10", "W1", "SU9", "MX2", "W12"]
        got = [s.id for s in sorted((N(id=i) for i in ids), key=runpy.scenario_sort_key)]
        self.assertEqual(got, ["MX2", "MX10", "SU9", "W1", "W12", "W52"])

    def test_cmd_list_runs_over_the_registry(self):
        import contextlib, io
        import run as runpy
        from types import SimpleNamespace as N
        with contextlib.redirect_stdout(io.StringIO()):
            runpy.cmd_list(N(area=None))

    def test_w52_failure_notice_pattern(self):
        import re
        from lib.scenarios import more
        rx = re.compile(more.UPDATE_FAILED_RX)
        for t in ["The installer reported success, but this is still Superba 0.1.1-alpha.9.",
                  "安装程序报告成功，但当前仍是 Superba 0.1.1-alpha.9。", "更新到 Superba 9.0.0 失败"]:
            self.assertTrue(rx.search(t), t)
        self.assertIsNone(rx.search("已是最新"))
