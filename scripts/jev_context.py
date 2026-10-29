"""Bounded routing context from curated Workflow facts, never tool bodies or prompts.

Text redaction is conservative and pattern-based; agents must keep personal data
and secrets out of curated task/todo summaries. Unknown facts stay unknown.
"""
from __future__ import annotations

import re
from pathlib import Path

VERSION = "tool-context-2"
ORIGINS = {"automatic_tool", "explicit_decision", "direct_advice", "worker_selection",
           "session_routing", "task_routing", "failure_followup"}


def summary(value, limit=220):
    if not isinstance(value, str) or not value.strip():
        return "unknown"
    # Do not accept code blocks or multiline bodies as curated prose.
    if len(value) > 2000 or any(part in value for part in ("\n", "\r", "```", "{", "}", "-----BEGIN")):
        return "withheld: not bounded prose"
    from workflow_audit import redact
    text = redact(value)
    for pattern in (
        r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b",
        r"https?://\S+", r"(?<!\w)(?:~?/|[A-Za-z]:[\\/])\S+",
        r"\b(?:\d{1,3}\.){3}\d{1,3}\b", r"(?<!\w)\+?\d[\d ()-]{7,}\d\b",
        r"\b[A-Z][a-z]+(?:\s+[A-Z][a-z]+)+\b",
        r"\b[\w.-]+(?:[/\\][\w.-]+)+\b",
    ):
        text = re.sub(pattern, "[redacted]", text)
    return " ".join(text.split())[:limit] or "unknown"


def command_intent(argv):
    """Observational labels only: these never authorize an action."""
    args = list(argv)
    while args and ("=" in args[0] or args[0] in {"env", "command"}):
        args.pop(0)
    if not args:
        return "unknown"
    program = Path(args[0]).name
    sub = args[1] if len(args) > 1 else ""
    if program == "git":
        return {"status":"inspect_repository", "diff":"inspect_changes", "log":"inspect_history",
                "show":"inspect_history", "add":"stage_changes", "commit":"record_commit",
                "push":"publish_changes", "fetch":"fetch_changes"}.get(sub, "repository_operation")
    if program in {"rg", "grep", "cat", "sed", "head", "tail", "ls", "find"}:
        return "inspect_local_data" if "-i" not in args else "edit_files"
    if program in {"pytest", "shellcheck", "verify.sh", "review.sh"} or (
            program in {"python", "python3"} and "unittest" in args[:4]):
        return "run_checks"
    if program in {"sh", "bash"} and len(args) > 1 and args[1].startswith("tests/"):
        return "run_checks"
    if program in {"cargo", "npm", "pnpm", "yarn", "make"}:
        return {"test":"run_checks", "check":"run_checks", "lint":"run_checks", "fmt":"format_code",
                "build":"build_artifacts", "install":"install_dependencies", "publish":"publish_artifacts"}.get(sub, "build_tool_operation")
    if program == "docker":
        return "run_container_checks" if "tests" in args and "run" in args else "container_operation"
    if program in {"curl", "wget"}:
        return "http_request"
    if program in {"rm", "rmdir"}:
        return "delete_files"
    if program in {"cp", "mv", "touch", "mkdir", "tee"}:
        return "write_files"
    if program in {"systemctl", "launchctl"}:
        operation = next((value for value in args[1:] if value in {"restart", "start", "stop", "status", "show", "print", "kickstart"}), "unknown")
        if operation in {"restart", "start", "stop"} and "harness-audit.service" in args:
            return operation + "_project_audit_dashboard"
        return {"restart":"restart_service", "start":"start_service", "stop":"stop_service",
                "status":"inspect_service_status", "show":"inspect_service_status", "print":"inspect_service_status",
                "kickstart":"restart_service"}.get(operation, "manage_service")
    if program == "gh" and sub == "run":
        return "inspect_ci_status" if any(value in {"list", "view", "watch"} for value in args[2:4]) else "ci_operation"
    return "run_program"


def action_facts(payload):
    tool = payload.get("tool_name", "")
    data = payload.get("tool_input")
    data = data if isinstance(data, dict) else {}
    if tool in {"Bash", "exec_command", "shell"}:
        command = data.get("cmd", data.get("command"))
        intents = []
        try:
            import permit
            if isinstance(command, list) and all(isinstance(v, str) for v in command):
                intents = [command_intent(command)]
            elif isinstance(command, str) and len(command) <= 32000:
                text, bodies = permit.preprocess(command)
                segments = permit.split_segments(permit.tokenize(text))
                intents = [command_intent(permit.read_segment(s, list(bodies)).argv) for s in segments[:4]]
                if len(segments) > 4:
                    intents.append("additional_actions_not_summarized")
        except (ValueError, permit.Unparseable):
            intents = ["unknown_shell_action"]
        result = ["Action channel: shell", "Action intents: " + ", ".join(dict.fromkeys(intents or ["unknown"]))]
    else:
        kind = "edit_files" if tool in {"Write", "Edit", "MultiEdit", "apply_patch"} or tool.endswith("apply_patch") else "specialized_tool"
        result = ["Action channel: " + kind]
        patch = data.get("input", data.get("patch", ""))
        if isinstance(payload.get("tool_input"), str):
            patch = payload["tool_input"]
        names = re.findall(r"^\*\*\* (Add|Update|Delete) File: (.+)$", patch, re.M) if isinstance(patch, str) else []
        if not names:
            path = data.get("file_path", data.get("path"))
            if isinstance(path, str):
                names = [("Update", path)]
        if names:
            types = {".py":"Python logic", ".rs":"Rust logic", ".html":"web presentation", ".md":"documentation", ".sh":"shell automation", ".json":"structured configuration"}
            result.append("Proposed artifact changes: " + ", ".join(dict.fromkeys(
                operation.lower() + " " + types.get(Path(name).suffix, "project artifact") for operation, name in names[:8])))
            result.append("Proposed artifact count: " + str(len(names)))
    if isinstance(data.get("description"), str):
        result.append("Agent-supplied action purpose (unverified): " + summary(data["description"]))
    return result


def action_type(payload):
    labels = " ".join(action_facts(payload))
    # Compound actions retain their most consequential type.
    for kind, signals in (
        ("publication", ("publish_", "restart_", "start_service", "stop_service")),
        ("implementation", ("edit_files", "write_files", "delete_files", "format_code", "stage_changes", "record_commit", "install_dependencies")),
        ("verification", ("run_checks", "run_container_checks", "inspect_ci_status")),
        ("inspection", ("inspect_",))):
        if any(signal in labels for signal in signals):
            return kind
    return "unknown"


def build(payload, context):
    active = context.get("todos", {}).get(context.get("active_todo_id"), {})
    task = context.get("task_summary", {})
    facts = ["Task purpose: " + summary(task.get("description") or context.get("plan_summary")),
             "Current plan purpose: " + summary(context.get("plan_summary")),
             "Active step: " + summary(active.get("description")),
             "Acceptance criterion: " + summary(active.get("criterion")),
             "Active step status: " + (active.get("status") if active.get("status") in {"pending", "in_progress", "completed", "blocked"} else "unknown")]
    facts.extend(action_facts(payload))
    facts.append("Action type: " + action_type(payload))
    supplied = context.get("jev_action_context", {})
    scoped = (supplied.get("request_id") == context.get("request_id")
              and supplied.get("todo_id") == context.get("active_todo_id")
              and supplied.get("plan_revision") == context.get("plan_revision"))
    if supplied and scoped:
        facts.append("Agent-supplied proposed behavior (unverified): " + summary(supplied.get("action_summary")))
        for finding in supplied.get("findings", [])[:4]:
            facts.append("Agent-reported evidence finding (not independently established): " + summary(finding, 180))
        for gap in supplied.get("gaps", [])[:4]:
            facts.append("Relevant unresolved prerequisite (agent-reported): " + summary(gap, 180))
        if not supplied.get("gaps"):
            facts.append("Relevant prerequisites: no gaps reported; this is not proof that none exist")
    else:
        facts.append("Proposed behavior, evidence findings and relevant prerequisites: unknown; no current action context supplied")
    items = list(context.get("todos", {}).values())
    completed = sorted([item for item in items if item.get("status") == "completed"], key=lambda item: item.get("updated_at", ""))
    blocked = [item for item in items if item.get("status") == "blocked"]
    facts.append(f"Plan evidence: {len(completed)} completed steps; {len(blocked)} blocked steps")
    for item in completed[-2:]:
        facts.append("Agent-recorded step evidence (not independently established): " + summary(item.get("evidence"), 160))
    for item in blocked[:2]:
        facts.append("Unresolved blocked step: " + summary(item.get("description"), 160))
    reads = context.get("evidence_revision")
    facts.append("Read observations: " + (str(min(reads, 10000)) if type(reads) is int and reads >= 0 else "unknown") + "; count alone does not establish sufficient evidence")
    recent = [item for item in context.get("routing_observations", [])
              if item.get("task_id") == context.get("task_id") and item.get("request_id") == context.get("request_id")]
    if recent:
        facts.append("Recent read results (tool status, not correctness): " + ", ".join(
            item["kind"] + ":" + item["status"] for item in recent[-6:]))
    for kind in ("verify", "review"):
        check = context.get("checks", {}).get(kind, {})
        status = "unknown" if type(check.get("exit_code")) is not int else "passed" if check["exit_code"] == 0 else "failed"
        facts.append(f"Last recorded {kind}: {status}; freshness for current files is not established by this summary")
    facts.append("This choice concerns the next useful step, not permission or final completion. Do not demand proof of an action's outcome before performing it; later verification remains mandatory.")
    return {"goal":"Assess whether the proposed next action has enough relevant supporting context. Judge inspection, implementation, verification and publication by their own prerequisites; do not infer support from read counts or task alignment alone.",
            "facts":facts, "origin":"automatic_tool", "context_version":VERSION,
            "choices":{"execute":"The described action has sufficient relevant support for its immediate prerequisites. Checks that can only follow execution remain later obligations.",
                       "gather_context":"A specific relevant prerequisite is unresolved and a targeted read can resolve it before this action.",
                       "insufficient_information":"The action or its supporting findings are too underspecified to assess. The agent should supply a concrete behavior summary and relevant findings or gaps; do not invent a prerequisite or default to approval."}}



def observe_read(payload, context):
    """Keep only read kind/status in local policy state; never retain arguments/output."""
    tool = payload.get("tool_name", "")
    kind = None
    if tool in {"Read", "read_file"}:
        kind = "file_read"
    elif tool in {"Grep", "Glob", "list_dir"}:
        kind = "source_search"
    elif any(tool.endswith(name) for name in ("search_graph", "get_code_snippet", "trace_path", "get_architecture")):
        kind = "graph_lookup"
    elif tool in {"Bash", "exec_command", "shell"}:
        intents = " ".join(action_facts(payload))
        if "inspect_" in intents:
            kind = "local_inspection"
    if kind is None:
        return False
    response = payload.get("tool_response")
    response = response if isinstance(response, dict) else {}
    code = response.get("exit_code")
    status = "failed" if response.get("is_error") is True or payload.get("hook_event_name") == "PostToolUseFailure" else (
        "success" if type(code) is int and code == 0 else "failed" if type(code) is int else "returned")
    observations = context.setdefault("routing_observations", [])
    observations.append({"kind":kind, "status":status, "task_id":context.get("task_id"), "request_id":context.get("request_id")})
    context["routing_observations"] = observations[-12:]
    return True
