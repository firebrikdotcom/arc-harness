#!/usr/bin/env python3
"""Automatic Jev decision moments; no provider credentials or requests."""
import copy
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'scripts'))
import decision_checkpoint as decision
import jev_moments
import workflow_audit as audit
import workflow_gate

PRIVATE = ('rotate the key for jane@example.com', 'src/billing/charge.php')


class Moments(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.env = patch.dict(os.environ, HARNESS_DB_ROOT=str(self.root), HARNESS_JEV_CHECKPOINTS='1',
                              HARNESS_WORKFLOW_STATE=str(self.root / 'workflow.sqlite'))
        self.env.start()
        self.config = {'typesafe': {'mode': 'active', 'model': 'jev-1.13.0', 'decision_ttl_seconds': 300,
                                   'checkpoints': True}, 'dispatch': {'enabled': False}, 'telemetry': {'enabled': False}}
        self.patches = [patch('harness_config.load', side_effect=lambda: copy.deepcopy(self.config)),
                        patch.object(decision, 'emit'),
                        patch.dict(sys.modules, orchestration_common=types.SimpleNamespace(db_root=lambda project: self.root)),
                        patch.object(decision.advice, 'load_router', return_value=object()),
                        patch.object(decision.advice, 'validate_context'),
                        patch.object(decision.advice, 'advise', side_effect=self.provider)]
        for item in self.patches:
            item.start()
        self.context = {'request_id': 'r1', 'request_seq': 3, 'plan_revision': 1,
                        'task_summary': {'description': 'Fix the parser'},
                        'todos': {'a': {'id': 'a', 'description': 'Write the failing test', 'status': 'pending'},
                                  'b': {'id': 'b', 'description': 'Change the parser', 'status': 'pending'}}}
        with audit.context_db() as db:
            audit.save(db, 'session', self.context)
        self.sent = []
        self.answer = None

    def tearDown(self):
        for item in reversed(self.patches):
            item.stop()
        self.env.stop()
        self.temp.cleanup()

    def provider(self, context, router, db, delegate):
        self.sent.append(context)
        choice, confidence = self.answer
        path = self.root / 'advice.json'
        path.write_text(json.dumps({'usage': {}}))
        return dict(call_id='id', status='evaluated', latency_ms=1, fallback_reason=None, confidence=confidence,
                    answers={'recommendation': {'choice': choice, 'confidence': confidence}}, record_path=str(path))

    def origin(self):
        return self.sent[-1]['checkpoint']['interaction_origin'], self.sent[-1]['checkpoint']['family']

    def test_prompt_effort_shares_only_the_prompt_size(self):
        self.answer = ('think_deeper', 0.9)
        line = jev_moments.prompt_effort(self.root, 'session', PRIVATE[0] + ' in ' + PRIVATE[1])
        self.assertEqual(line, 'Jev effort for this prompt: think_deeper. ' + jev_moments.EFFORT['think_deeper'])
        sent = json.dumps(self.sent)
        self.assertIn('New prompt size: 7 words', sent)
        self.assertIn('File paths or file names mentioned: 1', sent)
        for private in PRIVATE + ('jane', 'charge'):
            self.assertNotIn(private, sent)
        self.assertEqual(self.origin(), ('prompt_routing', 'reasoning_allocation'))
        self.assertEqual(self.sent[-1]['checkpoint']['context_version'], 'prompt-routing-2')

    def test_prompt_shape_separates_questions_instructions_and_scope(self):
        def shape(prompt):
            return dict(fact.split(': ', 1) for fact in jev_moments.prompt_shape(prompt))
        question = shape('why is the parser slow?')
        self.assertEqual((question['Prompt form'], question['Separate asks (list items or sentences)'],
                          question['Opens by referring back to earlier conversation']), ('question', '1', 'no'))
        follow_up = shape('do all of those, commit and push')
        self.assertEqual((follow_up['Prompt form'], follow_up['Opens by referring back to earlier conversation']),
                         ('instruction', 'yes'))
        listed = shape('Fix these:\n- scripts/a.py crashes\n- README.md is stale\n- add a test')
        self.assertEqual((listed['Separate asks (list items or sentences)'],
                          listed['File paths or file names mentioned'], listed['Contains pasted text or code']),
                         ('3', '2', 'no'))
        self.assertEqual(shape('see\n```\nx = 1\n```')['Contains pasted text or code'], 'yes')
        self.assertEqual(shape(None)['New prompt size'], '0 words (text not shared)')

    def test_low_confidence_and_disabled_jev_stay_silent(self):
        self.answer = ('direct', 0.5)
        self.assertIsNone(jev_moments.prompt_effort(self.root, 'session', 'hello'))
        calls = len(self.sent)
        for mode, flag in (('shadow', '1'), ('active', '0')):
            self.config['typesafe']['mode'] = mode
            with patch.dict(os.environ, HARNESS_JEV_CHECKPOINTS=flag):
                self.assertIsNone(jev_moments.verify_failed(self.root, 'session', 4, 1))
        self.assertEqual(len(self.sent), calls)

    def test_plan_first_step_names_the_todo(self):
        self.answer = ('step_2', 0.8)
        context = audit.load(audit.context_db(), 'session')
        self.assertEqual(jev_moments.plan_first_step(self.root, 'session', context, 'Repair parsing'),
                         'Jev suggests starting with b: Change the parser')
        self.assertEqual(self.origin(), ('plan_prioritization', 'prioritization'))
        single = {**context, 'todos': {'a': context['todos']['a']}}
        calls = len(self.sent)
        self.assertIsNone(jev_moments.plan_first_step(self.root, 'session', single, 'One step'))
        self.assertEqual(len(self.sent), calls)

    def test_verify_failure_and_search_choices(self):
        self.answer = ('investigate', 0.75)
        self.assertEqual(jev_moments.verify_failed(self.root, 'session', 5, 2),
                         'Jev next step: investigate. ' + jev_moments.NEXT_STEPS['investigate'])
        self.assertEqual(self.origin(), ('verify_followup', 'progress_assessment'))
        self.answer = ('targeted_search', 0.9)
        self.assertEqual(jev_moments.search_strategy(self.root, 'session'), 'targeted_search')
        self.assertEqual(self.origin(), ('search_selection', 'evidence_selection'))

    def test_review_grading_lists_only_disagreements_and_hides_paths_and_evidence(self):
        findings = {'findings': [
            {'severity': 'minor', 'file': PRIVATE[1], 'line': 4, 'claim': 'Charge is applied twice', 'evidence': 'secret evidence body'},
            {'severity': 'major', 'file': PRIVATE[1], 'claim': 'Missing null check', 'evidence': ''},
            {'severity': 'minor', 'file': PRIVATE[1], 'claim': 'Fixed already', 'evidence': 'x', 'status': 'fixed'}]}
        self.answer = ('major', 0.9)
        self.assertEqual(jev_moments.grade_findings(self.root, 'review-run', findings),
                         ['Jev grades finding 1 as major (reviewer: minor).'])
        self.assertEqual(len(self.sent), 2)
        sent = json.dumps(self.sent)
        for private in (PRIVATE[1], 'secret evidence body'):
            self.assertNotIn(private, sent)

    def test_stop_records_handoff_silently_and_never_fails(self):
        self.answer = ('keep_working', 0.9)
        out = io.StringIO()
        payload = json.dumps({'hook_event_name': 'Stop', 'session_id': 'session', 'cwd': str(self.root)})
        with patch.object(workflow_gate.hook_input, 'read', return_value=payload), patch.object(sys, 'stdout', out):
            self.assertEqual(workflow_gate.main(), 0)
        self.assertEqual(out.getvalue(), '{}\n')
        self.assertEqual(self.origin(), ('handoff_check', 'handoff_assessment'))
        self.assertEqual(self.sent[-1]['checkpoint']['context_version'], 'handoff-check-2')
        self.assertIn('Final reply ends with a question to the user: unknown', json.dumps(self.sent[-1]))
        with patch.object(jev_moments, 'record_handoff', side_effect=RuntimeError('store down')):
            workflow_gate.record_handoff({'session_id': 'session', 'cwd': str(self.root)})

    def test_handoff_facts_count_changes_and_read_the_final_reply_locally(self):
        project = self.root / 'repo'
        project.mkdir()
        subprocess.run(['git', 'init', '-q', str(project)], check=True)
        (project / 'a.txt').write_text('a')
        (project / 'b.txt').write_text('b')
        transcript = self.root / 'session.jsonl'
        reply = 'I changed ' + PRIVATE[1] + '. Should I push it?'
        transcript.write_text('\n'.join(json.dumps(entry) for entry in (
            {'type': 'assistant', 'message': {'content': [{'type': 'text', 'text': 'earlier'}]}},
            {'type': 'assistant', 'message': {'content': [{'type': 'text', 'text': reply}]}},
            {'type': 'user', 'message': {'content': 'tool result'}})) + '\n')
        self.answer = ('ask_user', 0.9)
        jev_moments.record_handoff(project, 'session', {'transcript_path': str(transcript)})
        sent = json.dumps(self.sent[-1])
        self.assertIn('Uncommitted changed files: 2', sent)
        self.assertIn('Final reply ends with a question to the user: yes', sent)
        self.assertNotIn(PRIVATE[1], sent)
        facts = jev_moments.handoff_facts(self.root / 'missing', {}, {'last_assistant_message': 'Done.'})
        self.assertIn('Uncommitted changed files: unknown', facts)
        self.assertIn('Final reply ends with a question to the user: no', facts)

    def test_prompt_advice_is_printed_once_and_failures_are_silent(self):
        self.answer = ('direct', 0.95)
        out = io.StringIO()
        with patch.object(sys, 'stdout', out):
            workflow_gate.advise_prompt({'session_id': 'session', 'cwd': str(self.root), 'prompt': 'rename a variable'})
            with patch.object(jev_moments, 'prompt_effort', side_effect=RuntimeError('down')):
                workflow_gate.advise_prompt({'session_id': 'session', 'cwd': str(self.root), 'prompt': 'x'})
        self.assertEqual(out.getvalue(), 'Jev effort for this prompt: direct. ' + jev_moments.EFFORT['direct'] + '\n')

    def test_cli_prints_nothing_when_jev_is_unavailable(self):
        out = io.StringIO()
        with patch.object(jev_moments, 'verify_failed', side_effect=RuntimeError('down')), patch.object(sys, 'stdout', out):
            self.assertEqual(jev_moments.main(['verify-failed', '--project', str(self.root), '--session-id', 'session']), 0)
        self.assertEqual(out.getvalue(), '')


if __name__ == '__main__':
    unittest.main()
