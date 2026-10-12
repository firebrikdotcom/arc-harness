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
        # The delegation switch under test is this temp dir's, never the machine's.
        self.config_home = self.root / "config"
        self.env.update({"FAKE_STATE_PATH": str(self.state_capture), "HARNESS_DB_ROOT": str(self.db), "TYPESAFE_MODEL": "fixture",
                         "HARNESS_CONFIG_HOME": str(self.config_home)})
        # Nothing here may depend on the caller's shell: not the machine's activation flags,
        # and not the native session identity, which would move run and record paths.
        for key in ("HARNESS_TYPESAFE_ACTIVE", "HARNESS_TYPESAFE_OPERATOR_ACTIVATION", "HARNESS_TYPESAFE_ROLLOUT_PERCENT",
                    "HARNESS_AUDIT_ENABLED", "HARNESS_AUDIT_URL", "HARNESS_JEV_DELEGATION",
                    "HARNESS_SESSION_ID", "CODEX_THREAD_ID", "CLAUDE_SESSION_ID", "CLAUDE_CODE_SESSION_ID"):
            self.env.pop(key, None)

    def switch(self, action: str, *extra: str, env: dict | None = None) -> dict:
        return json.loads(self.run_cli("jev", action, "--json", *extra, env=env).stdout)

    def run_cli(self, *args: str, expected: int = 0, env: dict | None = None) -> subprocess.CompletedProcess[str]:
        result = subprocess.run(
            [str(CLI), *args], cwd=self.root, env=env or self.env,
            text=True, capture_output=True, check=False,
        )
        self.assertEqual(result.returncode, expected, result.stderr + result.stdout)
        return result

    def route(self, *, mode: str | None = None, expected: int = 0, env: dict | None = None) -> subprocess.CompletedProcess[str]:
        # No --mode means the switch decides, as the hook and a plain `harness route` do.
        return self.run_cli(
            "route", "--state", str(self.metadata), "--project", str(self.root),
            "--router", str(self.router), *(("--mode", mode) if mode else ()), expected=expected, env=env,
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

    def write_run(self, run_id: str, status: str, loops: int, events: list[str]) -> None:
        run = self.db / "runs" / run_id
        run.mkdir(parents=True)
        (run / "state").write_text(f"RUN_ID={run_id}\nRUN_STATUS={status}\nLOOPS_USED={loops}\n")
        (run / "log").write_text("".join(f"2026-09-29T00:00:00Z\tbuild\t{event}\n" for event in events))

    def test_history_facts_are_bucketed_enums_without_paths(self) -> None:
        verified = ["plan start", "plan done", "build start",
                    f"build gate: verify record at 2026-09-29T00:00:00Z head={'a' * 40}", "build done"]
        self.write_run("20260901T000000Z-1", "complete", 0, verified)
        self.write_run("20260902T000000Z-2", "complete", 2, verified)
        self.write_run("20260903T000000Z-3", "aborted", 1, ["plan start", "plan done", "build start", "abort"])
        self.write_run("20260904T000000Z-4", "active", 0, ["plan start", "step private note about /secret/path"])
        (self.db / "runs" / "current").write_text("20260904T000000Z-4\n")
        (self.db / "records").mkdir()
        (self.db / "records" / "verify.state").write_text(f"RECORD_KIND=verify\nPROJECT_ROOT={self.root}\nEXIT=1\n")
        record = json.loads(self.route().stdout)
        sent = json.loads(self.state_capture.read_text())["state"]
        expected = {"prior_runs": "few", "verified_builds": "few", "unverified_builds": "one",
                    "runs_with_loops": "few", "total_loops": "few", "aborted_runs": "one", "last_verify": "failed"}
        self.assertEqual(sent["signals"]["history"], expected)
        self.assertEqual(record["history"], expected)
        encoded = json.dumps(sent)
        for leaked in (str(self.root), "secret", "20260904T000000Z-4", "a" * 40):
            self.assertNotIn(leaked, encoded)

    def test_history_comes_from_the_project_target_database(self) -> None:
        target = self.root / "target"
        target.mkdir()
        registered = subprocess.run([str(ROOT / "scripts/harness-target.sh"), "register", str(target)],
                                    env=self.env, text=True, capture_output=True, check=True).stdout.strip()
        run = Path(registered) / "db" / "runs" / "20260901T000000Z-1"
        run.mkdir(parents=True)
        (run / "state").write_text("RUN_STATUS=complete\nLOOPS_USED=0\n")
        (run / "log").write_text("2026-09-01T00:00:00Z\tbuild\tbuild start\n")
        # Run from outside the target, as the documented launcher form does.
        self.run_cli("route", "--state", str(self.metadata), "--project", str(target), "--router", str(self.router))
        history = json.loads(self.state_capture.read_text())["state"]["signals"]["history"]
        self.assertEqual((history["prior_runs"], history["unverified_builds"]), ("one", "one"))
        self.assertFalse((self.db / "runs").exists())

    def test_history_facts_for_a_new_target_are_none(self) -> None:
        self.route()
        history = json.loads(self.state_capture.read_text())["state"]["signals"]["history"]
        self.assertEqual(set(history.values()), {"none"})
        self.assertEqual(len(history), 7)

    def test_history_window_and_buckets(self) -> None:
        import importlib.util
        spec = importlib.util.spec_from_file_location("task_route_under_test", ROOT / "scripts/task_route.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual([module.count_bucket(n) for n in (0, 1, 2, 5, 6)], ["none", "one", "few", "few", "many"])
        for index in range(module.HISTORY_WINDOW + 5):
            self.write_run(f"202609{index:04d}T000000Z-{index}", "complete", 0, ["build start"])
        history = module.history_facts(self.db)
        self.assertEqual((history["prior_runs"], history["unverified_builds"], history["last_verify"]), ("many", "many", "none"))

    def test_deterministic_route_does_not_read_history(self) -> None:
        self.metadata.write_text(json.dumps({**METADATA, "known_failures": 1}))
        self.assertNotIn("history", json.loads(self.route().stdout))

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

    def test_switch_off_by_default_routes_in_shadow(self):
        self.assertEqual((self.switch("status")["enabled"], self.switch("status")["source"]), (False, "default"))
        record = json.loads(self.route().stdout)
        self.assertEqual((record["mode"], record["mode_source"], record["delegation"]), ("shadow", "default", False))

    def test_switch_on_routes_active_and_persists(self):
        state = self.switch("on", "--reason", "pilot reviewed")
        self.assertEqual((state["enabled"], state["mode"], state["source"], state["model"]), (True, "active", "file", "fixture"))
        stored = self.config_home / "jev-delegation.json"
        self.assertTrue(stored.is_file())
        self.assertEqual(stored.stat().st_mode & 0o777, 0o600)
        self.assertEqual(json.loads(stored.read_text())["reason"], "pilot reviewed")
        record = json.loads(self.route().stdout)
        self.assertEqual((record["source"], record["recommendation"], record["routing_mode"]), ("typesafe", "proceed", "active"))
        self.assertEqual((record["mode_source"], record["delegation"], record["rollout_selected"]), ("delegation", True, True))
        self.assertNotIn("--shadow", json.loads(self.state_capture.read_text())["args"])
        # A fresh shell sees the same switch: it lives in the file, not the environment.
        self.assertTrue(self.switch("status")["enabled"])
        self.assertFalse(self.switch("off")["enabled"])
        self.assertEqual(json.loads(self.route().stdout)["routing_mode"], "shadow")

    def test_switch_pins_the_model_the_router_requests(self):
        self.switch("on", "--model", "jev-9.9.9")
        env = {k: v for k, v in self.env.items() if k != "TYPESAFE_MODEL"}
        self.router.write_text(FAKE_ROUTER.replace("'returned': os.environ.get('FAKE_MODEL', 'fixture')",
                                                   "'returned': os.environ.get('TYPESAFE_MODEL', 'unset')"))
        record = json.loads(self.route(env=env).stdout)
        self.assertEqual((record["routing_mode"], record["model"]["returned"]), ("active", "jev-9.9.9"))
        latest = self.run_cli("jev", "on", "--model", "jev-latest", expected=2)
        self.assertIn("exact", latest.stderr)

    def test_switch_on_without_an_exact_pin_holds_back_to_shadow_and_says_why(self):
        self.config_home.mkdir()
        (self.config_home / "jev-delegation.json").write_text(json.dumps({"enabled": True, "model": None}))
        env = {k: v for k, v in self.env.items() if k != "TYPESAFE_MODEL"}
        record = json.loads(self.route(env=env).stdout)
        self.assertEqual((record["mode"], record["routing_mode"], record["recommendation"]), ("active", "shadow", "default"))
        self.assertIn("exact model pin", record["delegation_gate"])
        self.assertIn("--shadow", json.loads(self.state_capture.read_text())["args"])
        # Asked for explicitly, the same gap is an error rather than a quiet holdback.
        self.assertIn("active routing is gated", self.route(mode="active", env=env, expected=2).stderr)

    def test_environment_and_flag_override_the_switch(self):
        self.switch("on")
        env = {**self.env, "HARNESS_JEV_DELEGATION": "off"}
        self.assertEqual(self.switch("status", env=env)["source"], "environment")
        self.assertEqual(json.loads(self.route(env=env).stdout)["routing_mode"], "shadow")
        record = json.loads(self.route(mode="shadow").stdout)
        self.assertEqual((record["routing_mode"], record["mode_source"]), ("shadow", "flag"))
        bad = self.route(env={**self.env, "HARNESS_JEV_DELEGATION": "maybe"}, expected=2)
        self.assertIn("HARNESS_JEV_DELEGATION", bad.stderr)

    def test_corrupt_switch_file_is_an_input_error(self):
        self.config_home.mkdir()
        (self.config_home / "jev-delegation.json").write_text("{not json")
        self.assertIn("delegation switch", self.route(expected=2).stderr)
        self.assertIn("delegation switch", self.run_cli("jev", "status", expected=2).stderr)

    def test_deterministic_gates_still_decide_with_the_switch_on(self):
        self.switch("on")
        for field in ("approval_required", "required_checks_pending", "user_choice_explicit"):
            self.metadata.write_text(json.dumps({**METADATA, field: True}))
            self.assertEqual(json.loads(self.route().stdout)["source"], "deterministic")
        self.metadata.write_text(json.dumps({**METADATA, "reversibility": "irreversible"}))
        self.assertEqual(json.loads(self.route().stdout)["recommendation"], "reasoning_model")
        self.assertFalse(self.state_capture.exists())

    def test_launcher_follows_jev_when_the_switch_is_on(self):
        default = self.command_file("default.json", "DEFAULT")
        routine = self.command_file("routine.json", "ROUTINE")
        common = (
            "launch", "--state", str(self.metadata), "--project", str(self.root),
            "--router", str(self.router), "--default-command", str(default),
            "--routine-command", str(routine),
        )
        self.assertEqual(self.run_cli(*common).stdout.strip(), "DEFAULT")
        self.switch("on")
        self.assertEqual(self.run_cli(*common).stdout.strip(), "ROUTINE")
        self.assertEqual(self.run_cli(*common, "--mode", "shadow").stdout.strip(), "DEFAULT")
        self.switch("off")
        self.assertEqual(self.run_cli(*common).stdout.strip(), "DEFAULT")


if __name__ == "__main__":
    unittest.main()
