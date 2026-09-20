#!/usr/bin/env python3
"""Route compact task metadata before an agent starts.

This module never reads or sends a user prompt, source file, or diff. TypeSafe is
optional; failure returns the caller's default path. Active routing is gated by
recorded outcomes and an explicit operator switch.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
KINDS = {"change", "bug", "review", "research", "question", "ops"}
AREAS = {"mobile", "frontend", "backend", "infrastructure", "docs", "other"}
ACTIONS = {"start_routine_agent", "run_targeted_check", "start_deep_agent", "ask_for_missing_input"}
UNCERTAINTY = {"none", "scope_unclear", "test_gap", "unknown_dependency", "conflicting_evidence", "other"}
DIFF_SIZES = {"none", "small", "medium", "large"}
REVERSIBILITY = {"reversible", "irreversible"}
ALLOWED = {
    "version", "task_kind", "area", "proposed_action", "reversibility",
    "uncertainty_reason", "diff_size", "changed_file_count", "known_failures",
    "required_checks_pending", "approval_required", "user_choice_explicit",
}
REQUIRED = {"version", "task_kind", "area", "proposed_action", "reversibility", "uncertainty_reason"}
RECOMMENDATIONS = {"proceed", "targeted_check", "reasoning_model", "ask_user", "deterministic_rule"}


class InputError(ValueError):
    pass


def load_metadata(path: Path) -> dict[str, Any]:
    if path.stat().st_size > 4096:
        raise InputError("task metadata exceeds 4096 bytes")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise InputError(f"cannot read task metadata: {error}") from error
    if not isinstance(data, dict):
        raise InputError("task metadata must be an object")
    extra = set(data) - ALLOWED
    missing = REQUIRED - set(data)
    if extra or missing:
        raise InputError(f"unknown fields: {sorted(extra)}; missing fields: {sorted(missing)}")
    if type(data["version"]) is not int or data["version"] != 1:
        raise InputError("version must be 1")
    for name, choices in (
        ("task_kind", KINDS), ("area", AREAS), ("proposed_action", ACTIONS),
        ("reversibility", REVERSIBILITY), ("uncertainty_reason", UNCERTAINTY),
        ("diff_size", DIFF_SIZES),
    ):
        if name in data and (not isinstance(data[name], str) or data[name] not in choices):
            raise InputError(f"{name} must be one of {sorted(choices)}")
    for name in ("changed_file_count", "known_failures"):
        value = data.get(name, 0)
        if type(value) is not int or not 0 <= value <= 10000:
            raise InputError(f"{name} must be an integer from 0 to 10000")
        data[name] = value
    for name in ("required_checks_pending", "approval_required", "user_choice_explicit"):
        value = data.get(name, False)
        if type(value) is not bool:
            raise InputError(f"{name} must be boolean")
        data[name] = value
    data.setdefault("diff_size", "none")
    return data


def deterministic_route(data: dict[str, Any]) -> tuple[str, str] | None:
    if data["approval_required"]:
        return "ask_user", "authorization is owned by the user"
    if data["known_failures"]:
        return "reasoning_model", "a known failure needs normal diagnosis"
    if data["required_checks_pending"]:
        return "targeted_check", "required checks cannot be skipped"
    if data["user_choice_explicit"]:
        return "deterministic_rule", "the user already chose the route"
    if data["reversibility"] == "irreversible":
        return "reasoning_model", "irreversible work is not delegated to TypeSafe"
    if data["uncertainty_reason"] == "none":
        return "default", "no ambiguous route remains"
    return None


def api_state(data: dict[str, Any]) -> dict[str, Any]:
    return {
        "decision": {
            "question": "Does this reversible task need a targeted check or deeper reasoning before a routine agent starts?",
            "proposed_action": data["proposed_action"],
            "reversibility": data["reversibility"],
        },
        "task": {"kind": data["task_kind"], "area": data["area"]},
        "signals": {
            "uncertainty_reason": data["uncertainty_reason"],
            "diff_size": data["diff_size"],
            "changed_file_count": data["changed_file_count"],
            "known_failures": data["known_failures"],
        },
        "constraints": ["Required checks and user choices are deterministic gates"],
    }


def active_eligible() -> tuple[bool, str]:
    if os.environ.get("HARNESS_TYPESAFE_ACTIVE") != "1":
        return False, "set HARNESS_TYPESAFE_ACTIVE=1 after reviewing the outcome report"
    log_root = Path(os.environ.get("TYPESAFE_LOG_DIR") or (Path(os.environ.get("TYPESAFE_HOME", str(Path.home() / ".typesafe-routing"))) / "logs"))
    outcomes = log_root / "outcomes.jsonl"
    valid_calls: dict[str, str] = {}
    outcomes_by_call: dict[str, str] = {}
    under = 0
    try:
        for path in log_root.glob("*.jsonl"):
            if path.name == "outcomes.jsonl":
                continue
            for line in path.read_text(encoding="utf-8").splitlines():
                row = json.loads(line)
                call_id = row.get("call_id")
                policy = row.get("policy") or {}
                observed = policy.get("observed_recommendation")
                if row.get("shadow") is True and row.get("exit_code") == 0 and call_id and observed in RECOMMENDATIONS:
                    valid_calls[call_id] = observed
        for line in outcomes.read_text(encoding="utf-8").splitlines():
            row = json.loads(line)
            call_id = row.get("call_id")
            if call_id in outcomes_by_call:
                return False, f"duplicate outcome for call {call_id}"
            if call_id:
                outcomes_by_call[call_id] = row.get("outcome", "")
            under += row.get("outcome") == "under_escalated"
    except (OSError, ValueError) as error:
        return False, f"cannot inspect outcome log: {error}"
    correct = sum(outcomes_by_call.get(call_id) == "correct" for call_id in valid_calls)
    routine_correct = sum(
        observed == "proceed" and outcomes_by_call.get(call_id) == "correct"
        for call_id, observed in valid_calls.items()
    )
    if correct < 30 or routine_correct < 5 or under:
        return False, (
            "need 30 correct shadow outcomes, including 5 correct routine routes, "
            f"and zero under-escalations (correct={correct}, routine={routine_correct}, under={under})"
        )
    return True, ""


def invoke_router(state: dict[str, Any], router: Path, mode: str) -> tuple[dict[str, Any] | None, str | None]:
    if not router.is_file():
        return None, f"router missing: {router}"
    command = [sys.executable, str(router), "route", "--strict"]
    if mode == "shadow":
        command.append("--shadow")
    try:
        result = subprocess.run(command, input=json.dumps(state), text=True, capture_output=True, timeout=35, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        return None, f"router unavailable: {error}"
    if result.returncode:
        return None, f"router exit {result.returncode}: {result.stderr.strip()[:250]}"
    try:
        answer = json.loads(result.stdout)
        policy = answer["policy"]
        if policy["recommendation"] not in RECOMMENDATIONS:
            raise ValueError("unknown recommendation")
        if mode == "shadow" and policy["recommendation"] != "reasoning_model":
            raise ValueError("shadow router did not preserve normal reasoning")
    except (ValueError, KeyError, TypeError) as error:
        return None, f"malformed router output: {error}"
    return answer, None


def write_record(record: dict[str, Any], db_root: Path) -> Path:
    directory = db_root / "routes"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(directory, 0o700)
    record_path = directory / f"{record['id']}.json"
    fd, temporary = tempfile.mkstemp(prefix=".route-", dir=directory)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(record, stream, sort_keys=True)
            stream.write("\n")
        os.replace(temporary, record_path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return record_path


def route_task(metadata: Path, project: Path, db_root: Path, router: Path, mode: str) -> dict[str, Any]:
    data = load_metadata(metadata)
    if mode == "active":
        eligible, reason = active_eligible()
        if not eligible:
            raise InputError(f"active routing is gated: {reason}")
    fixed = deterministic_route(data)
    answer = error = None
    if fixed:
        recommendation, reason = fixed
        source = "deterministic"
    else:
        answer, error = invoke_router(api_state(data), router, mode)
        if answer is None:
            recommendation, reason, source = "default", error or "router unavailable", "fallback"
        else:
            policy = answer["policy"]
            recommendation = "default" if mode == "shadow" else policy["recommendation"]
            reason = policy.get("reason", "")
            source = "typesafe"
    record: dict[str, Any] = {
        "id": str(uuid.uuid4()),
        "at": datetime.now(timezone.utc).isoformat(),
        "project": str(project.resolve()),
        "mode": mode,
        "source": source,
        "recommendation": recommendation,
        "reason": reason,
        "metadata": data,
    }
    if answer:
        record["observed_recommendation"] = answer["policy"].get("observed_recommendation")
        record["call_id"] = answer.get("call_id")
        record["usage"] = answer.get("usage")
        record["latency_ms"] = answer.get("latency_ms")
    record["record_path"] = str(write_record(record, db_root))
    return record


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", required=True, type=Path, help="compact enum-only task metadata JSON")
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument("--mode", choices=("shadow", "active"), default="shadow")
    parser.add_argument("--router", type=Path, default=Path(os.environ.get("HARNESS_TYPESAFE_ROUTER", str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py"))))
    parser.add_argument("--db-root", type=Path, default=Path(os.environ.get("HARNESS_DB_ROOT", str(ROOT / ".harness-db"))))
    args = parser.parse_args(argv)
    if not args.project.is_dir():
        parser.error(f"project does not exist: {args.project}")
    try:
        result = route_task(args.state, args.project, args.db_root, args.router, args.mode)
    except (InputError, OSError) as error:
        parser.error(str(error))
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
