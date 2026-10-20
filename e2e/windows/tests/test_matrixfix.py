"""Confirmed matrix regressions: exact UI semantics and independent evidence."""
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, call, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import flows
from app import App
from migration import (check_migrated, check_restored, migration_count,
                       wait_migration)
from scenarios.core import (routed_usage_turn, usage_open_detail, usage_provider_group,
                            usage_show_window)
from strings import Strings
from ui import Ui, UiError


class Clock:
    def __init__(self):
        self.now = 0

    def time(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def checking_step():
    step = Mock()

    def check(ok, message, **kwargs):
        if not ok:
            raise AssertionError(message)
    step.check.side_effect = check
    step.__enter__ = Mock(return_value=step)
    step.__exit__ = Mock(return_value=False)
    return step


class MatrixTests(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        for name, fn in [('time.time', self.clock.time), ('time.monotonic', self.clock.time),
                         ('time.sleep', self.clock.sleep)]:
            p = patch(name, fn)
            p.start()
            self.addCleanup(p.stop)
        self.run = SimpleNamespace(strings={lang: Strings(lang) for lang in ('en', 'zh-CN')},
                                   agent=Mock(), vm=Mock(), state={}, session_snapshot='wintest')
        self.run.agent.procs.return_value = []
        self.run.agent.call.return_value = []
        self.step = checking_step()
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.step.t.evdir = Path(directory.name)
        self.step.t.seq.side_effect = iter(range(1, 100))

    def ui(self, lang='en'):
        u = Ui(self.run, lang)
        u.click = Mock()
        u.click_at = Mock()
        u.exists = Mock(return_value=True)

        def snapshot():
            names = u.texts()
            rows = [dict(name=name, type='Text', enabled=True, rect=None) for name in names]
            if u.L('movers.title.migrate') in names:
                rows.append(dict(name=u.L('movers.stop'), type='Button', enabled=True,
                                 rect=[10, 20, 100, 30]))
            return rows

        u.snapshot = Mock(side_effect=snapshot)
        return u

    def test_migration_count_current_locales_and_former_slash_copy(self):
        for lang in ('en', 'zh-CN'):
            strings = Strings(lang)
            for restore in (False, True):
                key = 'switch.sessions.restored' if restore else 'switch.sessions.migrated'
                for count in (0, 299, 600):
                    with self.subTest(lang=lang, restore=restore, count=count):
                        self.assertEqual(migration_count(strings, strings.t(key, done=count, total=600),
                                                         restore=restore), (count, 600))
        self.assertEqual(migration_count(Strings('en'), 'Migrated 299 / 600'), (299, 600))
        self.assertEqual(migration_count(Strings('en'), 'Migrated 299 / 600 sessions'), (299, 600))

    def test_scan_history_pager_invalid_and_restore_counts_are_not_migration_progress(self):
        for text in ['600', '1 / 600', 'Scanned 600 sessions', 'Migration history', 'No migrations yet',
                     'Migrated 601 of 600 sessions', 'Migrated 299 of 601 sessions',
                     'prefix Migrated 299 of 600 sessions', 'Restored 299 of 600 sessions', 'Done']:
            with self.subTest(text=text):
                self.assertIsNone(migration_count(Strings('en'), text))

    def test_migration_ends_without_waiting_for_history_to_disappear(self):
        u = self.ui()
        u.texts = Mock(side_effect=[
            ['Migrating sessions', 'Migrated 299 of 600 sessions', 'Now: file.jsonl'],
            ['Migration history', 'Done', '600', 'Migrate to openai…']])
        wait_migration(self.run, self.step, u)
        self.assertEqual(self.clock.now, 1)
        u.click.assert_not_called()

    def test_completion_and_scanned_total_do_not_satisfy_progress(self):
        u = self.ui()
        u.texts = Mock(side_effect=[['Migrating sessions', '600', 'Migrated 600 of 600 sessions'],
                                   ['Migration history', 'Done']])
        with self.assertRaisesRegex(AssertionError, 'intermediate migrated progress'):
            wait_migration(self.run, self.step, u)

    def test_progress_does_not_satisfy_completion_while_dialog_remains(self):
        u = self.ui()
        u.texts = Mock(return_value=['Migrating sessions', 'Migrated 299 of 600 sessions', 'Now: file'])
        with self.assertRaisesRegex(UiError, 'did not finish'):
            wait_migration(self.run, self.step, u, timeout=3)
        self.assertTrue(self.step.save.called)

    def test_lock_error_seen_during_migration_is_not_lost_after_completion(self):
        u = self.ui()
        u.texts = Mock(side_effect=[['Migrating sessions', 'Migrated 299 of 600 sessions',
                                    'Now: file', 'too many open files'], ['Done']])
        with self.assertRaisesRegex(AssertionError, 'no lock / EMFILE'):
            wait_migration(self.run, self.step, u)

    def test_stop_and_undo_is_clicked_while_intermediate_progress_is_active(self):
        u = self.ui()
        u.texts = Mock(side_effect=[['Migrating sessions', 'Migrated 299 of 600 sessions', 'Now: file'],
                                   ['Stopping the migration'], ['Migration history', 'Rolled back']])
        wait_migration(self.run, self.step, u, stop=True)
        u.click_at.assert_called_once_with(60, 35, relative=False)
        u.click.assert_not_called()
        self.assertEqual(self.clock.now, 2)

    def test_slow_ssh_diagnostics_and_transfers_cannot_hide_moving_progress(self):
        u = self.ui()
        observed = []

        def texts():
            count = int(self.clock.now) * 200
            names = (['Migrating sessions', f'Migrated {count} of 600 sessions', 'Now: file']
                     if count < 600 else ['Migration history', 'Done'])
            observed.append(names)
            return names

        u.texts = Mock(side_effect=texts)
        self.run.vm = Mock()
        self.run.vm.ps.side_effect = lambda *a, **kw: self.clock.sleep(9)
        self.run.agent.procs.side_effect = lambda *a, **kw: self.clock.sleep(9)
        self.run.agent.call.return_value = [dict(pid=42, handles=575)]
        u.tree = Mock(side_effect=lambda **kw: self.clock.sleep(24))

        def transfer(*args):
            self.assertEqual(observed[-1], ['Migration history', 'Done'])
            self.clock.sleep(9)

        self.run.vm.scp_from.side_effect = transfer
        wait_migration(self.run, self.step, u)
        self.run.vm.ps.assert_not_called()
        self.run.agent.procs.assert_not_called()
        self.assertTrue(all(c.args == ('proc.handles',) and c.kwargs == {'name': 'superba'}
                            for c in self.run.agent.call.call_args_list))
        u.tree.assert_not_called()
        samples = self.step.save.call_args_list[0].args[1]
        self.assertEqual([x['count'] for x in samples], [(0, 600), (200, 600), (400, 600), None])
        self.assertTrue(all(x['handles'] == [dict(pid=42, handles=575)] for x in samples))
        self.assertEqual(self.run.agent.shot.call_count, 3)
        self.assertEqual(self.run.vm.scp_from.call_count, 3)

    def test_stop_is_clicked_before_any_screenshot_file_transfer(self):
        u = self.ui()
        u.texts = Mock(side_effect=[['Migrating sessions', 'Migrated 299 of 600 sessions', 'Now: file'],
                                   ['Stopping the migration'], ['Migration history', 'Rolled back']])
        self.run.vm = Mock()

        def transfer(*args):
            u.click_at.assert_called_once_with(60, 35, relative=False)
            self.assertEqual(u.texts.call_count, 3)

        self.run.vm.scp_from.side_effect = transfer
        wait_migration(self.run, self.step, u, stop=True)
        self.run.vm.scp_from.assert_called_once()

    def test_slow_screenshot_cannot_delay_stop_past_completion(self):
        u = self.ui()
        u.texts = Mock(side_effect=[['Migrating sessions', 'Migrated 289 of 600 sessions', 'Now: file'],
                                   ['Stopping the migration'], ['Migration history', 'Rolled back']])

        def click(*args, **kwargs):
            self.assertLess(self.clock.now, 3, 'the migration would finish before a delayed click')

        u.click_at.side_effect = click
        self.run.agent.shot.side_effect = lambda *a, **kw: self.clock.sleep(4)
        wait_migration(self.run, self.step, u, stop=True)
        u.click_at.assert_called_once_with(60, 35, relative=False)
        self.assertEqual(self.run.agent.shot.call_count, 1)

    def test_stop_needs_one_visible_enabled_button_in_the_same_progress_snapshot(self):
        for buttons in ([], [dict(name='Stop and undo', type='Button', enabled=False,
                                 rect=[10, 20, 100, 30])]):
            with self.subTest(buttons=buttons):
                u = self.ui()
                u.snapshot = Mock(return_value=[
                    dict(name=n, type='Text', enabled=True, rect=None)
                    for n in ['Migrating sessions', 'Migrated 299 of 600 sessions', 'Now: file']
                ] + buttons)
                with self.assertRaisesRegex(AssertionError, 'Stop and undo is enabled'):
                    wait_migration(self.run, self.step, u, stop=True)
                u.click_at.assert_not_called()

    def test_data_proof_rejects_unmigrated_files_and_incomplete_undo(self):
        count = dict(threads=600, rollouts=600, distinct=600)
        before = {'report': {'by_provider': {'krill': count}}, 'history': []}
        after = {'report': {'by_provider': {'openai': count}}, 'history': [
            dict(id='new', rolled_back=False, by_source_provider={'krill': 600}, threads=600, rollouts=600)]}
        hashes = {f'file{i}': 'before' for i in range(600)}
        changed = {p: 'after' for p in hashes}
        self.assertEqual(check_migrated(self.step, before, after, hashes, changed, hashes), 'new')
        with self.assertRaisesRegex(AssertionError, 'exactly the 600'):
            check_migrated(self.step, before, after, hashes, hashes, hashes)
        with self.assertRaisesRegex(AssertionError, 'byte-for-byte'):
            check_restored(self.step, before, before, hashes, {**hashes, 'file1': 'after'})
        with self.assertRaisesRegex(AssertionError, 'row/provider'):
            check_restored(self.step, before, after, hashes, hashes)
        check_restored(self.step, before, before, hashes, hashes)

    def test_router_confirmation_localized_delayed_and_actual_on_state(self):
        for lang in ('en', 'zh-CN'):
            with self.subTest(lang=lang):
                u = self.ui(lang)
                u.texts = Mock(side_effect=[[], [], [],
                    [u.L('confirm.title'), u.L('restart.reason.certificate_trust_changed')],
                    [u.L('banner.running.title', dest='router')], [], []])
                u.exists.side_effect = [False, True, True, True]
                phases, _ = flows.wait_switch_done(self.run, self.step, u, timeout=30, router_restart=True)
                self.assertIn('[full restart: router certificate]', phases)
                u.click.assert_called_once_with(key='confirm.full_restart', type='Button',
                                                within=u.modal('confirm.title'))

    def test_router_confirmation_never_accepts_an_unrelated_reason(self):
        u = self.ui()
        u.texts = Mock(return_value=[u.L('confirm.title'), u.L('restart.reason.no_cdp')])
        with self.assertRaisesRegex(UiError, 'unexpected'):
            flows.wait_switch_done(self.run, self.step, u, router_restart=True)
        u.click.assert_not_called()

    def test_default_switch_waiter_does_not_autoapprove_certificate_confirmation(self):
        u = self.ui()
        u.texts = Mock(return_value=[u.L('confirm.title'), u.L('restart.reason.certificate_trust_changed')])
        with self.assertRaisesRegex(UiError, 'did not finish'):
            flows.wait_switch_done(self.run, self.step, u, timeout=4)
        u.click.assert_not_called()

    def test_router_confirmation_is_never_clicked_twice(self):
        u = self.ui()
        u.texts = Mock(return_value=[u.L('confirm.title'), u.L('restart.reason.certificate_trust_changed')])
        with self.assertRaisesRegex(UiError, 'repeated'):
            flows.wait_switch_done(self.run, self.step, u, router_restart=True)
        u.click.assert_called_once()

    def test_w32_always_sends_own_turn_and_waits_for_its_new_success(self):
        t = SimpleNamespace(run=self.run, step=Mock(return_value=self.step))
        old = dict(id=1, source='router', thread_id='old', status={'kind': 'ok'})
        wrong_thread = dict(id=2, source='router', thread_id='other', status={'kind': 'ok'})
        pending = dict(id=3, source='router', thread_id='w32', status={'kind': 'running'})
        success = {**pending, 'status': {'kind': 'ok'}}
        def turn(*args, **kwargs):
            self.run.state['thread'] = 'w32'
        with patch('scenarios.core.usage_rows', side_effect=[[old], [old, wrong_thread, pending],
                                                          [old, wrong_thread, success]]) as read:
            with patch('scenarios.core.flows.turn', side_effect=turn) as send:
                routed_usage_turn(t)
        send.assert_called_once_with(self.run, t, 'E2E-W32', new_chat=True)
        self.assertEqual(read.call_count, 3)
        self.assertEqual(self.clock.now, 2)

    def test_w32_old_successes_and_new_failures_cannot_pass(self):
        self.run.state['thread'] = 'w32'
        t = SimpleNamespace(run=self.run, step=Mock(return_value=self.step))
        rows = [dict(id=1, source='router', thread_id='w32', status={'kind': 'ok'}),
                dict(id=2, source='router', thread_id='w32', status={'kind': 'failed'})]
        with patch('scenarios.core.usage_rows', return_value=rows), patch('scenarios.core.flows.turn') as send:
            with self.assertRaisesRegex(AssertionError, 'new successful router row'):
                routed_usage_turn(t, timeout=2)
        send.assert_called_once()

    def test_provider_combo_uses_visible_bounds_and_checks_the_selected_value(self):
        u = self.ui()
        u.wait = Mock(side_effect=[{'rect': [10, 20, 100, 30]}, {'value': 'Model'}, {'value': 'Provider'}])
        u.find = Mock(return_value=[{'type': 'RadioButton', 'rect': [10, 60, 100, 30]}])
        u.click_at = Mock()
        usage_provider_group(u, self.step)
        self.assertEqual([c.args for c in u.click_at.call_args_list], [(60, 35), (60, 75)])
        u.click.assert_not_called()
        self.assertEqual(u.wait.call_count, 3)

    def test_usage_prepares_an_unobscured_window_after_the_completed_turn(self):
        u = self.ui()
        overlay = {'hwnd': 23, 'visible': True, 'minimized': False}
        hidden = {'hwnd': 24, 'visible': False, 'minimized': False}
        minimized = {'hwnd': 23, 'minimized': True}
        self.run.agent.call.side_effect = [
            [overlay, hidden], minimized, {'foreground': True, 'minimized': False}]
        usage_show_window(u, self.step)
        self.assertEqual(self.run.agent.call.call_args_list, [
            call('windows', process_re='^ChatGPT$'),
            call('window', hwnd=23, minimize=True),
            call('window', foreground=True, process_re='^superba$')])
        u.click_at.assert_not_called()

    def test_usage_refuses_a_chatgpt_window_that_stays_over_the_page(self):
        u = self.ui()
        self.run.agent.call.side_effect = [
            [{'hwnd': 23, 'visible': True, 'minimized': False}], {'minimized': False}]
        with self.assertRaisesRegex(AssertionError, 'ChatGPT window was minimized'):
            usage_show_window(u, self.step)
        self.assertEqual(self.run.agent.call.call_count, 2)

    def test_usage_refuses_a_background_superba_window(self):
        u = self.ui()
        self.run.agent.call.side_effect = [[], {'foreground': False, 'minimized': False}]
        with self.assertRaisesRegex(AssertionError, 'Superba is active'):
            usage_show_window(u, self.step)

    def test_usage_detail_uses_real_click_and_requires_both_tabs(self):
        u = Mock()
        usage_open_detail(u, self.step, {'name': '12:34, E2E-W32, Done, gpt-6.1-sol'})
        u.act.assert_called_once_with('click', name='12:34, E2E-W32, Done, gpt-6.1-sol',
                                      type='DataItem', enabled=True)
        self.assertEqual([c.kwargs['key'] for c in u.wait.call_args_list],
                         ['requests.detail.general', 'requests.detail.timing'])
        u.click.assert_not_called()

    def test_usage_detail_does_not_pass_when_click_leaves_list_visible(self):
        u = Mock()
        u.wait.side_effect = UiError('detail tab not found')
        with self.assertRaisesRegex(UiError, 'detail tab not found'):
            usage_open_detail(u, self.step, {'name': '12:34, E2E-W32'})


@unittest.skipUnless(shutil.which('node'), 'node needed to execute the renderer predicate')
class ModelDialogDomTests(unittest.TestCase):
    def test_only_visible_enabled_exact_keep_button_in_model_intro_dialog_is_clicked(self):
        fixture = r'''
const cases = JSON.parse(process.argv[1]);
const out = [];
for (const fixture of cases) {
  const clicks = [];
  const button = (label, visible=true, disabled=false) => ({innerText:label,
    offsetParent:visible?{}:null, getClientRects:()=>[], disabled,
    getAttribute:()=>null, click:()=>clicks.push(label)});
  const buttons = [button(fixture.label, fixture.buttonVisible, fixture.disabled), button('Try new model')];
  global.document = {querySelectorAll:()=>[{innerText:fixture.title,
    offsetParent:fixture.visible?{}:null, getClientRects:()=>[], querySelectorAll:()=>buttons}]};
  const result = eval(process.argv[2]);
  out.push({result,clicks});
}
process.stdout.write(JSON.stringify(out));
'''
        good = dict(title='Introducing example-model\nSome description', visible=True,
                    label='Continue with current model', buttonVisible=True, disabled=False)
        cases = [good, {**good, 'title': 'Delete conversation?'}, {**good, 'visible': False},
                 {**good, 'buttonVisible': False}, {**good, 'disabled': True},
                 {**good, 'label': 'Continue'}, {**good, 'title': 'Loading model…'}]
        p = subprocess.run(['node', '-e', fixture, json.dumps(cases), App.KEEP_MODEL_JS],
                           check=True, capture_output=True, text=True)
        result = json.loads(p.stdout)
        self.assertEqual(result[0], {'result': True, 'clicks': ['Continue with current model']})
        self.assertEqual(result[1:], [{'result': False, 'clicks': []}] * (len(cases) - 1))


if __name__ == '__main__':
    unittest.main()
