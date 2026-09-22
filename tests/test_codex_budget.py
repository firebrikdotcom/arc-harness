"""Offline regression tests for Codex App Server token-budget control."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/codex_budget.py"
sys.path.insert(0, str(ROOT / "scripts"))

import agent_launch  # noqa: E402


FAKE_CODEX = r'''#!/usr/bin/env python3
import json
import os
import sys

log_path = os.environ["FAKE_CODEX_LOG"]
get_count = 0
existing = os.environ.get("FAKE_EXISTING_GOAL") == "1"
notify = os.environ.get("FAKE_TURN_NOTIFICATION", "1") == "1"
existing_status = os.environ.get("FAKE_GOAL_STATUS", "active")
created_at = int(os.environ.get("FAKE_CREATED_AT", "0"))
cwd = os.environ.get("FAKE_CWD", "")

def goal(status="active", used=0, budget=5):
    return {
        "threadId": "thr-new", "objective": "fixture", "status": status,
        "tokenBudget": budget, "tokensUsed": used, "timeUsedSeconds": 1,
        "createdAt": created_at, "updatedAt": created_at,
    }

for line in sys.stdin:
    message = json.loads(line)
    with open(log_path, "a", encoding="utf-8") as stream:
        stream.write(json.dumps(message, sort_keys=True) + "\n")
    if "id" not in message:
        continue
    method = message["method"]
    params = message.get("params", {})
    if method == "initialize":
        result = {"userAgent": "fixture"}
    elif method == "thread/list":
        result = {"data": [
            {"id": "thr-old", "cwd": cwd, "createdAt": created_at,
             "updatedAt": created_at, "status": {"type": "notLoaded"}},
            {"id": "thr-new", "cwd": cwd, "createdAt": created_at + 1,
             "updatedAt": created_at + 1, "status": {"type": "idle"}},
        ], "nextCursor": None}
    elif method == "thread/goal/get":
        get_count += 1
        if get_count == 1:
            result = {"goal": goal(status=existing_status, used=4) if existing else None}
        else:
            if notify:
                print(json.dumps({"method": "turn/started", "params": {
                    "threadId": params["threadId"],
                    "turn": {"id": "turn-9", "status": "inProgress", "items": []},
                }}), flush=True)
            result = {"goal": goal(used=7)}
    elif method == "thread/goal/set":
        status = params.get("status", "active")
        result = {"goal": goal(status=status, used=4 if existing else 0, budget=params.get("tokenBudget", 5))}
    elif method == "thread/read":
        result = {"thread": {"id": params["threadId"], "turns": [
            {"id": "turn-fallback", "status": "inProgress", "items": []}
        ]}}
    elif method in {"turn/interrupt"}:
        result = {}
    else:
        print(json.dumps({"id": message["id"], "error": {"message": "unexpected " + method}}), flush=True)
        continue
    print(json.dumps({"id": message["id"], "result": result}), flush=True)
'''


FAKE_HARNESS = r'''#!/usr/bin/env python3
import json
import os
import sys
with open(os.environ["FAKE_HARNESS_LOG"], "a", encoding="utf-8") as stream:
    stream.write(json.dumps({"argv": sys.argv[1:], "db": os.environ.get("HARNESS_DB_ROOT")}) + "\n")
sys.exit(int(os.environ.get("FAKE_HARNESS_EXIT", "0")))
'''


class CodexBudgetTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="harness-codex-budget-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.codex = self.root / "codex"
        self.codex.write_text(FAKE_CODEX, encoding="utf-8")
        self.codex.chmod(0o755)
        self.harness = self.root / "harness"
        self.harness.write_text(FAKE_HARNESS, encoding="utf-8")
        self.harness.chmod(0o755)
        self.codex_log = self.root / "codex.jsonl"
        self.harness_log = self.root / "harness.jsonl"
        self.db = self.root / "db"
        self.env = {
            **os.environ,
            "FAKE_CODEX_LOG": str(self.codex_log),
            "FAKE_HARNESS_LOG": str(self.harness_log),
            "FAKE_CWD": str(self.root.resolve()),
            "FAKE_CREATED_AT": str(int(time.time())),
        }

    def run_budget(self, *args: str, expected: int = 0, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), *args, "--codex", str(self.codex), "--request-timeout", "2"],
            env=env or self.env,
            text=True,
            capture_output=True,
            check=False,
            timeout=5,
        )
        self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
        return result

    def requests(self) -> list[dict]:
        return [json.loads(line) for line in self.codex_log.read_text(encoding="utf-8").splitlines()]

    def test_watch_reports_delta_and_interrupts_exact_active_turn(self) -> None:
        result = self.run_budget(
            "--thread", "thr-new", "--tokens", "5", "--watch", "--interval", "0.01",
            "--harness", str(self.harness), "--db-root", str(self.db), expected=3,
        )
        output = json.loads(result.stdout)
        self.assertEqual((output["status"], output["turn_id"]), ("budgetLimited", "turn-9"))
        requests = self.requests()
        interrupt = next(item for item in requests if item.get("method") == "turn/interrupt")
        self.assertEqual(interrupt["params"], {"threadId": "thr-new", "turnId": "turn-9"})
        updates = [json.loads(line) for line in self.harness_log.read_text(encoding="utf-8").splitlines()]
        self.assertEqual(updates, [{
            "argv": ["step", "--tokens", "7", "--note", "Codex token-budget meter"],
            "db": str(self.db),
        }])

    def test_active_turn_falls_back_to_thread_read(self) -> None:
        environment = {**self.env, "FAKE_TURN_NOTIFICATION": "0"}
        result = self.run_budget(
            "--thread", "thr-new", "--tokens", "5", "--watch", "--interval", "0.01",
            expected=3, env=environment,
        )
        self.assertEqual(json.loads(result.stdout)["turn_id"], "turn-fallback")
        self.assertIn("thread/read", [item.get("method") for item in self.requests()])

    def test_harness_pause_interrupts_without_rewriting_goal_status(self) -> None:
        environment = {**self.env, "FAKE_HARNESS_EXIT": "3"}
        result = self.run_budget(
            "--thread", "thr-new", "--tokens", "9", "--watch", "--interval", "0.01",
            "--harness", str(self.harness), "--db-root", str(self.db), expected=3,
            env=environment,
        )
        self.assertEqual(json.loads(result.stdout)["status"], "harnessPaused")
        self.assertIn("turn/interrupt", [item.get("method") for item in self.requests()])
        self.assertEqual(len([item for item in self.requests() if item.get("method") == "thread/goal/set"]), 1)

    def test_attaching_preserves_existing_goal_usage(self) -> None:
        environment = {**self.env, "FAKE_EXISTING_GOAL": "1"}
        result = self.run_budget("--thread", "thr-new", "--tokens", "9", env=environment)
        self.assertEqual(json.loads(result.stdout)["goal"]["tokensUsed"], 4)
        goal_set = next(item for item in self.requests() if item.get("method") == "thread/goal/set")
        self.assertNotIn("objective", goal_set["params"])
        self.assertEqual(goal_set["params"]["tokenBudget"], 9)

    def test_paused_goal_is_not_reactivated(self) -> None:
        environment = {**self.env, "FAKE_EXISTING_GOAL": "1", "FAKE_GOAL_STATUS": "paused"}
        result = self.run_budget("--thread", "thr-new", "--tokens", "9", expected=2, env=environment)
        self.assertIn("resume or start a new goal deliberately", result.stderr)
        self.assertNotIn("thread/goal/set", [item.get("method") for item in self.requests()])

    def test_discovers_newest_thread_and_excludes_inherited_thread(self) -> None:
        started = int(self.env["FAKE_CREATED_AT"])
        self.run_budget(
            "--discover-cwd", str(self.root), "--started-at", str(started),
            "--exclude-thread", "thr-old", "--tokens", "5", "--interval", "0.01",
        )
        goal_set = next(item for item in self.requests() if item.get("method") == "thread/goal/set")
        self.assertEqual(goal_set["params"]["threadId"], "thr-new")
        thread_list = next(item for item in self.requests() if item.get("method") == "thread/list")
        self.assertEqual(thread_list["params"]["sourceKinds"], ["appServer"])

    def test_discovery_times_out_without_new_thread(self) -> None:
        result = self.run_budget(
            "--discover-cwd", str(self.root), "--started-at", str(int(time.time()) + 60),
            "--discover-timeout", "0.03", "--interval", "0.01", "--tokens", "5", expected=2,
        )
        self.assertIn("no new Codex thread appeared", result.stderr)
        self.assertNotIn("thread/goal/set", [item.get("method") for item in self.requests()])

    def test_invalid_budget_is_rejected_before_starting_server(self) -> None:
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "--thread", "thr-new", "--tokens", "nope"],
            env=self.env, text=True, capture_output=True, check=False,
        )
        self.assertEqual(result.returncode, 2)
        self.assertFalse(self.codex_log.exists())

    def test_launcher_uses_only_an_explicit_budget(self) -> None:
        self.assertIsNone(agent_launch.configured_token_budget({}))
        self.assertIsNone(agent_launch.configured_token_budget({"HARNESS_BUDGET_TOKENS": "unknown"}))
        self.assertEqual(agent_launch.configured_token_budget({"HARNESS_BUDGET_TOKENS": "600"}), 600)
        with self.assertRaises(agent_launch.InputError):
            agent_launch.configured_token_budget({"HARNESS_BUDGET_TOKENS": "lots"})

    def test_launcher_refuses_to_promise_a_budget_for_direct_cli(self) -> None:
        self.assertTrue(agent_launch.command_runs_codex(["/opt/homebrew/bin/codex"]))
        self.assertFalse(agent_launch.command_runs_codex(["/usr/bin/other-agent"]))


if __name__ == "__main__":
    unittest.main()
