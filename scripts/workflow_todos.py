"""Deterministic todo-plan policy, independent of audit collection switches."""
from datetime import datetime, timezone
import json
import uuid

TERMINAL = {"completed", "removed"}
STATUSES = {"pending", "in_progress", "completed", "blocked"}

def timestamp():
    return datetime.now(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")

def text(value, name):
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > 2000:
        raise ValueError(name + " must be nonempty and at most 2000 bytes")
    return value

def reset_task(context):
    for key in ("todos", "plan_revision", "confirmed_request", "active_todo_id", "plan_mode", "plan_scope_at", "checks", "task_status", "last_execution_at"):
        context.pop(key, None)

def unresolved(context):
    return [item for item in context.get("todos", {}).values() if item["required"] and item["status"] not in TERMINAL]

def assert_plan(context, active=False):
    if context.get("confirmed_request") != context.get("request_seq", 0) or context.get("plan_mode") != "execution" or not context.get("todos"):
        raise ValueError("Register or confirm a complete todo plan for the current prompt: harness workflow todo plan --items JSON --reason SUMMARY")
    if active and context.get("todos", {}).get(context.get("active_todo_id"), {}).get("status") != "in_progress":
        raise ValueError("Select an active todo: harness workflow todo update --id ID --status in_progress --reason SUMMARY")

def assert_complete(context):
    if context.get("plan_mode") == "question" and context.get("confirmed_request") == context.get("request_seq", 0):
        return
    assert_plan(context)
    if unresolved(context):
        raise ValueError("Required todos remain unresolved")
    for kind in ("verify", "review"):
        check = context.get("checks", {}).get(kind, {})
        threshold = max(context.get("plan_scope_at", ""), context.get("last_execution_at", ""))
        fresh = bool(check.get("at")) and datetime.fromisoformat(check["at"].replace("Z", "+00:00")) >= datetime.fromisoformat(threshold.replace("Z", "+00:00"))
        if check.get("exit_code") != 0 or check.get("failures", 0) != 0 or not fresh:
            raise ValueError("Fresh passing " + kind + " is required before completion")

def handle(args, sid, context, record):
    action = args.todo_action
    if action == "show":
        return
    reason = text(args.reason, "reason")
    todos = context.setdefault("todos", {})
    if action == "exempt":
        if unresolved(context):
            raise ValueError("An unresolved execution plan cannot be exempted; record a blocked outcome or revise the plan")
        context["plan_mode"] = "question"
        context["confirmed_request"] = context.get("request_seq", 0)
        context.pop("active_todo_id", None)
        record("plan_exempted", sid, context, description=reason, status="question")
        return
    if action == "confirm":
        if not todos or context.get("plan_mode") != "execution":
            raise ValueError("An execution plan must exist before confirmation")
        context["confirmed_request"] = context.get("request_seq", 0)
        record("plan_confirmed", sid, context, description=reason, revision=context.get("plan_revision", 1))
        return
    revision = context.get("plan_revision", 0) + 1
    if action == "plan":
        items = json.loads(args.items)
        if not isinstance(items, list) or not 1 <= len(items) <= 200:
            raise ValueError("Plan must contain 1 to 200 todos")
        next_items = {}
        for item in items:
            if not isinstance(item, dict) or set(item) - {"id", "description", "criterion", "required"}:
                raise ValueError("Todos accept id, description, criterion and required only")
            identifier = item.get("id", str(uuid.uuid4()))
            if not isinstance(identifier, str) or not identifier or len(identifier) > 128 or identifier in next_items:
                raise ValueError("Todo IDs must be short, nonempty and unique")
            if type(item.get("required", True)) is not bool:
                raise ValueError("required must be a boolean")
            previous = todos.get(identifier)
            current = {"id":identifier,"description":text(item.get("description"),"description"),"criterion":text(item.get("criterion"),"criterion"),"required":item.get("required",True),"status":"pending","evidence":""}
            if previous and all(previous[key] == current[key] for key in ("description", "criterion", "required")) and previous["status"] != "removed":
                current = previous.copy()
            next_items[identifier] = current
        for identifier, previous in todos.items():
            if identifier not in next_items and previous["status"] != "removed":
                removed = {**previous,"status":"removed"}
                record("todo_removed", sid, context, todo_id=identifier, description=previous["description"], criterion=previous["criterion"], status="removed", previous_status=previous["status"], reason=reason, revision=revision)
                next_items[identifier] = removed
            elif identifier not in next_items:
                next_items[identifier] = previous
        for identifier, current in next_items.items():
            previous = todos.get(identifier)
            if current["status"] != "removed" and current != previous:
                record("todo_updated" if previous else "todo_created", sid, context, todo_id=identifier, description=current["description"], criterion=current["criterion"], todo_required=int(current["required"]), status=current["status"], previous_status=previous["status"] if previous else "", previous_description=previous["description"] if previous else "", previous_criterion=previous["criterion"] if previous else "", reason=reason, revision=revision)
        context["todos"] = next_items
        context["plan_mode"] = "execution"
        context["confirmed_request"] = context.get("request_seq", 0)
        context["plan_scope_at"] = timestamp()
        if next_items.get(context.get("active_todo_id"), {}).get("status") != "in_progress": context.pop("active_todo_id", None)
        record("plan_registered" if not todos else "plan_revised", sid, context, description=reason, revision=revision, todo_count=sum(item["status"] != "removed" for item in next_items.values()))
    elif action == "update":
        assert_plan(context)
        current = todos.get(args.id)
        if not current or current["status"] == "removed": raise ValueError("Unknown or removed todo")
        if args.status == "in_progress" and context.get("active_todo_id") not in (None, args.id):
            raise ValueError("Resolve or pause the active todo before selecting another")
        evidence = text(args.evidence, "evidence") if args.status == "completed" else ""
        previous_status = current["status"]
        current.update(status=args.status, evidence=evidence)
        if args.status == "in_progress": context["active_todo_id"] = args.id
        elif context.get("active_todo_id") == args.id: context.pop("active_todo_id", None)
        record("todo_updated", sid, context, todo_id=args.id, description=current["description"], criterion=current["criterion"], status=args.status, evidence=evidence, previous_status=previous_status, reason=reason, revision=revision)
    context["plan_revision"] = revision
    context["task_status"] = "running"
