"""Configuration migration, shared writers, and real no-call semantics."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import harness_config as config
import audit_transport
import context_advice


class ConfigTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.home = Path(self.temp.name)
        self.env = patch.dict(os.environ, {"HARNESS_CONFIG_HOME": str(self.home),
                                          "HARNESS_AUDIT_ENABLED": "1", "HARNESS_JEV_DELEGATION": "off"})
        self.env.start()
        self.addCleanup(self.env.stop)
        self.addCleanup(self.temp.cleanup)
        for key in ("HARNESS_CONFIG", "HARNESS_AUDIT_SETTINGS"):
            os.environ.pop(key, None)

    def test_migration_preserves_pin_mode_and_documented_toggles(self):
        (self.home / "jev-delegation.json").write_text(json.dumps({"enabled": True, "model": "fixture", "reason": "existing"}))
        config.update()
        value = json.loads(config.path().read_text())
        self.assertEqual(value["typesafe"]["mode"], "active")
        self.assertEqual(value["typesafe"]["model"], "fixture")
        self.assertFalse(value["dispatch"]["enabled"])
        self.assertIn("_doc", value["dispatch"])
        self.assertEqual(config.path().stat().st_mode & 0o777, 0o600)
        (self.home / "jev-delegation.json").write_text("broken backup")
        self.assertEqual(config.load(), value)

    def test_concurrent_section_writes_preserve_other_switches_and_docs(self):
        config.update()
        commands = [
            [sys.executable, str(ROOT / "scripts/harness_config.py"), "update", "--section", section, "--values", json.dumps(values)]
            for section, values in [("typesafe", {"mode": "off"}), ("dispatch", {"enabled": True}),
                                    ("telemetry", {"workflow_prompts": True})]
        ]
        with concurrent.futures.ThreadPoolExecutor() as pool:
            results = list(pool.map(lambda cmd: subprocess.run(cmd, capture_output=True), commands))
        self.assertTrue(all(result.returncode == 0 for result in results))
        value = config.load()
        self.assertEqual(value["typesafe"]["mode"], "off")
        self.assertTrue(value["dispatch"]["enabled"])
        self.assertTrue(value["telemetry"]["workflow_prompts"])
        self.assertIn("_doc", value["telemetry"])

    def test_disabled_typesafe_never_calls_router_even_with_active_flag(self):
        config.update("typesafe", {"mode": "off", "model": "fixture"})
        task = self.home / "task.json"
        task.write_text(json.dumps({"version": 1, "task_kind": "change", "area": "backend",
                                   "proposed_action": "start_routine_agent", "reversibility": "reversible",
                                   "uncertainty_reason": "test_gap"}))
        from unittest.mock import Mock
        import task_route
        with patch.object(task_route, "invoke_router", side_effect=AssertionError("router invoked")), patch.object(task_route, "audit_route_record"):
            result = task_route.route_task(task, self.home, self.home / "db", Path("missing"), "active")
        self.assertEqual(result["routing_mode"], "off")
        router = Mock()
        router.pinned_model.return_value = "fixture"
        context = {"version": 2, "checkpoint": {"baseline_action": "keep", "bypass_reason": "none"},
                   "context": {}, "questions": {"recommendation": {"type": "choice", "criteria": {"keep": "keep", "change": "change"}}}}
        advice = context_advice.advise(context, router, self.home / "db", delegate=True)
        self.assertEqual(advice["action"], "keep")
        self.assertEqual(advice["status"], "bypassed")
        router.post_json.assert_not_called()

    def test_collection_master_switch_and_invalid_config(self):
        config.update("telemetry", {"workflow_prompts": True})
        self.assertTrue(audit_transport.enabled("workflow.prompt_recorded"))
        config.update("telemetry", {"enabled": False})
        self.assertFalse(audit_transport.enabled("workflow.prompt_recorded"))
        self.assertFalse(audit_transport.enabled("agent.completed"))
        config.path().write_text('{"version":1}')
        with self.assertRaises(config.ConfigError):
            audit_transport.enabled()


if __name__ == "__main__":
    unittest.main()
