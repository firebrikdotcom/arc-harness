"""Offline regression tests for the TypeSafe task-entry boundary."""

from __future__ import annotations

import json
import hashlib
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / "scripts/harness"
METADATA = {
    "version": 1,
    "task_kind": "change",
    "area": "mobile",
    "proposed_action": "start_routine_agent",
    "reversibility": "reversible",
    "uncertainty_reason": "test_gap",
    "diff_size": "small",
    "changed_file_count": 2,
}

FAKE_ROUTER = '''
import json, os, sys
from pathlib import Path
state = json.load(sys.stdin)
Path(os.environ['FAKE_STATE_PATH']).write_text(json.dumps({'state': state, 'args': sys.argv[1:]}))
observed = os.environ.get('FAKE_RECOMMENDATION', 'proceed')
recommendation = 'reasoning_model' if '--shadow' in sys.argv else observed
print(json.dumps({'call_id': 'fixture-call', 'policy': {'recommendation': recommendation,
      'observed_recommendation': observed, 'reason': 'fixture'},
      'model': {'requested': 'fixture', 'returned': os.environ.get('FAKE_MODEL', 'fixture'), 'drift': False},
      'usage': {'input_tokens': 12, 'output_tokens': 2}, 'latency_ms': 5}))
'''


class TaskRoutingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="harness-task-route-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.metadata = self.root / "task.json"
        self.metadata.write_text(json.dumps(METADATA))
        self.router = self.root / "fake_router.py"
        self.router.write_text(FAKE_ROUTER)
        self.state_capture = self.root / "sent-state.json"
        self.db = self.root / "db"
        self.env = os.environ.copy()
        self.env.update({"FAKE_STATE_PATH": str(self.state_capture), "HARNESS_DB_ROOT": str(self.db), "TYPESAFE_MODEL": "fixture"})
        for key in ("HARNESS_TYPESAFE_ACTIVE", "HARNESS_TYPESAFE_OPERATOR_ACTIVATION", "HARNESS_TYPESAFE_ROLLOUT_PERCENT", "HARNESS_AUDIT_ENABLED", "HARNESS_AUDIT_URL"):
            self.env.pop(key, None)

    def run_cli(self, *args: str, expected: int = 0, env: dict | None = None) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            [str(CLI), *args], cwd=self.root, env=env or self.env,
            text=True, capture_output=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
        return result

    def route(self, *, mode: str = "shadow", expected: int = 0, env: dict | None = None) -> subprocess.CompletedProcess[str]:
        return self.run_cli(
            "route", "--state", str(self.metadata), "--project", str(self.root),
            "--router", str(self.router), "--mode", mode, expected=expected, env=env,
        )

    def eligible_env(self) -> dict[str, str]:
        logs = self.root / "typesafe-logs"
        logs.mkdir()
        (logs / "2026-09-19.jsonl").write_text(
            "".join(json.dumps({"call_id": f"call-{i}", "shadow": True, "exit_code": 0,
                                "kind": "task_entry", "model_requested": "fixture", "model_returned": "fixture",
                                "policy_version": hashlib.sha256(self.router.read_bytes() + (ROOT / "scripts/task_route.py").read_bytes()).hexdigest(),
                                "policy": {"observed_recommendation": "proceed"}}) + "\n"
                    for i in range(30))
        )
        (logs / "outcomes.jsonl").write_text(
            "".join(json.dumps({"call_id": f"call-{i}", "outcome": "correct"}) + "\n" for i in range(30))
        )
        return {**self.env, "HARNESS_TYPESAFE_ACTIVE": "1", "TYPESAFE_LOG_DIR": str(logs)}

    def command_file(self, name: str, marker: str) -> Path:
        path = self.root / name
        path.write_text(json.dumps([sys.executable, "-c", f"print('{marker}')"]))
        return path

    def test_rejects_raw_prompt_and_does_not_call_router(self) -> None:
        data = {**METADATA, "prompt": "private user request"}
        self.metadata.write_text(json.dumps(data))
        result = self.route(expected=2)
        self.assertIn("unknown fields", result.stderr)
        self.assertFalse(self.state_capture.exists())

    def test_deterministic_failure_bypasses_api(self) -> None:
        self.metadata.write_text(json.dumps({**METADATA, "known_failures": 1}))
        record = json.loads(self.route().stdout)
        self.assertEqual((record["source"], record["recommendation"]), ("deterministic", "reasoning_model"))
        self.assertFalse(self.state_capture.exists())

    def test_required_check_and_authorization_cannot_be_waived(self) -> None:
        self.metadata.write_text(json.dumps({**METADATA, "required_checks_pending": True}))
        self.assertEqual(json.loads(self.route().stdout)["recommendation"], "targeted_check")
        self.metadata.write_text(json.dumps({**METADATA, "approval_required": True}))
        self.assertEqual(json.loads(self.route().stdout)["recommendation"], "ask_user")
        self.assertFalse(self.state_capture.exists())

    def test_shadow_calls_router_without_sending_prompt_and_uses_default(self) -> None:
        record = json.loads(self.route().stdout)
        self.assertEqual((record["source"], record["recommendation"]), ("typesafe", "default"))
        self.assertEqual(record["observed_recommendation"], "proceed")
        sent = json.loads(self.state_capture.read_text())
        self.assertIn("--strict", sent["args"])
        self.assertIn("--shadow", sent["args"])
        self.assertEqual(sent["state"]["task"], {"kind": "change", "area": "mobile"})
        self.assertNotIn("prompt", json.dumps(sent["state"]).lower())
        stored = Path(record["record_path"])
        self.assertTrue(stored.is_file())
        self.assertEqual(stored.stat().st_mode & 0o777, 0o600)

    def test_missing_router_falls_back_to_default(self) -> None:
        result = self.run_cli(
            "route", "--state", str(self.metadata), "--router", str(self.root / "missing.py"),
        )
        record = json.loads(result.stdout)
        self.assertEqual((record["source"], record["recommendation"]), ("fallback", "default"))

    def test_active_requires_opt_in_and_real_outcomes(self) -> None:
        self.assertIn("active routing is gated", self.route(mode="active", expected=2).stderr)
        environment = self.eligible_env()
        record = json.loads(self.route(mode="active", env=environment).stdout)
        self.assertEqual((record["source"], record["recommendation"]), ("typesafe", "proceed"))
        self.assertEqual((record["routing_mode"], record["rollout_percent"], record["rollout_selected"]), ("active", 100, True))
        self.assertNotIn("--shadow", json.loads(self.state_capture.read_text())["args"])
        with (Path(environment["TYPESAFE_LOG_DIR"]) / "outcomes.jsonl").open("a") as stream:
            stream.write(json.dumps({"outcome": "under_escalated"}) + "\n")
        self.assertIn("active routing is gated", self.route(mode="active", env=environment, expected=2).stderr)

    def test_operator_activation_allows_full_rollout_with_explicit_acknowledgement(self) -> None:
        environment = {**self.env, "HARNESS_TYPESAFE_ACTIVE": "1", "HARNESS_TYPESAFE_OPERATOR_ACTIVATION": "1"}
        record = json.loads(self.route(mode="active", env=environment).stdout)
        self.assertEqual((record["routing_mode"], record["rollout_percent"], record["rollout_selected"]), ("active", 100, True))
        self.assertTrue(record["operator_activation"])

    def test_active_rollout_holdback_keeps_normal_path_and_records_cohort(self) -> None:
        environment = self.eligible_env()
        environment["HARNESS_TYPESAFE_ROLLOUT_PERCENT"] = "0"
        record = json.loads(self.route(mode="active", env=environment).stdout)
        self.assertEqual((record["source"], record["recommendation"]), ("typesafe", "default"))
        self.assertEqual((record["routing_mode"], record["rollout_percent"], record["rollout_selected"]), ("shadow", 0, False))
        self.assertIn("--shadow", json.loads(self.state_capture.read_text())["args"])

    def test_invalid_rollout_percentage_is_rejected(self) -> None:
        environment = {**self.env, "HARNESS_TYPESAFE_ROLLOUT_PERCENT": "101"}
        result = self.route(env=environment, expected=2)
        self.assertIn("HARNESS_TYPESAFE_ROLLOUT_PERCENT", result.stderr)

    def test_launcher_keeps_default_in_shadow_then_selects_routine_in_active(self) -> None:
        default = self.command_file("default.json", "DEFAULT")
        routine = self.command_file("routine.json", "ROUTINE")
        common = (
            "launch", "--state", str(self.metadata), "--project", str(self.root),
            "--router", str(self.router), "--default-command", str(default),
            "--routine-command", str(routine),
        )
        self.assertEqual(self.run_cli(*common).stdout.strip(), "DEFAULT")
        self.assertEqual(self.run_cli(*common, "--mode", "active", env=self.eligible_env()).stdout.strip(), "ROUTINE")

    def test_launcher_active_rollout_holdback_keeps_default(self) -> None:
        default = self.command_file("default.json", "DEFAULT")
        routine = self.command_file("routine.json", "ROUTINE")
        common = (
            "launch", "--state", str(self.metadata), "--project", str(self.root),
            "--router", str(self.router), "--default-command", str(default),
            "--routine-command", str(routine), "--mode", "active",
        )
        environment = self.eligible_env()
        environment["HARNESS_TYPESAFE_ROLLOUT_PERCENT"] = "0"
        self.assertEqual(self.run_cli(*common, env=environment).stdout.strip(), "DEFAULT")

    def test_launcher_never_uses_shell_interpolation(self) -> None:
        marker = self.root / "would-have-run"
        default = self.root / "literal.json"
        default.write_text(json.dumps([sys.executable, "-c", "import sys; print(sys.argv[1])", f"$(touch {marker})"]))
        output = self.run_cli(
            "launch", "--state", str(self.metadata), "--router", str(self.router),
            "--default-command", str(default),
        ).stdout.strip()
        self.assertEqual(output, f"$(touch {marker})")
        self.assertFalse(marker.exists())

    def test_launcher_stops_on_user_owned_input(self) -> None:
        self.metadata.write_text(json.dumps({**METADATA, "approval_required": True}))
        default = self.command_file("default.json", "SHOULD_NOT_RUN")
        result = self.run_cli(
            "launch", "--state", str(self.metadata), "--default-command", str(default), expected=3,
        )
        self.assertFalse(json.loads(result.stdout)["launched"])
        self.assertNotIn("SHOULD_NOT_RUN", result.stdout)

    def test_agent_name_supplies_default_command_without_launching(self) -> None:
        result = self.run_cli(
            "launch", "--state", str(self.metadata), "--router", str(self.router),
            "--agent", "codex", "--dry-run",
        )
        output = json.loads(result.stdout)
        self.assertEqual(output["command"], ["codex"])
        self.assertFalse(output["launched"])

    def test_codex_launch_refuses_explicit_unenforceable_token_cap(self) -> None:
        environment = {**self.env, "HARNESS_BUDGET_TOKENS": "40000"}
        result = self.run_cli(
            "launch", "--state", str(self.metadata), "--router", str(self.router),
            "--agent", "codex", "--project", str(self.root), expected=2, env=environment,
        )
        self.assertIn("cannot be interrupted through a separate App Server", result.stderr)


    def test_wrong_cohort_and_unversioned_evidence_cannot_activate(self):
        environment = self.eligible_env()
        calls = Path(environment["TYPESAFE_LOG_DIR"]) / "2026-09-19.jsonl"
        original = calls.read_text()
        for field, value in [("kind", "dynamic_advice"), ("model_returned", "old-model"),
                             ("model_requested", "old-model"), ("policy_version", "old-policy")]:
            rows = [json.loads(line) for line in original.splitlines()]
            for row in rows:
                row[field] = value
            calls.write_text("".join(json.dumps(row) + "\n" for row in rows))
            self.assertIn("active routing is gated", self.route(mode="active", env=environment, expected=2).stderr)
        calls.write_text(original)
        environment["FAKE_MODEL"] = "unexpected-model"
        record = json.loads(self.route(mode="active", env=environment).stdout)
        self.assertEqual(record["source"], "fallback")
        self.assertEqual(record["recommendation"], "default")

    def test_malformed_outcome_log_refuses_activation(self):
        environment = self.eligible_env()
        calls = Path(environment["TYPESAFE_LOG_DIR"]) / "2026-09-19.jsonl"
        calls.write_text("[]\n")
        result = self.route(mode="active", env=environment, expected=2)
        self.assertIn("cannot inspect outcome log", result.stderr)

    def test_explicit_choice_bypasses_jev(self):
        self.metadata.write_text(json.dumps({**METADATA, "user_choice_explicit": True}))
        self.assertEqual(json.loads(self.route().stdout)["source"], "deterministic")
        self.assertFalse(self.state_capture.exists())


if __name__ == "__main__":
    unittest.main()
