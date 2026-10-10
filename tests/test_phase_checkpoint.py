"""Offline tests for automatic Jev shadow checkpoints at harness seams."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
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
        # The fixture database uses the legacy run and record paths, so the native session
        # identity of whoever runs the suite must not leak in: not into subprocesses (self.env)
        # and not into the in-process calls that read os.environ.
        isolated = patch.dict(os.environ)
        isolated.start()
        self.addCleanup(isolated.stop)
        for key in ("HARNESS_SESSION_ID", "CODEX_THREAD_ID", "CLAUDE_SESSION_ID", "CLAUDE_CODE_SESSION_ID", "HARNESS_JEV_DELEGATION"):
            os.environ.pop(key, None)
        self.config_home = self.root / "config"
        os.environ["HARNESS_CONFIG_HOME"] = str(self.config_home)
        self.env = {**os.environ, "HARNESS_JEV_CHECKPOINTS": "1", "HARNESS_TYPESAFE_ROUTER": str(FAKE_ROUTER),
                    "FAKE_ROUTER_LOG_DIR": str(self.root / "logs"), "HARNESS_AUDIT_ENABLED": "0",
                    "TYPESAFE_MODEL": "fixture"}
        for key in ("FAKE_ROUTER_FAIL", "FAKE_ROUTER_CHOICE", "FAKE_ROUTER_BOOL"):
            self.env.pop(key, None)

    def switch_on(self) -> None:
        self.config_home.mkdir(exist_ok=True)
        (self.config_home / "jev-delegation.json").write_text(json.dumps({"enabled": True, "model": "fixture"}))

    def write_state(self, **values: str) -> None:
        (self.run_dir / "state").write_text("".join(f"{k}={v}\n" for k, v in values.items()))

    def write_digests(self, *digests: str) -> None:
        (self.run_dir / "command-digests").write_text("".join(f"{d}\n" for d in digests))

    def start_new_run(self, run_id: str, **values: str) -> None:
        self.run_dir = self.db / "runs" / run_id
        self.run_dir.mkdir(parents=True)
        (self.db / "runs" / "current").write_text(f"{run_id}\n")
        state = {"RUN_STATUS": "active", "CURRENT_PHASE": "build", "STEPS_USED": "1", "LOOPS_USED": "0", "RUN_ID": run_id}
        self.write_state(**{**state, **values})

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

    def test_change_shape_and_test_path_rules(self) -> None:
        self.assertTrue(MODULE.is_test_path("tests/helpers.py"))
        self.assertTrue(MODULE.is_test_path("src/test_api.py"))
        self.assertTrue(MODULE.is_test_path("web/Button.spec.tsx"))
        self.assertTrue(MODULE.is_test_path("app/UserServiceTest.php"))
        self.assertFalse(MODULE.is_test_path("src/latest.py"))
        self.assertFalse(MODULE.is_test_path("contest/entry.py"))
        self.assertFalse(MODULE.is_test_path("src/Contest.php"))
        self.assertEqual(MODULE.change_shape(["src/app.py", "tests/test_app.py"]), "source_and_tests")
        self.assertEqual(MODULE.change_shape(["src/app.py", "README.md"]), "source_only")
        self.assertEqual(MODULE.change_shape(["tests/test_app.py"]), "tests_only")
        self.assertEqual(MODULE.change_shape(["docs/a.md", "config.json"]), "docs_or_config_only")
        self.assertEqual(MODULE.change_shape([]), "none")
        self.assertEqual([MODULE.line_bucket(n) for n in (0, 50, 51, 300, 1500, 1501)],
                         ["none", "small", "medium", "medium", "large", "very_large"])

    def test_signals_count_changed_lines_including_untracked_files(self) -> None:
        (self.project / "app.py").write_text("print('hi')\n" + "x = 1\n" * 60)
        signals = MODULE.git_signals(self.project)
        self.assertEqual((signals["change_shape"], signals["changed_lines_bucket"]), ("source_and_tests", "medium"))

    def test_changed_line_bucket_counts_untracked_deleted_and_capped_files(self) -> None:
        repo = self.root / "lines"
        repo.mkdir()
        git(repo, "init", "-q")
        (repo / "big.py").write_text("x = 1\n" * 60)
        git(repo, "add", ".")
        git(repo, "commit", "-q", "-m", "init")
        (repo / "notes.py").write_text("a\nb\n")
        self.assertEqual(MODULE.git_signals(repo)["changed_lines_bucket"], "small")
        (repo / "notes.py").write_text("a\n" * 60)
        self.assertEqual(MODULE.git_signals(repo)["changed_lines_bucket"], "medium")
        (repo / "notes.py").unlink()
        (repo / "big.py").unlink()
        self.assertEqual(MODULE.git_signals(repo)["changed_lines_bucket"], "medium")
        git(repo, "checkout", "--", "big.py")
        (repo / "huge.log").write_bytes(b"\n" * (MODULE.UNTRACKED_LINE_LIMIT_BYTES + 1))
        self.assertEqual(MODULE.git_signals(repo)["changed_lines_bucket"], "none")
        clean = self.root / "clean"
        clean.mkdir()
        git(clean, "init", "-q")
        self.assertEqual((MODULE.git_signals(clean)["changed_lines_bucket"], MODULE.git_signals(clean)["change_shape"]), ("none", "none"))

    def test_verify_result_keeps_a_private_history_and_facts_use_it(self) -> None:
        records = self.db / "records"
        records.mkdir()
        for exit_code in ("1", "0", "0"):
            (records / "verify.state").write_text(f"RECORD_EPOCH=200\nEXIT={exit_code}\nFAILURES={exit_code}\nRAN=3\n")
            self.assertEqual(self.run_event("verify-result", "--exit", exit_code).returncode, 0)
        history = [json.loads(line) for line in (records / "verify-history.jsonl").read_text().splitlines()]
        self.assertEqual([h["exit"] for h in history], [1, 0, 0])
        self.assertEqual({h["run_id"] for h in history}, {"run-1"})
        self.assertEqual(oct((records / "verify-history.jsonl").stat().st_mode & 0o777), "0o600")
        state = {"RUN_ID": "run-1", "STEPS_USED": "4", "LOOPS_USED": "0"}
        facts = MODULE.verify_facts(self.db, state, self.project)
        self.assertIn("Verification history for this target (last 3): 2 passed, 1 failed; the most recent passed (2 in a row).", facts)
        self.assertIn("Verifications in this run so far: small count, 1 failed.", facts)
        self.assertIn("Working tree changed since the most recent verification: no.", facts)
        (self.project / "app.py").write_text("print('changed')\n")
        self.assertIn("Working tree changed since the most recent verification: yes.", MODULE.verify_facts(self.db, state, self.project))
        self.assertIn("Verifications in this run so far: none.", MODULE.verify_facts(self.db, {"RUN_ID": "run-9"}, self.project))
        self.run_event("verify-start")
        [record] = self.advice_records()
        text = json.dumps(record["context"])
        self.assertNotIn(history[-1]["tree"], text)
        self.assertNotIn("run-1", text)

    def test_repository_without_a_commit_counts_staged_lines(self) -> None:
        repo = self.root / "fresh"
        repo.mkdir()
        git(repo, "init", "-q")
        (repo / "main.py").write_text("x = 1\n" * 60)
        git(repo, "add", "main.py")
        self.assertEqual(MODULE.git_signals(repo)["changed_lines_bucket"], "medium")
        fingerprint = MODULE.tree_fingerprint(repo)
        self.assertIsNotNone(fingerprint)
        (repo / "main.py").write_text("x = 2\n")
        self.assertNotEqual(fingerprint, MODULE.tree_fingerprint(repo))

    def test_tree_fingerprint_changes_with_untracked_and_staged_edits(self) -> None:
        first = MODULE.tree_fingerprint(self.project)
        self.assertEqual(first, MODULE.tree_fingerprint(self.project))
        (self.project / "tests" / "test_secretive_name.py").write_text("x = 2\n")
        second = MODULE.tree_fingerprint(self.project)
        self.assertNotEqual(first, second)
        (self.project / "new_module.py").write_text("")
        third = MODULE.tree_fingerprint(self.project)
        self.assertNotEqual(second, third)
        (self.project / "app.py").write_text("print('staged')\n")
        fourth = MODULE.tree_fingerprint(self.project)
        git(self.project, "add", "app.py")
        self.assertEqual(fourth, MODULE.tree_fingerprint(self.project))
        (self.project / "app.py").write_text("print('staged then edited')\n")
        self.assertNotEqual(fourth, MODULE.tree_fingerprint(self.project))

    def test_verify_history_is_not_written_when_checkpoints_are_disabled(self) -> None:
        records = self.db / "records"
        records.mkdir()
        (records / "verify.state").write_text("RECORD_EPOCH=200\nEXIT=0\n")
        env = {k: v for k, v in self.env.items() if k != "HARNESS_JEV_CHECKPOINTS"}
        self.assertEqual(self.run_event("verify-result", "--exit", "0", env=env).stdout, "")
        self.assertFalse((records / "verify-history.jsonl").exists())

    def test_newer_verify_state_than_history_is_the_most_recent_result(self) -> None:
        records = self.db / "records"
        records.mkdir()
        (records / "verify-history.jsonl").write_text(json.dumps({"epoch": 100, "exit": 0, "tree": "x", "run_id": "run-1"}) + "\n")
        (records / "verify.state").write_text("RECORD_EPOCH=200\nEXIT=1\nFAILURES=1\n")
        facts = MODULE.verify_facts(self.db, {"RUN_ID": "run-1", "RUN_STARTED_EPOCH": "50"}, self.project)
        self.assertIn("Verification history for this target (last 2): 1 passed, 1 failed; the most recent failed (1 in a row).", facts)
        self.assertIn("Verifications in this run so far: small count, 1 failed.", facts)
        self.assertIn("Working tree changed since the most recent verification: unknown.", facts)
        (records / "verify.state").write_text("RECORD_EPOCH=100\nEXIT=0\n")
        self.assertEqual(len(MODULE.verify_history(self.db)), 1)

    def test_verify_history_is_written_without_an_active_run_and_is_bounded(self) -> None:
        self.write_state(RUN_STATUS="complete", RUN_ID="run-1")
        records = self.db / "records"
        records.mkdir()
        (records / "verify-history.jsonl").write_text("".join(json.dumps({"epoch": i, "exit": 0}) + "\n" for i in range(150)) + "not json\n")
        (records / "verify.state").write_text("RECORD_EPOCH=999\nEXIT=1\nFAILURES=2\nRAN=3\n")
        result = self.run_event("verify-result", "--exit", "1")
        self.assertIn("skipped: no active harness run", result.stdout)
        lines = (records / "verify-history.jsonl").read_text().splitlines()
        self.assertEqual(len(lines), MODULE.VERIFY_HISTORY_KEEP)
        self.assertEqual((json.loads(lines[-1])["epoch"], json.loads(lines[-1])["failures"]), (999, 2))

    def test_unwritable_history_never_blocks_verify_labels(self) -> None:
        self.run_event("verify-start")
        (self.db / "records").write_text("not a directory\n")
        result = self.run_event("verify-result", "--exit", "0")
        self.assertIn("verify history not written", result.stdout)
        self.assertIn("labeled evidence_assessment/verify-predict-2 correct", result.stdout)

    def test_legacy_verify_state_without_history_still_yields_a_fact(self) -> None:
        records = self.db / "records"
        records.mkdir()
        (records / "verify.state").write_text("RECORD_EPOCH=150\nEXIT=1\nFAILURES=1\n")
        facts = MODULE.verify_facts(self.db, {"RUN_ID": "run-1", "RUN_STARTED_EPOCH": "100"}, self.project)
        self.assertIn("Verification history for this target (last 1): 0 passed, 1 failed; the most recent failed (1 in a row).", facts)
        self.assertIn("Verifications in this run so far: small count, 1 failed.", facts)
        self.assertIn("Working tree changed since the most recent verification: unknown.", facts)
        self.assertEqual(MODULE.verify_facts(self.root / "empty", {}, self.project), ["Verification history for this target: none recorded yet."])

    def test_previous_run_history_facts(self) -> None:
        for name, status, loops, started in (("r-a", "complete", "0", "10"), ("r-b", "complete", "2", "20"),
                                             ("r-c", "aborted", "0", "30"), ("r-d", "active", "0", "40")):
            directory = self.db / "runs" / name
            directory.mkdir()
            (directory / "state").write_text(f"RUN_ID={name}\nRUN_STATUS={status}\nLOOPS_USED={loops}\nRUN_STARTED_EPOCH={started}\n")
        facts = MODULE.run_history_facts(self.db, {"RUN_ID": "run-1"})
        self.assertEqual(facts, ["Previous runs of this target: small completed, small aborted, small left unfinished.",
                                 "Of the last 2 completed runs, 1 re-entered an earlier phase."])
        # The current run is excluded even when it is complete.
        self.assertEqual(MODULE.run_history_facts(self.db, {"RUN_ID": "r-b"})[1], "Of the last 1 completed runs, 0 re-entered an earlier phase.")
        # Only the five most recent completed runs count, ordered by start time, not by name.
        for index in range(6):
            directory = self.db / "runs" / f"z-{index}"
            directory.mkdir()
            loops = "1" if index == 0 else "0"
            (directory / "state").write_text(f"RUN_ID=z-{index}\nRUN_STATUS=complete\nLOOPS_USED={loops}\nRUN_STARTED_EPOCH={100 + index}\n")
        facts = MODULE.run_history_facts(self.db, {"RUN_ID": "run-1"})
        self.assertEqual(facts, ["Previous runs of this target: medium completed, small aborted, small left unfinished.",
                                 "Of the last 5 completed runs, 0 re-entered an earlier phase."])
        self.assertEqual(MODULE.run_history_facts(self.root / "empty", {"RUN_ID": "x"}),
                         ["Previous runs of this target: none completed, none aborted, none left unfinished."])

    def test_v2_handoff_questions_ask_about_loops_not_progress_md(self) -> None:
        self.run_event("plan-done")
        self.run_event("review-handoff", "--verify-exit", "0")
        plan, review = sorted(self.advice_records(), key=lambda r: r["question_version"])
        self.assertEqual((plan["question_version"], review["question_version"]), ("phase-plan-2", "review-handoff-2"))
        for record in (plan, review):
            self.assertEqual(set(record["questions"]), {"recommendation", "loop_likely"})
            self.assertNotIn("progress.md", json.dumps(record["questions"]))
            self.assertNotIn("progress.md", json.dumps(record["context"]))
            facts = record["context"]["context"]["facts"]
            self.assertTrue(any(f.startswith("Change shape: ") for f in facts))
            self.assertTrue(any(f.startswith("Previous runs of this target") for f in facts))
            self.assertTrue(any(f.startswith("Semantic retrieval") for f in facts))

    def test_legacy_pending_items_without_run_id_keep_their_behaviour(self) -> None:
        directory = self.db / "advice-pending"
        self.run_event("plan-done")
        [path] = list(directory.glob("*.json"))
        item = json.loads(path.read_text())
        item.pop("run_id")
        item["question_version"] = "phase-plan-1"
        path.write_text(json.dumps(item))
        self.start_new_run("run-2")
        self.assertNotIn("labeled", self.run_event("build-start").stdout)
        result = self.run_event("run-complete")
        self.assertIn("labeled handoff_assessment/phase-plan-1 correct", result.stdout)

    def test_plan_done_emits_shadow_checkpoint_and_pending_oracle(self) -> None:
        result = self.run_event("plan-done")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("handoff_assessment/phase-plan-2 shadow recommendation proceed_to_build agrees with baseline", result.stdout)
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
        self.assertIn("labeled evidence_assessment/verify-predict-2 correct", result.stdout)
        self.assertIn("labeled reasoning_allocation/phase-build-2 correct", result.stdout)
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
        self.assertIn("labeled handoff_assessment/phase-plan-2 under_escalated", result.stdout)
        self.assertIn("pilot 1/30 labeled shadow decisions", result.stdout)
        self.assertEqual(self.pending(), [])

    def test_review_handoff_baseline_follows_verification_result(self) -> None:
        self.run_event("review-handoff", "--verify-exit", "1")
        [record] = self.advice_records()
        self.assertEqual(record["baseline_action"], "needs_more_work")
        self.assertIn("Verification during review failed.", record["context"]["context"]["facts"])

    def test_tool_repeat_checkpoint_waits_for_its_oracle(self) -> None:
        self.write_digests("111", "222", "111", "111")
        result = self.run_event("tool-repeat", "--repeats", "3")
        self.assertIn("progress_assessment/tool-repeat-2", result.stdout)
        [pending] = self.pending()
        self.assertEqual((pending["resolver"], pending["digest"], pending["digest_index"], pending["phase_at"]),
                         ("tool_repeat", "111", 4, "build"))
        [record] = self.advice_records()
        self.assertNotIn("111", json.dumps(record["context"]))
        self.assertIn("Shell commands recorded in this run: medium count, small distinct.", record["context"]["context"]["facts"])
        self.assertEqual(self.run_event("verify-start").stdout.count("labeled progress_assessment"), 0)
        pending = json.loads(subprocess.run([sys.executable, str(ROOT / "scripts/context_advice.py"), "--pending", "--db-root", str(self.db)],
                                            capture_output=True, text=True, check=True).stdout)
        self.assertEqual(pending["unlabeled_total"], 2)

    def test_tool_repeat_tracks_the_repeated_digest_despite_a_parallel_append(self) -> None:
        self.write_digests("111", "111", "111", "222")
        self.run_event("tool-repeat", "--repeats", "3", env={**self.env, "FAKE_ROUTER_CHOICE": "change_approach"})
        [pending] = self.pending()
        self.assertEqual((pending["digest"], pending["digest_index"]), ("111", 3))
        self.assertEqual(MODULE.repeated_digest(["a", "b", "b", "a", "b", "a"], 3), ("a", 6))
        self.assertEqual(MODULE.repeated_digest([], 3), (None, 0))
        # A digest that already crossed the threshold is not the one that just reached it.
        self.assertEqual(MODULE.repeated_digest(list("bbbaaab"), 3), ("a", 6))
        self.write_digests("111", "111", "111", "222", "111")
        self.assertIn("labeled progress_assessment/tool-repeat-2 correct", self.run_event("verify-start").stdout)

    def test_tool_repeat_recurring_command_labels_stuck(self) -> None:
        self.write_digests("111", "111", "111")
        self.run_event("tool-repeat", "--repeats", "3", env={**self.env, "FAKE_ROUTER_CHOICE": "change_approach"})
        self.write_digests("111", "111", "111", "222", "111")
        result = self.run_event("verify-start")
        self.assertIn("labeled progress_assessment/tool-repeat-2 correct", result.stdout)
        [outcome] = self.outcomes()
        self.assertIn("issued again after the checkpoint", outcome["evidence"])

    def test_tool_repeat_loop_after_checkpoint_labels_retry_under_escalated(self) -> None:
        self.write_digests("111", "111", "111")
        self.run_event("tool-repeat", "--repeats", "3")
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="build", STEPS_USED="9", LOOPS_USED="1", RUN_ID="run-1")
        result = self.run_event("verify-start")
        self.assertIn("labeled progress_assessment/tool-repeat-2 under_escalated", result.stdout)

    def test_tool_repeat_clean_finish_labels_hold_over_escalated_at_run_complete(self) -> None:
        self.write_digests("111", "111", "111")
        self.run_event("tool-repeat", "--repeats", "3", env={**self.env, "FAKE_ROUTER_CHOICE": "gather_more_evidence"})
        self.write_digests("111", "111", "111", "222", "333")
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="review", PHASE_BUILD="done", STEPS_USED="9", LOOPS_USED="0", RUN_ID="run-1")
        self.assertNotIn("labeled progress_assessment", self.run_event("review-handoff", "--verify-exit", "0").stdout)
        result = self.run_event("run-complete")
        self.assertIn("labeled progress_assessment/tool-repeat-2 over_escalated", result.stdout)
        [outcome] = [o for o in self.outcomes() if "command" in o["evidence"]]
        self.assertIn("did not recur", outcome["evidence"])

    def test_tool_repeat_from_a_superseded_run_is_resolved_from_that_run(self) -> None:
        self.write_digests("111", "111", "111")
        self.run_event("tool-repeat", "--repeats", "3")
        self.start_new_run("run-2")
        result = self.run_event("verify-start")
        self.assertIn("labeled progress_assessment/tool-repeat-2 unknown", result.stdout)
        [outcome] = self.outcomes()
        self.assertIn("ended before the phase", outcome["evidence"])

    def test_superseded_run_that_finished_the_phase_labels_not_stuck(self) -> None:
        self.write_digests("111", "111", "111")
        self.run_event("tool-repeat", "--repeats", "3")
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="review", PHASE_BUILD="done", LOOPS_USED="0", RUN_ID="run-1")
        self.start_new_run("run-2")
        result = self.run_event("plan-done")
        self.assertIn("labeled progress_assessment/tool-repeat-2 correct", result.stdout)

    def test_unlabelable_pending_items_never_block_checkpoints(self) -> None:
        self.run_event("verify-start")
        [valid] = self.pending()
        directory = self.db / "advice-pending"
        # Advice records removed by operator retention, and an item without a call id.
        (directory / "stale.json").write_text(json.dumps(dict(valid, call_id="00000000-0000-0000-0000-0000000000aa", run_id="run-0")))
        (directory / "dead.json").write_text(json.dumps(dict(valid, call_id="00000000-0000-0000-0000-0000000000bb")))
        (directory / "no-id.json").write_text(json.dumps({"resolver": "tool_repeat", "run_id": "run-1", "digest": "1",
                                                         "digest_index": 1, "recommendation": "change_approach"}))
        # null-answers.json: its run has no state, so it resolves as an unknown stale label and is dropped because
        # it has no advice record. list-rec.json: its unhashable recommendation raises TypeError in label_tool_repeat.
        # AttributeError is covered by verify-null.json below (null answers in label_verify).
        (directory / "null-answers.json").write_text(json.dumps(dict(valid, call_id="00000000-0000-0000-0000-0000000000cc",
                                                                    run_id="run-0", resolver="run_complete", answers=None,
                                                                    recommendation=["x"])))
        (directory / "list-rec.json").write_text(json.dumps({"resolver": "tool_repeat", "run_id": "run-1", "call_id": "z",
                                                            "digest": "1", "digest_index": 1, "recommendation": ["x"]}))
        self.write_digests("1", "1")
        before = len(self.advice_records())
        for event in ("plan-done", "build-start"):
            result = self.run_event(event)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.advice_records()), before + 2)
        self.assertFalse(any((directory / name).exists() for name in ("stale.json", "no-id.json", "null-answers.json", "list-rec.json")))
        (directory / "verify-null.json").write_text(json.dumps(dict(valid, call_id="00000000-0000-0000-0000-0000000000dd", answers=None)))
        result = self.run_event("verify-result", "--exit", "0")
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        self.assertFalse((directory / "verify-null.json").exists())
        self.assertIn("could not label 00000000", result.stdout)
        self.assertIn("labeled evidence_assessment/verify-predict-2 correct", result.stdout)
        self.assertFalse((directory / "dead.json").exists())

    def test_resolution_failure_never_suppresses_the_event_checkpoint(self) -> None:
        self.write_digests("1", "1", "1")
        self.run_event("tool-repeat", "--repeats", "3", env={**self.env, "FAKE_ROUTER_CHOICE": "change_approach"})
        [valid] = self.pending()
        directory = self.db / "advice-pending"
        (directory / "zz-valid.json").write_text(json.dumps(valid))
        (directory / f"{valid['call_id']}.json").unlink()
        # Sorted before the valid item: a list, broken JSON, and malformed fields in two resolvers.
        (directory / "a-list.json").write_text("[1, 2]")
        (directory / "b-broken.json").write_text("{not json")
        (directory / "c-odd.json").write_text(json.dumps({"resolver": "tool_repeat", "run_id": "run-1", "call_id": "x",
                                                         "digest": "1", "digest_index": "nope", "loops_at": "zz"}))
        (directory / "d-stale.json").write_text(json.dumps({"resolver": "run_complete", "run_id": "run-0", "call_id": "y",
                                                           "loops_at": "zz", "recommendation": "proceed_to_build"}))
        self.write_digests("1", "1", "1", "1")
        result = self.run_event("plan-done")
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        self.assertIn("handoff_assessment/phase-plan-2 shadow recommendation", result.stdout)
        self.assertIn("labeled progress_assessment/tool-repeat-2 correct", result.stdout)
        self.assertNotIn("pending oracles not resolved", result.stdout)
        # Only this plan-done oracle and the well-formed but still-waiting tool repeat remain.
        plan_oracle = [f"{p['call_id']}.json" for p in self.pending() if p.get("question_version") == "phase-plan-2"]
        self.assertEqual(sorted(p.name for p in directory.glob("*.json")), sorted(plan_oracle + ["c-odd.json"]))
        self.assertNotIn("pending oracles not resolved", self.run_event("build-start").stdout)
        self.assertEqual(MODULE.as_int("zz"), 0)
        # pending_items tolerates a non-object file on its own, even before drop_corrupt_pending runs.
        (directory / "e-list.json").write_text("[3]")
        self.assertEqual([path.name for path, _ in MODULE.pending_items(self.db, "tool_repeat")], ["c-odd.json"])
        self.assertEqual((MODULE.as_int(3), MODULE.as_int("4"), MODULE.as_int(True), MODULE.as_int(None)), (3, 4, 0, 0))

    def test_malformed_integer_fields_read_as_zero_and_are_still_labeled(self) -> None:
        self.run_event("plan-done")
        [path] = list((self.db / "advice-pending").glob("*.json"))
        item = json.loads(path.read_text())
        path.write_text(json.dumps(dict(item, loops_at="zz")))
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="review", LOOPS_USED="1", RUN_ID="run-1")
        self.assertIn("labeled handoff_assessment/phase-plan-2 under_escalated", self.run_event("run-complete").stdout)
        # The same tolerance applies to another run's leftover item.
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="plan", LOOPS_USED="0", RUN_ID="run-1")
        self.run_event("plan-done")
        [path] = list((self.db / "advice-pending").glob("*.json"))
        path.write_text(json.dumps(dict(json.loads(path.read_text()), loops_at="zz")))
        self.write_state(RUN_STATUS="aborted", CURRENT_PHASE="build", LOOPS_USED="1", RUN_ID="run-1")
        self.start_new_run("run-2")
        self.assertIn("labeled handoff_assessment/phase-plan-2 under_escalated", self.run_event("build-start").stdout)

    def test_stale_oracles_are_labeled_from_their_own_run(self) -> None:
        self.run_event("plan-done")
        self.run_event("verify-start")
        self.write_state(RUN_STATUS="active", CURRENT_PHASE="build", LOOPS_USED="1", RUN_ID="run-1")
        self.start_new_run("run-2")
        result = self.run_event("build-start")
        self.assertIn("labeled handoff_assessment/phase-plan-2 under_escalated", result.stdout)
        self.assertIn("labeled evidence_assessment/verify-predict-2 unknown", result.stdout)
        # The new run's own build-start oracle is untouched by the old run.
        self.assertEqual([p["resolver"] for p in self.pending()], ["first_verify"])
        self.assertEqual(self.run_event("verify-result", "--exit", "0").stdout.count("labeled"), 1)

    def test_stale_handoff_from_a_completed_run_uses_that_runs_loops(self) -> None:
        for loops, expected in (("0", "correct"), ("1", "under_escalated")):
            with self.subTest(loops=loops):
                self.run_dir = self.db / "runs" / "run-1"
                (self.db / "runs" / "current").write_text("run-1\n")
                self.write_state(RUN_STATUS="active", CURRENT_PHASE="plan", LOOPS_USED="0", RUN_ID="run-1")
                self.run_event("plan-done")
                self.write_state(RUN_STATUS="complete", CURRENT_PHASE="review", LOOPS_USED=loops, RUN_ID="run-1")
                self.start_new_run(f"run-{loops}-next")
                result = self.run_event("build-start")
                self.assertIn(f"labeled handoff_assessment/phase-plan-2 {expected}", result.stdout)

    def test_other_runs_handoff_is_not_labeled_by_this_runs_loops(self) -> None:
        self.run_event("plan-done")
        self.start_new_run("run-2", LOOPS_USED="1")
        result = self.run_event("run-complete")
        self.assertIn("labeled handoff_assessment/phase-plan-2 unknown", result.stdout)

    def test_tool_repeat_label_matrix(self) -> None:
        self.assertEqual(MODULE.label_tool_repeat({"recommendation": "change_approach"}, True, "r")[0], "correct")
        self.assertEqual(MODULE.label_tool_repeat({"recommendation": "change_approach"}, False, "r")[0], "over_escalated")
        self.assertEqual(MODULE.label_tool_repeat({"recommendation": "retry_same_command"}, True, "r")[0], "under_escalated")
        self.assertEqual(MODULE.label_tool_repeat({"recommendation": "retry_same_command"}, False, "r")[0], "correct")
        self.assertEqual(MODULE.label_tool_repeat({"recommendation": "change_approach"}, None, "r")[0], "unknown")
        self.assertEqual(MODULE.label_tool_repeat({"recommendation": None}, True, "r")[0], "unknown")

    def test_router_failure_records_fallback_and_never_fails_the_caller(self) -> None:
        result = self.run_event("verify-start", env={**self.env, "FAKE_ROUTER_FAIL": "1"})
        self.assertEqual(result.returncode, 0)
        self.assertIn("evidence_assessment/verify-predict-2 fallback (ConnectionError); baseline kept", result.stdout)
        self.assertEqual(self.pending(), [])
        self.assertEqual(self.run_event("verify-result", "--exit", "0").returncode, 0)
        self.assertEqual(self.outcomes(), [])

    def test_missing_router_skips_cleanly(self) -> None:
        result = self.run_event("verify-start", env={**self.env, "HARNESS_TYPESAFE_ROUTER": str(self.root / "missing.py")})
        self.assertEqual(result.returncode, 0)
        self.assertIn("checkpoint skipped", result.stdout)

    def test_session_start_asks_for_a_pilot_decision_once_the_target_is_met(self) -> None:
        with patch.object(MODULE.advice, "pilot", return_value={"labeled": 410, "target": 30}), \
             patch.object(MODULE.task_route, "route_task", return_value={"recommendation": "proceed", "source": "typesafe"}):
            line = MODULE.session_start(self.project, self.db, self.root / "router.py")
        self.assertIn("Pilot complete (410 labeled, 30 needed)", line)
        self.assertIn("promote or retire", line)
        self.assertNotIn("Pilot: 410/30", line)

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

    def test_session_start_reports_the_delegated_route_when_the_switch_is_on(self) -> None:
        self.switch_on()
        result = self.run_event("session-start", env={**self.env, "FAKE_STATE_PATH": str(self.root / "state.json")})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith(
            "Jev delegated route for this session: proceed (source typesafe, active; follow it unless a deterministic rule decides)."),
            result.stdout)
        self.assertNotIn("--shadow", json.loads((self.root / "state.json").read_text())["args"])
        route = json.loads(next((self.db / "routes").glob("*.json")).read_text())
        self.assertEqual((route["routing_mode"], route["mode_source"], route["recommendation"]), ("active", "delegation", "proceed"))

    def test_session_start_names_the_holdback_when_the_switch_cannot_apply(self) -> None:
        self.switch_on()
        env = {k: v for k, v in self.env.items() if k != "TYPESAFE_MODEL"}
        (self.config_home / "jev-delegation.json").write_text(json.dumps({"enabled": True, "model": None}))
        result = self.run_event("session-start", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Jev shadow route for this session", result.stdout)
        self.assertIn("Delegation is on but held back: active routing requires an exact model pin", result.stdout)

    def test_gate_checkpoints_stay_shadow_observations_when_the_switch_is_on(self) -> None:
        self.switch_on()
        result = self.run_event("plan-done")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("shadow recommendation", result.stdout)
        self.assertIn("baseline kept", result.stdout)
        record = self.advice_records()[0]
        self.assertEqual((record["delegation"], record["delegated_action"]), ("shadow", None))

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
