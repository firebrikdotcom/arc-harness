#!/usr/bin/env python3
"""Emit and resolve automatic Jev shadow checkpoints at harness seams.

The phase gates, verify, review, and the tool hooks call this script so that
every harness run produces shadow checkpoints and independently labeled
outcomes without the agent authoring JSON by hand. Each checkpoint records the
action the harness was going to take anyway, asks Jev a bounded question over
enum-only signals derived from git and run state, and is later labeled from a
deterministic oracle: the verify exit code, the first verify after build start,
or whether the run looped back to an earlier phase.

Nothing here executes, authorizes, waives, or reorders any harness gate. When
HARNESS_JEV_CHECKPOINTS is not "1", every command is a no-op.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

import context_advice as advice  # noqa: E402
import task_route  # noqa: E402

ENABLE_ENV = "HARNESS_JEV_CHECKPOINTS"
POLICY_VERSION = "shadow-1"
DEFAULT_TIMEOUT_SECONDS = "10"
LANGUAGE_BY_EXTENSION = {
    "py": "python", "sh": "shell", "bash": "shell", "zsh": "shell",
    "ts": "javascript", "tsx": "javascript", "js": "javascript", "jsx": "javascript", "vue": "javascript",
    "svelte": "javascript", "css": "stylesheet", "scss": "stylesheet",
    "php": "php", "go": "go", "rs": "rust", "rb": "ruby", "java": "java", "cs": "csharp",
    "kt": "kotlin", "swift": "swift", "dart": "dart",
    "md": "markdown", "json": "config", "yaml": "config", "yml": "config", "toml": "config",
    "tf": "infrastructure", "sql": "sql", "html": "html",
}
AREA_BY_LANGUAGE = {
    "javascript": "frontend", "stylesheet": "frontend", "html": "frontend",
    "python": "backend", "php": "backend", "go": "backend", "rust": "backend", "ruby": "backend",
    "java": "backend", "csharp": "backend", "sql": "backend",
    "kotlin": "mobile", "swift": "mobile", "dart": "mobile",
    "infrastructure": "infrastructure", "shell": "infrastructure",
    "markdown": "docs",
}
GO_ACTIONS = {"proceed_to_build", "ready_for_handoff"}
HOLD_ACTIONS = {"refine_plan", "needs_more_work"}


class Skip(Exception):
    """The checkpoint does not apply here; say why and exit 0."""


def enabled() -> bool:
    return os.environ.get(ENABLE_ENV) == "1"


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def bucket(count: int) -> str:
    if count <= 0:
        return "none"
    if count <= 3:
        return "small"
    if count <= 15:
        return "medium"
    return "large"


def git(project: Path, *arguments: str) -> list[str]:
    try:
        result = subprocess.run(["git", "-C", str(project), *arguments], capture_output=True,
                                text=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return []
    if result.returncode:
        return []
    return [line for line in result.stdout.splitlines() if line.strip()]


def language_counts(paths: list[str]) -> Counter:
    counts: Counter = Counter()
    for path in paths:
        name = path.rsplit("/", 1)[-1]
        extension = name.rsplit(".", 1)[-1].lower() if "." in name else ""
        language = LANGUAGE_BY_EXTENSION.get(extension)
        if language:
            counts[language] += 1
    return counts


def git_signals(project: Path) -> dict[str, Any]:
    """Enum-only facts about the working tree; no path or content leaves this function."""
    status = git(project, "status", "--porcelain", "--untracked-files=all")
    dirty = [line[3:] for line in status if len(line) > 3 and not line[3:].startswith((".harness-db/", "harness-db/"))]
    tracked = git(project, "ls-files") if project.is_dir() else []
    languages = language_counts(dirty) or language_counts(tracked[:2000])
    lowered = [path.lower() for path in dirty]
    return {
        "is_git": bool(git(project, "rev-parse", "--is-inside-work-tree")),
        "dirty_count": len(dirty),
        "dirty_bucket": bucket(len(dirty)),
        "languages": [name for name, _ in languages.most_common(3)],
        "language_counts": dict(languages),
        "tests_changed": any("test" in path or "spec" in path for path in lowered),
        "docs_changed": any(path.endswith(".md") or path.startswith("docs/") for path in lowered),
        "progress_changed": any(path.endswith("progress.md") for path in lowered),
        "config_changed": any(path.endswith((".json", ".yaml", ".yml", ".toml")) for path in lowered),
        "has_tests_dir": any((project / name).is_dir() for name in ("tests", "test", "spec", "__tests__")),
    }


def area_for(languages: list[str] | dict[str, int], project: Path) -> str:
    """Majority area by file count; ties and unknown languages stay 'other'."""
    if any((project / name).is_dir() for name in ("android", "ios")):
        return "mobile"
    weights = languages if isinstance(languages, dict) else {name: 1 for name in languages}
    votes: Counter = Counter()
    for language, count in weights.items():
        votes[AREA_BY_LANGUAGE.get(language, "other")] += count
    if not votes:
        return "other"
    area, count = votes.most_common(1)[0]
    if list(votes.values()).count(count) > 1:
        return "other"
    return area


def read_kv(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.is_file():
        return values
    for line in path.read_text(encoding="utf-8").splitlines():
        key, separator, value = line.partition("=")
        if separator:
            values[key] = value
    return values


def run_state(db_root: Path) -> dict[str, str] | None:
    current = db_root / "runs" / "current"
    if not current.is_file():
        return None
    run_id = current.read_text(encoding="utf-8").splitlines()[0].strip() if current.read_text(encoding="utf-8").strip() else ""
    state = read_kv(db_root / "runs" / run_id / "state") if run_id else {}
    return state or None


def require_active_run(db_root: Path) -> dict[str, str]:
    state = run_state(db_root)
    if not state or state.get("RUN_STATUS") != "active":
        raise Skip("no active harness run")
    return state


def int_value(state: dict[str, str], key: str) -> int:
    value = state.get(key, "0")
    return int(value) if value.isdigit() else 0


def last_verify(db_root: Path) -> dict[str, str]:
    return read_kv(db_root / "records" / "verify.state")


def verify_facts(db_root: Path, state: dict[str, str]) -> list[str]:
    record = last_verify(db_root)
    if not record.get("RECORD_EPOCH", "").isdigit():
        return ["No verification record exists yet in this run."]
    since = int_value(state, "PHASE_BUILD_STARTED_EPOCH")
    if int(record["RECORD_EPOCH"]) < since:
        return ["No verification has run since the build phase started."]
    outcome = "passed" if record.get("EXIT") == "0" else "failed"
    return [f"The most recent verification in this run {outcome} (checks run: {record.get('RAN', 'unknown')}, failures: {record.get('FAILURES', 'unknown')})."]


def base_facts(signals: dict[str, Any], state: dict[str, str]) -> list[str]:
    languages = ", ".join(signals["languages"]) or "none detected"
    return [
        f"Dirty file count bucket: {signals['dirty_bucket']}.",
        f"Languages touched: {languages}.",
        f"Test files changed: {'yes' if signals['tests_changed'] else 'no'}.",
        f"Documentation changed: {'yes' if signals['docs_changed'] else 'no'}.",
        f"progress.md changed: {'yes' if signals['progress_changed'] else 'no'}.",
        f"Project has a test directory: {'yes' if signals['has_tests_dir'] else 'no'}.",
        f"Harness steps used so far: {bucket(int_value(state, 'STEPS_USED'))}.",
        f"Phase loops used so far: {int_value(state, 'LOOPS_USED')}.",
    ]


CONSTRAINTS = [
    "Required checks, permissions, failures, and completion gates are deterministic and cannot be waived.",
    "Answer only from the supplied facts; missing evidence is unknown, not false.",
]


def checkpoint(family: str, question_version: str, baseline: str, goal: str, facts: list[str],
               questions: dict[str, Any], risks: list[str] | None = None) -> dict[str, Any]:
    context: dict[str, Any] = {"goal": goal, "facts": facts, "constraints": CONSTRAINTS}
    if risks:
        context["risks"] = risks
    return {
        "version": 2,
        "checkpoint": {"family": family, "question_version": question_version, "policy_version": POLICY_VERSION,
                       "baseline_action": baseline, "bypass_reason": "none", "state_build_ms": None},
        "context": context,
        "questions": questions,
    }


def build_plan_done(signals: dict[str, Any], state: dict[str, str], db_root: Path) -> tuple[dict, str]:
    return checkpoint(
        "handoff_assessment", "phase-plan-1", "proceed_to_build",
        "Assess whether the recorded plan is ready to hand to the build phase.",
        base_facts(signals, state),
        {
            "recommendation": {"type": "choice", "instructions": "Should the run proceed to build now?",
                               "criteria": {"proceed_to_build": "Hand the recorded plan to the build phase now.",
                                            "refine_plan": "Refine or record the plan before building."}},
            "plan_recorded": {"type": "boolean", "instructions": "The plan for this run has been recorded in progress.md."},
            "clarification_needed": {"type": "boolean", "instructions": "A user-owned decision is still missing before building can start."},
        },
    ), "run_complete"


def build_build_start(signals: dict[str, Any], state: dict[str, str], db_root: Path) -> tuple[dict, str]:
    return checkpoint(
        "reasoning_allocation", "phase-build-1", "routine",
        "Allocate reasoning effort for the build phase that is starting.",
        base_facts(signals, state) + verify_facts(db_root, state),
        {
            "recommendation": {"type": "choice", "instructions": "How much reasoning does this build phase need before its first verification passes?",
                               "criteria": {"routine": "Routine implementation; the first verification is expected to pass.",
                                            "targeted_check": "Run one targeted check before implementing further.",
                                            "deep_reasoning": "Deeper investigation is needed; the first verification is likely to fail."}},
            "rework_risk": {"type": "score", "instructions": "How much rework would a wrong allocation cause?",
                            "criteria": ["Minor: one short fix", "Moderate: one failed verify cycle", "Major: the change needs redesign"]},
        },
        risks=["Underestimating effort produces a failed verification cycle."],
    ), "first_verify"


def build_verify_start(signals: dict[str, Any], state: dict[str, str], db_root: Path) -> tuple[dict, str]:
    return checkpoint(
        "evidence_assessment", "verify-predict-1", "run_full_verification",
        "Predict whether the pending changes will pass the project's verification checks.",
        base_facts(signals, state) + verify_facts(db_root, state),
        {
            "recommendation": {"type": "choice", "instructions": "What should happen next?",
                               "criteria": {"run_full_verification": "Run full verification now; it is expected to pass.",
                                            "fix_before_verifying": "Failures are likely; more fixes are needed first."}},
            "will_pass": {"type": "boolean", "instructions": "The pending changes will pass the project's verification checks."},
        },
    ), "verify_result"


def build_review_handoff(signals: dict[str, Any], state: dict[str, str], db_root: Path, verify_exit: int) -> tuple[dict, str]:
    baseline = "ready_for_handoff" if verify_exit == 0 else "needs_more_work"
    facts = base_facts(signals, state) + [f"Verification during review {'passed' if verify_exit == 0 else 'failed'}."]
    return checkpoint(
        "handoff_assessment", "review-handoff-1", baseline,
        "Assess whether the reviewed change is ready to hand off without another build loop.",
        facts,
        {
            "recommendation": {"type": "choice", "instructions": "Is the change ready to hand off?",
                               "criteria": {"ready_for_handoff": "Ready; no further build loop is expected.",
                                            "needs_more_work": "Another build loop is expected before handoff."}},
            "docs_and_progress_current": {"type": "boolean", "instructions": "Documentation and progress.md reflect the change."},
        },
    ), "run_complete"


def build_tool_repeat(signals: dict[str, Any], state: dict[str, str], repeats: int) -> tuple[dict, str]:
    facts = base_facts(signals, state) + [f"The same shell command has now been issued {repeats} times in this run.",
                                          f"Current phase: {state.get('CURRENT_PHASE', 'unknown')}."]
    return checkpoint(
        "progress_assessment", "tool-repeat-1", "retry_same_command",
        "Assess whether repeating the same command is still making progress.",
        facts,
        {
            "recommendation": {"type": "choice", "instructions": "What is the most productive next step?",
                               "criteria": {"retry_same_command": "Run the same command again.",
                                            "change_approach": "Try a different investigation or fix.",
                                            "gather_more_evidence": "Read more evidence before acting."}},
            "stuck": {"type": "boolean", "instructions": "The agent is repeating an approach that is not producing progress."},
        },
    ), "none"


def write_pending(db_root: Path, result: dict[str, Any], context: dict[str, Any], resolver: str, state: dict[str, str]) -> None:
    if resolver == "none" or result.get("status") != "evaluated":
        return
    directory = db_root / "advice-pending"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(directory, 0o700)
    answers = result.get("answers") or {}
    compact = {}
    for key, answer in answers.items():
        if answer.get("type") == "boolean":
            compact[key] = answer.get("probability")
        else:
            compact[key] = answer.get(answer.get("type"))
    payload = {
        "call_id": result["call_id"], "family": context["checkpoint"]["family"],
        "question_version": context["checkpoint"]["question_version"], "resolver": resolver,
        "run_id": state.get("RUN_ID"), "created_at": now(),
        "baseline_action": context["checkpoint"]["baseline_action"],
        "recommendation": compact.get("recommendation"), "answers": compact,
        "loops_at": int_value(state, "LOOPS_USED"),
    }
    target = directory / f"{result['call_id']}.json"
    fd, temporary = tempfile.mkstemp(prefix=".pending-", dir=directory)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        os.fchmod(fd, 0o600)
        json.dump(payload, stream, sort_keys=True)
        stream.write("\n")
    os.replace(temporary, target)


def pending_items(db_root: Path, resolver: str) -> list[tuple[Path, dict[str, Any]]]:
    directory = db_root / "advice-pending"
    items = []
    for path in sorted(directory.glob("*.json")) if directory.is_dir() else []:
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError):
            continue
        if data.get("resolver") == resolver:
            items.append((path, data))
    return items


def label_verify(item: dict[str, Any], exit_code: int) -> tuple[str, str]:
    passed = exit_code == 0
    probability = item.get("answers", {}).get("will_pass")
    if isinstance(probability, (int, float)):
        predicted = probability >= 0.5
    else:
        predicted = item.get("recommendation") == "run_full_verification"
    if predicted == passed:
        label = "correct"
    elif predicted and not passed:
        label = "under_escalated"
    else:
        label = "over_escalated"
    return label, f"Verification exited {exit_code}; Jev predicted pass with probability {probability if probability is not None else 'unknown'}."


def label_allocation(item: dict[str, Any], exit_code: int) -> tuple[str, str]:
    recommendation = item.get("recommendation")
    if exit_code == 0:
        label = "correct" if recommendation == "routine" else "over_escalated"
    else:
        label = "correct" if recommendation == "deep_reasoning" else "under_escalated"
    return label, f"The first verification after build start exited {exit_code}; Jev recommended {recommendation}."


def label_handoff(item: dict[str, Any], loops_now: int) -> tuple[str, str]:
    looped = loops_now > int(item.get("loops_at") or 0)
    recommendation = item.get("recommendation")
    if recommendation in GO_ACTIONS:
        label = "under_escalated" if looped else "correct"
    elif recommendation in HOLD_ACTIONS:
        label = "correct" if looped else "over_escalated"
    else:
        label = "unknown"
    return label, f"The run {'re-entered an earlier phase' if looped else 'completed without re-entering an earlier phase'} after this handoff; Jev recommended {recommendation}."


def resolve(db_root: Path, router: Any, resolver: str, labeler, *arguments: Any) -> list[str]:
    lines = []
    for path, item in pending_items(db_root, resolver):
        label, evidence = labeler(item, *arguments)
        outcome = {"call_id": item["call_id"], "action_taken": item.get("baseline_action") or "baseline",
                   "outcome": label, "evidence": evidence}
        try:
            advice.label_outcome(outcome, router, db_root)
            lines.append(f"labeled {item['family']}/{item['question_version']} {label} ({item['call_id'][:8]})")
        except advice.InputError as error:
            lines.append(f"could not label {item['call_id'][:8]}: {error}")
        path.unlink(missing_ok=True)
    return lines


def emit(context: dict[str, Any], resolver: str, router: Any, db_root: Path, state: dict[str, str]) -> str:
    result = advice.advise(advice.validate_context(context, router), router, db_root)
    write_pending(db_root, result, context, resolver, state)
    family = context["checkpoint"]["family"]
    version = context["checkpoint"]["question_version"]
    if result["status"] != "evaluated":
        return f"{family}/{version} {result['status']} ({result.get('fallback_reason') or 'no evaluation'}); baseline kept."
    recommendation = (result.get("answers") or {}).get("recommendation", {}).get("choice")
    agreement = "agrees with" if recommendation == context["checkpoint"]["baseline_action"] else "differs from"
    return f"{family}/{version} shadow recommendation {recommendation} {agreement} baseline {context['checkpoint']['baseline_action']} ({result['call_id'][:8]}); baseline kept."


def pilot_lines(db_root: Path) -> list[str]:
    summary = advice.pilot(db_root)
    lines = [f"pilot {summary['labeled']}/{summary['target']} labeled shadow decisions"
             f" ({summary['correct']} correct, {summary['unlabeled_evaluated']} unlabeled, {summary['fallback']} fallback)."]
    unlabeled = advice.pending(db_root, limit=10)["unlabeled"]
    if unlabeled:
        ids = ", ".join(f"{row['family']}:{row['call_id'][:8]}" for row in unlabeled if row.get("call_id"))
        lines.append(f"unlabeled checkpoints (label with harness advise --label CALL_ID ...): {ids}")
    return lines


def session_metadata(project: Path, db_root: Path, signals: dict[str, Any]) -> dict[str, Any]:
    state = run_state(db_root)
    record = last_verify(db_root)
    failing = bool(state and state.get("RUN_STATUS") == "active" and record and record.get("EXIT") not in {"", "0"})
    return {
        "version": 1, "task_kind": "change", "area": area_for(signals["language_counts"], project),
        "proposed_action": "start_routine_agent", "reversibility": "reversible",
        "uncertainty_reason": "scope_unclear", "diff_size": signals["dirty_bucket"],
        "changed_file_count": min(signals["dirty_count"], 10000), "known_failures": 1 if failing else 0,
        "required_checks_pending": False, "approval_required": False, "user_choice_explicit": False,
    }


def session_start(project: Path, db_root: Path, router: Path) -> str:
    signals = git_signals(project)
    if not signals["is_git"]:
        raise Skip("not a git worktree")
    metadata = session_metadata(project, db_root, signals)
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
        json.dump(metadata, handle)
        path = Path(handle.name)
    try:
        route = task_route.route_task(path, project, db_root, router, "shadow")
    finally:
        path.unlink(missing_ok=True)
    observed = route.get("observed_recommendation") or route.get("recommendation")
    summary = advice.pilot(db_root)
    return (f"Jev shadow route for this session: {observed} (source {route['source']}, shadow; existing rules decide). "
            f"Pilot: {summary['labeled']}/{summary['target']} labeled shadow decisions.")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("event", choices=("plan-done", "build-start", "verify-start", "verify-result", "review-handoff",
                                          "run-complete", "tool-repeat", "session-start", "pilot"))
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument("--db-root", type=Path, default=Path(os.environ.get("HARNESS_DB_ROOT", str(ROOT / ".harness-db"))))
    parser.add_argument("--router", type=Path, default=Path(os.environ.get("HARNESS_TYPESAFE_ROUTER", str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py"))))
    parser.add_argument("--exit", type=int, dest="exit_code", help="verification exit code for verify-result")
    parser.add_argument("--verify-exit", type=int, help="verification exit code observed by review")
    parser.add_argument("--repeats", type=int, help="how many times the same command has been issued")
    parser.add_argument("--force", action="store_true", help=f"run even when {ENABLE_ENV} is not 1")
    args = parser.parse_args(argv)
    if not (enabled() or args.force):
        return 0
    os.environ.setdefault("HARNESS_JEV_TIMEOUT", DEFAULT_TIMEOUT_SECONDS)
    os.environ.setdefault("HARNESS_JEV_ATTEMPTS", "1")
    lines: list[str] = []
    try:
        if args.event == "pilot":
            lines = pilot_lines(args.db_root)
        elif args.event == "session-start":
            lines = [session_start(args.project, args.db_root, args.router)]
        else:
            state = require_active_run(args.db_root)
            router = advice.load_router(args.router)
            if args.event == "verify-result":
                if args.exit_code is None:
                    parser.error("verify-result requires --exit")
                lines = resolve(args.db_root, router, "verify_result", label_verify, args.exit_code)
                lines += resolve(args.db_root, router, "first_verify", label_allocation, args.exit_code)
            elif args.event == "run-complete":
                lines = resolve(args.db_root, router, "run_complete", label_handoff, int_value(state, "LOOPS_USED"))
                lines += pilot_lines(args.db_root)
            else:
                signals = git_signals(args.project)
                if args.event == "plan-done":
                    context, resolver = build_plan_done(signals, state, args.db_root)
                elif args.event == "build-start":
                    context, resolver = build_build_start(signals, state, args.db_root)
                elif args.event == "verify-start":
                    context, resolver = build_verify_start(signals, state, args.db_root)
                elif args.event == "review-handoff":
                    if args.verify_exit is None:
                        parser.error("review-handoff requires --verify-exit")
                    context, resolver = build_review_handoff(signals, state, args.db_root, args.verify_exit)
                else:
                    if args.repeats is None:
                        parser.error("tool-repeat requires --repeats")
                    context, resolver = build_tool_repeat(signals, state, args.repeats)
                lines = [emit(context, resolver, router, args.db_root, state)]
    except Skip as reason:
        lines = [f"checkpoint skipped: {reason}"]
    except (advice.InputError, task_route.InputError, OSError, ValueError) as error:
        # Checkpoints are shadow observations; a broken checkpoint never fails the caller.
        lines = [f"checkpoint skipped: {type(error).__name__}: {str(error)[:160]}"]
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
