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
        offline = copy.deepcopy(d.harness_config.DEFAULTS)
        offline['typesafe']['mode'] = 'off'
        config_path = self.root / 'config.json'
        config_path.write_text(json.dumps(offline))
        self.env = patch.dict(os.environ, {'HARNESS_CONFIG': str(config_path), 'HARNESS_DISPATCH_DB': str(self.root / 'dispatch.sqlite'), 'HARNESS_AUDIT_ENABLED': '0', 'HARNESS_WORKFLOW_STATE': str(self.root / 'workflow.sqlite')})
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
        return d.create({'workers': list(workers), 'assessment': self.assessment(), 'request_id': 'request-a'}, self.project, 'session')['run_id']
    def assessment(self, strategy='delegate'):
        return {'task_size': 'substantial', 'strategy': strategy, 'reason': 'Independent bounded inspection' if strategy == 'delegate' else 'Coupled change needs shared context', 'model_fit': 'Native smaller model fits inspection; parent verifies', 'latency_tradeoff': 'Run independent inspection concurrently to offset setup cost'}
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
    def test_assessment_required_and_scope_expansion_invalidates_plan(self):
        import workflow_audit
        trivial = {'workers': [], 'rationale': 'One small edit', 'acceptance': ['Check']}
        d.create(trivial, self.project, 'session')
        self.assertTrue(d.session_has_plan(self.project, 'session'))
        with workflow_audit.context_db() as db:
            context = workflow_audit.load(db, 'session')
            context['todos'] = {key: {'id': key, 'required': True, 'status': 'pending', 'criterion': 'Pass'} for key in ('build', 'check')}
            workflow_audit.save(db, 'session', context)
        self.assertFalse(d.session_has_plan(self.project, 'session'))

        with self.assertRaisesRegex(ValueError, 'substantial work requires assessment'):
            d.create(trivial, self.project, 'session')
        plan = {**trivial, 'assessment': self.assessment('main_only')}
        result = d.create(plan, self.project, 'session')
        self.assertTrue(d.session_has_plan(self.project, 'session'))
        self.assertEqual(d.status(result['run_id'])['run']['assessment']['strategy'], 'main_only')
        with workflow_audit.context_db() as db:
            context = workflow_audit.load(db, 'session')
            context['todos']['build']['status'] = 'completed'
            workflow_audit.save(db, 'session', context)
        self.assertTrue(d.session_has_plan(self.project, 'session'))
        with workflow_audit.context_db() as db:
            context['todos']['build']['criterion'] = 'New acceptance scope'
            workflow_audit.save(db, 'session', context)
        self.assertFalse(d.session_has_plan(self.project, 'session'))

    def test_existing_dispatch_database_migrates_without_rewriting_history(self):
        with d.sqlite3.connect(d.db_path()) as db:
            db.execute('CREATE TABLE runs (id TEXT PRIMARY KEY, project TEXT, session TEXT, status TEXT, created REAL, request_id TEXT)')
            db.execute('INSERT INTO runs VALUES (?,?,?,?,?,?)', ('old', str(self.project), 'session', 'completed', 10, None))
        with d.connection() as db:
            run = d.get_run(db, 'old')
            self.assertIsNone(run['assessment'])
            self.assertIsNone(run['measurement'])
            self.assertEqual(run['created'], 10)

    def test_assessment_rejects_missing_text_and_inconsistent_strategy(self):
        for changes in ({'reason': ''}, {'model_fit': ' '}, {'latency_tradeoff': 'x' * 1001}, {'strategy': 'main_only'}, {'task_size': 'trivial'}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                d.validate({'workers': [self.worker()], 'assessment': {**self.assessment(), **changes}}, self.project, self.config)
        with self.assertRaisesRegex(ValueError, 'requires assessment'):
            d.create({'workers': [self.worker()]}, self.project, 'session')

    def test_whole_task_measurement_preserves_unknowns_and_counts_parent_time(self):
        plan = {'workers': [], 'rationale': 'Coupled work', 'acceptance': ['Check'], 'assessment': self.assessment('main_only')}
        run = d.create(plan, self.project, 'session')['run_id']
        created = d.status(run)['run']['created']
        with patch.object(d.time, 'time', return_value=created + 10), patch.object(d.audit_transport, 'emit') as emit:
            value = d.measure(run, {'rework_count': 2, 'quality_passed': True, 'comparison_key': 'bounded_fix'})['measurement']
        self.assertEqual(value['elapsed_ms'], 10000)
        self.assertIsNone(value['cost_usd'])
        self.assertIsNone(value['total_tokens'])
        self.assertEqual(d.status(run)['run']['measurement'], value)
        self.assertEqual(emit.call_args.args[0], 'orchestration.run_measured')
        self.assertEqual(emit.call_args.args[1]['strategy'], 'main_only')
        self.assertNotIn('model_fit', emit.call_args.args[1])
        with self.assertRaisesRegex(ValueError, 'already recorded'):
            d.measure(run, {})

    def test_measurement_requires_terminal_workers_and_valid_numbers(self):
        run = self.create(self.worker())
        with self.assertRaisesRegex(ValueError, 'all workers'):
            d.measure(run, {})
        d.next_workers(run)
        d.bind(run, 'one', 'native-1')
        self.complete(run)
        for report in ({'cost_usd': float('nan')}, {'total_tokens': True}, {'rework_count': -1}, {'quality_passed': 'yes'}, {'comparison_key': 'private data'}, {'elapsed_ms': 0}):
            with self.subTest(report=report), self.assertRaises(ValueError):
                d.measure(run, report)
        value = d.measure(run, {'cost_usd': 0, 'total_tokens': 400, 'rework_count': 0, 'quality_passed': False})['measurement']
        self.assertEqual(value['total_tokens'], 400)
        self.assertEqual(value['cost_usd'], 0)
    def test_contract_rejects_cycles_overlap_and_symlink_escape(self):
        cases = [[self.worker(dependencies=['one'])], [self.worker(access='edit', owned_paths=['src']), self.worker('two', access='edit', owned_paths=['src/file'])], [self.worker(owned_paths=['../secret'])]]
        (self.project / 'escape').symlink_to(self.root)
        cases.append([self.worker(owned_paths=['escape/file'])])
        for workers in cases:
            with self.assertRaises(ValueError):
                d.validate({'workers': workers, 'assessment': self.assessment()}, self.project, self.config)
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
        self.config['timeout_seconds'] = 5
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
        self.config['timeout_seconds'] = 5
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
        self.config['timeout_seconds'] = 5
        self.config['profiles']['scan'] = {'provider': 'command', 'role': 'explorer', 'model': 'fixture', 'effort': 'low', 'command': [sys.executable, '-c', 'import json,sys;json.dump({"status":"completed","evidence":"real process","acceptance":[{"criterion":"Evidence checked","passed":True,"evidence":"checked"}]},open(sys.argv[1],"w"))', '{result}']}
        run = self.create(self.worker())
        result = d.run_workers(run)
        self.assertEqual(result['run']['status'], 'completed')
        self.assertEqual(result['workers'][0]['exit_code'], 0)
        self.assertIsNone(result['workers'][0]['total_tokens'])
        self.config['timeout_seconds'] = 1
        self.config['profiles']['scan']['command'] = [sys.executable, '-c', 'import time;time.sleep(30)']
        run = self.create(self.worker())
        result = d.run_workers(run)
        self.assertEqual(result['workers'][0]['exit_code'], 124)
        self.assertEqual(result['run']['status'], 'failed')


FAKE_AMP = r'''#!/usr/bin/env python3
"""Stand-in for the amp CLI: -ox starts a fake orb thread, threads export prints it."""
import json, os, re, subprocess, sys
from pathlib import Path
state = Path(os.environ['FAKE_AMP_DIR'])
mode = os.environ.get('FAKE_AMP_MODE', 'read')
args = sys.argv[1:]
if args[:2] == ['threads', 'export']:
    print((state / (args[2] + '.json')).read_text())
    sys.exit(0)
prompt = sys.stdin.read()
(state / 'prompt.txt').write_text(prompt)
(state / 'argv.json').write_text(json.dumps(args))
if mode == 'fail':
    print("Error: Session expired and could not be refreshed. Run 'amp login' to authenticate.", file=sys.stderr)
    sys.exit(1)
criteria = json.loads(next(line for line in prompt.splitlines() if line.startswith('Acceptance: '))[len('Acceptance: '):])
report = {'status': 'completed', 'evidence': 'Checked in the orb', 'artifacts': [],
          'acceptance': [{'criterion': c, 'passed': True, 'evidence': 'Checked'} for c in criteria],
          'audit': {'commands': [{'cmd': 'cat source.txt', 'exit': 0}], 'files_written': [], 'network': [], 'deviations': []}}
tools = [{'type': 'tool_use', 'name': 'shell_command', 'input': {'command': 'cat source.txt'}}]
if mode in ('edit', 'unowned', 'nocommit'):
    branch, base = re.search(r'git checkout -B (\S+) ([0-9a-f]+)', prompt).groups()
    clone = state / 'clone'
    run = lambda *a: subprocess.run(['git', '-C', str(clone), *a], check=True, capture_output=True, text=True).stdout.strip()
    subprocess.run(['git', 'clone', '-q', os.environ['FAKE_AMP_ORIGIN'], str(clone)], check=True)
    run('checkout', '-q', '-B', branch, base)
    name = 'source.txt' if mode == 'edit' else 'other.txt'
    (clone / name).write_text('orb result\n')
    run('add', name)
    run('-c', 'user.name=Orb', '-c', 'user.email=orb@localhost', 'commit', '-q', '-m', 'Orb change')
    run('push', '-q', 'origin', 'HEAD:refs/heads/' + branch)
    if mode != 'nocommit':
        report['commit'] = run('rev-parse', 'HEAD').upper()  # Case must not matter.
    report['audit']['files_written'] = [name]
    tools.append({'type': 'tool_use', 'name': 'apply_patch', 'input': {'patchText': '*** Begin Patch\n*** Update File: /home/user/workspace/repo/' + name + '\n*** End Patch'}})
usage = {'totalInputTokens': 100, 'cacheReadInputTokens': 80, 'outputTokens': 10}
last = {'role': 'assistant', 'state': {'type': 'complete', 'stopReason': 'end_turn'}, 'usage': usage, 'content': [{'type': 'text', 'text': json.dumps(report)}]}
agent = 'idle'
if mode == 'hang':
    last = {'role': 'assistant', 'state': {'type': 'complete', 'stopReason': 'tool_use'}, 'usage': usage, 'content': tools[:1]}
    agent = 'running'
export = {'id': 'T-fake', 'meta': {'lastKnownAgentState': {'state': agent}}, 'messages': [
    {'role': 'user', 'content': [{'type': 'text', 'text': prompt}]},
    {'role': 'assistant', 'state': {'type': 'complete', 'stopReason': 'tool_use'}, 'usage': usage, 'content': tools},
    {'role': 'user', 'content': [{'type': 'tool_result'}]}, last]}
(state / 'T-fake.json').write_text(json.dumps(export))
print('https://ampcode.com/threads/T-fake')
'''


class AmpOrbTest(unittest.TestCase):
    # Reuse the dispatcher fixture without rerunning its tests.
    worker, create, assessment, init_git, tearDown = (DispatcherTest.worker, DispatcherTest.create, DispatcherTest.assessment,
                                                      DispatcherTest.init_git, DispatcherTest.tearDown)

    def setUp(self):
        DispatcherTest.setUp(self)
        self.config['profiles']['orb'] = {'provider': 'amp', 'model': 'amp', 'effort': 'xhigh', 'role': '*'}
        self.amp = {'enabled': True, 'effort': 'xhigh', 'poll_seconds': 1}
        self.loader.stop()
        self.loader = patch.object(d.harness_config, 'load', side_effect=lambda: {'dispatch': self.config, 'amp': self.amp})
        self.loader.start()
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        (bin_dir / 'amp').write_text(FAKE_AMP)
        (bin_dir / 'amp').chmod(0o755)
        self.state = self.root / 'amp-state'
        self.state.mkdir()
        self.path = patch.dict(os.environ, {'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'], 'FAKE_AMP_DIR': str(self.state),
                                            'FAKE_AMP_ORIGIN': str(self.root / 'origin.git')})
        self.path.start()
        self.addCleanup(self.path.stop)
        # An orb clones the remote, so orb workers need a pushed, clean checkout.
        self.pushed_project()

    def pushed_project(self):
        self.init_git()
        subprocess.run(['git', 'init', '-q', '--bare', str(self.root / 'origin.git')], check=True)
        d.git(self.project, 'remote', 'add', 'origin', str(self.root / 'origin.git'))
        d.git(self.project, 'push', '-q', 'origin', 'HEAD:refs/heads/main')
        d.git(self.project, 'fetch', '-q', 'origin')

    def orb_run(self, mode, access='read'):
        os.environ['FAKE_AMP_MODE'] = mode
        run = self.create(self.worker(access=access))  # Lists only the native profile; the orb comes first.
        handoff = d.next_workers(run)['workers']
        if handoff:
            self.assertEqual(handoff[0]['provider'], 'amp')
            d.external(run, 'one')
        return run, d.status(run)['workers'][0]

    def test_amp_profiles_require_the_amp_switch(self):
        self.amp['enabled'] = False
        with self.assertRaisesRegex(ValueError, 'amp orb delegation is disabled'):
            self.create(self.worker(profiles=['orb']))

    def selected(self, *workers):
        run = self.create(*workers)
        return {w['id']: w for w in d.status(run)['workers']}

    def test_with_amp_on_every_worker_is_offered_an_orb_first(self):
        worker = self.selected(self.worker())['one']
        self.assertEqual((worker['profile_id'], worker['profile']), ('amp_orb', {'provider': 'amp', 'model': 'amp', 'effort': 'xhigh', 'role': '*'}))
        self.assertNotIn('orb_skipped', worker)

    def test_with_amp_off_selection_is_unchanged(self):
        self.amp['enabled'] = False
        worker = self.selected(self.worker())['one']
        self.assertEqual(worker['profile_id'], 'scan')
        self.assertNotIn('orb_skipped', worker)

    def test_workers_an_orb_cannot_take_fall_back_with_the_reason(self):
        with patch('shutil.which', return_value=None):
            worker = self.selected(self.worker())['one']
        self.assertEqual((worker['profile_id'], worker['orb_skipped']), ('scan', 'amp CLI not found'))
        # A reviewer that depends on another worker must read its results, which exist only here.
        chosen = self.selected(self.worker('build', access='edit', owned_paths=['other.txt']), self.worker('two', dependencies=['build']))
        self.assertEqual(chosen['build']['profile_id'], 'amp_orb')
        self.assertEqual((chosen['two']['profile_id'], chosen['two']['orb_skipped']),
                         ('scan', 'orb workers cannot see the results of the workers they depend on'))

    def test_a_ruled_out_orb_is_not_picked_again_from_the_listed_profiles(self):
        with patch('shutil.which', return_value=None):
            worker = self.selected(self.worker(profiles=['orb', 'scan']))['one']
            self.assertEqual(worker['profile_id'], 'scan')
            with self.assertRaisesRegex(ValueError, 'lists only orb profiles, and an orb cannot take it: amp CLI not found'):
                self.create(self.worker(profiles=['orb']))

    def test_read_workers_see_only_pushed_clean_code_and_read_that_commit(self):
        (self.project / 'source.txt').write_text('local edit\n')
        worker = self.selected(self.worker())['one']
        self.assertEqual((worker['profile_id'], worker['orb_skipped']), ('scan', 'orb workers cannot see uncommitted changes to tracked files'))
        (self.project / 'source.txt').write_text('base\n')
        head = d.git(self.project, 'rev-parse', 'HEAD').decode().strip()
        self.orb_run('read')
        self.assertIn('git checkout --detach ' + head, (self.state / 'prompt.txt').read_text())

    def test_read_worker_keeps_report_thread_export_and_usage(self):
        run, worker = self.orb_run('read')
        self.assertEqual(worker['status'], 'completed', worker.get('result'))
        prompt = (self.state / 'prompt.txt').read_text()
        self.assertIn('Read only: do not modify, commit or push anything.', prompt)
        self.assertNotIn(str(self.project), prompt, 'the orb never sees local paths')
        self.assertIn('"audit"', prompt)
        argv = json.loads((self.state / 'argv.json').read_text())
        self.assertEqual(argv[:3], ['-ox', '-m', 'high'])
        self.assertEqual(worker['thread_url'], 'https://ampcode.com/threads/T-fake')
        audit = worker['result']['audit']
        self.assertTrue(Path(audit['export']).is_file())
        self.assertEqual(audit['reported']['commands'][0]['cmd'], 'cat source.txt')
        self.assertEqual(audit['observed']['commands'], ['cat source.txt'])
        self.assertEqual((worker['input_tokens'], worker['cached_input_tokens'], worker['output_tokens'], worker['total_tokens']), (200, 160, 20, 220))

    def test_edit_worker_branch_is_fetched_into_owned_paths_and_integrates(self):
        run, worker = self.orb_run('edit', access='edit')
        self.assertEqual(worker['status'], 'completed', worker.get('result'))
        self.assertIn('git checkout -B harness/', (self.state / 'prompt.txt').read_text())
        self.assertEqual(worker['changed_paths'], ['source.txt'])
        self.assertIn('+orb result', Path(worker['patch']).read_text())
        self.assertEqual(worker['result']['audit']['unreported_files'], [])
        d.integrate(run, 'one')
        self.assertEqual((self.project / 'source.txt').read_text(), 'orb result\n')

    def test_edit_report_must_name_the_pushed_commit(self):
        _, worker = self.orb_run('nocommit', access='edit')
        self.assertEqual(worker['status'], 'failed')
        self.assertIn('edit report must include the pushed commit', worker['result']['evidence'])

    def test_cancel_while_amp_starts_still_records_the_thread(self):
        real = d.amp_cli
        def launch_then_cancel(*args, **kwargs):
            result = real(*args, **kwargs)
            if args[0] == '-ox':
                d.cancel(self.run_id, 'one')
            return result
        os.environ['FAKE_AMP_MODE'] = 'read'
        self.run_id = self.create(self.worker(profiles=['orb']))
        d.next_workers(self.run_id)
        with patch.object(d, 'amp_cli', side_effect=launch_then_cancel):
            d.external(self.run_id, 'one')
        worker = d.status(self.run_id)['workers'][0]
        self.assertEqual((worker['status'], worker['thread_url']), ('cancelled', 'https://ampcode.com/threads/T-fake'))
        self.assertIn('not stopped', worker['orb_note'])

    def test_a_slow_export_is_retried(self):
        real, calls = d.amp_cli, []
        def stall_once(*args, **kwargs):
            if args[:2] == ('threads', 'export') and not calls:
                calls.append(args)
                raise subprocess.TimeoutExpired(cmd='amp', timeout=120)
            return real(*args, **kwargs)
        with patch.object(d, 'amp_cli', side_effect=stall_once):
            _, worker = self.orb_run('read')
        self.assertEqual((worker['status'], len(calls)), ('completed', 1))

    def test_edit_outside_owned_paths_fails(self):
        _, worker = self.orb_run('unowned', access='edit')
        self.assertEqual((worker['status'], worker['error']), ('failed', 'worker changed paths outside ownership'))

    def test_edit_workers_need_a_pushed_base_and_clean_owned_paths(self):
        (self.project / 'new.txt').write_text('untracked\n')
        worker = self.selected(self.worker(access='edit', owned_paths=['new.txt']))['one']
        self.assertEqual((worker['profile_id'], worker['orb_skipped']), ('scan', 'orb workers cannot see uncommitted changes in their owned paths'))
        (self.project / 'new.txt').unlink()
        d.git(self.project, '-c', 'user.name=Test', '-c', 'user.email=test@localhost', 'commit', '-q', '--allow-empty', '-m', 'Unpushed')
        worker = self.selected(self.worker(access='edit'))['one']
        self.assertEqual(worker['orb_skipped'], 'orb workers need the current commit pushed to a remote')
        self.assertFalse((self.state / 'prompt.txt').exists(), 'no orb starts')

    def test_launch_failure_and_unfinished_thread_fail_visibly(self):
        _, worker = self.orb_run('fail')
        self.assertEqual(worker['status'], 'failed')
        self.assertIn('Session expired', worker['result']['evidence'])
        self.config['timeout_seconds'] = 1
        _, worker = self.orb_run('hang')
        self.assertEqual((worker['status'], worker['exit_code']), ('failed', 124))
        self.assertIn('may still be running: https://ampcode.com/threads/T-fake', worker['result']['evidence'])


if __name__ == '__main__':
    unittest.main()
