"""Offline dispatcher contracts and actual process/worktree lifecycle."""
import copy
import json
import os
from pathlib import Path
import subprocess
import time
import threading
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'scripts'))
import role_dispatch as d


class DispatcherTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.project = self.root / 'project'
        self.project.mkdir()
        self.config = {'enabled': True, 'max_workers': 1, 'max_context_bytes': 12000, 'timeout_seconds': 1,
                       'profiles': {'scan': {'provider': 'native', 'model': 'test-model', 'effort': 'low', 'role': 'explorer', 'agent_type': 'explorer'}}}
        self.env = patch.dict(os.environ, {'HARNESS_DISPATCH_DB': str(self.root / 'dispatch.sqlite'), 'HARNESS_AUDIT_ENABLED': '0'})
        self.env.start()
        self.loader = patch.object(d.harness_config, 'load', return_value={'dispatch': self.config})
        self.loader.start()
    def tearDown(self):
        self.loader.stop()
        self.env.stop()
        self.temp.cleanup()
    def worker(self, name='one', **changes):
        value = {'id': name, 'role': 'explorer', 'responsibility': 'Inspect one bounded concern', 'acceptance': ['Evidence checked'], 'owned_paths': ['source.txt'], 'context': 'Small context', 'dependencies': [], 'access': 'read', 'profiles': ['scan']}
        value.update(changes)
        return value
    def create(self, *workers):
        return d.create({'workers': list(workers), 'request_id': 'request-a'}, self.project, 'session')['run_id']
    def complete(self, run, name='one'):
        return d.finish(run, name, {'status': 'completed', 'evidence': 'Ran targeted checks', 'acceptance': [{'criterion': 'Evidence checked', 'evidence': 'Targeted check passed', 'passed': True}]})
    def test_native_lifecycle_capacity_and_dependencies(self):
        run = self.create(self.worker(), self.worker('two', dependencies=['one']))
        handoff = d.next_workers(run)['workers']
        self.assertEqual(len(handoff), 1)
        self.assertEqual(handoff[0]['arguments']['fork_turns'], 'none')
        self.assertEqual(d.next_workers(run)['workers'], [])
        d.bind(run, 'one', 'native-1')
        with self.assertRaises(ValueError):
            d.finish(run, 'one', {'status': 'completed', 'evidence': 'claim'})
        self.complete(run)
        self.assertEqual(d.next_workers(run)['workers'][0]['worker_id'], 'two')
        d.bind(run, 'two', 'native-2')
        result = self.complete(run, 'two')
        self.assertEqual(result['run']['status'], 'completed')
        self.assertIsNone(result['workers'][0]['total_tokens'])
        self.assertTrue(d.session_has_plan(self.project, 'session', 'request-a'))
        self.assertFalse(d.session_has_plan(self.project, 'session', 'request-b'))
    def test_failures_block_dependents_and_cancel_native(self):
        run = self.create(self.worker(), self.worker('two', dependencies=['one']))
        d.next_workers(run)
        d.bind(run, 'one', 'agent')
        handoff = d.cancel(run, 'one')['handoffs'][0]
        self.assertEqual(handoff['arguments']['target'], 'agent')
        self.assertEqual(d.status(run)['workers'][0]['status'], 'cancelling')
        self.assertEqual(d.next_workers(run)['workers'], [])
        d.cancel(run, 'one', acknowledged=True, evidence='Parent interrupt tool reported terminated')
        d.next_workers(run)
        self.assertEqual(d.status(run)['workers'][1]['status'], 'blocked')
    def test_toggle_and_main_only(self):
        self.config['enabled'] = False
        self.assertEqual(d.create({}, self.project, 'session')['status'], 'disabled')
        self.config['enabled'] = True
        result = d.create({'workers': [], 'rationale': 'One small edit', 'acceptance': ['Required verification']}, self.project, 'session')
        self.assertTrue(d.session_has_plan(self.project, 'session'))
        self.config['enabled'] = False
        self.assertEqual(d.next_workers(result['run_id'])['status'], 'disabled')
    def test_contract_rejects_cycles_overlap_and_symlink_escape(self):
        cases = [[self.worker(dependencies=['one'])], [self.worker(access='edit', owned_paths=['src']), self.worker('two', access='edit', owned_paths=['src/file'])], [self.worker(owned_paths=['../secret'])]]
        (self.project / 'escape').symlink_to(self.root)
        cases.append([self.worker(owned_paths=['escape/file'])])
        for workers in cases:
            with self.assertRaises(ValueError):
                d.validate({'workers': workers}, self.project, self.config)
    def test_editing_snapshot_and_patch_excludes_parent_changes(self):
        d.git(self.project, 'init')
        (self.project / 'source.txt').write_text('base\n')
        d.git(self.project, 'add', '.')
        d.git(self.project, '-c', 'user.name=Test', '-c', 'user.email=test@localhost', 'commit', '-m', 'Initial fixture')
        (self.project / 'source.txt').write_text('dirty parent\n')
        (self.project / 'untracked.txt').write_text('untracked parent\n')
        run = self.create(self.worker(access='edit'))
        handoff = d.next_workers(run)['workers'][0]
        workspace = Path(handoff['workspace'])
        self.assertEqual((workspace / 'source.txt').read_text(), 'dirty parent\n')
        self.assertEqual((workspace / 'untracked.txt').read_text(), 'untracked parent\n')
        (workspace / 'source.txt').write_text('worker result\n')
        d.bind(run, 'one', 'native')
        report = self.complete(run)['workers'][0]
        self.assertIn('-dirty parent', Path(report['patch']).read_text())
        self.assertNotIn('untracked parent', Path(report['patch']).read_text())
        self.assertEqual((self.project / 'source.txt').read_text(), 'dirty parent\n')
    def init_git(self):
        d.git(self.project, 'init')
        (self.project / 'source.txt').write_text('base\n')
        d.git(self.project, 'add', '.')
        d.git(self.project, '-c', 'user.name=Test', '-c', 'user.email=test@localhost', 'commit', '-m', 'Initial fixture')

    def test_snapshot_excludes_sensitive_untracked_and_tracked_paths(self):
        self.init_git()
        (self.project / '.env').write_text('synthetic fixture')
        (self.project / 'private.pem').write_text('synthetic fixture')
        run = self.create(self.worker(access='edit'))
        workspace = Path(d.next_workers(run)['workers'][0]['workspace'])
        self.assertFalse((workspace / '.env').exists())
        self.assertFalse((workspace / 'private.pem').exists())
        self.assertNotIn('.env', d.git(workspace, 'ls-files').decode().splitlines())
        self.assertEqual((self.project / '.env').read_text(), 'synthetic fixture')
        with self.assertRaises(ValueError):
            self.create(self.worker(access='edit', owned_paths=['.env']))

    def test_dependency_patch_applied_before_worker_and_parent_integration(self):
        self.init_git()
        run = self.create(self.worker(access='edit'), self.worker('two', access='edit', owned_paths=['second.txt'], dependencies=['one']))
        first = Path(d.next_workers(run)['workers'][0]['workspace'])
        (first / 'source.txt').write_text('predecessor content\n')
        with self.assertRaises(ValueError):
            self.complete(run)
        d.bind(run, 'one', 'native-1')
        self.complete(run)
        second = Path(d.next_workers(run)['workers'][0]['workspace'])
        self.assertEqual((second / 'source.txt').read_text(), 'predecessor content\n')
        self.assertEqual((self.project / 'source.txt').read_text(), 'base\n')
        d.integrate(run, 'one')
        self.assertEqual((self.project / 'source.txt').read_text(), 'predecessor content\n')

    def test_changed_ancestor_is_not_authorized_by_owned_descendant(self):
        self.init_git()
        (self.project / 'src').mkdir()
        (self.project / 'src/file').write_text('owned leaf')
        d.git(self.project, 'add', '--', 'src/file')
        d.git(self.project, '-c', 'user.name=Test', '-c', 'user.email=test@localhost', 'commit', '-m', 'Add nested fixture')
        run = self.create(self.worker(access='edit', owned_paths=['src/file']))
        workspace = Path(d.next_workers(run)['workers'][0]['workspace'])
        (workspace / 'src/file').unlink()
        (workspace / 'src').rmdir()
        (workspace / 'src').write_text('unauthorized ancestor replacement')
        d.bind(run, 'one', 'native')
        result = self.complete(run)
        self.assertEqual(result['workers'][0]['status'], 'failed')
        with self.assertRaises(ValueError):
            d.integrate(run, 'one')

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "External process supervision requires Linux /proc")
    def test_cancel_terminates_real_term_ignoring_group(self):
        self.config['timeout_seconds'] = 30
        self.config['profiles']['scan'] = {'provider': 'command', 'role': 'explorer', 'model': 'fixture', 'effort': 'low', 'command': [sys.executable, '-c', 'import signal,time,pathlib;signal.signal(signal.SIGTERM,signal.SIG_IGN);pathlib.Path(' + repr(str(self.root / 'child-ready')) + ').touch();time.sleep(30)']}
        run = self.create(self.worker())
        d.next_workers(run)
        thread = threading.Thread(target=d.external, args=(run, 'one'))
        thread.start()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            worker = d.status(run)['workers'][0]
            if worker.get('pid') and (self.root / 'child-ready').exists():
                break
            time.sleep(.03)
        self.assertTrue((self.root / 'child-ready').exists())
        d.cancel(run, 'one')
        thread.join(timeout=3)
        self.assertFalse(thread.is_alive())
        self.assertEqual(d.group_members(worker['pid']), {})
        self.assertEqual(d.status(run)['workers'][0]['status'], 'cancelled')

    def test_profile_selection_uses_redacted_unique_scope(self):
        self.config['profiles']['second'] = dict(self.config['profiles']['scan'])
        worker = self.worker(profiles=['scan', 'second'])
        with patch('decision_checkpoint.evaluate', return_value={'action': 'second', 'id': 'receipt'}) as evaluate, patch('decision_checkpoint.claim'), patch('decision_checkpoint.outcome'):
            run = self.create(worker)
        kwargs = evaluate.call_args.kwargs
        self.assertNotIn('Small context', json.dumps(kwargs, default=str))
        self.assertEqual(kwargs['scope']['worker_id'], 'one')
        self.assertEqual(d.status(run)['workers'][0]['profile_id'], 'second')

    def test_real_multi_profile_checkpoint_claim_matches_current_request(self):
        import workflow_audit
        import harness_config
        self.config['profiles']['second'] = dict(self.config['profiles']['scan'])
        config = copy.deepcopy(harness_config.DEFAULTS)
        config['typesafe']['mode'] = 'off'
        config['dispatch'] = self.config
        with patch.dict(os.environ, {'HARNESS_DB_ROOT': str(self.root / 'target-db'), 'HARNESS_WORKFLOW_STATE': str(self.root / 'workflow.sqlite')}), patch.object(d.harness_config, 'load', return_value=config):
            with workflow_audit.context_db() as db:
                workflow_audit.save(db, 'session', {'request_id': 'request-a', 'request_seq': 2, 'plan_revision': 1})
            run = self.create(self.worker(profiles=['scan', 'second']))
        self.assertEqual(d.status(run)['workers'][0]['profile_id'], 'scan')

    def test_profile_follows_confident_jev_and_otherwise_the_main_agents_first_choice(self):
        import decision_checkpoint
        import workflow_audit
        import harness_config
        self.config['profiles']['second'] = dict(self.config['profiles']['scan'])
        config = copy.deepcopy(harness_config.DEFAULTS)
        config['typesafe']['mode'] = 'active'
        config['dispatch'] = self.config
        record = self.root / 'advice.json'
        record.write_text('{}')
        for confidence, expected in ((0.9, 'second'), (0.69, 'scan')):
            answer = {'call_id': 'c', 'status': 'evaluated', 'latency_ms': 1, 'fallback_reason': None, 'confidence': confidence,
                      'answers': {'recommendation': {'choice': 'second', 'confidence': confidence}}, 'record_path': str(record)}
            with patch.dict(os.environ, {'HARNESS_DB_ROOT': str(self.root / f'db-{confidence}'), 'HARNESS_WORKFLOW_STATE': str(self.root / 'workflow.sqlite')}), \
                 patch.object(d.harness_config, 'load', return_value=config), \
                 patch.object(decision_checkpoint.advice, 'load_router', return_value=object()), \
                 patch.object(decision_checkpoint.advice, 'validate_context'), \
                 patch.object(decision_checkpoint.advice, 'advise', return_value=answer):
                with workflow_audit.context_db() as db:
                    workflow_audit.save(db, 'session', {'request_id': 'request-a', 'request_seq': 2, 'plan_revision': 1})
                run = self.create(self.worker(profiles=['scan', 'second']))
            self.assertEqual(d.status(run)['workers'][0]['profile_id'], expected, confidence)

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "External process supervision requires Linux /proc")
    def test_concurrent_execute_acquires_one_atomic_launch_lease(self):
        report = {'status': 'completed', 'evidence': 'one real process', 'acceptance': [{'criterion': 'Evidence checked', 'passed': True, 'evidence': 'verified'}]}
        count = self.root / 'launch-count'
        code = 'import pathlib; p=pathlib.Path(' + repr(str(count)) + ');p.open("a").write("launch\\n");print(' + repr(json.dumps(report)) + ')'
        self.config['profiles']['scan'] = {'provider': 'command', 'role': 'explorer', 'model': 'fixture', 'effort': 'low', 'command': [sys.executable, '-c', code]}
        run = self.create(self.worker())
        d.next_workers(run)
        entered, release = threading.Event(), threading.Event()
        real_popen = subprocess.Popen
        launches, errors = [], []
        def delayed_launch(*args, **kwargs):
            launches.append(1)
            self.assertEqual(kwargs.get('stdin'), subprocess.DEVNULL)
            entered.set()
            self.assertTrue(release.wait(3))
            return real_popen(*args, **kwargs)
        def execute():
            try:
                d.external(run, 'one')
            except ValueError as error:
                errors.append(str(error))
        with patch.object(d.subprocess, 'Popen', side_effect=delayed_launch):
            first = threading.Thread(target=execute)
            second = threading.Thread(target=execute)
            first.start()
            self.assertTrue(entered.wait(3))
            second.start()
            time.sleep(.05)
            release.set()
            first.join(5)
            second.join(5)
        self.assertFalse(first.is_alive() or second.is_alive())
        self.assertEqual(len(launches), 1)
        self.assertEqual(len(errors), 1)
        self.assertEqual(count.read_text().splitlines(), ['launch'])
        self.assertEqual(d.status(run)['workers'][0]['status'], 'completed')

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "External process supervision requires Linux /proc")
    def test_status_refresh_cannot_overwrite_concurrent_completion(self):
        self.config['profiles']['scan']['provider'] = 'command'
        run = self.create(self.worker())
        d.next_workers(run)
        identity = d.process_identity(os.getpid())
        with d.connection() as db:
            worker = d.workers(db, run)[0]
            worker.update(status='running', pid=os.getpid(), process_start=identity['start'])
            d.save(db, run, worker)
        entered, release = threading.Event(), threading.Event()
        real_identity = d.process_identity
        def delayed_identity(pid):
            if threading.current_thread().name == 'status-refresh':
                entered.set()
                self.assertTrue(release.wait(3))
            return real_identity(pid)
        with patch.object(d, 'process_identity', side_effect=delayed_identity):
            refresh = threading.Thread(target=d.status, args=(run,), name='status-refresh')
            complete = threading.Thread(target=self.complete, args=(run,))
            refresh.start()
            self.assertTrue(entered.wait(3))
            complete.start()
            time.sleep(.05)
            release.set()
            refresh.join(5)
            complete.join(5)
        self.assertFalse(refresh.is_alive() or complete.is_alive())
        worker = d.status(run)['workers'][0]
        self.assertEqual(worker['status'], 'completed')
        self.assertEqual(worker['result']['evidence'], 'Ran targeted checks')

    def test_external_supervision_refuses_platforms_without_process_identity(self):
        with patch.object(d.Path, 'exists', return_value=False):
            with self.assertRaisesRegex(ValueError, 'requires Linux /proc'):
                d.external('unstarted', 'one')

    def test_default_adapters_reduce_optional_context_and_restrict_access(self):
        profile = {'provider': 'codex', 'model': 'fixture', 'effort': 'low'}
        with patch.object(d, 'codex_optional_features', return_value=['apps', 'plugins', 'memories']):
            command = d.default_command(profile, 'read')
            self.assertEqual(command[command.index('--sandbox') + 1], 'read-only')
            self.assertIn('skills.include_instructions=false', command)
            for feature in ('apps', 'plugins', 'memories'):
                self.assertIn(feature, command)
            self.assertNotIn('--ignore-user-config', command)
            self.assertNotIn('--ignore-rules', command)
            with patch.dict(os.environ, {'HARNESS_PARENT_SANDBOX': 'read-only'}):
                edit = d.default_command(profile, 'edit')
                self.assertEqual(edit[edit.index('--sandbox') + 1], 'read-only')
        profile['provider'] = 'claude'
        command = d.default_command(profile, 'read')
        self.assertEqual(command[command.index('--tools') + 1], 'Read')
        self.assertEqual(command[-2:], ['--', '{prompt}'])
        import argparse
        parser = argparse.ArgumentParser()
        parser.add_argument('--tools', nargs='+')
        parser.add_argument('prompt')
        parsed = parser.parse_args(['--tools', 'Read', '--', 'Unique worker packet'])
        self.assertEqual(parsed.prompt, 'Unique worker packet')
        self.assertEqual(parsed.tools, ['Read'])
        self.assertEqual(command[command.index('--effort') + 1], 'low')

    def test_fenced_report_parsing_preserves_strict_acceptance_contract(self):
        report = {'status': 'completed', 'acceptance': [], 'evidence': 'verified'}
        self.assertEqual(d.parse_terminal_report('Verified result.\n```json\n' + json.dumps(report) + '\n```'), report)
        self.assertIsNone(d.parse_terminal_report('Result has no terminal report'))

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "External process supervision requires Linux /proc")
    def test_external_stdout_reports_and_real_usage_shapes(self):
        report = {'status': 'completed', 'evidence': 'stdout acceptance', 'acceptance': [{'criterion': 'Evidence checked', 'passed': True, 'evidence': 'stdout verified'}]}
        for provider_shape in ('codex', 'claude', 'claude_structured'):
            event = {'type': 'item.completed', 'item': {'type': 'agent_message', 'text': json.dumps(report)}} if provider_shape == 'codex' else {'type': 'result', 'result': json.dumps(report), 'usage': {'input_tokens': 5, 'output_tokens': 2, 'cache_creation_input_tokens': 3, 'cache_read_input_tokens': 4}, 'total_cost_usd': .012}
            if provider_shape == 'claude_structured':
                event['structured_output'] = report
                event.pop('result')
            events = [event]
            if provider_shape == 'codex':
                events.append({'type': 'turn.completed', 'usage': {'input_tokens': 10, 'cached_input_tokens': 7, 'output_tokens': 2}})
            code = ';'.join('print(' + repr(json.dumps(value)) + ')' for value in events)
            self.config['profiles']['scan'] = {'provider': 'command', 'role': 'explorer', 'model': 'fixture', 'effort': 'low', 'command': [sys.executable, '-c', code]}
            run = self.create(self.worker())
            handoff = d.next_workers(run)['workers'][0]
            self.assertEqual(handoff['tool'], 'exec_command')
            self.assertIn('execute', handoff['arguments']['cmd'])
            d.external(run, 'one')
            result = d.status(run)['workers'][0]
            self.assertEqual(result['status'], 'completed')
            self.assertEqual(result['total_tokens'], 12 if provider_shape == 'codex' else 14)
            self.assertEqual(result['cost_usd'], None if provider_shape == 'codex' else .012)
            self.assertEqual(result['cached_input_tokens'], 7 if provider_shape == 'codex' else 4)
            self.assertEqual(result['input_tokens'], 10 if provider_shape == 'codex' else 12)
            self.assertGreater(result['context_bytes'], len('Small context'))

    @unittest.skipUnless(Path("/proc/self/stat").exists(), "External process supervision requires Linux /proc")
    def test_external_process_terminal_report_and_timeout(self):
        self.config['profiles']['scan'] = {'provider': 'command', 'role': 'explorer', 'model': 'fixture', 'effort': 'low', 'command': [sys.executable, '-c', 'import json,sys;json.dump({"status":"completed","evidence":"real process","acceptance":[{"criterion":"Evidence checked","passed":True,"evidence":"checked"}]},open(sys.argv[1],"w"))', '{result}']}
        run = self.create(self.worker())
        result = d.run_workers(run)
        self.assertEqual(result['run']['status'], 'completed')
        self.assertEqual(result['workers'][0]['exit_code'], 0)
        self.assertIsNone(result['workers'][0]['total_tokens'])
        self.config['profiles']['scan']['command'] = [sys.executable, '-c', 'import time;time.sleep(30)']
        run = self.create(self.worker())
        result = d.run_workers(run)
        self.assertEqual(result['workers'][0]['exit_code'], 124)
        self.assertEqual(result['run']['status'], 'failed')


if __name__ == '__main__':
    unittest.main()
