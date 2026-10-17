"""Offline tests for Jev at harness seams: the session-start route, quiet gates and verify history."""

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

    def test_phase_gates_are_deterministic_and_never_ask_jev(self) -> None:
        # With or without a run and with the switch on, a gate event makes no call, no label and no output.
        for status in ("active", "complete"):
            self.write_state(RUN_STATUS=status, RUN_ID="run-1")
            for event in (("plan-done",), ("build-start",), ("verify-start",), ("run-complete",),
                          ("review-handoff", "--verify-exit", "1"), ("tool-repeat", "--repeats", "3")):
                result = self.run_event(*event, env={**self.env, "FAKE_STATE_PATH": str(self.root / "state.json")})
                self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""), event)
            self.switch_on()
        self.assertEqual((self.advice_records(), self.pending(), self.outcomes()), ([], [], []))
        self.assertFalse((self.root / "state.json").exists())

    def test_signals_are_enum_only(self) -> None:
        signals = MODULE.git_signals(self.project)
        self.assertTrue(signals["is_git"])
        self.assertEqual(signals["dirty_bucket"], "small")
        self.assertTrue(signals["tests_changed"] and signals["progress_changed"] and signals["docs_changed"])
        metadata = json.dumps(MODULE.session_metadata(self.project, self.db, signals))
        self.assertNotIn("secretive", metadata)
        self.assertNotIn(str(self.project), metadata)

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

    def test_verify_result_keeps_a_private_history(self) -> None:
        records = self.db / "records"
        records.mkdir()
        for exit_code in ("1", "0", "0"):
            (records / "verify.state").write_text(f"RECORD_EPOCH=200\nEXIT={exit_code}\nFAILURES={exit_code}\nRAN=3\n")
            result = self.run_event("verify-result", "--exit", exit_code)
            self.assertEqual((result.returncode, result.stdout), (0, ""))
        history = [json.loads(line) for line in (records / "verify-history.jsonl").read_text().splitlines()]
        self.assertEqual([h["exit"] for h in history], [1, 0, 0])
        self.assertEqual({h["run_id"] for h in history}, {"run-1"})
        self.assertEqual(MODULE.verify_history(self.db), history)
        self.assertEqual(oct((records / "verify-history.jsonl").stat().st_mode & 0o777), "0o600")
        self.assertEqual(self.advice_records(), [])

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

    def test_verify_history_is_written_without_an_active_run_and_is_bounded(self) -> None:
        self.write_state(RUN_STATUS="complete", RUN_ID="run-1")
        records = self.db / "records"
        records.mkdir()
        (records / "verify-history.jsonl").write_text("".join(json.dumps({"epoch": i, "exit": 0}) + "\n" for i in range(150)) + "not json\n")
        (records / "verify.state").write_text("RECORD_EPOCH=999\nEXIT=1\nFAILURES=2\nRAN=3\n")
        result = self.run_event("verify-result", "--exit", "1")
        self.assertEqual((result.returncode, result.stdout), (0, ""))
        lines = (records / "verify-history.jsonl").read_text().splitlines()
        self.assertEqual(len(lines), MODULE.VERIFY_HISTORY_KEEP)
        self.assertEqual((json.loads(lines[-1])["epoch"], json.loads(lines[-1])["failures"]), (999, 2))

    def test_unwritable_history_never_fails_verify(self) -> None:
        (self.db / "records").write_text("not a directory\n")
        result = self.run_event("verify-result", "--exit", "0")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "verify history not written: FileExistsError")

    def test_session_start_without_a_router_leaves_the_main_agent_path(self) -> None:
        self.switch_on()
        result = self.run_event("session-start", env={**self.env, "HARNESS_TYPESAFE_ROUTER": str(self.root / "missing.py")})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Jev session route not followed", result.stdout)
        self.assertIn("the main agent decides", result.stdout)
        route = json.loads(next((self.db / "routes").glob("*.json")).read_text())
        self.assertEqual((route["source"], route["recommendation"], route["needs_reasoning"]), ("fallback", "default", True))

    def test_session_start_writes_one_shadow_route_from_enum_metadata(self) -> None:
        (self.project / "util.py").write_text("y = 2\n")  # python outweighs markdown: area backend
        result = self.run_event("session-start", env={**self.env, "FAKE_STATE_PATH": str(self.root / "state.json")})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "Jev session route observed in shadow mode, not followed: the main agent decides.")
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
        self.assertEqual(result.stdout.strip(), "Jev session route: proceed (confidence 0.9, minimum 0.7); "
                                                "follow it unless a deterministic rule decides.")
        route = json.loads(next((self.db / "routes").glob("*.json")).read_text())
        self.assertEqual((route["routing_mode"], route["mode_source"], route["recommendation"]), ("active", "delegation", "proceed"))

    def test_session_start_below_the_threshold_leaves_the_main_agent_path(self) -> None:
        self.switch_on()
        result = self.run_event("session-start", env={**self.env, "FAKE_ROUTER_CONFIDENCE": "0.69"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "Jev session route not followed (low_confidence; confidence 0.69, "
                                                "minimum 0.7): the main agent decides.")
        route = json.loads(next((self.db / "routes").glob("*.json")).read_text())
        self.assertEqual((route["recommendation"], route["action"], route["escalation_reason"]),
                         ("default", "normal_reasoning", "low_confidence"))

    def test_session_start_names_the_holdback_when_the_switch_cannot_apply(self) -> None:
        self.switch_on()
        env = {k: v for k, v in self.env.items() if k != "TYPESAFE_MODEL"}
        (self.config_home / "jev-delegation.json").write_text(json.dumps({"enabled": True, "model": None}))
        result = self.run_event("session-start", env=env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("observed in shadow mode, not followed", result.stdout)
        self.assertIn("Delegation is on but held back: active routing requires an exact model pin", result.stdout)

    def test_session_start_outside_git_skips(self) -> None:
        plain = self.root / "plain"
        plain.mkdir()
        result = subprocess.run([sys.executable, str(SCRIPT), "session-start", "--project", str(plain), "--db-root", str(self.db)],
                                env=self.env, capture_output=True, text=True, check=False)
        self.assertEqual((result.returncode, result.stdout.strip()), (0, "checkpoint skipped: not a git worktree"))

    def test_area_detection(self) -> None:
        self.assertEqual(MODULE.area_for(["javascript", "stylesheet"], self.project), "frontend")
        self.assertEqual(MODULE.area_for(["python", "javascript"], self.project), "other")
        self.assertEqual(MODULE.area_for({"python": 3, "markdown": 1}, self.project), "backend")
        self.assertEqual(MODULE.area_for({"kotlin": 1, "csharp": 1}, self.project), "other")
        self.assertEqual(MODULE.area_for([], self.project), "other")


if __name__ == "__main__":
    unittest.main()
