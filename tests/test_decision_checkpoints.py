#!/usr/bin/env python3
"""Isolated decision lifecycle tests; no provider credentials or requests."""
import copy
import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'scripts'))
import decision_checkpoint as decision
import workflow_gate


class Decisions(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.env = patch.dict(os.environ, HARNESS_DB_ROOT=str(self.root), HARNESS_WORKFLOW_STATE=str(self.root / 'workflow.sqlite'))
        self.env.start()
        self.config = {'typesafe': {'mode': 'active', 'model': 'jev-1.13.0', 'decision_ttl_seconds': 300,
                                   'enforce_decisions': True}, 'dispatch': {'enabled': False}, 'telemetry': {'enabled': False}}
        self.settings = patch('harness_config.load', side_effect=lambda: copy.deepcopy(self.config))
        self.settings.start()
        self.telemetry = patch.object(decision, 'emit')
        self.emitted = self.telemetry.start()
        self.db = patch.dict(sys.modules, orchestration_common=types.SimpleNamespace(db_root=lambda project: self.root))
        self.db.start()
        self.args = dict(project=self.root, session_id='session', family='tool_selection', goal='Choose next step',
                         facts=['A bounded choice remains'], choices={'execute': 'Execute', 'read': 'Read'},
                         baseline='execute', scope={'request_id': 'r1', 'action_hash': 'abc'}, state={'revision': 1})

    def tearDown(self):
        self.db.stop()
        self.telemetry.stop()
        self.settings.stop()
        self.env.stop()
        self.temp.cleanup()

    def provider(self, context, router, db, delegate):
        path = self.root / 'advice.json'
        path.write_text(json.dumps({'usage': {'input_tokens': 3}}))
        return dict(call_id='provider-id', status='evaluated', latency_ms=12, fallback_reason=None,
                    answers={'recommendation': {'choice': 'read'}}, record_path=str(path))

    def evaluate(self, **changes):
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), \
             patch.object(decision.advice, 'advise', side_effect=self.provider) as api:
            result = decision.evaluate(**{**self.args, **changes})
            return result, api.call_count

    def test_modes_and_deterministic_bypass(self):
        for mode, action, count in [('off', 'execute', 0), ('shadow', 'execute', 1), ('active', 'read', 1)]:
            self.config['typesafe']['mode'] = mode
            row, calls = self.evaluate()
            self.assertEqual((row['action'], calls), (action, count))
        row, calls = self.evaluate(bypass='required_check')
        self.assertEqual((row['status'], row['action'], calls), ('bypassed', 'execute', 0))

    def test_exact_cache_state_and_scope(self):
        first, _ = self.evaluate()
        cached, calls = self.evaluate()
        self.assertEqual(calls, 0)
        self.assertNotEqual(first['id'], cached['id'])
        self.assertTrue(cached['cache_hit'])
        for changes in ({'state': {'revision': 2}}, {'scope': {'request_id': 'r2'}}, {'session_id': 'other'},
                        {'choices': {'execute': 'Changed', 'read': 'Read'}}):
            _, calls = self.evaluate(**changes)
            self.assertEqual(calls, 1)

    def test_claim_rejects_replay_and_records_observed_outcome(self):
        row, _ = self.evaluate()
        args = dict(project=self.root, identifier=row['id'], session_id='session', scope=self.args['scope'],
                    state=self.args['state'], action_hash='abc')
        with self.assertRaises(ValueError):
            decision.claim(**{**args, 'session_id': 'wrong'})
        decision.claim(**args)
        with self.assertRaises(ValueError):
            decision.claim(**args)
        done = decision.outcome(project=self.root, identifier=row['id'], session_id='session', exit_code=0)
        self.assertEqual(done['outcome']['label'], 'unknown')
        event, payload = self.emitted.call_args.args
        self.assertEqual(event, 'orchestration.decision_outcome')
        self.assertEqual(payload['outcome'], 'unknown')
        self.assertEqual(payload['usage']['input_tokens'], 3)
        self.assertEqual(payload['exit_code'], 0)
        self.assertIn('action_duration_ms', payload)
        self.assertEqual(payload['duration_ms'], row['duration_ms'])
        with self.assertRaises(ValueError):
            decision.outcome(project=self.root, identifier=row['id'], session_id='session')

    def test_outage_cached_without_repeated_api(self):
        with patch.object(decision.advice, 'load_router', side_effect=OSError) as api:
            row = decision.evaluate(**self.args)
            second = decision.evaluate(**self.args)
            self.assertEqual(api.call_count, 1)
            self.assertEqual(row['status'], 'fallback')
            self.assertTrue(second['cache_hit'])
            self.assertEqual(row['action'], 'execute')

    def test_current_prompt_invalidates_old_receipt(self):
        row, _ = self.evaluate()
        import workflow_audit
        with workflow_audit.context_db() as db:
            workflow_audit.save(db, 'session', {'request_id': 'new', 'request_seq': 2, 'plan_revision': 1})
        with self.assertRaisesRegex(ValueError, 'current prompt'):
            decision.claim(project=self.root, identifier=row['id'], session_id='session',
                           scope=self.args['scope'], state=self.args['state'], action_hash='abc')

    def test_active_hook_blocks_changed_choice_without_consuming(self):
        payload = {'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                   'tool_name': 'Edit', 'tool_input': {'file_path': 'a'}}
        with patch.object(decision, 'evaluate', return_value={'action': 'gather_context'}), \
             patch.object(decision, 'claim') as consume:
            with self.assertRaisesRegex(ValueError, 'gather_context'):
                workflow_gate.decision_seam(payload, {'request_id': 'r1'})
            consume.assert_not_called()

    def test_invalid_context_is_rejected_without_provider_call(self):
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context', side_effect=decision.advice.InputError('invalid')), \
             patch.object(decision.advice, 'advise') as api:
            with self.assertRaises(decision.advice.InputError):
                decision.evaluate(**self.args)
            api.assert_not_called()

    def test_stop_and_bookkeeping_do_not_route(self):
        self.assertEqual(workflow_gate.authorize({'hook_event_name': 'Stop'}), 0)
        payload = {'cwd': str(Path(__file__).resolve().parent.parent), 'session_id': 'session',
                   'tool_input': {'command': 'scripts/harness decision status --session-id session'}}
        self.assertTrue(workflow_gate.management_command(payload))
        with patch.object(decision, 'evaluate', side_effect=AssertionError('unexpected provider')):
            workflow_gate.decision_seam({**payload, 'hook_event_name': 'PreToolUse', 'tool_name': 'Bash'}, {})

    def context(self):
        import workflow_audit
        value = {'request_id': 'request', 'request_seq': 1, 'plan_revision': 1, 'confirmed_request': 1,
                 'plan_mode': 'execution', 'active_todo_id': 'task',
                 'todos': {'task': {'status': 'in_progress', 'required': True}}}
        with workflow_audit.context_db() as db:
            workflow_audit.save(db, 'session', value)
        return value

    def test_read_revises_evidence_and_retries_same_denied_edit(self):
        self.context()
        payload = {'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                   'tool_name': 'Edit', 'tool_input': {'file_path': 'a'}}
        actions = iter(['gather_context', 'execute'])
        def provider(context, router, db, delegate):
            result = self.provider(context, router, db, delegate)
            result['answers']['recommendation']['choice'] = next(actions)
            return result
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), \
             patch.object(decision.advice, 'advise', side_effect=provider) as api:
            with self.assertRaisesRegex(ValueError, 'gather_context'):
                workflow_gate.authorize(payload)
            with self.assertRaisesRegex(ValueError, 'gather_context'):
                workflow_gate.authorize(payload)
            self.assertEqual(api.call_count, 1)
            workflow_gate.authorize({**payload, 'tool_name': 'Read', 'tool_input': {'file_path': 'a'}})
            workflow_gate.authorize(payload)
            self.assertEqual(api.call_count, 2)

    def test_dispatch_execution_requires_todo_and_create_breaks_bootstrap_cycle(self):
        self.config['dispatch']['enabled'] = True
        project = str(Path(__file__).resolve().parent.parent)
        payload = {'cwd': project, 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                   'tool_name': 'Bash'}
        for subcommand in ('create', 'run', 'next', 'integrate', 'bind', 'complete'):
            command = {**payload, 'tool_input': {'command': 'scripts/harness dispatch ' + subcommand}}
            self.assertFalse(workflow_gate.management_command(command))
            with self.assertRaisesRegex(ValueError, 'complete todo plan'):
                workflow_gate.authorize(command)
        self.context()
        create = {**payload, 'tool_input': {'command': 'scripts/harness dispatch create --plan task.json'}}
        with patch.object(decision, 'evaluate', side_effect=AssertionError('bootstrap must not route')):
            workflow_gate.authorize(create)
        with patch.dict(sys.modules, role_dispatch=types.SimpleNamespace(session_has_plan=lambda *args: False)):
            with self.assertRaisesRegex(ValueError, 'dispatch create'):
                workflow_gate.authorize({**payload, 'tool_input': {'command': 'scripts/harness dispatch run --run fixture'}})

    def test_independent_dispatch_and_decision_toggles(self):
        import io
        from contextlib import redirect_stdout
        payload = {'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                   'tool_name': 'Edit', 'tool_input': {'file_path': 'a'}}
        self.config['typesafe']['enforce_decisions'] = False
        self.config['dispatch']['enabled'] = True
        with patch.dict(sys.modules, role_dispatch=types.SimpleNamespace(session_has_plan=lambda *args: False)), \
             patch.object(decision, 'evaluate', side_effect=AssertionError('dispatch-only must not call provider')):
            with self.assertRaisesRegex(ValueError, 'Dispatch plan required'):
                workflow_gate.decision_seam(payload, {'request_id': 'r1'})
            output = io.StringIO()
            with redirect_stdout(output):
                workflow_gate.decision_seam({'hook_event_name': 'UserPromptSubmit'}, {})
            self.assertIn('Dispatch policy', output.getvalue())
            self.assertNotIn('Decision policy', output.getvalue())
        with patch.dict(sys.modules, role_dispatch=types.SimpleNamespace(session_has_plan=lambda *args: True)), \
             patch.object(decision, 'evaluate', side_effect=AssertionError('dispatch-only must not call provider')):
            workflow_gate.decision_seam(payload, {'request_id': 'r1'})
        self.config['dispatch']['enabled'] = False
        with patch.object(decision, 'evaluate', side_effect=AssertionError('both disabled')):
            workflow_gate.decision_seam(payload, {})
        self.config['typesafe']['enforce_decisions'] = True
        with patch.object(decision, 'evaluate', return_value={'action': 'gather_context'}) as evaluate:
            with self.assertRaisesRegex(ValueError, 'gather_context'):
                workflow_gate.decision_seam(payload, {'request_id': 'r1'})
            evaluate.assert_called_once()

    def test_dispatch_cancel_remains_available_without_todo(self):
        self.config['dispatch']['enabled'] = True
        payload = {'cwd': str(Path(__file__).resolve().parent.parent), 'session_id': 'session',
                   'hook_event_name': 'PreToolUse', 'tool_name': 'Bash',
                   'tool_input': {'command': 'scripts/harness dispatch cancel --run fixture'}}
        self.assertTrue(workflow_gate.management_command(payload))
        with patch.object(decision, 'evaluate', side_effect=AssertionError('stop must not route')):
            workflow_gate.authorize(payload)

    def test_config_and_bounded_inline_plan_bootstrap(self):
        self.context()
        self.config['typesafe']['enforce_decisions'] = False
        self.config['dispatch']['enabled'] = True
        project = str(Path(__file__).resolve().parent.parent)
        payload = {'cwd': project, 'session_id': 'session', 'hook_event_name': 'PreToolUse', 'tool_name': 'Bash'}
        plan = json.dumps({'workers': [], 'rationale': 'Small task', 'acceptance': ['Required checks']})
        commands = ['scripts/harness config update --section dispatch --values \'{"enabled":false}\'',
                    "scripts/harness dispatch create --plan /dev/stdin <<'JSON'\n" + plan + "\nJSON",
                    "cat > tasks/dispatch-plan.json <<'JSON'\n" + plan + "\nJSON"]
        with patch.dict(sys.modules, role_dispatch=types.SimpleNamespace(session_has_plan=lambda *args: False, validate=lambda *args: [])):
            for command in commands:
                workflow_gate.authorize({**payload, 'tool_input': {'command': command}})
            write = {**payload, 'tool_name': 'Write', 'tool_input': {'file_path': 'tasks/dispatch-plan.json', 'content': plan}}
            workflow_gate.authorize(write)
            with self.assertRaisesRegex(ValueError, 'Dispatch plan required'):
                workflow_gate.authorize({**write, 'tool_input': {'file_path': 'source.py', 'content': plan}})
            with self.assertRaisesRegex(ValueError, 'Dispatch plan required'):
                workflow_gate.authorize({**payload, 'tool_input': {'command': commands[-1] + '\npython3 malicious.py'}})

    def test_role_plan_uses_prompt_project_despite_tool_workdir(self):
        self.config['typesafe']['enforce_decisions'] = False
        self.config['dispatch']['enabled'] = True
        projects = []
        def planned(project, *args):
            projects.append(project)
            return True
        payload = {'cwd': '/different/tool/location', 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                   'tool_name': 'Edit', 'tool_input': {'file_path': 'a'}}
        with patch.dict(sys.modules, role_dispatch=types.SimpleNamespace(session_has_plan=planned)):
            workflow_gate.decision_seam(payload, {'orchestration_project': str(self.root), 'request_id': 'r'})
        self.assertEqual(projects, [self.root])

    def test_structured_response_needs_no_execution_plan_or_provider(self):
        self.config['dispatch']['enabled'] = True
        payload = {'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                   'tool_name': 'StructuredOutput', 'tool_input': {'status': 'completed', 'evidence': 'Read report'}}
        with patch.object(decision, 'evaluate', side_effect=AssertionError('formatting must not route')):
            workflow_gate.authorize(payload)
        import workflow_audit
        with workflow_audit.context_db() as db:
            self.assertNotIn('evidence_revision', workflow_audit.load(db, 'session'))
        for tool in ('Edit', 'Bash', 'StructuredOutputAction'):
            with self.assertRaisesRegex(ValueError, 'complete todo plan'):
                workflow_gate.authorize({**payload, 'tool_name': tool})

    def test_dispatch_gate_requires_plan(self):
        self.config['dispatch']['enabled'] = True
        payload = {'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                   'tool_name': 'Edit', 'tool_input': {'file_path': 'a'}}
        with patch.dict(sys.modules, role_dispatch=types.SimpleNamespace(session_has_plan=lambda *args: False)):
            with self.assertRaisesRegex(ValueError, 'Dispatch plan required'):
                workflow_gate.decision_seam(payload, {'request_id': 'r1'})


if __name__ == '__main__':
    unittest.main()
