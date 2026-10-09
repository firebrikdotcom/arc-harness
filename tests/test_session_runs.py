"""Real CLI regressions for native session isolation, resumes, and check ownership."""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
from run_paths import current_file, records_dir, session_key


class SessionRunsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.project = self.base / "project"
        self.project.mkdir()
        (self.project / "Makefile").write_text("test:\n\t@true\n")
        self.env = dict(os.environ)
        for key in list(self.env):
            if key.startswith(("HARNESS_", "CODEX_THREAD_ID", "CLAUDE_SESSION_ID", "CLAUDE_CODE_SESSION_ID", "CLAUDE_ENV_FILE",
                               "CLAUDECODE", "CODEX_SANDBOX")):
                self.env.pop(key)
        self.env.update(HARNESS_ROOT=str(ROOT), HARNESS_DB_ROOT=str(self.base / "db"),
                        HARNESS_JEV_CHECKPOINTS="0", HARNESS_AUDIT_ENABLED="0",
                        HARNESS_WORKFLOW_STATE=str(self.base / "workflow.sqlite"),
                        HARNESS_AUDIT_SETTINGS=str(self.base / "settings.json"),
                        HARNESS_AUDIT_OUTBOX=str(self.base / "outbox.sqlite"))
        (self.base / "settings.json").write_text('{"jev":false,"workflow":false}')
        result = subprocess.run([str(ROOT / "scripts/harness-target.sh"), "register", str(self.project)],
                                env=self.env, text=True, capture_output=True, check=True)
        self.db = Path(result.stdout.strip()) / "db"

    def call(self, sid, *args, expected=0, extra=None):
        env = {**self.env, **(extra or {})}
        if sid:
            env["HARNESS_SESSION_ID"] = sid
        result = subprocess.run([str(ROOT / "scripts/harness"), *args], cwd=self.project,
                                env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result.stdout

    def run_id(self, sid):
        pointer = current_file(self.db, sid) if sid else self.db / "runs/current"
        return pointer.read_text().strip()

    def prepare_build(self, sid):
        self.call(sid, "workflow", "todo", "plan", "--items",
                  '[{"id":"check","description":"Check","criterion":"Checks pass"}]', "--reason", "test")
        self.call(sid, "workflow", "todo", "update", "--id", "check", "--status", "in_progress", "--reason", "test")
        self.call(sid, "plan", "start")
        self.call(sid, "contract", "waive", "fixture task")
        self.call(sid, "plan", "done")
        self.call(sid, "build", "start")

    def sensor(self, sid, name):
        result = subprocess.run([str(ROOT / "scripts" / name), "--project", str(self.project)],
                                env={**self.env, "HARNESS_SESSION_ID": sid}, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_new_parallel_sessions_ignore_paused_legacy_run(self):
        self.call(None, "plan", "start", expected=3, extra={"HARNESS_BUDGET_STEPS": "1"})
        legacy = self.run_id(None)
        with ThreadPoolExecutor(max_workers=2) as pool:
            list(pool.map(lambda sid: self.call(sid, "plan", "start"), ("a", "b")))
        self.assertNotEqual(self.run_id("a"), self.run_id("b"))
        self.assertEqual(self.run_id(None), legacy)
        self.assertIn("paused", self.call(None, "status"))
        self.assertIn("loops     0/1", self.call("a", "status"))

    def test_resuming_same_session_preserves_pause_and_counters(self):
        self.call("a", "plan", "start", expected=3, extra={"HARNESS_BUDGET_STEPS": "1"})
        run = self.run_id("a")
        self.call("b", "plan", "start")
        self.call("a", "plan", "start", expected=3)
        self.call("b", "continue", "wrong session", expected=4)
        self.assertIn("paused", self.call("a", "status"))
        self.call("a", "continue", "explicit user continuation")
        self.assertEqual(self.run_id("a"), run)
        self.assertIn("continues 1/3", self.call("a", "status"))

    def test_native_environment_and_explicit_selection_agree(self):
        self.call(None, "plan", "start", extra={"CODEX_THREAD_ID": "native"})
        native = self.run_id("native")
        self.assertIn(native, self.call(None, "status", extra={"CLAUDE_SESSION_ID": "native"}))
        self.assertIn(native, self.call("other", "--session-id", "native", "status"))
        self.assertFalse((self.db / "runs/current").exists())

    def test_session_checks_cannot_satisfy_another_session(self):
        self.prepare_build("a")
        self.prepare_build("b")
        self.sensor("a", "verify.sh")
        self.call("a", "build", "done")
        self.assertIn("no verify record", self.call("b", "build", "done", expected=4))
        self.sensor("b", "verify.sh")
        self.call("b", "build", "done")
        self.call("a", "review", "start")
        self.call("b", "review", "start")
        self.sensor("a", "review.sh")
        self.assertIn("no review record", self.call("b", "review", "done", expected=4))
        self.sensor("b", "review.sh")
        for sid in ("a", "b"):
            self.call(sid, "workflow", "todo", "update", "--id", "check", "--status", "completed",
                      "--reason", "tested", "--evidence", "sensors passed")
            findings = self.base / ("findings-" + sid + ".json")
            findings.write_text(json.dumps({"tree_hash": "none", "reviewer": "fixture", "verdict": "approve", "findings": []}))
            self.call(sid, "review", "submit", str(findings))
            self.call(sid, "review", "done")
        previous = self.run_id("a")
        self.call("a", "plan", "start")
        self.assertNotEqual(previous, self.run_id("a"))
        self.assertIn("loops     0/1", self.call("a", "status"))
        self.assertTrue(records_dir(self.db, "a").is_dir())
        self.call("a", "workflow", "todo", "update", "--id", "check", "--status", "in_progress", "--reason", "next task")
        self.call("a", "contract", "waive", "fixture task")
        self.call("a", "plan", "done")
        self.call("a", "build", "start")
        self.assertIn("belongs to another run", self.call("a", "build", "done", expected=4))

    def test_node_fallback_selects_the_same_pointer(self):
        node = shutil.which("node")
        if not node:
            self.skipTest("Node unavailable")
        binary_dir = self.base / "bin"
        binary_dir.mkdir()
        (binary_dir / "node").symlink_to(node)
        (binary_dir / "dirname").symlink_to(shutil.which("dirname"))
        result = subprocess.run(["/bin/sh", str(ROOT / "scripts/run-paths.sh"), "current", str(self.db)],
                                env={**self.env, "PATH": str(binary_dir), "HARNESS_SESSION_ID": "native"},
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(current_file(self.db, "native")))

    def test_checkpoint_resolution_leaves_other_session_oracles_pending(self):
        import phase_checkpoint
        self.call("a", "plan", "start")
        self.call("b", "plan", "start")
        pending = self.db / "advice-pending"
        pending.mkdir()
        for resolver in ("tool_repeat", "verify_result", "first_verify", "run_complete"):
            (pending / (resolver + ".json")).write_text(json.dumps({"run_id": self.run_id("b"), "resolver": resolver}))
        current = {"RUN_ID": self.run_id("a"), "SESSION_KEY": session_key("a")}
        with patch.object(phase_checkpoint, "label_item", side_effect=AssertionError("other session consumed")):
            self.assertEqual(phase_checkpoint.resolve_tool_repeats(self.db, None, current), [])
            self.assertEqual(phase_checkpoint.resolve_stale(self.db, None, current), [])
        self.assertEqual(len(list(pending.glob("*.json"))), 4)

    def test_native_start_binding_works_with_collection_disabled(self):
        env_file = self.base / "environment"
        env_file.touch()
        environment = {**self.env, "CLAUDE_ENV_FILE": str(env_file), "HARNESS_AUTO_INIT": "0"}
        for source in ("startup", "resume", "clear", "compact"):
            result = subprocess.run([sys.executable, str(ROOT / "scripts/session_hook.py"),
                                     str(ROOT / "scripts/hooks/auto-init.sh")],
                                    input=json.dumps({"session_id": "native-resume", "hook_event_name": "SessionStart",
                                                      "source": source, "cwd": str(self.project)}),
                                    env=environment, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("export HARNESS_SESSION_ID=native-resume", env_file.read_text())

    def test_observer_uses_payload_session_without_touching_others(self):
        self.call("a", "plan", "start")
        self.call("b", "plan", "start")
        result = subprocess.run([sys.executable, str(ROOT / "scripts/observe_commands.py")],
                                input=json.dumps({"session_id": "a", "cwd": str(self.project), "tool_name": "Bash",
                                                  "tool_input": {"command": "make test"}}),
                                env={**self.env, "HARNESS_SESSION_ID": "b", "HARNESS_JEV_CHECKPOINTS": "1"},
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0)
        self.assertTrue((self.db / "runs" / self.run_id("a") / "command-digests").exists())
        self.assertFalse((self.db / "runs" / self.run_id("b") / "command-digests").exists())

    def test_identifier_is_hashed_and_cannot_traverse_paths(self):
        key = session_key("../../a/session")
        self.assertEqual(len(key), 64)
        self.assertEqual(current_file(self.db, "../../a/session"), self.db / "runs" / "sessions" / key / "current")
        with self.assertRaises(ValueError):
            session_key("a\nb")


if __name__ == "__main__":
    unittest.main()
