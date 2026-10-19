"""Confirmed matrix regressions: exact UI semantics and independent evidence."""
import json
import shutil
import subprocess
import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import flows
from app import App
from migration import (check_migrated, check_restored, migration_count,
                       wait_migration)
from scenarios.core import routed_usage_turn, usage_provider_group
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
                                   agent=Mock(), state={}, session_snapshot='wintest')
        self.run.agent.procs.return_value = []
        self.step = checking_step()

    def ui(self, lang='en'):
        u = Ui(self.run, lang)
        u.click = Mock()
        u.exists = Mock(return_value=True)
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
        u.click.assert_called_once_with(key='movers.stop', type='Button')
        self.assertEqual(self.clock.now, 2)

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
                u.exists.side_effect = [False, True, True]
                phases, _ = flows.wait_switch_done(self.run, self.step, u, timeout=30, router_restart=True)
                self.assertIn('[full restart: router certificate]', phases)
                u.click.assert_called_once_with(key='confirm.full_restart', type='Button')

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
