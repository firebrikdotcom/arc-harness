"""The UI's hook preferences control installed harness groups without replacing user hooks."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import harness_config as config


class HookPreferenceTests(unittest.TestCase):
    def test_each_event_has_one_collector_and_gate_only_disables_collection(self):
        for audit_enabled, gate_enabled in ((True, True), (True, False), (False, True), (False, False)):
            with self.subTest(audit=audit_enabled, gate=gate_enabled), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                env = os.environ | {
                    "HOME": str(root), "HARNESS_CONFIG_HOME": str(root / "config"),
                    "HARNESS_CONFIG": "", "HARNESS_AUDIT_SETTINGS": "",
                }
                subprocess.run(
                    [sys.executable, str(ROOT / "scripts/harness_config.py"), "update", "--section", "hooks", "--values",
                     json.dumps({"workflow_audit": audit_enabled, "workflow_gate": gate_enabled})],
                    env=env, check=True, capture_output=True,
                )
                paths = [root / "claude.json", root / "codex.json"]
                subprocess.run(
                    ["sh", str(ROOT / "scripts/install-hooks.sh"), "--claude", str(paths[0]), "--codex", str(paths[1]), "--root-file", str(root / "installed-root")],
                    env=env, check=True, capture_output=True,
                )
                for index, path in enumerate(paths):
                    hooks = json.loads(path.read_text())["hooks"]
                    events = ["UserPromptSubmit", "PreToolUse", "PostToolUse"]
                    if index == 0:
                        events.append("PostToolUseFailure")
                    for event in events:
                        commands = [hook["command"] for group in hooks.get(event, []) for hook in group["hooks"]
                                    if any(name in hook["command"] for name in ("workflow_audit.py", "workflow_gate.py"))]
                        self.assertEqual(len(commands), int(audit_enabled or gate_enabled), (path, event, commands))
                        if commands:
                            self.assertEqual("workflow_gate.py" in commands[0], gate_enabled)
                            self.assertEqual("HARNESS_AUDIT_ENABLED=0" in commands[0], not audit_enabled)

    def test_preferences_default_enabled_and_reject_invalid_values(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "config.json"
            defaults = config.load(file=path)["hooks"]
            self.assertTrue(all(value for key, value in defaults.items() if key != "_doc"))
            config.update("hooks", {"workflow_audit": True, "workflow_gate": False}, file=path)
            saved = config.load(file=path)["hooks"]
            self.assertTrue(saved["workflow_audit"])
            self.assertFalse(saved["workflow_gate"])
            with self.assertRaises(config.ConfigError):
                config.update("hooks", {"workflow_audit": "yes"}, file=path)

    def test_audit_only_preferences_install_only_workflow_audit_hooks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            home = root / "home"
            config_home = root / "config"
            claude = home / ".claude/settings.json"
            codex = home / ".codex/hooks.json"
            root_file = config_home / "harness/root"
            for path in (claude, codex):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(json.dumps({"hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "user-hook"}]}]}}))
            env = os.environ | {
                "HOME": str(home),
                "HARNESS_CONFIG_HOME": str(config_home),
                "HARNESS_CONFIG": "",
                "HARNESS_AUDIT_SETTINGS": "",
            }
            settings = {
                "session_startup": False,
                "workflow_audit": True,
                "workflow_gate": False,
                "command_observer": False,
                "failure_tracking": False,
                "worktree_cleanup": False,
                "session_log_cleanup": False,
                "retrieval_reminder": False,
            }
            subprocess.run(
                [sys.executable, str(ROOT / "scripts/harness_config.py"), "update", "--section", "hooks", "--values", json.dumps(settings)],
                env=env,
                check=True,
                capture_output=True,
                text=True,
            )
            subprocess.run(
                ["sh", str(ROOT / "scripts/install-hooks.sh"), "--claude", str(claude), "--codex", str(codex), "--root-file", str(root_file)],
                env=env,
                check=True,
                capture_output=True,
                text=True,
            )

            for path in (claude, codex):
                data = json.loads(path.read_text())
                commands = [hook["command"] for groups in data["hooks"].values() for group in groups for hook in group["hooks"]]
                self.assertTrue(any("workflow_audit.py" in command for command in commands), path)
                disabled = (
                    "workflow_gate.py",
                    "session_hook.py",
                    "observe_commands.py",
                    "failure_budget.py",
                    "worktree_cleanup.py",
                    "slim_session_logs.py",
                    "retrieval-reminder.sh",
                )
                self.assertFalse(any(marker in command for marker in disabled for command in commands), path)
                self.assertTrue(any(command == "user-hook" for command in commands), path)


if __name__ == "__main__":
    unittest.main()
