#!/usr/bin/env python3
"""Jev next-step advice after a failed command; no provider credentials or requests."""
import copy
import io
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
import failure_budget
import workflow_audit as audit

SECRET_COMMAND = 'make test TOKEN=do-not-send'
SECRET_ERROR = 'FAIL test_parse: customer jane@example.com rejected'


class FailureFollowup(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.env = patch.dict(os.environ, HARNESS_DB_ROOT=str(self.root), HARNESS_JEV_CHECKPOINTS='1',
                              HARNESS_WORKFLOW_STATE=str(self.root / 'workflow.sqlite'))
        self.env.start()
        self.config = {'typesafe': {'mode': 'active', 'model': 'jev-1.13.0', 'decision_ttl_seconds': 300,
                                   'checkpoints': True}, 'dispatch': {'enabled': False}, 'telemetry': {'enabled': False}}
        self.settings = patch('harness_config.load', side_effect=lambda: copy.deepcopy(self.config))
        self.settings.start()
        self.telemetry = patch.object(decision, 'emit')
        self.telemetry.start()
        self.db = patch.dict(sys.modules, orchestration_common=types.SimpleNamespace(db_root=lambda project: self.root))
        self.db.start()
        with audit.context_db() as db:
            audit.save(db, 'session', {'request_id': 'r1', 'task_summary': {'description': 'Fix the parser'}})
        self.sent = []
        self.answer = ('fix', 0.9)

    def tearDown(self):
        for patcher in (self.db, self.telemetry, self.settings, self.env):
            patcher.stop()
        self.temp.cleanup()

    def provider(self, context, router, db, delegate):
        self.sent.append(context)
        choice, confidence = self.answer
        path = self.root / 'advice.json'
        path.write_text(json.dumps({'usage': {}}))
        return dict(call_id='id', status='evaluated', latency_ms=1, fallback_reason=None, confidence=confidence,
                    answers={'recommendation': {'choice': choice, 'confidence': confidence}}, record_path=str(path))

    def payload(self, command=SECRET_COMMAND):
        return {'hook_event_name': 'PostToolUseFailure', 'tool_name': 'Bash', 'session_id': 'session',
                'tool_input': {'command': command}, 'error': SECRET_ERROR, 'cwd': str(self.root)}

    def advise(self, command=SECRET_COMMAND):
        with patch.object(decision.advice, 'load_router', return_value=object()), \
             patch.object(decision.advice, 'validate_context'), \
             patch.object(decision.advice, 'advise', side_effect=self.provider):
            return failure_budget.advise_next_step(self.payload(command), self.root, 'signature')

    def test_confident_choice_is_returned_without_command_or_error_text(self):
        self.assertEqual(self.advise(), 'fix')
        sent = json.dumps(self.sent)
        self.assertIn('Fix the parser', sent)
        for private in ('do-not-send', 'TOKEN', 'jane@example.com', 'test_parse'):
            self.assertNotIn(private, sent)
        checkpoint = self.sent[0]['checkpoint']
        self.assertEqual((checkpoint['family'], checkpoint['interaction_origin']), ('progress_assessment', 'failure_followup'))

    def test_low_confidence_leaves_the_decision_to_the_agent(self):
        self.answer = ('retry', 0.4)
        self.assertIsNone(self.advise())

    def test_inspection_failures_and_disabled_jev_make_no_call(self):
        self.assertIsNone(self.advise('grep -rn missing .'))
        self.config['typesafe']['mode'] = 'shadow'
        self.assertIsNone(self.advise())
        self.config['typesafe']['mode'] = 'active'
        with patch.dict(os.environ, HARNESS_JEV_CHECKPOINTS='0'):
            self.assertIsNone(self.advise())
        self.assertEqual(self.sent, [])

    def run_hook(self, advice):
        recorded = types.SimpleNamespace(returncode=0, stdout='', stderr='')
        stderr = io.StringIO()
        with patch.object(failure_budget.hook_input, 'read', return_value=json.dumps(self.payload())), \
             patch.object(failure_budget.subprocess, 'run', return_value=recorded), \
             patch.object(failure_budget, 'advise_next_step', side_effect=advice), \
             patch.object(sys, 'stderr', stderr):
            return failure_budget.main(), stderr.getvalue()

    def test_hook_hands_a_confident_step_to_the_agent(self):
        self.assertEqual(self.run_hook(lambda *args: 'fix'), (2, 'Jev next step: fix. ' + failure_budget.NEXT_STEPS['fix'] + '\n'))

    def test_hook_stays_silent_when_advice_is_absent_or_fails(self):
        self.assertEqual(self.run_hook(lambda *args: None), (0, ''))

        def unavailable(*args):
            raise RuntimeError('provider down')
        self.assertEqual(self.run_hook(unavailable), (0, ''))


if __name__ == '__main__':
    unittest.main()
