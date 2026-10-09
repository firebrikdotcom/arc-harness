#!/usr/bin/env python3
"""Correlated Workflow telemetry with configurable user-prompt collection."""
from __future__ import annotations
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import time
import shlex
import sqlite3
import subprocess
import sys
import uuid
from audit_transport import ROOT, emit, enabled, flush
import workflow_todos as todos
from run_paths import current_file, session_id


def now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")


def context_db() -> sqlite3.Connection:
    path = Path(os.environ.get("HARNESS_WORKFLOW_STATE", ROOT / ".harness-db/workflow-state.sqlite"))
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    db = sqlite3.connect(path, timeout=2)
    path.chmod(0o600)
    db.execute("CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, context TEXT NOT NULL)")
    return db


def record(kind: str, sid: str, context: dict, **facts) -> None:
    payload = {"schema_version": 1, "session_id": sid, "occurred_at": now(), **facts}
    if context.get("task_id"):
        payload["task_id"] = context["task_id"]
    if context.get("active_todo_id") and "todo_id" not in payload:
        payload["todo_id"] = context["active_todo_id"]
    if context.get("request_id"):
        payload["request_id"] = context["request_id"]
    emit("workflow." + kind, payload, flush_now=False)


def load(db: sqlite3.Connection, sid: str) -> dict:
    row = db.execute("SELECT context FROM sessions WHERE id=?", (sid,)).fetchone()
    return json.loads(row[0]) if row else {}


def save(db: sqlite3.Connection, sid: str, context: dict) -> None:
    db.execute("INSERT OR REPLACE INTO sessions VALUES (?,?)", (sid, json.dumps(context)))


def ensure_session(sid: str, context: dict) -> None:
    if not context.get("started"):
        record("session_started", sid, {}, status="running", source="local")
        context["started"] = True
    if not context.get("task_id"):
        context["task_id"] = str(uuid.uuid4())
        record("task_started", sid, context, name="Session work", description="Task summary has not been supplied.", status="running")


def short_label(value: str) -> str:
    return " ".join(value.split()[:5])


def runtime_metadata(sid: str, context: dict, payload: dict | None = None, agent: str | None = None) -> None:
    payload = payload or {}
    facts = {}
    cwd = payload.get("cwd")
    if isinstance(cwd, str) and Path(cwd).is_absolute() and len(cwd.encode()) <= 2000:
        facts["cwd"] = cwd
    actor = agent or payload.get("agent")
    if actor in ("codex", "Codex"):
        facts["agent"] = "Codex"
    elif actor in ("claude-code", "Claude Code"):
        facts["agent"] = "Claude Code"
    model = payload.get("model")
    if isinstance(model, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,199}", model):
        facts["model"] = model
    # Recover only runtime metadata from this exact session's native record.
    candidate = payload.get("transcript_path") or context.get("transcript_path")
    if not context.get("metadata_checked_at") or time.time() - context.get("metadata_checked_at", 0) > 10 or payload.get("hook_event_name") in ("SessionStart", "UserPromptSubmit"):
        override = os.environ.get("HARNESS_NATIVE_SESSION_STATE")
        states = [Path(override)] if override else sorted((Path.home() / ".codex").glob("state_*.sqlite"), reverse=True)
        for state in states:
            try:
                with sqlite3.connect(state.resolve().as_uri() + "?mode=ro", uri=True, timeout=0.1) as native_db:
                    columns = {row[1] for row in native_db.execute("PRAGMA table_info(threads)")}
                    fields = [key for key in ("name", "title", "model", "rollout_path", "cwd") if key in columns]
                    if not fields or "id" not in columns:
                        continue
                    row = native_db.execute("SELECT " + ",".join(fields) + " FROM threads WHERE id=?", (sid,)).fetchone()
                    if row:
                        values = dict(zip(fields, row))
                        facts.setdefault("agent", "Codex")
                        title = values.get("name") or values.get("title")
                        if isinstance(title, str) and title.strip():
                            facts["session_name"] = short_label(title)[:2000]
                        native_cwd = values.get("cwd")
                        if isinstance(native_cwd, str) and Path(native_cwd).is_absolute() and len(native_cwd.encode()) <= 2000:
                            facts.setdefault("cwd", native_cwd)
                        native_model = values.get("model")
                        if isinstance(native_model, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,199}", native_model):
                            facts.setdefault("model", native_model)
                        candidate = candidate or values.get("rollout_path")
                        break
            except (OSError, sqlite3.Error, ValueError):
                continue
    if not candidate and re.fullmatch(r"[A-Za-z0-9-]{1,256}", sid):
        paths = list((Path.home() / ".codex/sessions").glob("*/*/*/*" + sid + ".jsonl"))
        paths += list((Path.home() / ".claude/projects").glob("*/" + sid + ".jsonl"))
        candidate = str(paths[0]) if len(paths) == 1 else None
    if candidate and (not context.get("model") or payload.get("hook_event_name") in ("SessionStart", "UserPromptSubmit") or time.time() - context.get("metadata_checked_at", 0) > 10):
        try:
            path = Path(candidate)
            with path.open("rb") as stream:
                head = stream.read(65536)
                stream.seek(0, 2)
                size = stream.tell()
                start = max(len(head), size - 1_000_000)
                stream.seek(start)
                tail = stream.read(1_000_000)
            lines = head.splitlines() + (tail.splitlines()[1:] if start < size else [])
            records = []
            for line in lines:
                try:
                    entry = json.loads(line)
                    if isinstance(entry, dict):
                        records.append(entry)
                except (ValueError, UnicodeError):
                    pass
            native = next((r for r in records if r.get("type") == "session_meta" and r.get("payload", {}).get("id") == sid), None)
            matched = native is not None or any(r.get("sessionId") == sid for r in records)
            if matched:
                context["transcript_path"] = str(path)
                facts.setdefault("agent", "Codex" if native else "Claude Code")
                for entry in records:
                    entry_cwd = entry.get("payload", {}).get("cwd") if native and entry.get("type") in ("session_meta", "turn_context") else entry.get("cwd") if not native and entry.get("sessionId") == sid else None
                    if isinstance(entry_cwd, str) and Path(entry_cwd).is_absolute() and len(entry_cwd.encode()) <= 2000 and "cwd" not in payload:
                        facts["cwd"] = entry_cwd
                    value = None
                    if native and entry.get("type") == "turn_context":
                        value = entry.get("payload", {}).get("model")
                    elif not native and entry.get("sessionId") == sid and entry.get("type") == "assistant":
                        value = entry.get("message", {}).get("model")
                    if isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,199}", value):
                        facts["model"] = value
                    title = entry.get("aiTitle") if entry.get("type") == "ai-title" else None
                    if isinstance(title, str) and title.strip():
                        facts["session_name"] = short_label(title)[:2000]
                # A model supplied by the hook describes the active call most precisely.
                if isinstance(model, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:/-]{0,199}", model):
                    facts["model"] = model
            context["metadata_checked_at"] = time.time()
        except (OSError, ValueError, TypeError, AttributeError):
            pass
    changed = {key: value for key, value in facts.items() if context.get(key) != value}
    if changed:
        record("session_updated", sid, {}, **changed)
        context.update(changed)
    herdr_metadata(sid, context, payload)


# Secrets that commonly appear in commands: key=value pairs, auth headers,
# credentials in URLs and long opaque tokens. Matches are replaced, not dropped,
# so the label still shows the shape of the command.
SECRET_PATTERNS = (
    (re.compile(r"(?i)\b(bearer|basic|token)\s+[A-Za-z0-9._~+/=-]{8,}"), r"\1 ***"),
    (re.compile(r"(?i)\b([\w.-]*(?:key|token|secret|passw(?:or)?d|pwd|auth|credential|cookie|session)[\w.-]*)(\s*[=:]\s*)(\"[^\"]*\"|'[^']*'|\$\([^)]*\)|[^\s;&|]+)"), r"\1\2***"),
    (re.compile(r"(?i)(?<!\S)(-u|-p|--user|--pass\w*|--[\w-]*(?:token|secret|key|auth)[\w-]*)(\s+)(\"[^\"]*\"|'[^']*'|[^\s;&|]+)"), r"\1\2***"),
    (re.compile(r"://[^/\s:@]+:[^/\s@]+@"), "://***@"),
    (re.compile(r"\b(?:sk|pk|rk|ghp|gho|ghs|ghu|github_pat|xox[abprs]|AKIA|ASIA|glpat|npm)[-_][A-Za-z0-9_-]{8,}"), "***"),
    (re.compile(r"\b(?=[A-Za-z0-9+/_=-]*\d)(?=[A-Za-z0-9+/_=-]*[A-Za-z])[A-Za-z0-9+/_=-]{32,}\b"), "***"),
)


def redact(text: str) -> str:
    for pattern, replacement in SECRET_PATTERNS:
        text = pattern.sub(replacement, text)
    return text


def tool_label(tool: str, tool_input) -> str | None:
    """A short, redacted line saying what a tool call did: the agent's own
    description of a command, otherwise the command itself, or the file name
    (never the folder) for file tools. File contents and patches are never kept."""
    if not isinstance(tool_input, dict):
        return None
    text = lambda key: tool_input.get(key) if isinstance(tool_input.get(key), str) else ""
    label = ""
    if tool in SHELL_TOOLS:
        command = tool_input.get("command", tool_input.get("cmd"))
        if isinstance(command, list):
            command = " ".join(str(part) for part in command)
        label = text("description") or (command if isinstance(command, str) else "")
    elif tool in ("Read", "Write", "Edit", "MultiEdit", "NotebookEdit", "view_image"):
        path = text("file_path") or text("notebook_path") or text("path")
        label = os.path.basename(path.rstrip("/"))
    elif tool == "apply_patch":
        patch = text("input") or text("patch")
        names = re.findall(r"^\*\*\* (?:Add|Update|Delete) File: (.+)$", patch, re.M)
        label = ", ".join(dict.fromkeys(os.path.basename(name.strip()) for name in names))
    elif tool in ("Grep", "Glob"):
        label = text("pattern")
    elif tool in ("Agent", "Task"):
        label = text("description")
    label = " ".join(redact(label).split())
    return label[:157] + "..." if len(label) > 160 else label or None


SHELL_TOOLS = ("Bash", "shell", "exec_command", "local_shell", "container.exec", "BashOutput")
COMMAND_LIMIT = 1900


def tool_command(tool: str, tool_input) -> str | None:
    """The redacted shell command itself, so a failed call shows exactly what ran
    even when the label is the agent's description. Kept under the service's
    2000-byte field limit; output is never recorded."""
    if tool not in SHELL_TOOLS or not isinstance(tool_input, dict):
        return None
    command = tool_input.get("command", tool_input.get("cmd"))
    if isinstance(command, list):
        command = shlex.join(str(part) for part in command)
    if not isinstance(command, str) or not command.strip():
        return None
    command = redact(command.strip())
    if len(command.encode()) > COMMAND_LIMIT:
        command = command.encode()[:COMMAND_LIMIT - 3].decode(errors="ignore") + "..."
    return command


def herdr_metadata(sid: str, context: dict, payload: dict) -> None:
    # Read the Herdr tab label at start and on each prompt so renames show up.
    tab_id = os.environ.get("HERDR_TAB_ID", "")
    if not re.fullmatch(r"[A-Za-z0-9:_-]{1,64}", tab_id):
        return
    if context.get("herdr_tab") and payload.get("hook_event_name") not in ("SessionStart", "UserPromptSubmit"):
        return
    try:
        result = subprocess.run([os.environ.get("HERDR_BIN_PATH") or "herdr", "tab", "get", tab_id],
            capture_output=True, text=True, timeout=1, check=False)
        label = json.loads(result.stdout)["result"]["tab"]["label"]
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        return
    if not isinstance(label, str) or not label.strip():
        return
    label = " ".join(label.split())[:200]
    # A separate event keeps other metadata accepted by servers that predate this field.
    if context.get("herdr_tab") != label:
        record("session_updated", sid, {}, herdr_tab=label)
        context["herdr_tab"] = label


def check_record(sid: str, context: dict, kind: str, path: Path) -> None:
    raw = path.read_text()
    digest = hashlib.sha256(raw.encode()).hexdigest()
    key = "check:" + kind + ":" + str(path)
    if context.get(key) == digest:
        return
    facts = dict(line.split("=", 1) for line in raw.splitlines() if "=" in line)
    exit_code = int(facts["EXIT"])
    record(kind if kind == "review" else "verification", sid, context,
           check=kind, exit_code=exit_code, outcome="passed" if exit_code == 0 else "failed",
           **{k.lower(): int(facts[k]) for k in ("RAN", "SKIPPED", "FAILURES") if facts.get(k, "").isdigit()})
    context[key] = digest
    context.setdefault("checks", {})[kind] = {"exit_code":exit_code,"failures":int(facts.get("FAILURES", "0")),"at":facts.get("RECORD_AT", ""),
                                              "tree":facts.get("TREE_HASH", ""),"project":facts.get("PROJECT_ROOT", "")}


def hook(agent: str | None = None) -> int:
    # Cap hook input in memory; persist only the fields enabled by collection policy.
    raw = sys.stdin.read(1_000_001)
    if len(raw) > 1_000_000:
        return 0
    payload = json.loads(raw or "{}")
    sid = session_id(payload.get("session_id"))
    if not sid or not enabled("workflow.session_started"):
        return 0
    event = payload.get("hook_event_name")
    if event not in ("SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse", "PostToolUseFailure"):
        return 0
    with context_db() as db:
        db.execute("BEGIN IMMEDIATE")
        context = load(db, sid)
        was_started = context.get("started")
        ensure_session(sid, context)
        runtime_metadata(sid, context, payload, agent)
        if event == "SessionStart":
            if was_started:
                source = payload.get("source")
                source = source if source in ("startup", "resume", "clear", "compact") else "resume"
                record("session_started", sid, {}, status="running", source=source)
            # The native environment file binds child harness commands to this session.
            env_file = os.environ.get("CLAUDE_ENV_FILE")
            if env_file:
                with open(env_file, "a") as output:
                    output.write("\nexport HARNESS_SESSION_ID=" + shlex.quote(sid) + "\n")
            print(f"Workflow audit on for session {sid} (prompt collection "
                  f"{'on' if enabled('workflow.prompt_recorded') else 'off'}). Before editing: "
                  f"{ROOT}/scripts/harness workflow todo plan --items JSON --reason TEXT, then todo update "
                  "--status in_progress; reads, harness commands, verify and review need no todo. "
                  "Commands: docs/setup.md, section Audited todo enforcement.")
        elif event == "UserPromptSubmit" and enabled("workflow.prompt_recorded"):
            prompt = payload.get("prompt")
            if isinstance(prompt, str) and prompt.strip():
                prompt_id = str(uuid.uuid4())
                # UTF-8 chunks preserve long prompts without truncation or oversized events.
                chunks, chunk = [], ""
                for char in prompt:
                    if len((chunk + char).encode()) > 2000:
                        chunks.append(chunk)
                        chunk = ""
                    chunk += char
                if chunk:
                    chunks.append(chunk)
                for index, text in enumerate(chunks):
                    record("prompt_recorded", sid, context, prompt_id=prompt_id,
                           part_index=index, part_count=len(chunks), text=text)
        elif event == "SessionEnd":
            record("session_ended", sid, {}, status="ended")
        elif event in ("PreToolUse", "PostToolUse", "PostToolUseFailure"):
            tool = payload.get("tool_name")
            call_id = payload.get("tool_use_id", payload.get("tool_call_id"))
            call_id = call_id if isinstance(call_id, str) and 0 < len(call_id.encode()) <= 256 else None
            if isinstance(tool, str) and 0 < len(tool.encode()) <= 200:
                facts = {"tool_name": tool, "source": event}
                if call_id:
                    facts["tool_call_id"] = call_id
                label = tool_label(tool, payload.get("tool_input"))
                command = tool_command(tool, payload.get("tool_input"))
                pending = context.setdefault("pending_tools", {})
                if event == "PreToolUse":
                    if call_id:
                        pending[call_id] = {"started_at": now(), "clock": time.monotonic(), "task_id": context["task_id"], "todo_id": context.get("active_todo_id"), "tool_name": tool, "label": label, "command": command}
                        # Bound state when a runtime never emits completion hooks.
                        while len(pending) > 1000:
                            pending.pop(next(iter(pending)))
                    if label:
                        facts["tool_label"] = label
                    if command:
                        facts["tool_command"] = command
                    record("tool_started", sid, context, **facts, outcome="running")
                else:
                    response = payload.get("tool_response")
                    response = response if isinstance(response, dict) else {}
                    code = response.get("exit_code", response.get("exitCode"))
                    code = code if type(code) is int else None
                    failed = event == "PostToolUseFailure" or response.get("isError") is True or response.get("is_error") is True or (code is not None and code != 0)
                    process = response.get("session_id")
                    active_process = code is None and type(process) is int and not failed
                    result = "failed" if failed else "process_running" if active_process else "succeeded" if code == 0 else "returned"
                    start = pending.pop(call_id, None) if call_id else None
                    binding = context
                    if start and start["tool_name"] == tool:
                        facts["started_at"] = start["started_at"]
                        facts["duration_ms"] = max(0, round((time.monotonic() - start["clock"]) * 1000))
                        binding = {"task_id": start["task_id"]}
                        if start.get("todo_id"):
                            facts["todo_id"] = start["todo_id"]
                        label = label or start.get("label")
                        command = command or start.get("command")
                    if label:
                        facts["tool_label"] = label
                    if command:
                        facts["tool_command"] = command
                    if active_process:
                        facts["process_id"] = str(process)
                    record("tool_failed" if failed else "tool_completed", sid, binding,
                           **facts, exit_code=code, outcome=result)
        save(db, sid, context)
    # The service delivery worker drains the queue; hook latency stays local.
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    hook_parser = sub.add_parser("hook")
    hook_parser.add_argument("--agent", choices=("codex", "claude-code"))
    sub.add_parser("refresh-metadata", help="Recover runtime metadata for already observed sessions")
    sub.add_parser("flush")
    gate = sub.add_parser("gate")
    gate.add_argument("--check", choices=("plan", "active", "complete"), required=True)
    todo = sub.add_parser("todo")
    todo.add_argument("todo_action", choices=("plan", "confirm", "update", "exempt", "show"))
    todo.add_argument("--items")
    todo.add_argument("--reason")
    todo.add_argument("--id")
    todo.add_argument("--status", choices=sorted(todos.STATUSES))
    todo.add_argument("--evidence")
    task = sub.add_parser("task", help="Supply a curated name and summary for the current task")
    task.add_argument("--name", required=True)
    task.add_argument("--description", required=True)
    task.add_argument("--new", action="store_true")
    task.add_argument("--parent-task-id")
    decision = sub.add_parser("decision")
    decision.add_argument("--description", required=True)
    decision.add_argument("--option", action="append", required=True)
    decision.add_argument("--selected", required=True)
    outcome = sub.add_parser("outcome")
    outcome.add_argument("--status", choices=("completed", "blocked", "running"), required=True)
    outcome.add_argument("--description", required=True)
    phase = sub.add_parser("phase")
    phase.add_argument("--db-root", type=Path, required=True)
    phase.add_argument("--phase", choices=("plan", "build", "review"), required=True)
    check = sub.add_parser("check")
    check.add_argument("--kind", choices=("verify", "review"), required=True)
    check.add_argument("--record", type=Path, required=True)
    for command in (task, decision, outcome, phase, check, todo, gate):
        command.add_argument("--session-id")
    args = parser.parse_args()
    if args.command == "hook":
        return hook(args.agent)
    if args.command == "flush":
        return 0 if flush() else 1
    if args.command == "refresh-metadata":
        if not enabled("workflow.session_updated"):
            return 0
        with context_db() as db:
            db.execute("BEGIN IMMEDIATE")
            for (observed_sid,) in db.execute("SELECT id FROM sessions").fetchall():
                context = load(db, observed_sid)
                context["metadata_checked_at"] = 0
                runtime_metadata(observed_sid, context)
                save(db, observed_sid, context)
        flush()
        return 0
    sid = session_id(args.session_id)
    if not sid:
        # An agent always has a session; a gate it cannot attribute fails closed.
        # A human at a terminal has no todo plan to check.
        if args.command == "gate" and (os.environ.get("CLAUDECODE") == "1" or os.environ.get("CODEX_SANDBOX")):
            raise ValueError("the todo gate needs this session's ID (HARNESS_SESSION_ID) inside an agent")
        if args.command in ("phase", "check", "gate"):
            return 0
        parser.error("--session-id is required when no native session environment is available")
    for key in ("name", "description", "parent_task_id"):
        value = getattr(args, key, None)
        if value is not None and (not value.strip() or len(value.encode()) > 2000):
            parser.error(key + " must be nonempty and at most 2000 bytes")
    if args.command == "decision":
        if len(args.option) > 20 or any(len(s.encode()) > 200 or not s.strip() for s in args.option) or args.selected not in args.option:
            parser.error("selected option must be one of up to 20 short, nonempty options")
    if args.command == "todo" and ((args.todo_action == "plan" and not args.items) or (args.todo_action == "update" and (not args.id or not args.status))):
        parser.error("plan needs --items; update needs --id and --status")
    with context_db() as db:
        db.execute("BEGIN IMMEDIATE")
        context = load(db, sid)
        ensure_session(sid, context)
        runtime_metadata(sid, context)
        if args.command == "todo":
            todos.handle(args, sid, context, record)
        elif args.command == "gate":
            if args.check == "complete": todos.assert_complete(context)
            else: todos.assert_plan(context, active=args.check == "active")
        elif args.command == "task":
            if args.new:
                if todos.unresolved(context) and context.get("task_status") != "blocked":
                    raise ValueError("Resolve the current plan or explicitly record a blocked task before starting another")
                context["task_id"] = str(uuid.uuid4())
                todos.reset_task(context)
            record("task_started" if args.new else "task_updated", sid, context,
                   name=args.name, description=args.description, parent_task_id=args.parent_task_id, status="running")
        elif args.command == "decision":
            record("decision", sid, context, description=args.description, options=args.option, selected=args.selected)
        elif args.command == "outcome":
            if args.status == "completed": todos.assert_complete(context)
            context["task_status"] = args.status
            record("task_completed" if args.status == "completed" else "task_blocked" if args.status == "blocked" else "task_updated",
                   sid, context, status=args.status, outcome=args.description)
        elif args.command == "phase":
            run_id = current_file(args.db_root, sid).read_text().strip()
            state = dict(line.split("=", 1) for line in (args.db_root / "runs" / run_id / "state").read_text().splitlines() if "=" in line)
            record("phase_changed", sid, context, run_id=run_id, phase=args.phase,
                   phase_status=state.get("PHASE_" + args.phase.upper(), "unknown"))
        elif args.command == "check":
            check_record(sid, context, args.kind, args.record)
        save(db, sid, context)
    flush()
    if args.command == "todo" and args.todo_action == "show":
        print(json.dumps({"session_id":sid,"task_id":context["task_id"],"todos":list(context.get("todos",{}).values()),"revision":context.get("plan_revision",0),"confirmed":context.get("confirmed_request")==context.get("request_seq",0)}))
    elif args.command == "todo":
        # One line: the full list is `todo show`, so every update stays cheap to read.
        live = [item for item in context.get("todos", {}).values() if item["status"] != "removed"]
        done = sum(item["status"] == "completed" for item in live)
        detail = f"{args.id} {args.status}; " if args.todo_action == "update" else ""
        ids = "; ids: " + ", ".join(item["id"] for item in live) if args.todo_action == "plan" else ""
        print(f"todo {args.todo_action}: {detail}{done}/{len(live)} complete, active {context.get('active_todo_id') or 'none'}, "
              f"revision {context.get('plan_revision', 0)}{ids}")
    if args.command in ("task", "decision", "outcome"):
        print(json.dumps({"session_id": sid, "task_id": context["task_id"], "recorded": True}))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, sqlite3.Error, KeyError) as error:
        # Audit infrastructure remains advisory; never include raw input in diagnostics.
        # Policy refusals are the harness's own messages and say what to do next.
        if type(error) is ValueError and "gate" in sys.argv:
            print("WORKFLOW GATE: " + str(error), file=sys.stderr)
        else:
            print("WORKFLOW COLLECTION UNAVAILABLE: " + type(error).__name__, file=sys.stderr)
        sys.exit(0 if "hook" in sys.argv or "phase" in sys.argv or "check" in sys.argv else 1)
