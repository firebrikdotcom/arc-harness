import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))

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

    def test_nonpassing_and_unknown_build_cannot_become_passes(self):
        for outcome in ("failed", "blocked", "inconclusive"):
            self.report["outcome"] = outcome
            self.report["reason"] = "not complete"
            behavior.validate_report(self.report, self.receipt)
        self.report["outcome"] = "passed"
        for value in ("none", "unknown"):
            self.report["source_build"] = value
            self.report["runtime_build"] = value
            with self.assertRaises(behavior.Invalid):
                behavior.validate_report(self.report, self.receipt)

    def test_artifact_escape(self):
        (self.root / "escape").symlink_to("/etc/passwd")
        for path in ("../etc/passwd", "/etc/passwd", "escape"):
            self.report["artifacts"] = [path]
            with self.assertRaises((behavior.Invalid, OSError), msg=path):
                behavior.validate_report(self.report, self.receipt)


if __name__ == "__main__":
    unittest.main()

class AttemptTests(unittest.TestCase):
    def setUp(self):
        ValidationTests.setUp(self)
        import os
        import subprocess
        from unittest.mock import patch
        ValidationTests.save(self)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        (self.root / ".gitignore").write_text("evidence/\n")
        (self.root / "evidence").mkdir()
        self.run_dir = self.root / ".run"
        self.run_dir.mkdir()
        (self.root / ".gitignore").write_text("evidence/\n.run/\n")
        env = {"HARNESS_BEHAVIOR_TARGET": str(self.root), "HARNESS_BEHAVIOR_RUN_ID": "run", "HARNESS_BEHAVIOR_RUN_DIR": str(self.run_dir)}
        self.patch = patch.dict(os.environ, env)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def test_receipt_integrity_and_staleness(self):
        from datetime import datetime, timezone
        directory = self.root / "evidence" / "attempt"
        attempt = behavior.begin(self.root, ["example"], str(directory))
        receipt = json.loads((self.run_dir / "behavior" / (attempt + ".json")).read_text())["receipt"]
        (directory / "proof").write_text("proof")
        now = datetime.now(timezone.utc).isoformat()
        report = {"version": 1, **{k: receipt[k] for k in ("attempt", "run", "project", "tree", "definition", "features")}, "outcome": "passed", "started_at": now, "ended_at": now, "source_build": "build", "runtime_build": "build", "platform": "test", "configuration": "test", "test_data": "fixture", "readiness": [{"description": "ready", "observed": "yes"}], "actions": [{"action": "test", "expected": "yes", "observed": "yes"}], "cleanup": [{"description": "fixture", "observed": "removed"}], "artifacts": ["proof"], "reason": ""}
        report_path = directory / "report.json"
        report_path.write_text(json.dumps(report))
        behavior.record(self.root, attempt, report_path)
        behavior.check(self.root, ["example"])
        (directory / "proof").write_text("changed")
        with self.assertRaises(behavior.Invalid):
            behavior.check(self.root, ["example"])
        (directory / "proof").write_text("proof")
        (self.root / "guide.md").write_text("changed")
        with self.assertRaises(behavior.Invalid):
            behavior.check(self.root, ["example"])

    def test_rejects_denied_evidence_directories(self):
        (self.root / ".harness-denylist").write_text("path ^evidence/private(/|$)\n")
        (self.root / "evidence/private").mkdir()
        with self.assertRaisesRegex(behavior.Invalid, "protected"):
            behavior.begin(self.root, ["example"], str(self.root / "evidence/private/attempt"))

    def test_rejects_unowned_and_unignored_directories(self):
        with self.assertRaises(behavior.Invalid):
            behavior.begin(self.root, ["example"], str(self.root / "unignored"))
        with self.assertRaises(behavior.Invalid):
            behavior.begin(self.root, ["example"], str(self.root / "evidence"))

class ReviewFindingTests(unittest.TestCase):
    def test_optional_evidence_labels_and_legacy_findings(self):
        spec = importlib.util.spec_from_file_location("review_findings", Path(__file__).resolve().parents[1] / "scripts/review_findings.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        finding = {"severity": "minor", "file": "x", "claim": "claim", "evidence": "evidence"}
        data = {"tree_hash": "abc", "reviewer": "independent", "verdict": "approve", "findings": [finding]}
        module.validate(data)
        for kind in ("reproduced", "source_analysis", "unverified_concern"):
            finding["evidence_kind"] = kind
            module.validate(data)
        finding["evidence_kind"] = "invented"
        with self.assertRaises(ValueError):
            module.validate(data)


class CliTests(AttemptTests):
    def test_cli_records_and_rechecks_without_driving_project(self):
        import os
        import subprocess
        from datetime import datetime, timezone
        script = str(Path(__file__).resolve().parents[1] / "scripts/behavior_verification.py")
        def run(*args):
            return subprocess.run([sys.executable, script, *args, "--project", str(self.root)],
                                  text=True, capture_output=True, env=os.environ.copy())
        evidence = self.root / "evidence/cli"
        begin = run("begin", "--feature", "example", "--evidence-dir", str(evidence))
        self.assertEqual(begin.returncode, 0, begin.stderr)
        attempt = begin.stdout.strip()
        receipt = json.loads((self.run_dir / "behavior" / (attempt + ".json")).read_text())["receipt"]
        (evidence / "proof").write_text("observed")
        now = datetime.now(timezone.utc).isoformat()
        report = {"version": 1, **{k: receipt[k] for k in ("attempt", "run", "project", "tree", "definition", "features")},
                  "outcome": "passed", "started_at": now, "ended_at": now, "source_build": "build",
                  "runtime_build": "build", "platform": "fixture", "configuration": "isolated", "test_data": "owned",
                  "readiness": [{"description": "ready", "observed": "yes"}],
                  "actions": [{"action": "inspect", "expected": "yes", "observed": "yes"}],
                  "cleanup": [{"description": "fixture", "observed": "removed"}], "artifacts": ["proof"], "reason": ""}
        report_file = evidence / "report.json"
        report_file.write_text(json.dumps(report))
        self.assertEqual(run("record", "--attempt", attempt, "--report", str(report_file)).returncode, 0)
        self.assertEqual(run("check", "--features", "example").returncode, 0)
        (evidence / "proof").write_text("modified")
        self.assertNotEqual(run("check", "--features", "example").returncode, 0)

    def test_cli_rejects_wrong_project_and_unknown_feature(self):
        import subprocess
        script = str(Path(__file__).resolve().parents[1] / "scripts/behavior_verification.py")
        result = subprocess.run([sys.executable, script, "check", "--project", str(self.root),
                                 "--features", "unknown"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("registered IDs", result.stderr)
