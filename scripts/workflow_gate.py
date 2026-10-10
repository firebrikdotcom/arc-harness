#!/usr/bin/env python3
"""Fail-closed todo guard for native local tool hooks."""
import io
import json
from pathlib import Path
import shlex
import sys
import uuid
import permit
import workflow_audit as audit
from workflow_todos import assert_plan, assert_complete

READ_TOOLS = {"Read", "Grep", "Glob", "read_file", "list_dir", "update_plan", "request_user_input", "request_user_input_async", "webrun", "web.run", "web__run"}
GRAPH_READS = {"search_graph", "trace_path", "get_code_snippet", "check_index_coverage", "query_graph", "get_architecture", "list_projects", "index_status"}
# Commands that only read. Writes by redirection are caught separately by the parser.
READ_ONLY = {"cat", "head", "tail", "wc", "ls", "tree", "grep", "egrep", "fgrep", "rg", "cut", "tr", "uniq",
             "diff", "cmp", "file", "stat", "du", "df", "pwd", "echo", "printf", "which", "type", "basename",
             "dirname", "realpath", "readlink", "date", "id", "whoami", "hostname", "uname", "jq", "column",
             "nl", "od", "xxd", "md5sum", "sha1sum", "sha256sum", "cksum", "pdfinfo", "test", "[", "true",
             "false", "cd", "less", "more", "env", "printenv", "nproc"}
GIT_READS = {"status", "log", "diff", "show", "rev-parse", "ls-files", "blame", "grep", "describe", "shortlog",
             "rev-list", "merge-base", "cat-file", "ls-tree", "for-each-ref", "show-ref", "whatchanged"}

def matches(word, target, cwd):
    path = Path(word)
    return (path if path.is_absolute() else cwd/path).resolve() == audit.ROOT/target

def management_segment(words, cwd):
    if matches(words[0], "scripts/harness", cwd):
        return len(words) >= 2 and words[1] in {"workflow", "plan", "build", "review", "status", "root", "step", "brief"}
    if words[0] in {"python3", "/usr/bin/python3"} and len(words) > 2 and matches(words[1], "scripts/workflow_audit.py", cwd):
        return words[2] in {"task", "todo", "decision", "outcome", "gate", "flush"}
    if matches(words[0], "scripts/action.sh", cwd):
        return len(words) == 3 and words[1] == "validate"
    # The evidence sensors may always run: completion depends on them.
    return matches(words[0], "scripts/verify.sh", cwd) or matches(words[0], "scripts/review.sh", cwd)

def read_only_segment(words):
    name = Path(words[0]).name
    if name == "adb":
        # Inventory only: never allow arbitrary adb shell, connect, or server actions.
        return words[1:] in (["devices"], ["devices", "-l"], ["mdns", "services"])
    if name == "git":
        rest = [word for word in words[1:] if not word.startswith("-")]
        if not rest:
            return False
        if rest[0] in ("stash", "worktree"):
            # Bare `git stash` pushes; only the listing form reads.
            return rest[1:] == ["list"]
        return rest[0] in GIT_READS or (rest[0] in ("branch", "tag", "remote")
                                        and all(word in ("-a", "-r", "-v", "-vv", "--list", "-l") for word in words[2:]))
    if name == "find":
        return not any(word in ("-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls") for word in words)
    if name == "sed":
        return not any(word.startswith("-i") or word.startswith("--in-place") for word in words)
    if name == "sort":
        return not any(word.startswith("-o") or word.startswith("--output") for word in words)
    return name in READ_ONLY

def management_command(payload):
    """A shell call the todo gate lets through without an active todo: harness
    bookkeeping, the verify and review sensors, and read-only commands, alone or
    joined with &&, ;, or pipes. Command substitution and any write disqualify it."""
    data = payload.get("tool_input") or {}
    command = data.get("command", data.get("cmd", "")) if isinstance(data, dict) else ""
    if not isinstance(command, str) or not command.strip() or any(value in command for value in ("`", "$(", "<(", ">(")):
        return False
    cwd = Path(payload.get("cwd") or ".")
    workdir = data.get("workdir") or data.get("cwd")
    if workdir:
        if not isinstance(workdir, str):
            return False
        cwd = (cwd / workdir).resolve()
    try:
        analysis = permit.analyse(command, str(cwd), cwd.resolve())
        text, bodies = permit.preprocess(command)
        segments = permit.split_segments(permit.tokenize(text))
    except permit.Unparseable:
        return False
    if analysis.inline or analysis.targets or analysis.opaque or bodies:
        return False
    for segment in segments:
        words = permit.read_segment(segment, []).argv
        if words and not (management_segment(words, cwd) or read_only_segment(words)):
            return False
    return True

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
            print(f"Todo gate: confirm (todo confirm), revise (todo plan), or exempt (todo exempt, for a question) the plan for this prompt "
                  f"via {audit.ROOT}/scripts/harness workflow todo ... --session-id {sid} --reason TEXT; reads need no todo.")
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
