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
        self.root = Path(self.temp.name).resolve()
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
                         scope={'request_id': 'r1', 'action_hash': 'abc'}, state={'revision': 1})

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
                    answers={'recommendation': {'choice': 'read', 'confidence': 0.9}}, confidence=0.9, record_path=str(path))

    def evaluate(self, **changes):
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), \
             patch.object(decision.advice, 'advise', side_effect=self.provider) as api:
            result = decision.evaluate(**{**self.args, **changes})
            return result, api.call_count

    def test_modes_and_deterministic_bypass(self):
        for mode, action, count in [('off', 'normal_reasoning', 0), ('shadow', 'normal_reasoning', 1), ('active', 'read', 1)]:
            self.config['typesafe']['mode'] = mode
            row, calls = self.evaluate()
            self.assertEqual((row['action'], calls), (action, count))
        # A deterministic bypass names its own action; nothing else uses a baseline.
        row, calls = self.evaluate(bypass='required_check', baseline='execute')
        self.assertEqual((row['status'], row['action'], row['action_source'], calls), ('bypassed', 'execute', 'deterministic', 0))

    def jev(self, choice, confidence):
        def provider(context, router, db, delegate):
            result = self.provider(context, router, db, delegate)
            result['answers']['recommendation'] = {'choice': choice, 'confidence': confidence}
            result['confidence'] = confidence
            return result
        return patch.object(decision.advice, 'advise', side_effect=provider)

    def test_confidence_threshold_decides_who_chooses(self):
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'):
            for confidence, action, needs in ((0.7, 'read', False), (0.69, 'normal_reasoning', True), (0.0, 'normal_reasoning', True)):
                with self.jev('read', confidence):
                    row = decision.evaluate(**{**self.args, 'state': {'revision': confidence}})
                self.assertEqual((row['action'], row['needs_reasoning'], row['confidence']), (action, needs, confidence))
                if needs:
                    self.assertEqual((row['status'], row['escalation_reason']), ('fallback', 'low_confidence'))
            self.config['typesafe']['min_confidence'] = 0.5
            with self.jev('read', 0.6):
                row = decision.evaluate(**{**self.args, 'state': {'revision': 'custom'}})
            self.assertEqual((row['action'], row['action_source'], row['min_confidence']), ('normal_reasoning', 'escalation', 0.7))

    def automatic_args(self):
        return {**self.args, 'origin':'automatic_tool', 'context_version':'tool-context-2',
                'choices':{'execute':'Execute', 'gather_context':'Read prerequisite', 'insufficient_information':'Supply missing findings'},
                'state':{'active_todo_id':'step', 'task_id':'task', 'evidence_revision':0}}

    def test_two_context_requests_share_budget_across_choices_and_revisions(self):
        args = self.automatic_args()
        with patch.object(decision.advice, 'load_router', return_value=object()), patch.object(decision.advice, 'validate_context'):
            with self.jev('gather_context', 0.9):
                first = decision.evaluate(**args)
            revised = {**args, 'scope':{**args['scope'], 'plan_revision':2, 'action_hash':'changed'},
                       'state':{**args['state'], 'evidence_revision':1}, 'facts':['Additional evidence supplied']}
            with self.jev('insufficient_information', 0.95):
                second = decision.evaluate(**revised)
            with patch.object(decision.advice, 'advise', side_effect=AssertionError('No third provider call')):
                third = decision.evaluate(**{**revised, 'state':{**revised['state'], 'evidence_revision':2}})
        self.assertEqual((first['context_requests'], second['context_requests'], third['context_requests']), (1,2,2))
        self.assertEqual((third['action'],third['needs_reasoning'],third['fallback_reason']), ('normal_reasoning',True,'context_request_limit'))
        budget = list((self.root/'decisions'/'context-rounds').glob('*.json'))
        self.assertEqual(len(budget),1)
        self.assertEqual(json.loads(budget[0].read_text()), {'count':2})
        self.assertEqual(budget[0].stat().st_mode & 0o777, 0o600)

    def test_cached_context_denials_are_bounded_and_new_prompt_or_step_resets(self):
        args = self.automatic_args()
        with patch.object(decision.advice, 'load_router', return_value=object()), patch.object(decision.advice, 'validate_context'), self.jev('gather_context', 0.9) as api:
            rows = [decision.evaluate(**args) for _ in range(4)]
            self.assertEqual(api.call_count,1)
            self.assertEqual([r['action'] for r in rows], ['gather_context','gather_context','normal_reasoning','normal_reasoning'])
            for changed in ({'scope':{**args['scope'],'request_id':'new'}}, {'state':{**args['state'],'active_todo_id':'other'}}, {'state':{**args['state'],'task_id':'other'}}):
                reset = decision.evaluate(**{**args, **changed})
                self.assertEqual(reset['context_requests'],1)

    def test_low_confidence_does_not_request_context_or_consume_budget(self):
        args = self.automatic_args()
        with patch.object(decision.advice, 'load_router', return_value=object()), patch.object(decision.advice, 'validate_context'):
            for confidence in (0.0,0.6999):
                with self.jev('gather_context', confidence):
                    row = decision.evaluate(**{**args,'facts':[str(confidence)]})
                self.assertTrue(row['needs_reasoning'])
                self.assertEqual(row['context_requests'],0)
            with self.jev('insufficient_information',0.7):
                row = decision.evaluate(**args)
                self.assertEqual((row['action'],row['context_requests']),('insufficient_information',1))

    def test_two_high_confidence_hook_denials_then_agent_receipt(self):
        self.context()
        with patch.object(decision.advice, 'load_router', return_value=object()), patch.object(decision.advice, 'validate_context'), self.jev('insufficient_information',0.99) as api:
            for number in (1,2):
                with self.assertRaisesRegex(ValueError, f'context request {number}/2'):
                    workflow_gate.authorize(self.gate_payload())
            workflow_gate.authorize(self.gate_payload())
            self.assertEqual(api.call_count,1)
        [accepted] = [row for row in self.receipts() if row.get('claimed_at')]
        self.assertEqual((accepted['action_source'],accepted['fallback_reason']),('agent_after_escalation','context_request_limit'))
        self.assertEqual(accepted['action'],'execute')

    def test_concurrent_context_requests_do_not_exceed_two(self):
        from concurrent.futures import ThreadPoolExecutor
        args = self.automatic_args()
        with patch.object(decision.advice, 'load_router', return_value=object()), patch.object(decision.advice, 'validate_context'), self.jev('gather_context',0.99) as api:
            with ThreadPoolExecutor(max_workers=4) as pool:
                rows = list(pool.map(lambda n: decision.evaluate(**{**args,'facts':[f'Action {n}']}),range(4)))
            self.assertEqual(api.call_count,2)
        self.assertEqual(sum(row['action']=='gather_context' for row in rows),2)
        self.assertEqual(sum(row.get('fallback_reason')=='context_request_limit' for row in rows),2)

    def test_context_budget_never_waives_todo_gate(self):
        self.context()
        with patch.object(decision.advice, 'load_router', return_value=object()), patch.object(decision.advice, 'validate_context'), self.jev('gather_context',0.99):
            for _ in range(2):
                with self.assertRaises(ValueError):
                    workflow_gate.authorize(self.gate_payload())
        import workflow_audit
        with workflow_audit.context_db() as db:
            current = workflow_audit.load(db,'session');current.pop('confirmed_request');workflow_audit.save(db,'session',current)
        with self.assertRaisesRegex(ValueError,'complete todo plan'):
            workflow_gate.authorize(self.gate_payload())

    def test_cache_prefers_a_resolved_receipt_over_open_escalations(self):
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), self.jev('read', 0.4) as api:
            first = decision.evaluate(**self.args)
            copies = [decision.evaluate(**self.args) for _ in range(3)]
            self.assertEqual(api.call_count, 1)
            self.assertTrue(all(row['needs_reasoning'] for row in [first, *copies]))
            decision.resolve(project=self.root, identifier=copies[1]['id'], session_id='session', action='execute',
                             reason='main agent decided', scope=self.args['scope'], state=self.args['state'])
            for _ in range(5):
                cached = decision.evaluate(**self.args)
                self.assertEqual((cached['action'], cached['needs_reasoning'], cached['action_source']),
                                 ('execute', False, 'agent_after_escalation'))
            with self.assertRaisesRegex(ValueError, 'original bounded choices'):
                decision.resolve(project=self.root, identifier=first['id'], session_id='session', action='delete',
                                 reason='not offered')

    def test_context_request_limit_zero_lets_the_agent_decide_at_once(self):
        self.config['typesafe']['context_request_limit'] = 0
        args = self.automatic_args()
        with patch.object(decision.advice, 'load_router', return_value=object()), patch.object(decision.advice, 'validate_context'), self.jev('gather_context', 0.99) as api:
            row = decision.evaluate(**args)
        # With no budget there is nothing to ask Jev: no provider call, no block, the agent decides.
        self.assertEqual(api.call_count, 0)
        self.assertEqual((row['action'], row['needs_reasoning'], row['fallback_reason'], row['context_requests']),
                         ('normal_reasoning', True, 'context_request_limit', 0))
        # An out-of-range or missing value falls back to the fixed maximum of two.
        self.config['typesafe']['context_request_limit'] = 9
        self.assertEqual(decision.context_request_limit(), 2)
        del self.config['typesafe']['context_request_limit']
        self.assertEqual(decision.context_request_limit(), 2)

    def test_configured_read_only_commands_and_tools_skip_the_decision_seam(self):
        self.config['typesafe']['read_only_commands'] = ['career-scheduler phone text', 'gog gmail search']
        self.config['typesafe']['read_only_tools'] = ['search_threads']
        self.context()
        shell = lambda command: {**self.gate_payload(), 'tool_name': 'Bash', 'tool_input': {'command': command}}
        with patch.object(decision, 'evaluate', side_effect=AssertionError('reads never reach the checkpoint')):
            workflow_gate.decision_seam(shell('/opt/career/bin/career-scheduler phone text --max 2000'), {'request_id': 'r1'})
            workflow_gate.decision_seam(shell('gog gmail search "from:greenhouse" | head'), {'request_id': 'r1'})
            workflow_gate.decision_seam({**self.gate_payload(), 'tool_name': 'mcp__gmail__search_threads', 'tool_input': {'query': 'x'}}, {'request_id': 'r1'})
            workflow_gate.decision_seam({**self.gate_payload(), 'tool_name': 'search_threads', 'tool_input': {}}, {'request_id': 'r1'})
        # A prefix is not a license for its siblings: submit and fill still go through the checkpoint.
        with patch.object(decision, 'evaluate', return_value={'action': 'gather_context'}):
            for command in ('career-scheduler phone submit --confirm', 'career-scheduler phone fill', 'gog gmail send --to a@b.c'):
                with self.assertRaisesRegex(ValueError, 'gather_context'):
                    workflow_gate.decision_seam(shell(command), {'request_id': 'r1'})
        # Declared reads revise the evidence counter like built-in reads, without needing an active todo.
        import workflow_audit
        with workflow_audit.context_db() as db:
            before = workflow_audit.load(db, 'session').get('evidence_revision', 0)
        with patch.object(decision, 'evaluate', side_effect=AssertionError('no checkpoint for a read')):
            workflow_gate.authorize(shell('career-scheduler phone text'))
        with workflow_audit.context_db() as db:
            self.assertEqual(workflow_audit.load(db, 'session').get('evidence_revision', 0), before + 1)

    def gate_payload(self):
        return {'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                'tool_name': 'Edit', 'tool_input': {'file_path': 'a'}}

    def receipts(self):
        return [json.loads(path.read_text()) for path in (self.root / 'decisions').glob('*.json')]

    def test_gate_below_the_threshold_lets_the_main_agent_run_its_call(self):
        self.context()
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), self.jev('gather_context', 0.69):
            workflow_gate.authorize(self.gate_payload())
        [receipt] = self.receipts()
        self.assertEqual((receipt['status'], receipt['action'], receipt['action_source'], receipt['escalation_reason']),
                         ('resolved', 'execute', 'agent_after_escalation', 'low_confidence'))
        self.assertTrue(receipt['claimed_at'])

    def test_gate_at_the_threshold_follows_jev(self):
        self.context()
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), self.jev('gather_context', 0.7):
            with self.assertRaisesRegex(ValueError, 'gather_context'):
                workflow_gate.authorize(self.gate_payload())
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), self.jev('execute', 0.95):
            workflow_gate.authorize({**self.gate_payload(), 'tool_input': {'file_path': 'b'}})
        accepted = [r for r in self.receipts() if r['action_source'] == 'jev' and r['action'] == 'execute']
        self.assertEqual(len(accepted), 1)
        self.assertTrue(accepted[0]['claimed_at'])

    def test_gate_runs_the_agent_call_when_jev_is_unavailable(self):
        self.context()
        with patch.object(decision.advice, 'load_router', side_effect=OSError):
            workflow_gate.authorize(self.gate_payload())
        [receipt] = self.receipts()
        self.assertEqual((receipt['fallback_reason'], receipt['action'], receipt['status']), ('OSError', 'execute', 'resolved'))

    def test_shadow_and_off_modes_leave_the_gate_to_the_main_agent(self):
        for mode in ('shadow', 'off'):
            self.config['typesafe']['mode'] = mode
            self.context()
            with patch.object(decision.advice, 'load_router', return_value=object()), \
                 patch.object(decision.advice, 'validate_context'), self.jev('gather_context', 0.99):
                workflow_gate.authorize({**self.gate_payload(), 'tool_input': {'file_path': mode}})
        self.assertEqual({r['action'] for r in self.receipts()}, {'execute'})

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
            self.assertEqual(row['action'], 'normal_reasoning')
            self.assertTrue(row['needs_reasoning'])

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

    def test_insufficient_information_requests_context_without_consuming(self):
        with patch.object(decision, 'evaluate', return_value={'action':'insufficient_information'}), patch.object(decision, 'claim') as consume:
            with self.assertRaisesRegex(ValueError, 'todo context --action-summary'):
                workflow_gate.decision_seam(self.gate_payload(), {'request_id':'r1'})
            consume.assert_not_called()

    def test_hook_passes_action_specific_context_and_audit_category(self):
        context = {'request_id':'r1', 'task_summary':{'description':'Improve routing context'},
                   'active_todo_id':'edit', 'todos':{'edit':{'description':'Add category fields', 'criterion':'Categorized reports render', 'status':'in_progress'}}}
        payload = {**self.gate_payload(), 'tool_input':{'file_path':'private/report.rs','old_string':'PRIVATE SOURCE','new_string':'PRIVATE CHANGE'}}
        with patch.object(decision, 'evaluate', return_value={'id':'receipt','action':'execute'}) as evaluate, patch.object(decision, 'claim'):
            workflow_gate.decision_seam(payload, context)
        sent = evaluate.call_args.kwargs
        self.assertEqual((sent['origin'], sent['context_version']), ('automatic_tool','tool-context-2'))
        text = json.dumps(sent['facts'])
        self.assertIn('Improve routing context', text)
        self.assertIn('Add category fields', text)
        self.assertIn('update Rust logic', text)
        for private in ('PRIVATE SOURCE','PRIVATE CHANGE','private/report.rs'):
            self.assertNotIn(private, text)

    def test_origin_and_context_version_invalidate_cached_decisions(self):
        first, _ = self.evaluate()
        second, calls = self.evaluate(origin='automatic_tool', context_version='tool-context-2')
        self.assertEqual(calls, 1)
        self.assertNotEqual(first['cache_key'], second['cache_key'])
        self.assertEqual(second['interaction_origin'], 'automatic_tool')

    def test_read_observation_without_a_session_is_not_persisted(self):
        with patch.object(workflow_gate.audit, 'context_db') as database:
            workflow_gate.decision_posttool({'tool_name':'Read','tool_response':{'content':'PRIVATE'}})
        database.assert_not_called()

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
            self.assertEqual(api.call_count, 1)
            accepted = [r for r in self.receipts() if r.get('claimed_at')]
            self.assertEqual(accepted[-1]['fallback_reason'], 'context_request_limit')

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
        # The denial carries the one-step main-only plan the gate lets through.
        with patch.dict(sys.modules, role_dispatch=types.SimpleNamespace(session_has_plan=lambda *args: False)):
            with self.assertRaises(ValueError) as denied:
                workflow_gate.decision_seam(payload, {'request_id': 'r1'})
        self.assertIn('dispatch create --session-id session --plan /dev/stdin', str(denied.exception))

    def prompt(self, text):
        import io
        from contextlib import redirect_stdout
        output = io.StringIO()
        with redirect_stdout(output):
            workflow_gate.authorize({'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'UserPromptSubmit', 'prompt': text})
        return output.getvalue()

    def stored(self):
        import workflow_audit
        with workflow_audit.context_db() as db:
            return workflow_audit.load(db, 'session')

    NOTICE = ('<task-notification>\n<task-id>b1</task-id>\n<status>completed</status>\n'
              '<summary>Background command "Run tests" completed (exit code 0)</summary>\n</task-notification>')

    def test_background_notice_keeps_the_current_prompt_and_its_plan(self):
        self.config['typesafe']['enforce_decisions'] = False
        self.context()
        edit = {'cwd': str(self.root), 'session_id': 'session', 'hook_event_name': 'PreToolUse',
                'tool_name': 'Edit', 'tool_input': {'file_path': 'a'}}
        before = self.stored()
        for notice in (self.NOTICE, '<system-reminder>note</system-reminder>\n' + self.NOTICE + '\n'):
            output = self.prompt(notice)
            self.assertIn('background notice', output)
            self.assertNotIn('Dispatch policy', output)
            after = self.stored()
            self.assertEqual((after['request_id'], after['request_seq'], after['confirmed_request']),
                             (before['request_id'], before['request_seq'], before['confirmed_request']))
            workflow_gate.authorize(edit)
        # Words from the person, even beside a notice, are a new prompt that needs its plan confirmed.
        for text in ('continue', self.NOTICE + '\nnow fix the other test'):
            self.assertIn('Todo gate: confirm', self.prompt(text))
            self.assertNotEqual(self.stored()['request_id'], before['request_id'])
            with self.assertRaisesRegex(ValueError, 'complete todo plan') as denied:
                workflow_gate.authorize(edit)
            self.assertIn('harness workflow todo confirm --reason', str(denied.exception))
            self.assertIn('harness workflow todo exempt --reason', str(denied.exception))
            self.context()
            before = self.stored()

    def test_select_active_todo_names_the_open_ids(self):
        from workflow_todos import TodoValidationError, assert_plan
        context = {'request_seq': 1, 'confirmed_request': 1, 'plan_mode': 'execution', 'active_todo_id': None,
                   'todos': {'done': {'status': 'completed', 'required': True}, 'next': {'status': 'pending', 'required': True},
                             'bad\x1bid': {'status': 'pending', 'required': True}}}
        with self.assertRaises(TodoValidationError) as denied:
            assert_plan(context, active=True)
        self.assertIn('(open: next): harness workflow todo update --id next --status in_progress', str(denied.exception))
        self.assertNotIn('\x1b', str(denied.exception))


if __name__ == '__main__':
    unittest.main()
