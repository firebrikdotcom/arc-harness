#!/usr/bin/env python3
"""Ask TypeSafe for a context-specific, advisory choice during agent work.

This command never executes its recommendation. Permissions, destructive work,
known failures, and required checks stay deterministic harness gates.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import hashlib
import math
import statistics
import time
import os
import re
import subprocess
import sys
import tempfile
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
import jev_delegation  # noqa: E402

MAX_BYTES = 32_000
MAX_OPTIONS = 8
OPTION_ID = re.compile(r"^[a-z][a-z0-9_-]{0,63}$")
FAMILIES = {"tool_selection", "reasoning_allocation", "identification", "prioritization",
            "evidence_selection", "evidence_assessment", "progress_assessment",
            "handoff_assessment", "context_selection", "clarification_assessment"}
BYPASSES = {"none", "explicit_rule", "user_choice", "required_check", "known_failure",
            "authorization", "irreversible", "unchanged_state", "not_bounded"}
OUTCOMES = {"correct", "incorrect", "over_escalated", "under_escalated", "unknown"}
CONTEXT_KEYS = {"goal", "facts", "constraints", "risks"}
PILOT_TARGET = 30
DEFAULT_POLICY_VERSION = "shadow-1"
DELEGATED_POLICY_VERSION = "delegated-1"


class InputError(ValueError):
    pass


def delegating() -> bool:
    """Whether `harness jev` is on; a corrupt switch file is an input error."""
    try:
        return jev_delegation.enabled()
    except jev_delegation.SwitchError as error:
        raise InputError(str(error)) from error


def load_router(path: Path) -> Any:
    if not path.is_file():
        raise InputError(f"router missing: {path}")
    spec = importlib.util.spec_from_file_location("harness_typesafe_router", path)
    if spec is None or spec.loader is None:
        raise InputError(f"cannot load router: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def strings(value: Any, name: str, *, minimum: int = 0, maximum: int = 32) -> list[str]:
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        raise InputError(f"{name} must contain {minimum} to {maximum} strings")
    if any(not isinstance(item, str) or not item.strip() or len(item) > 2000 for item in value):
        raise InputError(f"{name} must contain non-empty strings of at most 2000 characters")
    return value


def load_legacy_context(path: Path, router: Any) -> dict[str, Any]:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise InputError(f"cannot read advice context: {error}") from error
    if not isinstance(raw, dict) or set(raw) != {"version", "decision", "context", "options"}:
        raise InputError("context must contain exactly version, decision, context, and options")
    if raw["version"] != 1:
        raise InputError("version must be 1")
    decision = raw["decision"]
    if not isinstance(decision, dict) or set(decision) != {"question"}:
        raise InputError("decision must contain exactly question")
    if not isinstance(decision["question"], str) or not decision["question"].strip() or len(decision["question"]) > 2000:
        raise InputError("decision.question must be a non-empty string of at most 2000 characters")
    context = raw["context"]
    if not isinstance(context, dict) or set(context) - CONTEXT_KEYS or "goal" not in context:
        raise InputError("context must contain goal and may contain facts, constraints, and risks")
    if not isinstance(context["goal"], str) or not context["goal"].strip() or len(context["goal"]) > 4000:
        raise InputError("context.goal must be a non-empty string of at most 4000 characters")
    for key in CONTEXT_KEYS - {"goal"}:
        if key in context:
            strings(context[key], f"context.{key}")
    options = raw["options"]
    if not isinstance(options, list) or not 2 <= len(options) <= MAX_OPTIONS:
        raise InputError(f"options must contain 2 to {MAX_OPTIONS} choices")
    ids: set[str] = set()
    for option in options:
        if not isinstance(option, dict) or set(option) != {"id", "description"}:
            raise InputError("each option must contain exactly id and description")
        if not isinstance(option["id"], str) or not OPTION_ID.fullmatch(option["id"]):
            raise InputError("option ids must use lowercase letters, digits, underscores, or hyphens")
        if option["id"] in ids:
            raise InputError("option ids must be unique")
        ids.add(option["id"])
        if not isinstance(option["description"], str) or not option["description"].strip() or len(option["description"]) > 2000:
            raise InputError("option descriptions must be non-empty strings of at most 2000 characters")
    serialized = json.dumps(raw, separators=(",", ":"))
    if len(serialized.encode()) > MAX_BYTES:
        raise InputError(f"context exceeds {MAX_BYTES} bytes")
    redaction_counts: Any = router.Counter()
    router.redact(raw, redaction_counts)
    if redaction_counts:
        labels = ", ".join(f"{count} {kind}" for kind, count in sorted(redaction_counts.items()))
        raise InputError(f"context contains sensitive content ({labels}); remove it before requesting advice")
    return raw


def questions(context: dict[str, Any]) -> dict[str, Any]:
    if context["version"] == 2:
        return {key: {**q, "type": "noul" if q["type"] == "boolean" else q["type"]}
                for key, q in context["questions"].items()}
    return {
        "recommendation": {
            "type": "choice",
            "instructions": {
                "question": context["decision"]["question"],
                "focus": "Choose exactly one option using only the supplied context and constraints.",
            },
            "criteria": {option["id"]: {"what": option["description"]} for option in context["options"]},
        }
    }


def write_record(record: dict[str, Any], db_root: Path) -> Path:
    directory = db_root / "advice"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(directory, 0o700)
    target = directory / f"{record['id']}.json"
    fd, temporary = tempfile.mkstemp(prefix=".advice-", dir=directory)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(record, stream, sort_keys=True)
            stream.write("\n")
        os.replace(temporary, target)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return target


def finite(value: Any, minimum: float = 0, maximum: float = float("inf")) -> bool:
    return type(value) in (int, float) and math.isfinite(value) and minimum <= value <= maximum


def safe_input(raw: Any, router: Any) -> None:
    counts: Any = router.Counter()
    router.redact(raw, counts)
    if counts:
        raise InputError("context contains sensitive content; remove it before requesting advice")


def read_json(path: Path, maximum: int = MAX_BYTES) -> Any:
    if path.stat().st_size > maximum:
        raise InputError(f"input exceeds {maximum} bytes")
    return json.loads(path.read_text(encoding="utf-8"))


def text_field(value: Any, name: str, limit: int = 2000) -> None:
    if not isinstance(value, str) or not value.strip() or len(value) > limit:
        raise InputError(f"{name} must be a non-empty string of at most {limit} characters")


def load_context(path: Path, router: Any) -> dict[str, Any]:
    raw = read_json(path)
    if isinstance(raw, dict) and raw.get("version") == 1:
        return load_legacy_context(path, router)
    return validate_context(raw, router)


def validate_context(raw: Any, router: Any) -> dict[str, Any]:
    """Validate an in-memory v2 checkpoint exactly like a file-based one."""
    if not isinstance(raw, dict) or type(raw.get("version")) is not int:
        raise InputError("version must be 1 or 2")
    if raw["version"] == 1:
        raise InputError("v1 contexts must be supplied as a file")
    if len(json.dumps(raw, separators=(",", ":")).encode()) > MAX_BYTES:
        raise InputError(f"input exceeds {MAX_BYTES} bytes")
    if raw["version"] != 2 or set(raw) != {"version", "checkpoint", "context", "questions"}:
        raise InputError("v2 requires exactly version, checkpoint, context, and questions")
    checkpoint = raw["checkpoint"]
    required = {"family", "question_version", "policy_version", "baseline_action", "bypass_reason"}
    if not isinstance(checkpoint, dict) or not required <= set(checkpoint) or set(checkpoint) - required - {"state_build_ms"}:
        raise InputError("invalid checkpoint fields")
    if checkpoint["family"] not in FAMILIES or checkpoint["bypass_reason"] not in BYPASSES:
        raise InputError("unknown checkpoint family or bypass_reason")
    for key in ("question_version", "policy_version", "baseline_action"):
        text_field(checkpoint[key], key)
    if checkpoint.get("state_build_ms") is not None and not finite(checkpoint["state_build_ms"]):
        raise InputError("state_build_ms must be non-negative or null")
    context = raw["context"]
    if not isinstance(context, dict) or "goal" not in context or set(context) - CONTEXT_KEYS:
        raise InputError("context requires goal and optional facts, constraints, risks")
    text_field(context["goal"], "goal", 4000)
    for key in CONTEXT_KEYS - {"goal"}:
        if key in context:
            strings(context[key], key)
    batch = raw["questions"]
    if not isinstance(batch, dict) or len(batch) > MAX_OPTIONS or (not batch and checkpoint["bypass_reason"] == "none"):
        raise InputError("supply 1 to 8 independent questions, or an empty map for a bypass")
    for key, question in batch.items():
        if not OPTION_ID.fullmatch(key) or not isinstance(question, dict):
            raise InputError("invalid question id or object")
        kind = question.get("type")
        expected = {"type", "instructions"} if kind == "boolean" else {"type", "instructions", "criteria"}
        if kind not in {"choice", "score", "boolean"} or set(question) != expected:
            raise InputError("invalid question type or fields")
        text_field(question["instructions"], "instructions")
        if kind == "choice":
            criteria = question["criteria"]
            if not isinstance(criteria, dict) or not 2 <= len(criteria) <= MAX_OPTIONS:
                raise InputError("choice requires 2 to 8 criteria")
            for option, description in criteria.items():
                if not OPTION_ID.fullmatch(option):
                    raise InputError("invalid choice id")
                text_field(description, "criterion")
        elif kind == "score":
            strings(question["criteria"], "ordered score levels", minimum=2, maximum=MAX_OPTIONS)
    safe_input(raw, router)
    return raw


def pair(value: str, name: str) -> tuple[str, str]:
    key, separator, text = value.partition("=")
    if not separator or not key or not text:
        raise InputError(f"{name} must look like id=text")
    return key, text


def quick_context(args: argparse.Namespace) -> dict[str, Any]:
    """Build a v2 checkpoint from flags so no JSON file is needed mid-task."""
    questions: dict[str, Any] = {}
    if args.choice:
        criteria = dict(pair(item, "--choice") for item in args.choice)
        questions["recommendation"] = {
            "type": "choice",
            "instructions": args.question or "Which option best fits the supplied context?",
            "criteria": criteria,
        }
    for item in args.boolean or []:
        key, text = pair(item, "--boolean")
        questions[key] = {"type": "boolean", "instructions": text}
    for item in args.score or []:
        key, text = pair(item, "--score")
        instructions, separator, levels = text.partition(":")
        if not separator:
            raise InputError("--score must look like id=instructions:level one|level two")
        questions[key] = {"type": "score", "instructions": instructions, "criteria": levels.split("|")}
    context: dict[str, Any] = {"goal": args.goal or ""}
    for name in ("facts", "constraints", "risks"):
        values = getattr(args, name[:-1] if name != "facts" else "fact") or []
        if values:
            context[name] = values
    # Delegated and shadow checkpoints are separate cohorts in the reports.
    policy_version = args.policy_version or (DELEGATED_POLICY_VERSION if delegating() else DEFAULT_POLICY_VERSION)
    return {
        "version": 2,
        "checkpoint": {
            "family": args.family,
            "question_version": args.question_version,
            "policy_version": policy_version,
            "baseline_action": args.baseline or "",
            "bypass_reason": args.bypass,
            "state_build_ms": None,
        },
        "context": context,
        "questions": questions,
    }


def validated_answers(batch: dict, response: Any) -> dict:
    if not isinstance(response, dict) or not isinstance(response.get("answers"), dict):
        raise InputError("invalid answers object")
    answers = response["answers"]
    if set(answers) != set(batch):
        raise InputError("missing or unexpected answers")
    clean = {}
    for key, question in batch.items():
        answer = answers[key]
        kind = question["type"]
        if not isinstance(answer, dict) or answer.get("type", kind) != kind:
            raise InputError("invalid answer type")
        if kind == "noul":
            if not finite(answer.get("noul"), 0, 1):
                raise InputError("invalid Boolean probability")
            clean[key] = {"type": "boolean", "probability": answer["noul"]}
            continue
        if not finite(answer.get("confidence"), 0, 1):
            raise InputError("TypeSafe returned an invalid confidence")
        if kind == "choice":
            value = answer.get("choice")
            if not isinstance(value, str) or value not in question["criteria"]:
                raise InputError("TypeSafe returned a choice outside the supplied options")
            expected = set(question["criteria"])
        else:
            value = answer.get("score")
            if not finite(value, 0, len(question["criteria"]) - 1):
                raise InputError("invalid rubric score")
            expected = {str(i) for i in range(len(question["criteria"]))}
        probabilities = answer.get("probabilities")
        if probabilities is not None:
            if (not isinstance(probabilities, dict) or set(probabilities) != expected
                    or not all(finite(v, 0, 1) for v in probabilities.values())
                    or not math.isclose(sum(probabilities.values()), 1, abs_tol=0.001)):
                raise InputError("invalid probability distribution")
        clean[key] = {"type": kind, kind: value, "confidence": answer["confidence"],
                      "probabilities": probabilities}
    return clean


def request_timeout(router: Any) -> float:
    """Per-attempt timeout; hooks and phase gates bound it with HARNESS_JEV_TIMEOUT."""
    raw = os.environ.get("HARNESS_JEV_TIMEOUT", "").strip()
    try:
        value = float(raw) if raw else float(router.DEFAULT_TIMEOUT)
    except ValueError as error:
        raise InputError("HARNESS_JEV_TIMEOUT must be a positive number of seconds") from error
    if not math.isfinite(value) or value <= 0:
        raise InputError("HARNESS_JEV_TIMEOUT must be a positive number of seconds")
    return value


def request_options() -> dict[str, Any]:
    raw = os.environ.get("HARNESS_JEV_ATTEMPTS", "").strip()
    if not raw:
        return {}
    if not raw.isdigit() or not 1 <= int(raw) <= 5:
        raise InputError("HARNESS_JEV_ATTEMPTS must be an integer from 1 to 5")
    return {"attempts": int(raw)}


def audit_emit(*arguments: str) -> None:
    """Best-effort Arc telemetry; never affects the checkpoint result."""
    if os.environ.get("HARNESS_AUDIT_ENABLED") != "1":
        return
    emitter = ROOT / "scripts" / "audit_emit.py"
    try:
        result = subprocess.run([sys.executable, str(emitter), *arguments], capture_output=True,
                                text=True, timeout=3, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return
    if result.stderr.strip():
        print(result.stderr.strip(), file=sys.stderr)


def advise(context: dict[str, Any], router: Any, db_root: Path, delegate: bool | None = None) -> dict[str, Any]:
    """Evaluate one checkpoint.

    DELEGATE None follows the `harness jev` switch. When it resolves to True and
    Jev returns a `recommendation` choice, that choice is the result's `action`
    and `delegated` is true; otherwise the pre-recorded baseline is the action.
    Automatic phase-gate checkpoints pass False: the gates they predict are
    mechanical, so there is nothing for Jev to decide there.
    """
    request_id = str(uuid.uuid4())
    started = datetime.now(timezone.utc)
    checkpoint = context.get("checkpoint", {})
    batch = questions(context)
    # Use the switch's exact pin when the shell has no exact pin.
    try:
        model = (jev_delegation.pinned_model() if jev_delegation.state()["model"]
                 else router.pinned_model())
    except jev_delegation.SwitchError as error:
        raise InputError(str(error)) from error
    if delegate is None:
        delegate = delegating()
    delegated = bool(delegate) and context["version"] == 2
    record = {
        "id": request_id, "call_id": request_id, "at": started.isoformat(), "ts": started.isoformat(),
        "kind": "dynamic_advice", "family": checkpoint.get("family", "legacy_advice"),
        "question_version": checkpoint.get("question_version", "legacy-v1"),
        "question_hash": hashlib.sha256(json.dumps(batch, sort_keys=True).encode()).hexdigest(),
        "policy_version": checkpoint.get("policy_version", "legacy-v1"),
        "shadow": context["version"] == 2, "delegation": "active" if delegated else "shadow",
        "delegated_action": None, "context": context, "questions": batch,
        "baseline_action": checkpoint.get("baseline_action"), "state_build_ms": checkpoint.get("state_build_ms"),
        "model_requested": model, "model_returned": None, "usage": None, "latency_ms": None,
        "status": "pending", "fallback_reason": None, "answers": None,
    }
    # Persist the baseline before evaluation; do not send it to Jev (avoids anchoring).
    write_record(record, db_root)
    bypass = checkpoint.get("bypass_reason", "none")
    began = time.monotonic()
    if bypass != "none":
        record.update(status="bypassed", bypass_reason=bypass, exit_code=0)
    elif delegated and (not model or model.endswith("-latest")):
        record.update(status="fallback", fallback_reason="model_pin_required", exit_code=1)
    else:
        try:
            response, meta = router.post_json(
                router.api_url(), {"state": context["context"] if record["shadow"] else context,
                                   "model": model, "questions": batch},
                router.resolve_api_key()[0], request_timeout(router), **request_options())
            if isinstance(response, dict):
                returned = response.get("model")
                record["model_returned"] = returned if isinstance(returned, str) else None
                usage = response.get("usage")
                if isinstance(usage, dict):
                    record["usage"] = {k: v for k, v in usage.items()
                                       if k in {"input_tokens", "output_tokens", "total_tokens"} and type(v) is int and v >= 0} or None
            if isinstance(meta, dict):
                record["latency_ms"] = meta.get("latency_ms") if finite(meta.get("latency_ms")) else None
            record["answers"] = validated_answers(batch, response)
            record.update(status="evaluated", exit_code=0)
            if delegated and record["model_returned"] is None:
                record.update(status="fallback", fallback_reason="missing_model", exit_code=1)
            elif record["model_returned"] is not None and record["model_returned"] != model:
                record.update(status="fallback", fallback_reason="model_drift", exit_code=1)
        except Exception as error:
            # Exception text can echo credentials or provider content. Store only its type.
            record.update(status="fallback", fallback_reason=type(error).__name__, exit_code=1)
        record["evaluation_wall_ms"] = round((time.monotonic() - began) * 1000)
    chosen = None
    if delegated and record["status"] == "evaluated":
        chosen = ((record["answers"] or {}).get("recommendation") or {}).get("choice")
        record["delegated_action"] = chosen
    record_path = write_record(record, db_root)
    if bypass == "none":
        router.append_jsonl(router.requests_log_path(started), {k: v for k, v in record.items() if k != "context"})
        audit_emit("checkpoint", "--record", str(record_path))
    result = {k: record[k] for k in ("call_id", "status", "fallback_reason", "latency_ms", "answers", "shadow", "delegation")}
    baseline = record["baseline_action"] if record["shadow"] else "normal_reasoning"
    result.update(advisory=chosen is None, delegated=chosen is not None, baseline=baseline,
                  action=chosen if chosen is not None else baseline, record_path=str(record_path))
    if delegated and chosen is None:
        # The switch is on but there is nothing to follow: a bypass, a fallback, or no recommendation question.
        result["delegation_gate"] = record["status"] if record["status"] != "evaluated" else "no recommendation choice"
    # Keep the successful v1 CLI response compatible.
    if not record["shadow"] and record["status"] == "evaluated":
        result.update(choice=record["answers"]["recommendation"]["choice"],
                      confidence=record["answers"]["recommendation"]["confidence"])
    return result


def record_outcome(path: Path, router: Any, db_root: Path) -> dict:
    return label_outcome(read_json(path), router, db_root)


def label_outcome(raw: Any, router: Any, db_root: Path) -> dict:
    required = {"call_id", "action_taken", "outcome", "evidence"}
    metrics = {"total_decision_ms", "rework_ms", "baseline_ms"}
    if not isinstance(raw, dict) or not required <= set(raw) or set(raw) - required - metrics:
        raise InputError("outcome requires call_id, action_taken, outcome, evidence, and optional timing metrics")
    try:
        identifier = str(uuid.UUID(raw["call_id"]))
    except (ValueError, TypeError, AttributeError) as error:
        raise InputError("invalid call_id") from error
    for key in ("action_taken", "evidence"):
        text_field(raw[key], key)
    if raw["outcome"] not in OUTCOMES:
        raise InputError("unknown outcome label")
    for key in metrics:
        if raw.get(key) is not None and not finite(raw[key]):
            raise InputError("timing metrics must be non-negative or null")
    safe_input(raw, router)
    record = read_json(db_root / "advice" / f"{identifier}.json", maximum=256_000)
    if record.get("status") not in {"evaluated", "fallback", "bypassed"}:
        raise InputError("cannot label an unfinished evaluation")
    if record.get("status") != "evaluated" and raw["outcome"] != "unknown":
        raise InputError("a fallback or bypass has no usable Jev judgment to label")
    # Separate exclusive files prevent accidental relabeling and duplicate joins.
    directory = db_root / "advice-outcomes"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(directory, 0o700)
    target = directory / f"{identifier}.json"
    outcome = {**raw, **{key: raw.get(key) for key in metrics}, "at": datetime.now(timezone.utc).isoformat()}
    try:
        fd = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError as error:
        raise InputError("outcome already recorded") from error
    with os.fdopen(fd, "w") as stream:
        json.dump(outcome, stream, sort_keys=True, allow_nan=False)
        stream.write("\n")
    audit_emit("checkpoint-outcome", "--record", str(db_root / "advice" / f"{identifier}.json"),
               "--outcome-record", str(target))
    return {"recorded": identifier, "record_path": str(target)}


SCOPES = ("target", "machine")


def registry_dir(db_root: Path) -> Path:
    """Directory holding every registered target's private database."""
    if db_root.parent.parent.name == "targets":
        return db_root.parent.parent
    return db_root / "targets"


def scope_databases(db_root: Path, scope: str) -> list[tuple[str, Path]]:
    """(name, database) pairs in scope; the current database always comes first."""
    if scope not in SCOPES:
        raise InputError(f"scope must be one of {', '.join(SCOPES)}")
    current = db_root.parent.name if db_root.parent.parent.name == "targets" else "legacy"
    found = [(current, db_root)]
    if scope == "machine" and registry_dir(db_root).is_dir():
        seen = {db_root.resolve()}
        # The pre-registry shared database sits beside targets/; count it from every entry point.
        legacy = registry_dir(db_root).parent
        if legacy.resolve() not in seen and (legacy / "advice").is_dir():
            seen.add(legacy.resolve())
            found.append(("legacy", legacy))
        for state in sorted(registry_dir(db_root).glob("*/target.state")):
            database = state.parent / "db"
            if database.resolve() not in seen and (database / "advice").is_dir():
                seen.add(database.resolve())
                found.append((state.parent.name, database))
    return found


def records(name: str, db_root: Path, current: bool, skipped: list[str]):
    """(path, row, outcome) for one database. Other targets' unreadable files are skipped and counted."""
    for path in sorted((db_root / "advice").glob("*.json")):
        outcome_path = db_root / "advice-outcomes" / path.name
        try:
            row = json.loads(path.read_text())
            outcome = json.loads(outcome_path.read_text()) if outcome_path.is_file() else {}
        except (OSError, ValueError):
            if current:
                raise
            skipped.append(name)
            continue
        if isinstance(row, dict) and isinstance(outcome, dict):
            yield path, row, outcome
        elif current:
            raise ValueError(f"malformed advice record: {path.name}")
        else:
            skipped.append(name)


def pending(db_root: Path, limit: int = 50, scope: str = "target") -> dict:
    """Evaluated checkpoints that still have no independently supported label.

    A label is recorded in the database that owns the checkpoint, so machine scope names each row's target.
    """
    rows = []
    skipped: list[str] = []
    for name, database in scope_databases(db_root, scope):
        for _path, row, outcome in records(name, database, database == db_root, skipped):
            if row.get("status") != "evaluated" or outcome:
                continue
            recommendation = (row.get("answers") or {}).get("recommendation", {}).get("choice")
            rows.append({"call_id": row.get("call_id"), "at": row.get("at"), "family": row.get("family"),
                         "question_version": row.get("question_version"),
                         "baseline_action": row.get("baseline_action"), "recommendation": recommendation,
                         "target": name})
    rows.sort(key=lambda item: str(item.get("at")), reverse=True)
    result = {"scope": scope, "unlabeled": rows[:limit], "unlabeled_total": len(rows)}
    if scope == "target":
        for row in result["unlabeled"]:
            del row["target"]
    return result


def pilot(db_root: Path, scope: str = "machine") -> dict:
    """Progress toward the first 30 independently labeled shadow decisions.

    Machine scope sums every registered target's database; by_target keeps the breakdown.
    """
    summary: dict[str, Any] = {"target": PILOT_TARGET, "scope": scope, "labeled": 0, "correct": 0,
                               "unlabeled_evaluated": 0, "fallback": 0, "bypassed": 0, "by_family": {},
                               "by_target": {}, "unreadable_records": 0}
    skipped: list[str] = []
    for name, database in scope_databases(db_root, scope):
        part = summary["by_target"].setdefault(name, {"labeled": 0, "correct": 0, "unlabeled_evaluated": 0,
                                                       "fallback": 0, "bypassed": 0})
        for _path, row, outcome in records(name, database, database == db_root, skipped):
            family = summary["by_family"].setdefault(row.get("family") or "unknown", {"labeled": 0, "unlabeled": 0})
            status = row.get("status")
            if status in {"fallback", "bypassed"}:
                summary[status] += 1
                part[status] += 1
                continue
            if status != "evaluated":
                continue
            if outcome.get("outcome") in OUTCOMES - {"unknown"}:
                for holder in (summary, part):
                    holder["labeled"] += 1
                    holder["correct"] += outcome["outcome"] == "correct"
                family["labeled"] += 1
            else:
                summary["unlabeled_evaluated"] += 1
                part["unlabeled_evaluated"] += 1
                family["unlabeled"] += 1
    summary["unreadable_records"] = len(skipped)
    summary["remaining"] = max(0, PILOT_TARGET - summary["labeled"])
    summary["review_batch_ready"] = summary["labeled"] >= PILOT_TARGET
    return summary


COHORT_FIELDS = ("family", "question_version", "question_hash", "policy_version", "model_requested", "model_returned")


def report(db_root: Path, scope: str = "machine") -> dict:
    """Cohort report. Cohorts are keyed by family, question/policy version, and model, so
    machine scope pools identical cohorts across targets and never mixes versions."""
    groups: dict[tuple, dict] = {}
    skipped: list[str] = []
    for name, database in scope_databases(db_root, scope):
        for _path, row, outcome in records(name, database, database == db_root, skipped):
            key = tuple(row.get(k) for k in COHORT_FIELDS)
            group = groups.setdefault(key, {"cohort": dict(zip(COHORT_FIELDS, key)),
                "checkpoints": 0, "evaluated": 0, "fallback": 0, "bypassed": 0, "pending": 0,
                "labeled": 0, "correct": 0, "disagreements": 0, "comparable_choices": 0,
                "unresolved": 0, "latencies": [], "state_build": [], "total_times": [], "rework": [],
                "usage_known": 0, "tokens": 0, "paired_baselines": 0, "paired_delta_ms": 0})
            group["checkpoints"] += 1
            status = row.get("status", "evaluated" if row.get("exit_code") == 0 else "fallback")
            group[status] += 1
            if finite(row.get("latency_ms")):
                group["latencies"].append(row["latency_ms"])
            if finite(row.get("state_build_ms")):
                group["state_build"].append(row["state_build_ms"])
            usage = row.get("usage") or {}
            tokens = usage.get("total_tokens")
            if tokens is None and all(type(usage.get(k)) is int for k in ("input_tokens", "output_tokens")):
                tokens = usage["input_tokens"] + usage["output_tokens"]
            if type(tokens) is int:
                group["usage_known"] += 1
                group["tokens"] += tokens
            choice = (row.get("answers") or {}).get("recommendation", {}).get("choice")
            if status == "evaluated" and choice is not None and row.get("baseline_action") is not None:
                group["comparable_choices"] += 1
                group["disagreements"] += choice != row["baseline_action"]
            if status == "evaluated" and outcome.get("outcome") in OUTCOMES - {"unknown"}:
                group["labeled"] += 1
                group["correct"] += outcome["outcome"] == "correct"
            else:
                group["unresolved"] += 1
            for metric, target in (("total_decision_ms", "total_times"), ("rework_ms", "rework")):
                if finite(outcome.get(metric)):
                    group[target].append(outcome[metric])
            if finite(outcome.get("baseline_ms")) and finite(outcome.get("total_decision_ms")):
                group["paired_baselines"] += 1
                group["paired_delta_ms"] += outcome["total_decision_ms"] - outcome["baseline_ms"]
    for group in groups.values():
        for field in ("latencies", "state_build", "total_times", "rework"):
            values = group.pop(field)
            group[field + "_samples"] = len(values)
            group[field + "_median_ms"] = statistics.median(values) if values else None
        group["accuracy"] = group["correct"] / group["labeled"] if group["labeled"] else None
        if not group["usage_known"]:
            group["tokens"] = None
        if not group["paired_baselines"]:
            group["paired_delta_ms"] = None
    return {"scope": scope, "cohorts": list(groups.values()), "pilot": pilot(db_root, scope), "automatic_promotion": False,
            "note": "Shadow comparisons measure disagreement, not causal savings. Null means unknown."}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--context", type=Path, help="curated v1 advice or v2 shadow checkpoint JSON")
    mode.add_argument("--family", choices=sorted(FAMILIES), help="build a v2 shadow checkpoint from flags")
    mode.add_argument("--record", type=Path, help="record a labeled outcome JSON")
    mode.add_argument("--label", metavar="CALL_ID", help="record a labeled outcome from flags")
    mode.add_argument("--pending", action="store_true", help="list evaluated checkpoints without a label")
    mode.add_argument("--report", action="store_true", help="report local advice cohorts without API access")
    quick = parser.add_argument_group("flag-form checkpoint (with --family)")
    quick.add_argument("--baseline", help="the action you intend to take before asking")
    quick.add_argument("--goal", help="concise redacted goal")
    quick.add_argument("--question", help="instructions for the recommendation choice")
    quick.add_argument("--choice", action="append", metavar="ID=DESCRIPTION", help="one recommendation option; repeatable")
    quick.add_argument("--boolean", action="append", metavar="ID=STATEMENT", help="one Boolean question; repeatable")
    quick.add_argument("--score", action="append", metavar="ID=INSTRUCTIONS:LEVEL|LEVEL", help="one rubric score; repeatable")
    quick.add_argument("--fact", action="append", help="one redacted fact; repeatable")
    quick.add_argument("--constraint", action="append", help="one constraint; repeatable")
    quick.add_argument("--risk", action="append", help="one risk; repeatable")
    quick.add_argument("--bypass", choices=sorted(BYPASSES), default="none")
    quick.add_argument("--question-version", default="quick-1")
    quick.add_argument("--policy-version", default=None,
                       help=f"cohort name; default {DEFAULT_POLICY_VERSION}, or {DELEGATED_POLICY_VERSION} when 'harness jev' is on")
    label = parser.add_argument_group("flag-form outcome (with --label)")
    label.add_argument("--outcome", choices=sorted(OUTCOMES))
    label.add_argument("--action-taken", help="what actually happened")
    label.add_argument("--evidence", help="one sentence of independent support")
    parser.add_argument("--scope", choices=SCOPES, help="with --report or --pending: this target's database or every registered target on the machine (report default machine, pending default target)")
    parser.add_argument("--router", type=Path, default=Path(os.environ.get("HARNESS_TYPESAFE_ROUTER", str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py"))))
    parser.add_argument("--db-root", type=Path, default=Path(os.environ.get("HARNESS_DB_ROOT", str(ROOT / ".harness-db"))))
    args = parser.parse_args(argv)
    try:
        if args.scope and not (args.report or args.pending):
            raise InputError("--scope applies only to --report and --pending")
        if args.report:
            result = report(args.db_root, args.scope or "machine")
        elif args.pending:
            result = pending(args.db_root, scope=args.scope or "target")
        else:
            router = load_router(args.router)
            if args.record:
                result = record_outcome(args.record, router, args.db_root)
            elif args.label:
                if not (args.outcome and args.action_taken and args.evidence):
                    raise InputError("--label requires --outcome, --action-taken, and --evidence")
                result = label_outcome({"call_id": args.label, "outcome": args.outcome, "action_taken": args.action_taken,
                                        "evidence": args.evidence}, router, args.db_root)
            elif args.family:
                result = advise(validate_context(quick_context(args), router), router, args.db_root)
            else:
                result = advise(load_context(args.context, router), router, args.db_root)
    except (InputError, OSError, ValueError, TypeError) as error:
        parser.error(str(error))
    print(json.dumps(result, sort_keys=True, allow_nan=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
