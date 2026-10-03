#!/usr/bin/env python3
"""Fail-closed todo guard for native local tool hooks."""
import io
import json
from pathlib import Path
import shlex
import sys
import uuid
import workflow_audit as audit
from workflow_todos import assert_plan, assert_complete

READ_TOOLS = {"Read", "Grep", "Glob", "read_file", "list_dir", "update_plan", "request_user_input", "request_user_input_async"}
GRAPH_READS = {"search_graph", "trace_path", "get_code_snippet", "check_index_coverage", "query_graph", "get_architecture", "list_projects", "index_status"}

def management_command(payload):
    data = payload.get("tool_input") or {}
    command = data.get("command", data.get("cmd", "")) if isinstance(data, dict) else ""
    if not isinstance(command, str) or any(value in command for value in ("\n", "`", "$(")): return False
    try:
        lexer = shlex.shlex(command, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        words = list(lexer)
    except ValueError: return False
    if not words or any(word and all(char in ";&|<>()" for char in word) for word in words): return False
    cwd = Path(payload.get("cwd") or ".")
    def matches(word, target):
        path = Path(word)
        return (path if path.is_absolute() else cwd/path).resolve() == audit.ROOT/target
    if matches(words[0], "scripts/harness"):
        return (len(words) >= 2 and words[1] in {"workflow", "plan", "build", "review", "status", "root", "step"})
    if words[0] in {"python3", "/usr/bin/python3"} and len(words)>2 and matches(words[1], "scripts/workflow_audit.py"):
        return words[2] in {"task","todo","decision","outcome","gate","flush"}
    if matches(words[0], "scripts/action.sh"):
        return len(words)==3 and words[1]=="validate"
    return False

def authorize(payload):
    event = payload.get("hook_event_name")
    sid = audit.session_id(payload.get("session_id"))
    if not sid: raise ValueError("Native session ID is required by the todo gate")
    with audit.context_db() as db:
        db.execute("BEGIN IMMEDIATE")
        context = audit.load(db, sid)
        audit.ensure_session(sid, context)
        if event == "UserPromptSubmit":
            context["request_seq"] = context.get("request_seq", 0) + 1
            context["request_id"] = str(uuid.uuid4())
            context.pop("confirmed_request", None)
            audit.record("plan_required", sid, context, description="Confirm or revise the complete todo plan for this prompt", status="required")
            print(f"Todo gate: before execution, register a complete plan with {audit.ROOT}/scripts/harness workflow todo plan --session-id {sid} --items '[{{\"description\":\"Work item\",\"criterion\":\"Completion evidence\"}}]' --reason SUMMARY. Use todo show to get IDs, then todo update --id ID --status in_progress --reason SUMMARY. Existing plans need todo confirm or revision for each prompt. Questions with no execution use todo exempt --reason SUMMARY. Record evidence for completed items and pass verify/review before a completed outcome.")
        elif event == "PreToolUse":
            tool = payload.get("tool_name", "")
            readonly = tool in READ_TOOLS or any(tool.endswith("__"+name) for name in GRAPH_READS)
            if not readonly and not management_command(payload):
                assert_plan(context, active=True)
                context["last_execution_at"] = audit.now()
        elif event == "Stop":
            if context.get("task_status") != "blocked": assert_complete(context)
        else:
            raise ValueError("Unsupported policy event")
        audit.save(db, sid, context)
    return 0

def main():
    raw = sys.stdin.read(1_000_001)
    if len(raw)>1_000_000: raise ValueError("Hook payload exceeds policy limit")
    payload=json.loads(raw)
    if not isinstance(payload,dict): raise ValueError("Invalid hook payload")
    authorize(payload)
    if payload.get("hook_event_name") in {"PreToolUse", "UserPromptSubmit"}:
        sys.stdin=io.StringIO(raw)
        return audit.hook(sys.argv[1] if len(sys.argv)>1 else None)
    print("{}")
    return 0

if __name__ == "__main__":
    try: sys.exit(main())
    except Exception as error:
        # Denials disclose policy guidance, never raw hook payloads or tool content.
        reason = str(error) if isinstance(error, ValueError) else type(error).__name__
        print("TODO GATE: " + reason, file=sys.stderr)
        sys.exit(2)
