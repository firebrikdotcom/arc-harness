"""Workflow correlation, collection controls, privacy, and durable-delivery regressions."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import threading
import unittest
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import audit_transport
from run_paths import current_file
from unittest.mock import patch

class Handler(BaseHTTPRequestHandler):
    events = []
    fail = False
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.events.append(body)
        self.send_response(503 if self.fail else 201)
        self.end_headers()
    def log_message(self, *args): pass

class WorkflowTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        Handler.events = []
        Handler.fail = False
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.env = {**os.environ, "HARNESS_AUDIT_ENABLED":"1",
            "HARNESS_AUDIT_URL":f"http://127.0.0.1:{self.server.server_port}",
            "HARNESS_AUDIT_SETTINGS":str(self.base / "settings.json"),
            "HARNESS_AUDIT_OUTBOX":str(self.base / "outbox.sqlite"),
            "HARNESS_WORKFLOW_STATE":str(self.base / "state.sqlite"),
            "HARNESS_SESSION_ID":"test-session", "CLAUDE_ENV_FILE":""}
        for key in [k for k in self.env if k.startswith("HERDR_")]:
            del self.env[key]
    def config(self, jev, workflow, prompts=False):
        (self.base / "settings.json").write_text(json.dumps({"jev":jev,"workflow":workflow,"workflow_prompts":prompts}))
    def run_cli(self, *args, payload=None):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/workflow_audit.py"), *args],
            input=json.dumps(payload) if payload is not None else None, env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout
    def queued(self):
        path = self.base / "outbox.sqlite"
        if not path.exists(): return []
        with sqlite3.connect(path) as db:
            return [json.loads(r[0]) for r in db.execute("SELECT body FROM outbox ORDER BY rowid")]
    def test_queue_retries_the_same_event_id_after_ambiguous_failure(self):
        with patch.dict(os.environ, self.env):
            Handler.fail = True
            self.assertFalse(audit_transport.emit("jev.route", {"call_id":"a"}))
            pending = self.queued()
            self.assertEqual(len(pending), 1)
            Handler.fail = False
            self.assertTrue(audit_transport.flush())
            self.assertEqual(len(Handler.events), 2)
            self.assertEqual(Handler.events[0]["id"], Handler.events[1]["id"])
            self.assertEqual(self.queued(), [])
    def test_independent_switches_drop_disabled_queue_and_do_not_record_new_events(self):
        with patch.dict(os.environ, self.env):
            audit_transport.emit("jev.route", {"call_id":"a"}, flush_now=False)
            audit_transport.emit("workflow.session_started", {"session_id":"s"}, flush_now=False)
            self.config(False, True)
            self.assertTrue(audit_transport.emit("jev.route", {"call_id":"disabled"}))
            self.assertTrue(audit_transport.flush())
            self.assertEqual([e["event_type"] for e in Handler.events], ["workflow.session_started"])
            self.config(True, False)
            self.assertFalse(audit_transport.enabled("workflow.task_started"))
            self.assertTrue(audit_transport.enabled("jev.checkpoint"))
            self.env["HARNESS_AUDIT_ENABLED"] = "0"
            with patch.dict(os.environ, self.env): self.assertFalse(audit_transport.enabled())
    def test_hook_metadata_and_cli_events_share_task_without_storing_prompt_or_output(self):
        self.run_cli("hook", payload={"hook_event_name":"SessionStart","session_id":"s","cwd":str(self.base),"prompt":"PRIVATE PROMPT"})
        self.run_cli("hook", payload={"hook_event_name":"PostToolUse","session_id":"s","tool_name":"Bash","tool_input":{"command":"PRIVATE COMMAND"},"tool_response":{"stdout":"PRIVATE OUTPUT","exit_code":7}})
        self.run_cli("task", "--session-id", "s", "--name", "Repair example", "--description", "Curated summary")
        self.run_cli("decision", "--session-id", "s", "--description", "Choose verification", "--option", "full", "--option", "focused", "--selected", "full")
        self.run_cli("todo", "exempt", "--session-id", "s", "--reason", "Metadata-only fixture with no gated execution")
        self.run_cli("outcome", "--session-id", "s", "--status", "completed", "--description", "All checks passed")
        events = Handler.events + self.queued()
        # Prompts and tool output stay out; the command is kept only as a redacted label.
        self.assertNotIn("PRIVATE PROMPT", json.dumps(events))
        self.assertNotIn("PRIVATE OUTPUT", json.dumps(events))
        tasks = {e["payload"]["task_id"] for e in events if "task_id" in e["payload"]}
        self.assertEqual(len(tasks), 1)
        self.assertTrue(all(e["payload"]["session_id"] == "s" for e in events))
        failed = next(e for e in events if e["event_type"] == "workflow.tool_failed")
        self.assertEqual(failed["payload"]["exit_code"], 7)
        completed = next(e for e in events if e["event_type"] == "workflow.task_completed")
        self.assertEqual(completed["payload"]["outcome"], "All checks passed")
    def test_tool_calls_pair_by_id_preserve_task_and_distinguish_result_evidence(self):
        def event(kind, call, response=None):
            self.run_cli("hook", payload={"hook_event_name":kind,"session_id":"s","tool_name":"Bash","tool_use_id":call,"tool_input":{"command":"PRIVATE COMMAND"},"tool_response":response or {}})
        event("PreToolUse", "call-a")
        event("PreToolUse", "call-b")
        old_task = next(e["payload"]["task_id"] for e in self.queued() if e["event_type"] == "workflow.tool_started")
        self.run_cli("task", "--session-id", "s", "--new", "--name", "Next task", "--description", "Other scope")
        event("PostToolUse", "call-b", {"exit_code":0,"stdout":"PRIVATE OUTPUT"})
        event("PostToolUseFailure", "call-a", {"exit_code":7})
        event("PostToolUse", "call-c", {"session_id":123})
        event("PostToolUse", "call-d", {})
        rows = [e["payload"] for e in self.queued() if e["event_type"] in ("workflow.tool_completed", "workflow.tool_failed")]
        self.assertEqual([r["outcome"] for r in rows], ["succeeded","failed","process_running","returned"])
        self.assertEqual(rows[0]["tool_call_id"], "call-b")
        self.assertEqual(rows[0]["task_id"], old_task)
        self.assertGreaterEqual(rows[0]["duration_ms"], 0)
        self.assertIn("started_at",rows[1])
        self.assertNotIn("duration_ms", rows[2])
        self.assertEqual(rows[2]["process_id"], "123")
        # The command is kept as a short label; tool output never is.
        self.assertEqual(rows[1]["tool_label"], "PRIVATE COMMAND")
        self.assertNotIn("PRIVATE OUTPUT", json.dumps(self.queued()))

    def test_tool_labels_describe_calls_without_secrets_or_file_contents(self):
        import workflow_audit as audit
        label = audit.tool_label
        self.assertEqual(label("Bash", {"command": "make test", "description": "Run the test suite"}), "Run the test suite")
        self.assertEqual(label("exec_command", {"cmd": ["git", "status"]}), "git status")
        self.assertEqual(label("Read", {"file_path": "/home/me/project/src/ui.rs"}), "ui.rs")
        self.assertEqual(label("Write", {"file_path": "/x/notes.md", "content": "SECRET BODY"}), "notes.md")
        self.assertEqual(label("apply_patch", {"input": "*** Begin Patch\n*** Update File: src/a.rs\n+SECRET BODY\n*** Add File: b/c.html\n"}), "a.rs, c.html")
        self.assertIsNone(label("mcp__browser__navigate", {"url": "https://example.com"}))
        self.assertIsNone(label("Bash", "not a dict"))
        for command, kept, hidden in [
            ('curl -H "Authorization: Bearer abcdefgh12345678" https://user:pw@host.io', "curl -H", ["abcdefgh12345678", "user:pw"]),
            ("export API_KEY=sk-abc123def456ghi; ls", "export API_KEY=***; ls", ["sk-abc"]),
            ("mysql -u root -p hunter2 db", "mysql -u *** -p *** db", ["hunter2"]),
            ("curl --api-key abc123xyz https://x", "curl --api-key ***", ["abc123xyz"]),
            ("TOKEN=$(cat secret.txt) npm publish", "TOKEN=*** npm publish", ["secret.txt"]),
            ("echo ghp_0123456789abcdefABCDEF", "echo ***", ["ghp_"]),
            ("echo 0123456789abcdef0123456789abcdef01234567", "echo ***", ["0123456789abcdef"]),
            ("git push --force-with-lease", "git push --force-with-lease", []),
        ]:
            result = label("Bash", {"command": command})
            self.assertIn(kept, result)
            for secret in hidden:
                self.assertNotIn(secret, result)
        long = label("Bash", {"command": "echo " + "word " * 100})
        self.assertLessEqual(len(long), 160)
        self.assertTrue(long.endswith("..."))

    def test_tool_label_follows_the_call_from_start_to_failure(self):
        def event(kind, response=None):
            self.run_cli("hook", payload={"hook_event_name":kind,"session_id":"s","tool_name":"Bash","tool_use_id":"call-a","tool_input":{"command":"make test","description":"Run the test suite"},"tool_response":response or {}})
        event("PreToolUse")
        event("PostToolUseFailure", {"exit_code":2})
        rows = {e["event_type"]: e["payload"] for e in self.queued() if e["event_type"].startswith("workflow.tool_")}
        self.assertEqual(rows["workflow.tool_started"]["tool_label"], "Run the test suite")
        self.assertEqual(rows["workflow.tool_failed"]["tool_label"], "Run the test suite")
        # The description labels the call; the command itself says what failed.
        self.assertEqual(rows["workflow.tool_started"]["tool_command"], "make test")
        self.assertEqual(rows["workflow.tool_failed"]["tool_command"], "make test")

    def test_tool_command_keeps_the_full_redacted_command(self):
        import workflow_audit as audit
        command = audit.tool_command
        multi = "cd /repo && grep -rn x . | head -5\ngit config core.hooksPath"
        self.assertEqual(command("Bash", {"command": multi, "description": "Find branch rule"}), multi)
        self.assertEqual(command("exec_command", {"cmd": ["git", "commit", "-m", "two words"]}), "git commit -m 'two words'")
        self.assertIsNone(command("Read", {"file_path": "/x/a.md"}))
        self.assertIsNone(command("Bash", {"command": "  "}))
        self.assertIsNone(command("Bash", "not a dict"))
        redacted = command("Bash", {"command": "export API_KEY=sk-abc123def456ghi; curl https://user:pw@host.io"})
        self.assertNotIn("sk-abc", redacted)
        self.assertNotIn("user:pw", redacted)
        long = command("Bash", {"command": "echo " + "wörd " * 1000})
        self.assertLessEqual(len(long.encode()), 1900)
        self.assertTrue(long.endswith("..."))

    def gate(self, event, code=0, **fields):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/workflow_gate.py"), "codex"], input=json.dumps({"session_id":"policy-session","hook_event_name":event,"cwd":str(ROOT),**fields}), env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, code, result.stderr)
        return result

    def policy(self, *args, code=0):
        result = subprocess.run([sys.executable, str(ROOT / "scripts/workflow_audit.py"), "todo", *args, "--session-id", "policy-session"], env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, code, result.stderr)
        return result

    def test_todo_gate_blocks_execution_until_plan_and_active_item_and_rechecks_each_prompt(self):
        self.gate("UserPromptSubmit", prompt="PRIVATE PROMPT")
        self.gate("PreToolUse",2,tool_name="Bash",tool_input={"command":"touch PRIVATE_PATH"})
        self.gate("PreToolUse",tool_name="Read",tool_input={"file_path":"PRIVATE_PATH"})
        self.gate("PreToolUse",tool_name="Bash",tool_input={"command":str(ROOT/"scripts/harness")+" workflow todo show"})
        for suffix in (";touch PRIVATE_PATH", " && touch PRIVATE_PATH", " > PRIVATE_PATH", " $(touch PRIVATE_PATH)", " `touch PRIVATE_PATH`"):
            self.gate("PreToolUse",2,tool_name="Bash",tool_input={"command":str(ROOT/"scripts/harness")+" workflow todo show"+suffix})
        self.gate("PreToolUse",tool_name="Bash",tool_input={"command":str(ROOT/"scripts/harness")+" workflow todo show | cat"})
        self.policy("plan","--items",json.dumps([{"id":"a","description":"Repair parser","criterion":"Regression passes"}]),"--reason","Initial plan")
        self.gate("PreToolUse",2,tool_name="Write",tool_input={"file_path":"PRIVATE_PATH"})
        self.policy("update","--id","a","--status","in_progress","--reason","Working on parser")
        self.gate("PreToolUse",tool_name="Bash",tool_use_id="call-a",tool_input={"command":"PRIVATE COMMAND"})
        self.run_cli("hook",payload={"session_id":"policy-session","hook_event_name":"PostToolUse","tool_name":"Bash","tool_use_id":"call-a","tool_response":{"exit_code":0}})
        completed=next(e for e in self.queued() if e["event_type"]=="workflow.tool_completed")
        self.assertEqual(completed["payload"]["todo_id"],"a")
        self.gate("UserPromptSubmit",prompt="Steering prompt")
        self.gate("PreToolUse",2,tool_name="Bash",tool_input={"command":"touch PRIVATE_PATH"})
        self.policy("confirm","--reason","Steering does not change the work")
        self.gate("PreToolUse",tool_name="Bash",tool_input={"command":"PRIVATE COMMAND"})
        self.assertNotIn("PRIVATE PROMPT",json.dumps(self.queued()))
        self.gate("Stop",2)

    def test_plan_revisions_remove_items_with_reasons_and_completion_needs_fresh_checks(self):
        self.gate("UserPromptSubmit",prompt="Work")
        self.policy("plan","--items",json.dumps([{"id":"a","description":"Repair parser","criterion":"Regression passes"},{"id":"b","description":"Other work","criterion":"Other result"}]),"--reason","Initial plan")
        self.policy("plan","--items",json.dumps([{"id":"a","description":"Repair Unicode parser","criterion":"Unicode regression passes"}]),"--reason","Discovery changed scope")
        removed=next(e for e in Handler.events+self.queued() if e["event_type"]=="workflow.todo_removed")
        self.assertEqual(removed["payload"]["reason"],"Discovery changed scope")
        revised=next(e for e in Handler.events+self.queued() if e["event_type"]=="workflow.todo_updated")
        self.assertEqual(revised["payload"]["previous_description"],"Repair parser")
        self.policy("exempt","--reason","Cannot erase unfinished work",code=1)
        self.policy("update","--id","a","--status","completed","--reason","Done",code=1)
        self.policy("update","--id","a","--status","completed","--evidence","Unicode regression passed","--reason","Implementation checked")
        self.gate("Stop",2)
        for kind in ("verify","review"):
            path=self.base/(kind+".state")
            path.write_text("EXIT=0\nFAILURES=0\nRECORD_AT="+datetime.now(timezone.utc).isoformat().replace("+00:00","Z")+"\n")
            self.run_cli("check","--kind",kind,"--record",str(path),"--session-id","policy-session")
        self.gate("Stop")
        self.run_cli("outcome","--session-id","policy-session","--status","completed","--description","Checks passed")
        self.policy("plan","--items",json.dumps([{"id":"a","description":"New scope","criterion":"New check"}]),"--reason","New discovery")
        self.policy("update","--id","a","--status","completed","--evidence","Evidence supplied","--reason","Updated")
        self.gate("Stop",2)

    def test_reads_bookkeeping_and_sensors_need_no_active_todo_but_writes_do(self):
        self.gate("UserPromptSubmit",prompt="Work")
        harness = str(ROOT/"scripts/harness")
        for command in ("ls -la", "git status --short", "git log --oneline -3 | head -n 2", "cat README.md | grep -n x 2>/dev/null",
                        "find . -name '*.md' | wc -l", "sed -n 1,5p README.md", "cd scripts && ls",
                        harness+" workflow todo show && "+harness+" status", str(ROOT/"scripts/verify.sh")+" --project .",
                        str(ROOT/"scripts/review.sh")+" --project ."):
            self.gate("PreToolUse",tool_name="Bash",tool_input={"command":command})
        for command in ("echo x > PRIVATE_PATH", "touch PRIVATE_PATH", "sed -i s/a/b/ PRIVATE_PATH", "find . -delete",
                        "git commit -m x", "git branch -D x", "ls $(touch PRIVATE_PATH)", "python3 -c 'print(1)'", "sh -c ls",
                        "cat a | tee PRIVATE_PATH", "sort -o PRIVATE_PATH a"):
            self.gate("PreToolUse",2,tool_name="Bash",tool_input={"command":command})

    def test_completion_freshness_follows_project_files_not_unrelated_writes(self):
        project = self.base/"project"
        project.mkdir()
        subprocess.run(["git","init","-q",str(project)],check=True)
        (project/"a.txt").write_text("one\n")
        self.gate("UserPromptSubmit",prompt="Work")
        self.policy("plan","--items",json.dumps([{"id":"a","description":"Change a","criterion":"Checks pass"}]),"--reason","Plan")
        self.policy("update","--id","a","--status","completed","--evidence","Checks passed","--reason","Done")
        tree = subprocess.run(["sh",str(ROOT/"scripts/tree-hash.sh"),str(project)],capture_output=True,text=True,check=True).stdout.strip()
        for kind in ("verify","review"):
            path=self.base/(kind+".state")
            path.write_text("EXIT=0\nFAILURES=0\nRECORD_AT="+datetime.now(timezone.utc).isoformat().replace("+00:00","Z")+
                            "\nTREE_HASH="+tree+"\nPROJECT_ROOT="+str(project)+"\n")
            self.run_cli("check","--kind",kind,"--record",str(path),"--session-id","policy-session")
        self.gate("Stop")
        # An executed call that leaves the project untouched (a memory note, a scratch file) keeps the checks fresh.
        self.policy("update","--id","a","--status","in_progress","--reason","Write a note")
        self.gate("PreToolUse",tool_name="Write",tool_input={"file_path":str(self.base/"note.md")})
        self.policy("update","--id","a","--status","completed","--evidence","Checks passed","--reason","Done")
        self.gate("Stop")
        (project/"a.txt").write_text("two\n")
        result = self.gate("Stop",2)
        self.assertIn("Fresh passing verify", result.stderr)

    def test_gate_without_a_session_fails_closed_inside_an_agent(self):
        env = {k: v for k, v in self.env.items() if k not in ("HARNESS_SESSION_ID","CODEX_THREAD_ID","CLAUDE_SESSION_ID","CLAUDE_CODE_SESSION_ID")}
        human = subprocess.run([sys.executable,str(ROOT/"scripts/workflow_audit.py"),"gate","--check","active"],
                               env={k: v for k, v in env.items() if k not in ("CLAUDECODE","CODEX_SANDBOX")},text=True,capture_output=True)
        self.assertEqual(human.returncode,0,human.stderr)
        agent = subprocess.run([sys.executable,str(ROOT/"scripts/workflow_audit.py"),"gate","--check","active"],
                               env={**env,"CLAUDECODE":"1"},text=True,capture_output=True)
        self.assertEqual(agent.returncode,1)
        self.assertIn("needs this session's ID",agent.stderr)

    def test_policy_is_independent_of_collection_and_question_exemption_cannot_execute(self):
        self.config(False,False)
        self.gate("UserPromptSubmit",prompt="Question")
        self.gate("PreToolUse",2,tool_name="Bash",tool_input={"command":"touch question"})
        self.policy("exempt","--reason","Answer needs no local execution")
        self.gate("Stop")
        self.gate("PreToolUse",2,tool_name="Write",tool_input={"file_path":"file"})
        self.assertEqual(self.queued(),[])
        result=subprocess.run([sys.executable,str(ROOT/"scripts/workflow_gate.py")],input="{invalid",env=self.env,text=True,capture_output=True)
        self.assertEqual(result.returncode,2)

    def test_resumed_session_emits_a_new_start_without_replacing_its_task(self):
        self.run_cli("hook", payload={"hook_event_name":"SessionStart", "session_id":"s"})
        self.run_cli("hook", payload={"hook_event_name":"SessionEnd", "session_id":"s"})
        self.run_cli("hook", payload={"hook_event_name":"SessionStart", "session_id":"s", "source":"resume"})
        events = self.queued()
        starts = [e for e in events if e["event_type"] == "workflow.session_started"]
        self.assertEqual(len(starts), 2)
        self.assertEqual(starts[-1]["payload"]["source"], "resume")
        self.assertEqual(len([e for e in events if e["event_type"] == "workflow.task_started"]), 1)
        ended = next(e for e in events if e["event_type"] == "workflow.session_ended")
        self.assertNotIn("task_id", ended["payload"])

    def test_disabled_hook_is_silent_and_creates_no_context(self):
        self.config(True, False)
        self.assertEqual(self.run_cli("hook", payload={"hook_event_name":"SessionStart","session_id":"s"}), "")
        self.assertFalse((self.base / "state.sqlite").exists())
        self.assertEqual(self.queued(), [])
    def test_phase_and_verification_use_actual_session_id_and_deduplicate_sensor_records(self):
        db = self.base / "harness"
        (db / "runs/r1").mkdir(parents=True)
        pointer = current_file(db, "test-session")
        pointer.parent.mkdir(parents=True)
        pointer.write_text("r1")
        (db / "runs/r1/state").write_text("PHASE_BUILD=active\n")
        self.run_cli("phase", "--db-root", str(db), "--phase", "build")
        record = self.base / "verify.state"
        record.write_text("EXIT=1\nRAN=4\nSKIPPED=2\nFAILURES=1\nRECORD_EPOCH=100\n")
        self.run_cli("check", "--kind", "verify", "--record", str(record))
        self.run_cli("check", "--kind", "verify", "--record", str(record))
        checks = [e for e in Handler.events if e["event_type"] == "workflow.verification"]
        self.assertEqual(len(checks), 1)
        self.assertEqual(checks[0]["payload"]["outcome"], "failed")
        self.assertEqual(checks[0]["payload"]["session_id"], "test-session")
    def test_prompt_policy_is_dynamic_and_long_unicode_prompts_are_lossless(self):
        self.config(True, True)
        guidance = self.run_cli("hook", payload={"hook_event_name":"SessionStart","session_id":"s"})
        self.assertIn("prompt collection off", guidance)
        self.assertEqual(len(guidance.strip().splitlines()), 1)
        self.assertNotIn("never raw prompts", guidance)
        self.run_cli("hook", payload={"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"not collected"})
        self.assertFalse(any(e["event_type"] == "workflow.prompt_recorded" for e in self.queued()))
        self.config(True, True, True)
        prompt = "Build example 🐈\n" * 500
        self.run_cli("hook", payload={"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":prompt})
        parts = [e["payload"] for e in self.queued() if e["event_type"] == "workflow.prompt_recorded"]
        self.assertGreater(len(parts), 1)
        self.assertEqual("".join(p["text"] for p in sorted(parts, key=lambda p:p["part_index"])), prompt)
        self.assertEqual(len({p["prompt_id"] for p in parts}), 1)
        self.assertTrue(all(len(p["text"].encode()) <= 2000 and p["part_count"] == len(parts) for p in parts))
        self.config(True, True, False)
        self.run_cli("flush")
        self.assertFalse(any(e["event_type"] == "workflow.prompt_recorded" for e in Handler.events))
        self.assertEqual(self.queued(), [])
        self.config(True, False, True)
        self.run_cli("hook", payload={"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"workflow disabled"})
        self.assertEqual(self.queued(), [])

    def test_native_runtime_metadata_changes_are_recorded_without_prompt_content(self):
        transcript = self.base / "native.jsonl"
        transcript.write_text(json.dumps({"type":"session_meta","payload":{"id":"s","cwd":"/example/original directory"}}) + "\n" + json.dumps({"type":"turn_context","payload":{"model":"example-model-v1"}}) + "\n" + json.dumps({"type":"event_msg","payload":{"message":"PRIVATE PROMPT"}}) + "\n")
        payload = {"hook_event_name":"SessionStart","session_id":"s","transcript_path":str(transcript)}
        self.run_cli("hook", "--agent", "codex", payload=payload)
        metadata = [e["payload"] for e in self.queued() if e["event_type"] == "workflow.session_updated"]
        self.assertEqual(metadata[0]["agent"], "Codex")
        self.assertEqual(metadata[0]["model"], "example-model-v1")
        self.assertEqual(metadata[0]["cwd"], "/example/original directory")
        self.assertNotIn("PRIVATE PROMPT", json.dumps(self.queued()))
        self.run_cli("hook", "--agent", "codex", payload=payload)
        self.assertEqual(len([e for e in self.queued() if e["event_type"] == "workflow.session_updated"]), 1)
        self.run_cli("hook", "--agent", "codex", payload={**payload,"hook_event_name":"UserPromptSubmit","model":"example-model-v2","cwd":"/example/current directory","prompt":"PRIVATE PROMPT"})
        metadata = [e["payload"] for e in self.queued() if e["event_type"] == "workflow.session_updated"]
        self.assertEqual(metadata[-1]["model"], "example-model-v2")
        self.assertEqual(metadata[-1]["cwd"], "/example/current directory")
        self.assertNotIn("agent", metadata[-1])
        self.assertNotIn("PRIVATE PROMPT", json.dumps(self.queued()))

    def test_herdr_tab_label_is_recorded_and_refreshed_on_rename(self):
        label = self.base / "label"
        label.write_text("Audit redesign")
        fake = self.base / "herdr"
        fake.write_text("#!/bin/sh\n[ \"$1 $2 $3\" = \"tab get wD:tK\" ] || exit 1\n"
            "printf '{\"result\":{\"tab\":{\"label\":\"%s\",\"tab_id\":\"wD:tK\"}}}' \"$(cat " + str(label) + ")\"\n")
        fake.chmod(0o755)
        self.env.update({"HERDR_TAB_ID":"wD:tK", "HERDR_BIN_PATH":str(fake)})
        tabs = lambda: [e["payload"]["herdr_tab"] for e in self.queued() if "herdr_tab" in e["payload"]]
        self.run_cli("hook", payload={"hook_event_name":"SessionStart","session_id":"s"})
        self.assertEqual(tabs(), ["Audit redesign"])
        self.run_cli("hook", payload={"hook_event_name":"PreToolUse","session_id":"s","tool_name":"Bash"})
        self.run_cli("hook", payload={"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"PRIVATE PROMPT"})
        self.assertEqual(tabs(), ["Audit redesign"])
        label.write_text("Renamed tab")
        self.run_cli("hook", payload={"hook_event_name":"UserPromptSubmit","session_id":"s","prompt":"PRIVATE PROMPT"})
        self.assertEqual(tabs(), ["Audit redesign", "Renamed tab"])
        self.env["HERDR_TAB_ID"] = "bad id; rm"
        self.run_cli("hook", payload={"hook_event_name":"SessionStart","session_id":"other"})
        self.assertEqual(len(tabs()), 2)

    def test_wrong_session_transcript_does_not_supply_model_or_title(self):
        transcript = self.base / "wrong.jsonl"
        transcript.write_text(json.dumps({"type":"session_meta","payload":{"id":"another-session"}}) + "\n" + json.dumps({"type":"turn_context","payload":{"model":"wrong-model"}}) + "\n")
        self.run_cli("hook", "--agent", "codex", payload={"hook_event_name":"SessionStart","session_id":"s","transcript_path":str(transcript)})
        metadata = [e["payload"] for e in self.queued() if e["event_type"] == "workflow.session_updated"]
        self.assertEqual(metadata[0]["agent"], "Codex")
        self.assertNotIn("model", metadata[0])

    def test_claude_native_model_and_title_metadata_are_allowlisted(self):
        transcript = self.base / "native.jsonl"
        transcript.write_text("\n".join(json.dumps(r) for r in [
            {"type":"assistant","sessionId":"s","cwd":"/example/Working Tree","message":{"model":"example-opus","content":"PRIVATE OUTPUT"}},
            {"type":"ai-title","aiTitle":"Improve the audit dashboard session selectors"},
            {"type":"assistant","sessionId":"s","message":{"model":"<synthetic>"}},
        ]) + "\n")
        self.run_cli("hook", "--agent", "claude-code", payload={"hook_event_name":"SessionStart","session_id":"s","transcript_path":str(transcript)})
        metadata = next(e["payload"] for e in self.queued() if e["event_type"] == "workflow.session_updated")
        self.assertEqual(metadata["agent"], "Claude Code")
        self.assertEqual(metadata["model"], "example-opus")
        self.assertEqual(metadata["cwd"], "/example/Working Tree")
        self.assertEqual(metadata["session_name"], "Improve the audit dashboard session")
        self.assertNotIn("PRIVATE", json.dumps(self.queued()))

    def test_native_state_recovers_exact_session_title_and_model_without_prompt(self):
        state = self.base / "native.sqlite"
        with sqlite3.connect(state) as db:
            db.execute("CREATE TABLE threads (id TEXT, name TEXT, title TEXT, model TEXT, first_user_message TEXT, cwd TEXT)")
            db.execute("INSERT INTO threads VALUES (?,?,?,?,?,?)", ("s", "Repair session dashboard selection controls now", "unused title", "example-model", "PRIVATE PROMPT", "/example/State Working Tree"))
        self.env["HARNESS_NATIVE_SESSION_STATE"] = str(state)
        self.run_cli("hook", payload={"hook_event_name":"SessionStart","session_id":"s"})
        metadata = next(e["payload"] for e in self.queued() if e["event_type"] == "workflow.session_updated")
        self.assertEqual(metadata["session_name"], "Repair session dashboard selection controls")
        self.assertEqual(metadata["model"], "example-model")
        self.assertEqual(metadata["cwd"], "/example/State Working Tree")
        self.assertEqual(metadata["agent"], "Codex")
        self.assertNotIn("PRIVATE", json.dumps(self.queued()))

    def test_invalid_settings_do_not_silently_enable_collection(self):
        (self.base / "settings.json").write_text('{}')
        with patch.dict(os.environ, self.env):
            with self.assertRaises(ValueError): audit_transport.enabled()

if __name__ == "__main__": unittest.main()
