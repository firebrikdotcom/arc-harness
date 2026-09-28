"""Offline tests for automatic Jev shadow checkpoints at harness seams."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "phase_checkpoint.py"
FAKE_ROUTER = ROOT / "tests" / "fake_router.py"
SPEC = importlib.util.spec_from_file_location("phase_checkpoint", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def git(path: Path, *arguments: str) -> None:
    subprocess.run(["git", "-C", str(path), *arguments], check=True, capture_output=True,
                   env={**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
                        "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid"})


class PhaseCheckpointTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="harness-jev-checkpoint-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.project = self.root / "project"
        self.project.mkdir()
        git(self.project, "init", "-q")
        (self.project / "app.py").write_text("print('hi')\n")
        (self.project / "tests").mkdir()
        git(self.project, "add", ".")
        git(self.project, "commit", "-q", "-m", "init")
        (self.project / "progress.md").write_text("# plan\n")
        (self.project / "tests" / "test_secretive_name.py").write_text("x = 1\n")
        self.db = self.root / "db"
        self.run_dir = self.db / "runs" / "run-1"
        self.run_dir.mkdir(parents=True)
        (self.db / "runs" / "current").write_text("run-1\n")
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="build", STEPS_USED="4", LOOPS_USED="0",
                         PHASE_BUILD_STARTED_EPOCH="100", RUN_ID="run-1")
        self.env = {**os.environ, "HARNESS_JEV_CHECKPOINTS": "1", "HARNESS_TYPESAFE_ROUTER": str(FAKE_ROUTER),
                    "FAKE_ROUTER_LOG_DIR": str(self.root / "logs"), "HARNESS_AUDIT_ENABLED": "0"}
        for key in ("FAKE_ROUTER_FAIL", "FAKE_ROUTER_CHOICE", "FAKE_ROUTER_BOOL"):
            self.env.pop(key, None)

    def write_state(self, **values: str) -> None:
        (self.run_dir / "state").write_text("".join(f"{k}={v}\n" for k, v in values.items()))

    def run_event(self, *arguments: str, env: dict | None = None) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(SCRIPT), *arguments, "--project", str(self.project), "--db-root", str(self.db)],
                              env=env or self.env, capture_output=True, text=True, check=False)

    def advice_records(self) -> list[dict]:
        directory = self.db / "advice"
        return [json.loads(p.read_text()) for p in sorted(directory.glob("*.json"))] if directory.is_dir() else []

    def outcomes(self) -> list[dict]:
        directory = self.db / "advice-outcomes"
        return [json.loads(p.read_text()) for p in sorted(directory.glob("*.json"))] if directory.is_dir() else []

    def pending(self) -> list[dict]:
        directory = self.db / "advice-pending"
        return [json.loads(p.read_text()) for p in sorted(directory.glob("*.json"))] if directory.is_dir() else []

    def test_disabled_is_a_silent_no_op(self) -> None:
        env = {k: v for k, v in self.env.items() if k != "HARNESS_JEV_CHECKPOINTS"}
        result = self.run_event("verify-start", env=env)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""))
        self.assertEqual(self.advice_records(), [])

    def test_no_active_run_skips_without_records(self) -> None:
        self.write_state(RUN_STATUS="complete")
        result = self.run_event("plan-done")
        self.assertEqual(result.returncode, 0)
        self.assertIn("skipped: no active harness run", result.stdout)
        self.assertEqual(self.advice_records(), [])

    def test_signals_and_facts_are_enum_only(self) -> None:
        signals = MODULE.git_signals(self.project)
        self.assertTrue(signals["is_git"])
        self.assertEqual(signals["dirty_bucket"], "small")
        self.assertTrue(signals["tests_changed"] and signals["progress_changed"] and signals["docs_changed"])
        context, resolver = MODULE.build_verify_start(signals, {"STEPS_USED": "4", "LOOPS_USED": "0"}, self.db)
        text = json.dumps(context)
        self.assertNotIn("secretive", text)
        self.assertNotIn("progress.md changed: no", text)
        self.assertNotIn(str(self.project), text)
        self.assertEqual(resolver, "verify_result")

    def test_plan_done_emits_shadow_checkpoint_and_pending_oracle(self) -> None:
        result = self.run_event("plan-done")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("handoff_assessment/phase-plan-1 shadow recommendation proceed_to_build agrees with baseline", result.stdout)
        self.assertIn("baseline kept", result.stdout)
        [record] = self.advice_records()
        self.assertEqual((record["status"], record["shadow"], record["baseline_action"]), ("evaluated", True, "proceed_to_build"))
        self.assertEqual({q["type"] for q in record["questions"].values()}, {"choice", "noul"})
        [pending] = self.pending()
        self.assertEqual((pending["resolver"], pending["loops_at"], pending["recommendation"]), ("run_complete", 0, "proceed_to_build"))

    def test_plan_done_reports_retrieval_use_without_question_or_paths(self) -> None:
        retrieval = self.db / "retrieval"
        retrieval.mkdir()
        (retrieval / "a.state").write_text("RECORD_KIND=jevgrep\nRUN_ID=run-1\nCOMPLETE=yes\nQUESTION_SHA256=abc\n")
        (retrieval / "b.state").write_text("RECORD_KIND=jevgrep\nRUN_ID=run-1\nCOMPLETE=partial\n")
        (retrieval / "other-run.state").write_text("RECORD_KIND=jevgrep\nRUN_ID=run-0\nCOMPLETE=yes\n")
        self.assertEqual(self.run_event("plan-done").returncode, 0)
        [record] = self.advice_records()
        facts = json.dumps(record["context"])
        self.assertIn("Semantic retrieval (jevgrep) used in this run: yes (small count, complete results: 1).", facts)
        self.assertNotIn("abc", facts)
        self.assertNotIn(str(self.project), facts)

    def test_plan_done_reports_no_retrieval_when_no_records_exist(self) -> None:
        self.assertEqual(self.run_event("plan-done").returncode, 0)
        [record] = self.advice_records()
        self.assertIn("Semantic retrieval (jevgrep) used in this run: no.", json.dumps(record["context"]))

    def test_verify_cycle_labels_prediction_and_allocation_from_exit_code(self) -> None:
        self.assertEqual(self.run_event("build-start").returncode, 0)
        self.assertEqual(self.run_event("verify-start").returncode, 0)
        self.assertEqual({p["resolver"] for p in self.pending()}, {"first_verify", "verify_result"})
        result = self.run_event("verify-result", "--exit", "0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("labeled evidence_assessment/verify-predict-1 correct", result.stdout)
        self.assertIn("labeled reasoning_allocation/phase-build-1 correct", result.stdout)
        self.assertEqual(self.pending(), [])
        self.assertEqual(sorted(o["outcome"] for o in self.outcomes()), ["correct", "correct"])
        report = json.loads(subprocess.run([sys.executable, str(ROOT / "scripts/context_advice.py"), "--report", "--db-root", str(self.db)],
                                           capture_output=True, text=True, check=True).stdout)
        self.assertEqual((report["pilot"]["labeled"], report["pilot"]["remaining"]), (2, 28))

    def test_failed_verification_marks_confident_pass_prediction_under_escalated(self) -> None:
        self.run_event("verify-start")
        result = self.run_event("verify-result", "--exit", "1")
        self.assertIn("under_escalated", result.stdout)
        [outcome] = self.outcomes()
        self.assertEqual(outcome["outcome"], "under_escalated")
        self.assertIn("exited 1", outcome["evidence"])

    def test_second_verify_has_nothing_left_to_label(self) -> None:
        self.run_event("verify-start")
        self.run_event("verify-result", "--exit", "0")
        result = self.run_event("verify-result", "--exit", "1")
        self.assertEqual((result.returncode, result.stdout.strip()), (0, ""))
        self.assertEqual(len(self.outcomes()), 1)

    def test_run_complete_labels_handoffs_from_loops_and_prints_pilot(self) -> None:
        self.run_event("plan-done")
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="review", STEPS_USED="9", LOOPS_USED="1", RUN_ID="run-1")
        result = self.run_event("run-complete")
        self.assertIn("labeled handoff_assessment/phase-plan-1 under_escalated", result.stdout)
        self.assertIn("pilot 1/30 labeled shadow decisions", result.stdout)
        self.assertEqual(self.pending(), [])

    def test_review_handoff_baseline_follows_verification_result(self) -> None:
        self.run_event("review-handoff", "--verify-exit", "1")
        [record] = self.advice_records()
        self.assertEqual(record["baseline_action"], "needs_more_work")
        self.assertIn("Verification during review failed.", record["context"]["context"]["facts"])

    def test_tool_repeat_checkpoint_is_not_auto_labeled(self) -> None:
        result = self.run_event("tool-repeat", "--repeats", "3")
        self.assertIn("progress_assessment/tool-repeat-1", result.stdout)
        self.assertEqual(self.pending(), [])
        pending = json.loads(subprocess.run([sys.executable, str(ROOT / "scripts/context_advice.py"), "--pending", "--db-root", str(self.db)],
                                            capture_output=True, text=True, check=True).stdout)
        self.assertEqual(pending["unlabeled_total"], 1)

    def test_router_failure_records_fallback_and_never_fails_the_caller(self) -> None:
        result = self.run_event("verify-start", env={**self.env, "FAKE_ROUTER_FAIL": "1"})
        self.assertEqual(result.returncode, 0)
        self.assertIn("evidence_assessment/verify-predict-1 fallback (ConnectionError); baseline kept", result.stdout)
        self.assertEqual(self.pending(), [])
        self.assertEqual(self.run_event("verify-result", "--exit", "0").returncode, 0)
        self.assertEqual(self.outcomes(), [])

    def test_missing_router_skips_cleanly(self) -> None:
        result = self.run_event("verify-start", env={**self.env, "HARNESS_TYPESAFE_ROUTER": str(self.root / "missing.py")})
        self.assertEqual(result.returncode, 0)
        self.assertIn("checkpoint skipped", result.stdout)

    def test_session_start_writes_one_shadow_route_from_enum_metadata(self) -> None:
        (self.project / "util.py").write_text("y = 2\n")  # python outweighs markdown: area backend
        result = self.run_event("session-start", env={**self.env, "FAKE_STATE_PATH": str(self.root / "state.json")})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"^Jev shadow route for this session: proceed \(source typesafe, shadow; existing rules decide\)\. Pilot: 0/30")
        [route] = [json.loads(p.read_text()) for p in (self.db / "routes").glob("*.json")]
        self.assertEqual((route["mode"], route["recommendation"], route["metadata"]["uncertainty_reason"]),
                         ("shadow", "default", "scope_unclear"))
        sent = json.loads((self.root / "state.json").read_text())["state"]
        self.assertNotIn("secretive", json.dumps(sent))
        self.assertEqual(sent["task"], {"kind": "change", "area": "backend"})

    def test_session_start_outside_git_skips(self) -> None:
        plain = self.root / "plain"
        plain.mkdir()
        result = subprocess.run([sys.executable, str(SCRIPT), "session-start", "--project", str(plain), "--db-root", str(self.db)],
                                env=self.env, capture_output=True, text=True, check=False)
        self.assertEqual((result.returncode, result.stdout.strip()), (0, "checkpoint skipped: not a git worktree"))

    def test_label_matrices(self) -> None:
        self.assertEqual(MODULE.label_verify({"answers": {"will_pass": 0.9}}, 0)[0], "correct")
        self.assertEqual(MODULE.label_verify({"answers": {"will_pass": 0.2}}, 0)[0], "over_escalated")
        self.assertEqual(MODULE.label_verify({"answers": {}, "recommendation": "fix_before_verifying"}, 1)[0], "correct")
        self.assertEqual(MODULE.label_allocation({"recommendation": "deep_reasoning"}, 0)[0], "over_escalated")
        self.assertEqual(MODULE.label_allocation({"recommendation": "targeted_check"}, 1)[0], "under_escalated")
        self.assertEqual(MODULE.label_handoff({"recommendation": "needs_more_work", "loops_at": 0}, 1)[0], "correct")
        self.assertEqual(MODULE.label_handoff({"recommendation": "ready_for_handoff", "loops_at": 1}, 1)[0], "correct")
        self.assertEqual(MODULE.label_handoff({"recommendation": None, "loops_at": 0}, 0)[0], "unknown")

    def test_area_detection(self) -> None:
        self.assertEqual(MODULE.area_for(["javascript", "stylesheet"], self.project), "frontend")
        self.assertEqual(MODULE.area_for(["python", "javascript"], self.project), "other")
        self.assertEqual(MODULE.area_for({"python": 3, "markdown": 1}, self.project), "backend")
        self.assertEqual(MODULE.area_for({"kotlin": 1, "csharp": 1}, self.project), "other")
        self.assertEqual(MODULE.area_for([], self.project), "other")


if __name__ == "__main__":
    unittest.main()
