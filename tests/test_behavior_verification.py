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


if __name__ == "__main__":
    unittest.main()
