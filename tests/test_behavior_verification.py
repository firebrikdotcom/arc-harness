import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location("behavior", Path(__file__).resolve().parents[1] / "scripts/behavior_verification.py")
behavior = importlib.util.module_from_spec(spec)
spec.loader.exec_module(behavior)


class ValidationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "features").mkdir()
        (self.root / "guide.md").write_text("Guide")
        (self.root / "features/index.md").write_text("[Example](example.md)")
        (self.root / "features/example.md").write_text("Journey")
        (self.root / "requirements.md").write_text("REQ-1")
        self.data = {"version": 1, "enabled": True, "guide": "guide.md", "index": "features/index.md", "features": [{"id": "example", "document": "features/example.md", "references": [{"path": "requirements.md", "id": "REQ-1"}]}]}

    def save(self):
        (self.root / ".harness-verification.json").write_text(json.dumps(self.data))

    def test_disabled_and_missing(self):
        self.assertEqual(behavior.validate(self.root), "not enabled")
        self.data = {"enabled": False}
        self.save()
        self.assertEqual(behavior.validate(self.root), "disabled")

    def test_valid_and_duplicate(self):
        self.save()
        self.assertEqual(behavior.validate(self.root), "validated 1 features")
        self.data["features"].append(self.data["features"][0])
        self.save()
        with self.assertRaisesRegex(behavior.Invalid, "duplicate"):
            behavior.validate(self.root)

    def test_unsafe_paths_and_missing_reference(self):
        for path in ("../outside", "/etc/passwd", "features/../../outside"):
            self.data["guide"] = path
            self.save()
            with self.assertRaises(behavior.Invalid):
                behavior.validate(self.root)
        self.data["guide"] = "guide.md"
        self.data["features"][0]["references"][0]["id"] = "REQ-2"
        self.save()
        with self.assertRaisesRegex(behavior.Invalid, "not found"):
            behavior.validate(self.root)

    def test_symlink_escape_and_dead_index(self):
        (self.root / "escape").symlink_to("/etc/passwd")
        self.data["guide"] = "escape"
        self.save()
        with self.assertRaises(behavior.Invalid):
            behavior.validate(self.root)
        self.data["guide"] = "guide.md"
        (self.root / "features/index.md").write_text("no link")
        self.save()
        with self.assertRaisesRegex(behavior.Invalid, "index"):
            behavior.validate(self.root)

    def test_directory_manifest_and_nul_path_rejected(self):
        (self.root / ".harness-verification.json").mkdir()
        with self.assertRaisesRegex(behavior.Invalid, "not a file"):
            behavior.validate(self.root)
        (self.root / ".harness-verification.json").rmdir()
        self.data["guide"] = "guide\x00.md"
        self.save()
        with self.assertRaisesRegex(behavior.Invalid, "path"):
            behavior.validate(self.root)

    def test_oversized_manifest(self):
        (self.root / ".harness-verification.json").write_text(" " * (behavior.MAX_BYTES + 1))
        with self.assertRaisesRegex(behavior.Invalid, "exceeds"):
            behavior.validate(self.root)


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "proof.txt").write_text("observed")
        self.receipt = {"attempt": "a", "run": "r", "project": "/project", "tree": "abc", "definition": "def", "features": ["example"], "evidence_dir": str(self.root)}
        self.report = {"version": 1, **{k: v for k, v in self.receipt.items() if k != "evidence_dir"}, "outcome": "passed", "started_at": "2026-11-05T12:00:00Z", "ended_at": "2026-11-05T12:01:00Z", "source_build": "build", "runtime_build": "build", "platform": "test", "configuration": "isolated", "test_data": "fixture", "readiness": [{"description": "ready", "observed": "yes"}], "actions": [{"action": "type", "expected": "receipt", "observed": "receipt"}], "cleanup": [{"description": "fixture", "observed": "removed"}], "artifacts": ["proof.txt"], "reason": ""}

    def test_hash_and_integrity(self):
        first = behavior.validate_report(self.report, self.receipt)
        self.assertEqual(first[0]["size"], 8)
        (self.root / "proof.txt").write_text("changed")
        self.assertNotEqual(first, behavior.validate_report(self.report, self.receipt))

    def test_binding_and_pass_requirements(self):
        for field, value in (("run", "other"), ("runtime_build", "other"), ("cleanup", []), ("artifacts", [])):
            original = self.report[field]
            self.report[field] = value
            with self.assertRaises(behavior.Invalid, msg=field):
                behavior.validate_report(self.report, self.receipt)
            self.report[field] = original
        self.report["outcome"] = "blocked"
        self.report["reason"] = "device unavailable"
        self.report["artifacts"] = []
        self.assertEqual(behavior.validate_report(self.report, self.receipt), [])

    def test_artifact_escape(self):
        (self.root / "escape").symlink_to("/etc/passwd")
        for path in ("../etc/passwd", "/etc/passwd", "escape"):
            self.report["artifacts"] = [path]
            with self.assertRaises((behavior.Invalid, OSError), msg=path):
                behavior.validate_report(self.report, self.receipt)


if __name__ == "__main__":
    unittest.main()
