#!/usr/bin/env python3
"""Fail-closed todo guard for native local tool hooks."""
import io
import os
from contextlib import redirect_stdout
import json
from pathlib import Path
import re
import shlex
import sys
import traceback
import uuid
import permit
import workflow_audit as audit
from workflow_todos import assert_plan

READ_TOOLS = {"StructuredOutput", "Read", "Grep", "Glob", "read_file", "list_dir", "update_plan", "request_user_input", "request_user_input_async", "webrun", "web.run", "web__run"}
GRAPH_READS = {"search_graph", "trace_path", "get_code_snippet", "check_index_coverage", "query_graph", "get_architecture", "list_projects", "index_status"}
# Commands that only read. Writes by redirection are caught separately by the parser.
READ_ONLY = {"cat", "head", "tail", "wc", "ls", "tree", "grep", "egrep", "fgrep", "rg", "cut", "tr", "uniq",
             "diff", "cmp", "file", "stat", "du", "df", "pwd", "echo", "printf", "which", "type", "basename",
             "dirname", "realpath", "readlink", "date", "id", "whoami", "hostname", "uname", "jq", "column",
             "nl", "od", "xxd", "md5sum", "sha1sum", "sha256sum", "cksum", "pdfinfo", "test", "[", "true",
             "false", "cd", "less", "more", "env", "printenv", "nproc", "paste", "comm", "join", "tac", "rev",
             "seq", "fold", "expand"}
GIT_READS = {"status", "log", "diff", "show", "rev-parse", "ls-files", "blame", "grep", "describe", "shortlog",
             "rev-list", "merge-base", "cat-file", "ls-tree", "for-each-ref", "show-ref", "whatchanged"}

CURSOR_EVENTS = {"beforeSubmitPrompt": "UserPromptSubmit", "preToolUse": "PreToolUse", "stop": "Stop"}

def cursor_payload(payload):
    """Adapt imported policy hooks without accepting unrelated lifecycle events."""
    result = dict(payload)
    result["hook_event_name"] = CURSOR_EVENTS.get(payload.get("hook_event_name"), payload.get("hook_event_name"))
    # Conversation identity is stable across generations. Never adopt the
    # launching shell's session when the runtime omitted its own identity.
    sid = payload.get("conversation_id") or payload.get("session_id")
    if result["hook_event_name"] != "Stop":
        if not isinstance(sid, str) or not sid.strip():
            raise ValueError("Native conversation ID is required by the todo gate")
        audit.session_id(sid)
    result["session_id"] = sid
    roots = payload.get("workspace_roots")
    if not result.get("cwd") and isinstance(roots, list) and len(roots) == 1 and isinstance(roots[0], str):
        result["cwd"] = roots[0]
    return result

def matches(word, target, cwd):
    return permit.our_script(word, target, cwd, audit.ROOT)

# Harness bookkeeping that needs no active todo; continue, contract, prune,
# advise, and route still need one.
TODO_FREE = {"workflow", "plan", "build", "status", "root", "step", "brief", "decision"}

def named_sessions(words):
    """Every --session-id value in words, in either spelling."""
    found = []
    for index, word in enumerate(words):
        if word == "--session-id":
            found.append(words[index + 1] if index + 1 < len(words) else "")
        elif word.startswith("--session-id="):
            found.append(word.split("=", 1)[1])
    return found

def management_segment(words, cwd, sid=None, *, decision_bookkeeping=False):
    if matches(words[0], "scripts/harness", cwd):
        # A call aimed at another session is judged like any other command.
        if any(value != sid for value in named_sessions(words)):
            return False
        sub, second, _session = permit.harness_subcommand(words)
        if sub == "config":
            return second in ("show", "init", "update")
        if sub == "dispatch":
            return second in ("status", "cancel") or (decision_bookkeeping and second == "create")
        if sub == "review":
            return second in ("start", "done")
        if sub == "jev":
            return second == "status"
        return sub in TODO_FREE
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

def management_command(payload, *, decision_bookkeeping=False, reads_only=False):
    """A shell call the todo gate lets through without an active todo: harness
    bookkeeping, the verify and review sensors, and read-only commands, alone or
    joined with &&, ;, or pipes. Command substitution and any write disqualify it."""
    data = payload.get("tool_input") or {}
    command = data.get("command", data.get("cmd", "")) if isinstance(data, dict) else ""
    if not isinstance(command, str) or not command.strip() or any(value in command for value in ("`", "$(", "<(", ">(")):
        return False
    cwd = Path(payload.get("cwd") or ".")
    workdir = data.get("workdir") or data.get("cwd") or data.get("working_directory")
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
    if decision_bookkeeping and len(bodies) == 1 and len(segments) == 1:
        segment = permit.read_segment(segments[0], list(bodies))
        words = segment.argv
        if (words and matches(words[0], "scripts/harness", cwd)
                and permit.harness_subcommand(words)[:2] == ("dispatch", "create")
                and "--plan" in words and words[words.index("--plan") + 1:words.index("--plan") + 2] == ["/dev/stdin"]
                and not analysis.targets and not analysis.inline and not analysis.opaque
                and all(value == payload.get("session_id") for value in named_sessions(words))):
            try:
                plan = json.loads(bodies[0])
                return isinstance(plan, dict) and isinstance(plan.get("workers"), list) and len(bodies[0].encode()) <= 32000
            except ValueError:
                return False
    if analysis.inline or analysis.targets or analysis.opaque or bodies:
        return False
    for segment in segments:
        words = permit.read_segment(segment, []).argv
        if words and not ((not reads_only and management_segment(words, cwd, payload.get("session_id"), decision_bookkeeping=decision_bookkeeping)) or read_only_segment(words)):
            return False
    return True

NOTICE_BLOCK = re.compile(r"<(task-notification|system-reminder)>.*?</\1>", re.S)

def runtime_notice(payload):
    """A prompt made only of runtime notices (a background task's result) carries
    no new words from the person, so it does not open a new request."""
    prompt = payload.get("prompt")
    if not isinstance(prompt, str) or "<task-notification>" not in prompt:
        return False
    return not NOTICE_BLOCK.sub("", prompt).strip()

def authorize(payload):
    event = payload.get("hook_event_name")
    # Ending a conversational turn is not task completion. Do not access policy
    # state here: even a storage outage must not trap the user in a Stop loop.
    if event == "Stop":
        return 0
    sid = audit.session_id(payload.get("session_id"))
    if not sid: raise ValueError("Native session ID is required by the todo gate")
    with audit.context_db() as db:
        db.execute("BEGIN IMMEDIATE")
        context = audit.load(db, sid)
        audit.ensure_session(sid, context)
        if event == "UserPromptSubmit" and context.get("request_id") and runtime_notice(payload):
            # A background task finishing is not a new request from the person:
            # the current prompt, its confirmed plan, and its role plan carry on.
            print("Todo gate: background notice; the current prompt's todo and dispatch plans still apply.")
        elif event == "UserPromptSubmit":
            context["request_seq"] = context.get("request_seq", 0) + 1
            context["request_id"] = str(uuid.uuid4())
            context["orchestration_project"] = str(Path(payload.get("cwd") or ".").resolve())
            context.pop("confirmed_request", None)
            audit.record("plan_required", sid, context, description="Confirm or revise the complete todo plan for this prompt", status="required")
            print(f"Todo gate: confirm (todo confirm), revise (todo plan), or exempt (todo exempt, for a question) the plan for this prompt "
                  f"via {audit.ROOT}/scripts/harness workflow todo ... --session-id {sid} --reason TEXT; reads need no todo.")
        elif event == "PreToolUse":
            tool = payload.get("tool_name", "")
            readonly = tool in READ_TOOLS or any(tool.endswith("__"+name) for name in GRAPH_READS)
            if (readonly and tool != "StructuredOutput") or management_command(payload, reads_only=True):
                context["evidence_revision"] = context.get("evidence_revision", 0) + 1
            if not readonly and not management_command(payload):
                assert_plan(context, active=True)
                context["last_execution_at"] = audit.now()
        else:
            raise ValueError("Unsupported policy event")
        audit.save(db, sid, context)
    decision_seam(payload, context)
    return 0

def plan_bootstrap(payload, config):
    """Validate bounded JSON plan writes; todo and permission gates still apply."""
    data = payload.get('tool_input') or {}
    if not isinstance(data, dict):
        return False
    raw, path = data.get('content'), data.get('file_path', data.get('path', ''))
    if payload.get('tool_name') not in ('Write', 'write_file'):
        command = data.get('command', data.get('cmd', ''))
        if not isinstance(command, str) or any(token in command for token in ('`', '$(', '<(', '>(')):
            return False
        try:
            text, bodies = permit.preprocess(command)
            segments = permit.split_segments(permit.tokenize(text))
            if len(bodies) != 1 or len(segments) != 1:
                return False
            segment = permit.read_segment(segments[0], list(bodies))
            if segment.argv != ['cat'] or len(segment.outputs) != 1:
                return False
            raw, path = bodies[0], segment.outputs[0]
        except permit.Unparseable:
            return False
    if not isinstance(raw, str) or len(raw.encode()) > 32000 or not isinstance(path, str):
        return False
    target = Path(payload.get('cwd') or '.') / path
    if target.suffix != '.json' or 'dispatch' not in target.name.lower():
        return False
    try:
        plan = json.loads(raw)
        from role_dispatch import validate
        validate(plan, Path(payload.get('cwd') or '.').resolve(), config['dispatch'])
    except (ValueError, TypeError, KeyError):
        return False
    return True


def decision_seam(payload, context):
    """Bounded execution policy outside the workflow database transaction.

    Native hooks cannot observe internal thoughts; explicit checkpoints cover
    reasoning, evidence and context decisions. Permission checks remain separate.
    """
    import harness_config
    config = harness_config.load()
    enforcement = config['typesafe'].get('enforce_decisions', False)
    dispatch = config['dispatch']['enabled']
    if not enforcement and not dispatch:
        return
    event = payload.get('hook_event_name')
    if event == 'UserPromptSubmit':
        if runtime_notice(payload):
            return
        if enforcement:
            print('Decision policy: use harness decision evaluate for bounded reasoning, context, evidence and handoff judgments; deterministic rules bypass advice.')
        if dispatch:
            print('Dispatch policy: record a role plan for this prompt with harness dispatch create --plan PATH before execution, including main-only for trivial work.')
        return
    tool = payload.get('tool_name', '')
    if tool in READ_TOOLS or any(tool.endswith('__' + name) for name in GRAPH_READS) or management_command(payload, decision_bookkeeping=True) or plan_bootstrap(payload, config):
        return
    project = Path(context.get('orchestration_project') or context.get('cwd') or payload.get('cwd') or '.').resolve()
    sid = payload['session_id']
    if dispatch:
        from role_dispatch import session_has_plan
        if not session_has_plan(project, sid, context.get('request_id')):
            raise ValueError('Dispatch plan required for this prompt: use harness dispatch create --plan PATH, including main-only for trivial work. '
                             'Main-only in one step: ' + str(audit.ROOT) + '/scripts/harness dispatch create --session-id ' + sid
                             + ' --plan /dev/stdin <<\'JSON\' {"workers": [], "rationale": "WHY", "acceptance": ["CHECK"]} JSON (the JSON on its own line)')
    if not enforcement:
        return
    from decision_checkpoint import evaluate, claim, digest, resolve
    scope = {'request_id': context.get('request_id'), 'request_seq': context.get('request_seq'),
             'plan_revision': context.get('plan_revision'), 'action_hash': digest([tool, payload.get('tool_input')])}
    state = {'scope': scope, 'active_todo_id': context.get('active_todo_id'),
             'evidence_revision': context.get('evidence_revision', 0)}
    prior = {key: os.environ.get(key) for key in ('HARNESS_JEV_TIMEOUT', 'HARNESS_JEV_ATTEMPTS')}
    os.environ['HARNESS_JEV_TIMEOUT'] = '5'
    os.environ['HARNESS_JEV_ATTEMPTS'] = '1'
    try:
        from jev_context import build
        routing = build(payload, context)
        receipt = evaluate(project=project, session_id=sid, family='tool_selection',
        goal=routing['goal'], facts=routing['facts'], origin=routing['origin'], context_version=routing['context_version'],
        choices=routing['choices'],
        scope=scope, state=state)
    finally:
        for key, value in prior.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
    if receipt.get('needs_reasoning'):
        # Below the confidence threshold (or without a usable answer) the main agent
        # decides, and its decision is the tool call it proposed.
        receipt = resolve(project=project, identifier=receipt['id'], session_id=sid, action='execute',
                          reason='main agent proposed this tool call', scope=scope, state=state)
    if receipt['action'] == 'insufficient_information':
        raise ValueError('Decision checkpoint selected insufficient_information; supply current action context with harness workflow todo context --action-summary SUMMARY --finding FINDING --gap GAP --reason SUMMARY')
    if receipt['action'] != 'execute':
        raise ValueError('Decision checkpoint selected gather_context; perform relevant reads and evaluate the revised action state')
    claim(project=project, identifier=receipt['id'], session_id=sid, scope=scope, state=state, action_hash=scope['action_hash'])


def decision_posttool(payload):
    import harness_config
    if not harness_config.load()['typesafe'].get('enforce_decisions', False):
        return
    from decision_checkpoint import directory, digest, outcome
    sid = payload.get('session_id')
    if not isinstance(sid, str) or not sid:
        return
    with audit.context_db() as db:
        db.execute('BEGIN IMMEDIATE')
        context = audit.load(db, sid)
        from jev_context import observe_read
        if observe_read(payload, context):
            audit.save(db, sid, context)
    project = Path(context.get('orchestration_project') or context.get('cwd') or payload.get('cwd') or '.').resolve()
    fingerprint = digest([payload.get('tool_name', ''), payload.get('tool_input')])
    response = payload.get('tool_response') or {}
    code = response.get('exit_code') if isinstance(response, dict) else None
    duration = response.get('duration_ms') if isinstance(response, dict) else None
    for path in directory(project).glob('*.json'):
        row = json.loads(path.read_text())
        if row.get('parent_session_id') == sid and row.get('action_hash') == fingerprint and row.get('claimed_at') and not row.get('outcome'):
            outcome(project=project, identifier=row['id'], session_id=sid, exit_code=code, duration_ms=duration)
            break


def main():
    raw = sys.stdin.read(1_000_001)
    if len(raw)>1_000_000: raise ValueError("Hook payload exceeds policy limit")
    payload=json.loads(raw)
    if not isinstance(payload,dict): raise ValueError("Invalid hook payload")
    if payload.get("hook_event_name") in CURSOR_EVENTS or "cursor_version" in payload:
        return cursor_main(payload)
    if payload.get('hook_event_name') in ('PostToolUse', 'PostToolUseFailure'):
        decision_posttool(payload)
        sys.stdin = io.StringIO(raw)
        return audit.hook(sys.argv[1] if len(sys.argv)>1 else None)
    authorize(payload)
    if payload.get("hook_event_name") in {"PreToolUse", "UserPromptSubmit"}:
        sys.stdin=io.StringIO(raw)
        return audit.hook(sys.argv[1] if len(sys.argv)>1 else None)
    print("{}")
    return 0

def cursor_main(payload):
    event = payload.get("hook_event_name")
    try:
        normalized = cursor_payload(payload)
        # Cursor expects a single JSON response, not native prompt guidance on stdout.
        with redirect_stdout(io.StringIO()):
            authorize(normalized)
            if normalized["hook_event_name"] in {"PreToolUse", "UserPromptSubmit"}:
                sys.stdin = io.StringIO(json.dumps(normalized))
                audit.hook("cursor")
    except Exception as error:
        reason = str(error) if isinstance(error, ValueError) else type(error).__name__
        message = "TODO GATE: " + reason
        sid = payload.get("conversation_id") or payload.get("session_id")
        if isinstance(sid, str) and sid.strip() and len(sid.encode()) <= 256 and "\n" not in sid and "\r" not in sid:
            message += f". Use {audit.ROOT}/scripts/harness workflow todo with --session-id {shlex.quote(sid)}."
        response = {"continue": False, "user_message": message} if event in {"beforeSubmitPrompt", "UserPromptSubmit"} else {
            "permission": "deny", "user_message": message, "agent_message": message}
        print(json.dumps(response))
        print(message, file=sys.stderr)
        return 2
    if normalized["hook_event_name"] == "UserPromptSubmit":
        response = {"continue": True}
    elif normalized["hook_event_name"] == "PreToolUse":
        response = {"permission": "allow"}
    else:
        response = {}
    print(json.dumps(response))
    return 0

if __name__ == "__main__":
    try: sys.exit(main())
    except Exception as error:
        # Denials disclose policy guidance, never raw hook payloads or tool content.
        # A crash names its type and the harness frame that raised it, enough to find the fault.
        if isinstance(error, ValueError):
            reason = str(error)
        else:
            frames = [f for f in traceback.extract_tb(error.__traceback__) if f.filename.startswith(str(audit.ROOT))]
            reason = type(error).__name__ + (f" at {Path(frames[-1].filename).name}:{frames[-1].lineno}" if frames else "")
        print("TODO GATE: " + reason, file=sys.stderr)
        sys.exit(2)
