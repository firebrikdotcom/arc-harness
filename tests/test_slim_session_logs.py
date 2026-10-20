"""Idle session logs lose their base64 images; everything else, and busy logs, stay as they are."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts/slim_session_logs.py"
PNG = "iVBORw0KGgo" + "A" * 40_000
THINKING = "E" * 20_000
HTML = '<img src="data:image/gif;base64,' + "R" * 20_000 + '">'

LINES = [
    {"type": "user", "message": {"content": [{"type": "tool_result", "content": [
        {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": PNG}}]}]}},
    {"type": "response_item", "payload": {"type": "message", "content": [
        {"type": "input_text", "text": "look"}, {"type": "input_image", "image_url": "data:image/png;base64," + PNG}]}},
    {"type": "response_item", "payload": {"output": [{"type": "image", "data": PNG, "mimeType": "image/png"}]}},
    {"type": "event_msg", "payload": {"item": {"kind": "image_gen.generation", "result": PNG,
                                               "savedPath": "/srv/codex/generated_images/s/exec-1.png"}}},
    {"type": "assistant", "message": {"content": [{"type": "thinking", "thinking": "x", "signature": THINKING}]}},
    {"type": "response_item", "payload": {"output": HTML}},
    {"type": "event_msg", "payload": {"type": "token_count"}},
]


class SlimTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.base = Path(temp.name)
        self.logs = self.base / "sessions"
        self.logs.mkdir()
        self.env = {**os.environ, "HARNESS_SESSION_SLIM_STATE": str(self.base / "state.json")}

    def write(self, name, age=3600):
        path = self.logs / name
        path.write_text("".join(json.dumps(line) + "\n" for line in LINES))
        stamp = time.time() - age
        os.utime(path, (stamp, stamp))
        return path

    def sweep(self, *args):
        result = subprocess.run([sys.executable, str(SCRIPT), "--root", str(self.logs), "--idle", "60", *args],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_images_become_notes_and_everything_else_is_kept(self):
        path = self.write("idle.jsonl", age=120)
        size = path.stat().st_size
        mtime = path.stat().st_mtime_ns
        totals = self.sweep("--all")
        self.assertEqual(totals["images"], 4)
        self.assertGreater(totals["bytes"], 150_000)
        text = path.read_text()
        self.assertNotIn("AAAAAAAAAA", text)
        self.assertLess(path.stat().st_size, size - 150_000)
        self.assertEqual(path.stat().st_mtime_ns, mtime)
        lines = [json.loads(line) for line in text.splitlines()]
        self.assertEqual(lines[0]["message"]["content"][0]["content"][0]["type"], "text")
        self.assertIn("image/jpeg", lines[0]["message"]["content"][0]["content"][0]["text"])
        self.assertEqual(lines[1]["payload"]["content"][1]["type"], "input_text")
        self.assertEqual(lines[1]["payload"]["content"][0], {"type": "input_text", "text": "look"})
        self.assertEqual(lines[2]["payload"]["output"][0]["type"], "text")
        generated = lines[3]["payload"]["item"]
        self.assertEqual(generated["savedPath"], "/srv/codex/generated_images/s/exec-1.png")
        self.assertIn("saved at /srv/codex/generated_images/s/exec-1.png", generated["result"])
        # Thinking signatures, file contents that mention a data URI, and small lines are untouched.
        self.assertEqual(lines[4], LINES[4])
        self.assertEqual(lines[5], LINES[5])
        self.assertEqual(lines[6], LINES[6])
        self.assertEqual(self.sweep("--all")["images"], 0)

    def test_busy_recent_and_preexisting_logs_are_left_alone(self):
        old = self.write("before-install.jsonl", age=7200)
        self.sweep()  # The first sweep fixes the "from now on" cutoff; move it back past the idle window.
        state = self.base / "state.json"
        state.write_text(json.dumps({**json.loads(state.read_text()), "since": time.time() - 3000}))
        recent = self.write("recent.jsonl", age=5)
        held = self.write("held.jsonl", age=600)
        sizes = {p: p.stat().st_size for p in (old, recent, held)}
        with held.open("a"):
            self.assertEqual(self.sweep()["images"], 0)
        self.assertEqual({p: p.stat().st_size for p in sizes}, sizes)
        # Once released, the held log written after the first sweep is slimmed; the older one only with --all.
        self.assertEqual(self.sweep()["files"], 1)
        self.assertEqual(old.stat().st_size, sizes[old])
        self.assertEqual(self.sweep("--all")["files"], 1)
        self.assertLess(old.stat().st_size, sizes[old])

    def test_dry_run_reports_without_changing_files(self):
        path = self.write("idle.jsonl")
        size = path.stat().st_size
        totals = self.sweep("--all", "--dry-run")
        self.assertEqual(totals["images"], 4)
        self.assertEqual(path.stat().st_size, size)
        self.assertFalse((self.base / "state.json").exists())

    def test_spawn_returns_at_once(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--spawn"], input="{}", env={**self.env, "HOME": str(self.base)},
                                text=True, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
