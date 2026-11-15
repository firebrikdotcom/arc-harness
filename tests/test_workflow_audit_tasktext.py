"""Task-manager text inside its fence never reaches a Workflow audit event."""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import workflow_audit

SECRET = "Basecamp fixture: migrate the ledger for Açaí Ltd 🐈"
CLOSE = "<<<END-TASK-MANAGER-UNTRUSTED>>>"


def fence(inner, header=None):
    raw = inner.encode()
    header = header or f"sha256={hashlib.sha256(raw).hexdigest()} bytes={len(raw)}"
    return f"<<<TASK-MANAGER-UNTRUSTED {header}>>>\n{inner}\n{CLOSE}"


def marker(inner):
    raw = inner.encode()
    return f"[task-manager text withheld sha256={hashlib.sha256(raw).hexdigest()} bytes={len(raw)}]"


class HelperTests(unittest.TestCase):
    def test_unfenced_text_is_unchanged_byte_for_byte(self):
        for text in ("", "plain", "a <<< b >>> c\n\n  trailing \t", "‹‹‹TASK-MANAGER-UNTRUSTED neutralized", "é🐈" * 900,
                     "END-TASK-MANAGER-UNTRUSTED>>> no marker", "<<<END-TASK-MANAGER-UNTRUSTED no end"):
            self.assertEqual(workflow_audit.withhold_task_text(text), text)
            self.assertEqual(workflow_audit.withhold_task_text(text).encode(), text.encode())

    def test_region_is_replaced_and_digest_ignores_the_header(self):
        text = "before " + fence(SECRET) + " after"
        self.assertEqual(workflow_audit.withhold_task_text(text), "before " + marker(SECRET) + " after")
        lying = "x" + fence(SECRET, "sha256=" + "0" * 64 + " bytes=1") + "y"
        self.assertEqual(workflow_audit.withhold_task_text(lying), "x" + marker(SECRET) + "y")

    def test_several_regions_and_text_between_them(self):
        text = fence("one") + " middle " + fence("two\nlines") + " end"
        self.assertEqual(workflow_audit.withhold_task_text(text), marker("one") + " middle " + marker("two\nlines") + " end")

    def test_unclosed_or_damaged_fence_withholds_the_tail(self):
        tail = SECRET + "\nmore " + SECRET
        out = workflow_audit.withhold_task_text("head\n<<<TASK-MANAGER-UNTRUSTED sha256=ab bytes=3>>>\n" + tail)
        self.assertEqual(out, "head\n" + marker(tail))
        out = workflow_audit.withhold_task_text("head <<<TASK-MANAGER-UNTRUSTED " + SECRET)
        self.assertTrue(out.startswith("head [task-manager text withheld sha256="))
        self.assertNotIn(SECRET, out)

    def test_closing_line_without_an_opening_withholds_from_the_start(self):
        # A fragment of a packet: a tail, an offset read, or a cut tool result that lost the opening line.
        tail = "description:\n" + SECRET + "\nmore " + SECRET
        out = workflow_audit.withhold_task_text(tail + "\n" + CLOSE + "\nafter stays")
        self.assertEqual(out, marker(tail) + "\nafter stays")
        self.assertEqual(workflow_audit.withhold_task_text(CLOSE + " alone"), marker("") + " alone")
        # The same after a complete region, and before a later one.
        text = "a " + fence("one") + " " + SECRET + "\n" + CLOSE + " b " + fence("two") + " c"
        self.assertEqual(workflow_audit.withhold_task_text(text),
                         "a " + marker("one") + marker(" " + SECRET) + " b " + marker("two") + " c")
        out = workflow_audit.withhold_task_text(SECRET + CLOSE + " password=hunter2secret", workflow_audit.redact)
        self.assertEqual(out, marker(SECRET) + " password=***")

    def test_cleaning_touches_only_text_outside_the_fence(self):
        out = workflow_audit.withhold_task_text("password=hunter2secret " + fence(SECRET) + " token: abcdefgh12345678",
                                                workflow_audit.redact)
        self.assertEqual(out, "password=*** " + marker(SECRET) + " token: ***")


class HookTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        # Nothing listens on the audit URL: events stay in the outbox, where they are read.
        self.env = {**os.environ, "HARNESS_AUDIT_ENABLED": "1", "HARNESS_CONFIG": "",
                    "HARNESS_CONFIG_HOME": str(self.base / "config"), "HARNESS_AUDIT_URL": "http://127.0.0.1:9",
                    "HARNESS_AUDIT_SETTINGS": str(self.base / "settings.json"),
                    "HARNESS_AUDIT_OUTBOX": str(self.base / "outbox.sqlite"),
                    "HARNESS_WORKFLOW_STATE": str(self.base / "state.sqlite"),
                    "HARNESS_SESSION_ID": "test-session", "CLAUDE_ENV_FILE": ""}
        for key in [k for k in self.env if k.startswith("HERDR_")]:
            del self.env[key]
        (self.base / "settings.json").write_text(json.dumps(
            {"jev": True, "workflow": True, "workflow_prompts": True, "workflow_context": True}))

    def run_cli(self, *args, payload=None):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/workflow_audit.py"), *args],
                                input=json.dumps(payload) if payload is not None else None, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout

    def hook(self, **payload):
        self.run_cli("hook", payload={"session_id": "s", **payload})

    def queued(self):
        with sqlite3.connect(self.base / "outbox.sqlite") as db:
            return [json.loads(r[0]) for r in db.execute("SELECT body FROM outbox ORDER BY rowid")]

    def texts(self, kind):
        parts = [e["payload"] for e in self.queued() if e["event_type"] == "workflow." + kind]
        entries = {}
        for part in sorted(parts, key=lambda p: (p.get("entry_index", 0), p["part_index"])):
            entries.setdefault(part.get("entry_id", part["prompt_id"]), []).append(part["text"])
        return ["".join(chunks) for chunks in entries.values()]

    def assert_absent(self, *needles):
        # Every stored byte: the outbox and the local session state, including their journals.
        stored = json.dumps(self.queued(), ensure_ascii=False)
        for path in self.base.iterdir():
            if path.is_file() and path.name.startswith("outbox.sqlite"):
                stored += path.read_bytes().decode(errors="ignore")
        for needle in needles:
            self.assertNotIn(needle, stored)
            self.assertNotIn(json.dumps(needle)[1:-1], stored)

    def log(self, *entries):
        path = self.base / "claude.jsonl"
        with path.open("a") as handle:
            for entry in entries:
                handle.write(json.dumps(entry) + "\n")
        return path

    def test_prompt_and_tool_result_keep_only_the_digest(self):
        prompt = "Please plan this item.\n" + fence(SECRET) + "\nKeep it small."
        self.hook(hook_event_name="UserPromptSubmit", prompt=prompt)
        self.assertEqual(self.texts("prompt_recorded"), ["Please plan this item.\n" + marker(SECRET) + "\nKeep it small."])
        path = self.log({"type": "user", "message": {"content": prompt}},
                        {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Bash", "input": {"command": "seed", "note": fence(SECRET)}}]}},
                        {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": "ok\n" + fence(SECRET) + "\ndone"}]}},
                        {"type": "response_item", "payload": {"type": "function_call_output", "output": fence(SECRET)}})
        self.hook(hook_event_name="Stop", transcript_path=str(path))
        context = self.texts("context_recorded")
        self.assertEqual(len(context), 4)
        self.assertEqual(context[0], "Please plan this item.\n" + marker(SECRET) + "\nKeep it small.")
        self.assertEqual(context[2], "ok\n" + marker(SECRET) + "\ndone")
        self.assertEqual(context[3], marker(SECRET))
        self.assertTrue(all("[task-manager text withheld sha256=" in text for text in context))
        self.assert_absent(SECRET, "TASK-MANAGER-UNTRUSTED")

    def test_fence_across_chunk_and_truncation_boundaries_does_not_leak(self):
        # The fence opens in the first 2000-byte chunk and closes many chunks later, past the 16000-byte entry cut.
        inner = "\n".join(f"{SECRET} line {n}" for n in range(600))
        self.assertGreater(len(inner.encode()), 20000)
        text = "x" * 1900 + fence(inner) + " tail stays"
        self.hook(hook_event_name="UserPromptSubmit", prompt=text)
        path = self.log({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": text}]}})
        self.hook(hook_event_name="Stop", transcript_path=str(path))
        expected = "x" * 1900 + marker(inner) + " tail stays"
        self.assertEqual(self.texts("prompt_recorded"), [expected])
        self.assertEqual(self.texts("context_recorded"), [expected])
        parts = [e["payload"] for e in self.queued() if e["event_type"] == "workflow.context_recorded"]
        self.assertGreater(len(parts), 1)
        self.assertTrue(all(not p["truncated"] and p["entry_bytes"] == len(expected.encode()) for p in parts))
        self.assert_absent(SECRET, "Basecamp fixture", "line 599")

    def test_unclosed_fence_withholds_the_tail_of_prompt_and_context(self):
        tail = SECRET + "\nand the rest " + SECRET
        text = "intro\n<<<TASK-MANAGER-UNTRUSTED sha256=" + "a" * 64 + " bytes=9>>>\n" + tail
        self.hook(hook_event_name="UserPromptSubmit", prompt=text)
        path = self.log({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": text}]}},
                        {"type": "assistant", "message": {"content": [{"type": "text", "text": "next entry is untouched"}]}})
        self.hook(hook_event_name="Stop", transcript_path=str(path))
        self.assertEqual(self.texts("prompt_recorded"), ["intro\n" + marker(tail)])
        self.assertEqual(self.texts("context_recorded"), ["intro\n" + marker(tail), "next entry is untouched"])
        self.assert_absent(SECRET, "and the rest")

    def test_packet_fragment_without_its_opening_line_is_withheld(self):
        tail = "title: " + SECRET + "\ndescription:\nthe tail of a packet " + SECRET
        text = tail + "\n" + CLOSE + "\nexit 0"
        self.hook(hook_event_name="UserPromptSubmit", prompt=text)
        path = self.log({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": text}]}})
        self.hook(hook_event_name="Stop", transcript_path=str(path))
        self.assertEqual(self.texts("prompt_recorded"), [marker(tail) + "\nexit 0"])
        self.assertEqual(self.texts("context_recorded"), [marker(tail) + "\nexit 0"])
        self.assert_absent(SECRET, "tail of a packet", "TASK-MANAGER-UNTRUSTED")

    def test_unfenced_prompt_and_context_are_unchanged(self):
        text = "Plain <<< text >>> with ünïcode 🐈\n\tand spacing  kept\n" * 150
        self.hook(hook_event_name="UserPromptSubmit", prompt=text)
        path = self.log({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": text}]}})
        self.hook(hook_event_name="Stop", transcript_path=str(path))
        self.assertEqual([t.encode() for t in self.texts("prompt_recorded")], [text.encode()])
        self.assertEqual([t.encode() for t in self.texts("context_recorded")], [text.encode()])

    def test_tool_commands_labels_and_curated_text_are_covered(self):
        command = "printf '%s' '" + fence(SECRET) + "' | wc -c"
        for event in ("PreToolUse", "PostToolUse"):
            self.hook(hook_event_name=event, tool_name="Bash", tool_use_id="c1", tool_input={"command": command})
        self.hook(hook_event_name="PreToolUse", tool_name="Agent", tool_use_id="c2", tool_input={"description": fence(SECRET)})
        self.hook(hook_event_name="PreToolUse", tool_name="Grep", tool_use_id="c3", tool_input={"pattern": "<<<TASK-MANAGER-UNTRUSTED x>>> " + SECRET})
        self.run_cli("task", "--session-id", "s", "--name", "Item " + fence(SECRET), "--description", fence(SECRET))
        self.run_cli("decision", "--session-id", "s", "--description", fence(SECRET), "--option", "a", "--option", "b", "--selected", "a")
        self.run_cli("outcome", "--session-id", "s", "--status", "running", "--description", "done " + fence(SECRET))
        events = self.queued()
        started = [e["payload"] for e in events if e["event_type"] == "workflow.tool_started"]
        self.assertEqual(started[0]["tool_command"], "printf '%s' '" + marker(SECRET) + "' | wc -c")
        self.assertIn("[task-manager text withheld sha256=", started[1]["tool_label"])
        updated = [e["payload"] for e in events if e["event_type"] == "workflow.task_updated"]
        self.assertEqual(updated[0]["description"], marker(SECRET))
        self.assert_absent(SECRET, "TASK-MANAGER-UNTRUSTED")
        state = (self.base / "state.sqlite").read_bytes().decode(errors="ignore")
        self.assertNotIn(SECRET, state)


if __name__ == "__main__":
    unittest.main()
