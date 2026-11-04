"""Native user-prompt receipts for run-bound independent review approval."""
import json
import os
from datetime import datetime, timezone
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

import review_approval as approval
import workflow_gate


class ReviewApprovalTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.db = self.base / "db"
        self.project = self.base / "project"
        self.project.mkdir()
        (self.project / "README.md").write_text("review target\n", encoding="utf-8")
        self.session = "approval-test-session"
        self.run_id = "20261026T161330Z-77835"
        self.findings = self.base / "findings.json"
        self.write_findings()

    def write_findings(self):
        digest = approval.tree_hash(self.project, ROOT)
        self.findings.write_text(json.dumps({
            "tree_hash": digest,
            "reviewer": "independent-test-reviewer",
            "verdict": "approve",
            "findings": [],
        }) + "\n", encoding="utf-8")

    def request(self):
        return approval.request_or_check(
            db_root=self.db,
            session_id=self.session,
            run_id=self.run_id,
            findings=self.findings,
            project=self.project,
            harness_root=ROOT,
        )

    def test_bare_approved_issues_receipt_for_the_pending_request(self):
        self.assertFalse(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="approved",
        ))
        ready, message = self.request()
        self.assertFalse(ready)
        self.assertIn("`approved`", message)
        for prompt in ("not approved", "approved, but fix the tests", "don't approve",
                       "approved unless CI fails", "approved, wait for me", "disapproved",
                       "approval pending"):
            self.assertFalse(approval.capture_prompt(
                harness_root=ROOT, base_db=self.db, project=self.project,
                session_id=self.session, prompt=prompt,
            ))
        self.assertTrue(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="  Approved.\n",
        ))
        self.assertEqual(self.request(), (True, "Native chat approval receipt verified."))

    def test_natural_approval_replies_issue_receipt(self):
        for prompt in ("approved, that should suffice", "Yes, approved", "approve, go ahead",
                       "Approved - ship it", "approved, no more changes needed",
                       "approved, submit the review once, then open the PR after CI"):
            with self.subTest(prompt=prompt):
                state = approval.receipt_path(self.db, self.session)
                if state.exists():
                    state.unlink()
                self.request()
                self.assertTrue(approval.capture_prompt(
                    harness_root=ROOT, base_db=self.db, project=self.project,
                    session_id=self.session, prompt=prompt,
                ))

    def test_reply_from_another_project_approves_the_pending_request(self):
        target_db = self.db / "targets" / "other-worktree" / "db"
        ready, _ = approval.request_or_check(
            db_root=target_db, session_id=self.session, run_id=self.run_id,
            findings=self.findings, project=self.project, harness_root=ROOT,
        )
        self.assertFalse(ready)
        elsewhere = self.base / "elsewhere"
        elsewhere.mkdir()
        self.assertTrue(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=elsewhere,
            session_id=self.session, prompt="approved",
        ))
        ready, message = approval.request_or_check(
            db_root=target_db, session_id=self.session, run_id=self.run_id,
            findings=self.findings, project=self.project, harness_root=ROOT,
        )
        self.assertEqual((ready, message), (True, "Native chat approval receipt verified."))

    def request_with_base(self):
        return approval.request_or_check(
            db_root=self.db, session_id=self.session, run_id=self.run_id,
            findings=self.findings, project=self.project, harness_root=ROOT, base_db=self.db,
        )

    def test_latest_chat_approval_before_submit_needs_no_second_ask(self):
        self.assertFalse(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="approved, go ahead",
        ))
        ready, message = self.request_with_base()
        self.assertTrue(ready, message)
        self.assertEqual(approval.consume(
            db_root=self.db, session_id=self.session, run_id=self.run_id,
            findings=self.findings, project=self.project, harness_root=ROOT,
        ), (True, "Native chat approval receipt consumed."))
        # One approval message covers one submission.
        ready, _ = self.request_with_base()
        self.assertFalse(ready)

    def test_later_non_approval_or_stale_approval_does_not_count(self):
        approval.capture_prompt(harness_root=ROOT, base_db=self.db, project=self.project,
                                session_id=self.session, prompt="approved")
        approval.capture_prompt(harness_root=ROOT, base_db=self.db, project=self.project,
                                session_id=self.session, prompt="actually, change the docs first")
        ready, message = self.request_with_base()
        self.assertFalse(ready)
        self.assertIn("never ask them to run a terminal command", message)
        old = datetime.fromtimestamp(datetime.now(timezone.utc).timestamp() - 3600, timezone.utc).isoformat()
        approval.capture_prompt(harness_root=ROOT, base_db=self.db, project=self.project,
                                session_id=self.session, prompt="approved", prompt_at=old)
        self.assertFalse(self.request_with_base()[0])

    def test_codex_approval_before_submit_is_used_without_second_ask(self):
        sessions = self.base / "sessions"
        rollout = sessions / "2026" / "10" / "26" / ("rollout-1-" + self.session + ".jsonl")
        rollout.parent.mkdir(parents=True)
        rollout.write_text(json.dumps({
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "payload": {"type": "message", "role": "user",
                        "content": [{"type": "input_text", "text": "approved, that should suffice"}]},
        }) + "\n", encoding="utf-8")
        env = patch.dict(os.environ, {
            "HARNESS_DB_ROOT": str(self.db),
            "HARNESS_WORKFLOW_STATE": str(self.base / "workflow.sqlite"),
            "HARNESS_CODEX_SESSIONS": str(sessions),
        })
        env.start()
        self.addCleanup(env.stop)
        with patch.object(workflow_gate, "decision_seam"):
            workflow_gate.authorize({"hook_event_name": "PreToolUse", "session_id": self.session,
                                     "cwd": str(self.project), "tool_name": "Bash"})
        ready, message = self.request_with_base()
        self.assertTrue(ready, message)

    def test_bare_approve_review_issues_receipt(self):
        self.request()
        self.assertTrue(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="approve review",
        ))

    def test_only_native_exact_prompt_issues_one_time_bound_receipt(self):
        ready, message = self.request()
        self.assertFalse(ready)
        self.assertIn("reply `approved` in this chat", message)
        self.assertFalse(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="please approve review " + self.run_id,
        ))
        self.assertFalse(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="approve review another-run",
        ))
        self.assertTrue(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="approve review " + self.run_id,
        ))
        self.assertEqual(self.request(), (True, "Native chat approval receipt verified."))
        self.assertEqual(approval.consume(
            db_root=self.db,
            session_id=self.session,
            run_id=self.run_id,
            findings=self.findings,
            project=self.project,
            harness_root=ROOT,
        ), (True, "Native chat approval receipt consumed."))
        self.assertFalse(approval.consume(
            db_root=self.db,
            session_id=self.session,
            run_id=self.run_id,
            findings=self.findings,
            project=self.project,
            harness_root=ROOT,
        )[0])

    def test_receipt_rejects_changed_findings_or_project_tree(self):
        self.request()
        (self.project / "README.md").write_text("changed after request\n", encoding="utf-8")
        self.assertFalse(approval.capture_prompt(
            harness_root=ROOT, base_db=self.db, project=self.project,
            session_id=self.session, prompt="approve review " + self.run_id,
        ))

    def test_codex_native_user_message_reaches_receipt_capture(self):
        self.request()
        sessions = self.base / "sessions"
        rollout = sessions / "2026" / "10" / "26" / ("rollout-1-" + self.session + ".jsonl")
        rollout.parent.mkdir(parents=True)
        rollout.write_text(json.dumps({
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "approve review " + self.run_id}],
            },
        }) + "\n", encoding="utf-8")
        env = patch.dict(os.environ, {
            "HARNESS_DB_ROOT": str(self.db),
            "HARNESS_WORKFLOW_STATE": str(self.base / "workflow.sqlite"),
            "HARNESS_CODEX_SESSIONS": str(sessions),
        })
        env.start()
        self.addCleanup(env.stop)
        with patch.object(workflow_gate, "decision_seam"):
            workflow_gate.authorize({
                "hook_event_name": "PreToolUse",
                "session_id": self.session,
                "cwd": str(self.project),
                "tool_name": "Read",
            })
        state = approval.read_state(approval.receipt_path(self.db, self.session))
        self.assertEqual(state["status"], "approved")
        self.assertEqual(state["approval_source"], "native-user-prompt-hook")


if __name__ == "__main__":
    unittest.main()
