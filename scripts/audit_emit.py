#!/usr/bin/env python3
"""Best-effort bridge from harness measurements to the local Arc audit service."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import urllib.error
import urllib.request
import uuid
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_URL = "http://127.0.0.1:8080"
MAX_RECORD_BYTES = 32_000


def read_record(path: Path) -> dict[str, Any]:
    if path.stat().st_size > MAX_RECORD_BYTES:
        raise ValueError("audit source record is too large")
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError("audit source record must be an object")
    return value


def enabled() -> bool:
    return os.environ.get("HARNESS_AUDIT_ENABLED") == "1"


def audit_url() -> str:
    return os.environ.get("HARNESS_AUDIT_URL", DEFAULT_URL).rstrip("/")


def emit(event_type: str, payload: dict[str, Any]) -> bool:
    if not enabled():
        return True
    body = json.dumps(
        {"id": str(uuid.uuid4()), "event_type": event_type, "payload": payload},
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")
    if len(body) > MAX_RECORD_BYTES:
        raise ValueError("audit event is too large")
    request = urllib.request.Request(
        f"{audit_url()}/api/audit/events",
        data=body,
        headers={"Content-Type": "application/json", "Accept": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=2) as response:
            return 200 <= response.status < 300
    except (OSError, urllib.error.URLError, urllib.error.HTTPError) as error:
        print(f"AUDIT TELEMETRY UNAVAILABLE: {type(error).__name__}", file=sys.stderr)
        return False


def scalar(value: Any) -> Any:
    return value if isinstance(value, (str, int, float, bool)) or value is None else None


def route_payload(record: dict[str, Any]) -> dict[str, Any]:
    usage = record.get("usage") if isinstance(record.get("usage"), dict) else {}
    model = record.get("model") if isinstance(record.get("model"), dict) else {}
    return {
        "route_id": scalar(record.get("id")),
        "call_id": scalar(record.get("call_id")),
        "source": scalar(record.get("source")),
        "recommendation": scalar(record.get("recommendation")),
        "observed_recommendation": scalar(record.get("observed_recommendation")),
        "mode": scalar(record.get("mode")),
        "routing_mode": scalar(record.get("routing_mode")),
        "rollout_percent": scalar(record.get("rollout_percent")),
        "rollout_bucket": scalar(record.get("rollout_bucket")),
        "rollout_selected": scalar(record.get("rollout_selected")),
        "model_requested": scalar(model.get("requested")),
        "model_returned": scalar(model.get("returned")),
        "jev_input_tokens": scalar(usage.get("input_tokens")),
        "jev_output_tokens": scalar(usage.get("output_tokens")),
        "jev_total_tokens": scalar(usage.get("total_tokens")),
        "jev_latency_ms": scalar(record.get("latency_ms")),
    }


def route_event(args: argparse.Namespace) -> int:
    record = read_record(args.record)
    emit("jev.route", route_payload(record))
    return 0


def record_type_safe_outcome(args: argparse.Namespace, record: dict[str, Any]) -> None:
    call_id = record.get("call_id")
    if not isinstance(call_id, str):
        return
    evidence = args.evidence or "Automatic launch completion does not establish route correctness."
    command = [
        sys.executable,
        os.environ.get(
            "HARNESS_TYPESAFE_ROUTER",
            str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py"),
        ),
        "record",
        "--call-id",
        call_id,
        "--outcome",
        args.outcome,
        "--route-taken",
        args.route_taken,
        "--evidence",
        evidence,
        "--avoided",
        args.avoided,
    ]
    if args.avoided_cost_ms is not None:
        command.extend(["--avoided-cost-ms", str(args.avoided_cost_ms)])
    if args.total_decision_ms is not None:
        command.extend(["--total-decision-ms", str(args.total_decision_ms)])
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    if result.returncode:
        print("TYPE SAFE OUTCOME NOT RECORDED: route.py rejected the outcome", file=sys.stderr)


def outcome_payload(args: argparse.Namespace, record: dict[str, Any]) -> dict[str, Any]:
    return {
        "route_id": scalar(record.get("id")),
        "call_id": scalar(record.get("call_id")),
        "outcome": args.outcome,
        "route_taken": args.route_taken,
        "avoided": args.avoided,
        "evidence_sha256": hashlib.sha256((args.evidence or "").encode()).hexdigest(),
        "total_decision_ms": args.total_decision_ms,
        "avoided_cost_ms": args.avoided_cost_ms,
    }


def outcome_event(args: argparse.Namespace) -> int:
    record = read_record(args.record)
    record_type_safe_outcome(args, record)
    emit("jev.outcome", outcome_payload(args, record))
    return 0


def checkpoint_payload(record: dict[str, Any]) -> dict[str, Any]:
    """Compact facts about one shadow checkpoint; no context text leaves the machine."""
    usage = record.get("usage") if isinstance(record.get("usage"), dict) else {}
    answers = record.get("answers") if isinstance(record.get("answers"), dict) else {}
    recommendation = answers.get("recommendation") if isinstance(answers.get("recommendation"), dict) else {}
    return {
        "call_id": scalar(record.get("call_id")),
        "family": scalar(record.get("family")),
        "question_version": scalar(record.get("question_version")),
        "policy_version": scalar(record.get("policy_version")),
        "question_hash": scalar(record.get("question_hash")),
        "status": scalar(record.get("status")),
        "fallback_reason": scalar(record.get("fallback_reason")),
        "bypass_reason": scalar(record.get("bypass_reason")),
        "shadow": scalar(record.get("shadow")),
        "baseline_action": scalar(record.get("baseline_action")),
        "recommendation": scalar(recommendation.get("choice")),
        "recommendation_confidence": scalar(recommendation.get("confidence")),
        "question_count": len(record.get("questions") or {}),
        "model_requested": scalar(record.get("model_requested")),
        "model_returned": scalar(record.get("model_returned")),
        "jev_input_tokens": scalar(usage.get("input_tokens")),
        "jev_output_tokens": scalar(usage.get("output_tokens")),
        "jev_total_tokens": scalar(usage.get("total_tokens")),
        "jev_latency_ms": scalar(record.get("latency_ms")),
        "state_build_ms": scalar(record.get("state_build_ms")),
    }


def checkpoint_event(args: argparse.Namespace) -> int:
    record = read_record(args.record)
    emit("jev.checkpoint", checkpoint_payload(record))
    return 0


def checkpoint_outcome_event(args: argparse.Namespace) -> int:
    record = read_record(args.record)
    outcome = read_record(args.outcome_record)
    emit(
        "jev.checkpoint_outcome",
        {
            "call_id": scalar(record.get("call_id")),
            "family": scalar(record.get("family")),
            "question_version": scalar(record.get("question_version")),
            "policy_version": scalar(record.get("policy_version")),
            "baseline_action": scalar(record.get("baseline_action")),
            "outcome": scalar(outcome.get("outcome")),
            "action_taken": scalar(outcome.get("action_taken")),
            "evidence_sha256": hashlib.sha256(str(outcome.get("evidence", "")).encode()).hexdigest(),
            "total_decision_ms": scalar(outcome.get("total_decision_ms")),
            "rework_ms": scalar(outcome.get("rework_ms")),
            "baseline_ms": scalar(outcome.get("baseline_ms")),
        },
    )
    return 0


def token_values(args: argparse.Namespace) -> dict[str, int | None]:
    values: dict[str, int | None] = {
        "agent_input_tokens": args.input_tokens,
        "agent_output_tokens": args.output_tokens,
        "agent_total_tokens": args.total_tokens,
    }
    if values["agent_total_tokens"] is None and all(
        values[key] is not None for key in ("agent_input_tokens", "agent_output_tokens")
    ):
        values["agent_total_tokens"] = values["agent_input_tokens"] + values["agent_output_tokens"]  # type: ignore[operator]
    return values


def agent_event(args: argparse.Namespace) -> int:
    record = read_record(args.record)
    values = token_values(args)
    status = "provided" if values["agent_total_tokens"] is not None else "missing"
    emit(
        "agent.completed",
        {
            "route_id": scalar(record.get("id")),
            "call_id": scalar(record.get("call_id")),
            "profile": args.profile,
            "exit_code": args.exit_code,
            "token_measurement_status": status,
            **values,
        },
    )
    return 0


def token_event(args: argparse.Namespace) -> int:
    record = read_record(args.record) if args.record else {}
    emit(
        "agent.token_usage",
        {
            "route_id": scalar(record.get("id")),
            "call_id": scalar(record.get("call_id")),
            "agent_total_tokens_delta": args.delta,
            "agent_total_tokens": args.total,
            "token_measurement_status": "provided",
        },
    )
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    route = subparsers.add_parser("route")
    route.add_argument("--record", type=Path, required=True)
    route.set_defaults(handler=route_event)

    outcome = subparsers.add_parser("outcome")
    outcome.add_argument("--record", type=Path, required=True)
    outcome.add_argument("--outcome", choices=("correct", "over_escalated", "under_escalated", "unknown"), required=True)
    outcome.add_argument("--route-taken", required=True)
    outcome.add_argument("--evidence", default="")
    outcome.add_argument("--avoided", default="none")
    outcome.add_argument("--avoided-cost-ms", type=int)
    outcome.add_argument("--total-decision-ms", type=int)
    outcome.set_defaults(handler=outcome_event)

    agent = subparsers.add_parser("agent")
    agent.add_argument("--record", type=Path, required=True)
    agent.add_argument("--profile", required=True)
    agent.add_argument("--exit-code", type=int, required=True)
    agent.add_argument("--input-tokens", type=int)
    agent.add_argument("--output-tokens", type=int)
    agent.add_argument("--total-tokens", type=int)
    agent.set_defaults(handler=agent_event)

    checkpoint = subparsers.add_parser("checkpoint")
    checkpoint.add_argument("--record", type=Path, required=True)
    checkpoint.set_defaults(handler=checkpoint_event)

    checkpoint_outcome = subparsers.add_parser("checkpoint-outcome")
    checkpoint_outcome.add_argument("--record", type=Path, required=True)
    checkpoint_outcome.add_argument("--outcome-record", type=Path, required=True)
    checkpoint_outcome.set_defaults(handler=checkpoint_outcome_event)

    token = subparsers.add_parser("token")
    token.add_argument("--record", type=Path)
    token.add_argument("--delta", type=int, required=True)
    token.add_argument("--total", type=int)
    token.set_defaults(handler=token_event)

    args = parser.parse_args(argv)
    try:
        return args.handler(args)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"AUDIT TELEMETRY SKIPPED: {error}", file=sys.stderr)
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
