#!/usr/bin/env python3
"""Automatic Jev checkpoints at real decision points.

Each moment asks one bounded question and returns Jev's choice only when it
meets the confidence threshold; otherwise None and the agent decides. Moments
run only when HARNESS_JEV_CHECKPOINTS=1, typesafe.mode is active and
typesafe.checkpoints is true. A failure to ask is never an error for the caller.

Facts are curated task and step summaries (redacted by jev_context.summary),
counts and fixed labels. Prompts, commands, paths, source and tool output are
never sent.

  jev_moments.py search --project PATH [--session-id ID]
  jev_moments.py verify-failed --project PATH --ran N --failures N [--session-id ID]
  jev_moments.py grade-findings --project PATH --findings FILE --session-id ID
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from jev_context import summary  # noqa: E402

NEXT_STEPS = {
    "retry": "The failure looks transient (network, lock, timing); rerun the same command once.",
    "fix": "The failure points at a specific defect in the current change; fix it before rerunning.",
    "investigate": "The cause is unclear; read the failing code or logs before changing anything.",
    "ask_user": "The failure needs a credential, environment or decision only the user can supply.",
}
EFFORT = {
    "direct": "Handle it directly with normal reasoning; the request is routine and well scoped.",
    "think_deeper": "Plan first and reason carefully; the request is ambiguous, risky or spans several parts.",
    "delegate": "Split it and hand bounded parts to workers when dispatch is enabled.",
}
SEARCH = {
    "retrieval": "Run one semantic retrieval (jg) first; the question is behavioral or the code is unfamiliar.",
    "targeted_search": "Search directly; the symbol, string or file to look for is already known.",
}
SEVERITY = {
    "blocker": "Must be fixed before merge: wrong behavior, data loss, security or a broken contract.",
    "major": "Should be fixed before handoff: a real defect or gap with limited blast radius.",
    "minor": "A follow-up: style, naming, small clarity or an unlikely edge case.",
}
HANDOFF = {
    "hand_off": "Hand off now, stating any remaining gap.",
    "keep_working": "Keep working: an open step or failing check should be resolved first.",
    "ask_user": "A decision or input only the user can supply is needed.",
}


def enabled() -> bool:
    if os.environ.get("HARNESS_JEV_CHECKPOINTS") != "1":
        return False
    import harness_config
    settings = harness_config.load()["typesafe"]
    return settings.get("mode") == "active" and bool(settings.get("checkpoints"))


def ask(*, project: Path, session_id: str | None, family: str, goal: str, facts: list[str],
        choices: dict[str, str], scope: dict, origin: str) -> str | None:
    """Jev's confident choice, or None when disabled, unsure or unavailable."""
    try:
        if not isinstance(session_id, str) or not session_id or not enabled():
            return None
    except Exception:
        return None
    from decision_checkpoint import evaluate
    prior = {key: os.environ.get(key) for key in ("HARNESS_JEV_TIMEOUT", "HARNESS_JEV_ATTEMPTS")}
    os.environ["HARNESS_JEV_TIMEOUT"] = "5"
    os.environ["HARNESS_JEV_ATTEMPTS"] = "1"
    try:
        receipt = evaluate(project=Path(project).resolve(), session_id=session_id, family=family, goal=goal,
                           facts=facts, choices=choices, scope=scope, state={"scope": scope},
                           origin=origin, context_version=origin.replace("_", "-") + "-1")
    except Exception:
        return None
    finally:
        for key, value in prior.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
    action = None if receipt.get("needs_reasoning") else receipt.get("action")
    return action if action in choices else None


def load_context(session_id: str | None) -> dict:
    if not session_id:
        return {}
    import workflow_audit as audit
    with audit.context_db() as db:
        return audit.load(db, session_id)


def session_facts(context: dict) -> list[str]:
    task = context.get("task_summary") or {}
    todos = [item for item in context.get("todos", {}).values() if item.get("status") != "removed"]
    active = context.get("todos", {}).get(context.get("active_todo_id"), {})
    facts = ["Task purpose: " + summary(task.get("description") or context.get("plan_summary")),
             "Active step: " + summary(active.get("description")),
             "Plan steps: {} open, {} completed, {} blocked".format(
                 sum(item.get("status") in ("pending", "in_progress") for item in todos),
                 sum(item.get("status") == "completed" for item in todos),
                 sum(item.get("status") == "blocked" for item in todos))]
    for kind in ("verify", "review"):
        code = context.get("checks", {}).get(kind, {}).get("exit_code")
        facts.append(f"Last recorded {kind}: " + ("unknown" if type(code) is not int else "passed" if code == 0 else "failed"))
    return facts


def scope_of(context: dict, **extra) -> dict:
    return {"request_id": context.get("request_id"), "request_seq": context.get("request_seq"),
            "plan_revision": context.get("plan_revision"), **extra}


def prompt_effort(project: Path, session_id: str, prompt) -> str | None:
    """How much effort a new prompt needs; only its word count is shared, never its text."""
    context = load_context(session_id)
    words = len(prompt.split()) if isinstance(prompt, str) else 0
    facts = [*session_facts(context),
             f"New prompt size: {words} words (text not shared)",
             "Prompt number in this session: " + str(context.get("request_seq", "unknown"))]
    action = ask(project=project, session_id=session_id, family="reasoning_allocation",
                 goal="Choose how much effort the new request needs", facts=facts, choices=EFFORT,
                 scope=scope_of(context), origin="prompt_routing")
    return f"Jev effort for this prompt: {action}. {EFFORT[action]}" if action else None


def search_strategy(project: Path, session_id: str | None) -> str | None:
    context = load_context(session_id)
    facts = [*session_facts(context), "Semantic retrieval this run: none",
             "Read observations so far: " + str(context.get("evidence_revision", 0))]
    return ask(project=project, session_id=session_id, family="evidence_selection",
               goal="Choose how to start locating code for the current step", facts=facts, choices=SEARCH,
               scope=scope_of(context, moment="first_search"), origin="search_selection")


def plan_first_step(project: Path, session_id: str, context: dict, reason) -> str | None:
    pending = [item for item in context.get("todos", {}).values() if item.get("status") in ("pending", "in_progress")][:8]
    if len(pending) < 2:
        return None
    choices = {f"step_{index}": summary(item.get("description"), 160) for index, item in enumerate(pending, 1)}
    facts = ["Plan purpose: " + summary(reason or context.get("plan_summary")), f"Open steps: {len(pending)}",
             *session_facts(context)[3:]]
    action = ask(project=project, session_id=session_id, family="prioritization",
                 goal="Choose which plan step to start first", facts=facts, choices=choices,
                 scope=scope_of(context, moment="plan"), origin="plan_prioritization")
    if not action:
        return None
    item = pending[int(action.split("_")[1]) - 1]
    return f"Jev suggests starting with {item.get('id')}: {choices[action]}"


def verify_failed(project: Path, session_id: str | None, ran: int, failures: int) -> str | None:
    context = load_context(session_id)
    facts = [*session_facts(context)[:3], f"Verification ran {ran} checks; {failures} failed",
             "Failure kind: project verification (scripts/verify.sh)"]
    action = ask(project=project, session_id=session_id, family="progress_assessment",
                 goal="Choose the next step after failed verification", facts=facts, choices=NEXT_STEPS,
                 scope=scope_of(context, moment="verify", failures=failures), origin="verify_followup")
    return f"Jev next step: {action}. {NEXT_STEPS[action]}" if action else None


def grade_findings(project: Path, session_id: str, findings: dict) -> list[str]:
    """Second-opinion severity per open finding; lists only disagreements with the reviewer."""
    notes = []
    items = [item for item in findings.get("findings", []) if isinstance(item, dict) and item.get("status", "open") == "open"]
    for index, item in enumerate(items[:8], 1):
        evidence = item.get("evidence") if isinstance(item.get("evidence"), str) else ""
        facts = ["Reviewer severity: " + str(item.get("severity") if item.get("severity") in SEVERITY else "unknown"),
                 "Claim: " + summary(item.get("claim")),
                 "Evidence supplied: " + (f"yes, {len(evidence)} characters (text not shared)" if evidence.strip() else "no"),
                 "Exact line given: " + ("yes" if isinstance(item.get("line"), int) else "no")]
        action = ask(project=project, session_id=session_id, family="evidence_assessment",
                     goal="Grade the severity of one review finding", facts=facts, choices=SEVERITY,
                     scope={"moment": "review", "finding": index, "claim": summary(item.get("claim"), 80)},
                     origin="review_grading")
        if action and action != item.get("severity"):
            notes.append(f"Jev grades finding {index} as {action} (reviewer: {item.get('severity')}).")
    return notes


def record_handoff(project: Path, session_id: str) -> None:
    """Shadow only: Jev's handoff judgment is recorded and never shown or followed."""
    context = load_context(session_id)
    ask(project=project, session_id=session_id, family="handoff_assessment",
        goal="Decide whether the agent should hand off at the end of this turn", facts=session_facts(context),
        choices=HANDOFF, scope=scope_of(context, moment="stop"), origin="handoff_check")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("moment", choices=("search", "verify-failed", "grade-findings"))
    parser.add_argument("--project", type=Path, required=True)
    parser.add_argument("--session-id")
    parser.add_argument("--ran", type=int, default=0)
    parser.add_argument("--failures", type=int, default=0)
    parser.add_argument("--findings", type=Path)
    args = parser.parse_args(argv)
    try:
        from run_paths import session_id as native_session
        sid = args.session_id or native_session()
        if args.moment == "search":
            result = search_strategy(args.project, sid)
            lines = [result] if result else []
        elif args.moment == "verify-failed":
            result = verify_failed(args.project, sid, args.ran, args.failures)
            lines = [result] if result else []
        else:
            lines = grade_findings(args.project, sid, json.loads(args.findings.read_text())) if args.findings else []
    except Exception:
        return 0
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
