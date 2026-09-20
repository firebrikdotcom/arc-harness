#!/usr/bin/env python3
"""Ask TypeSafe for a context-specific, advisory choice during agent work.

This command never executes its recommendation. Permissions, destructive work,
known failures, and required checks stay deterministic harness gates.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import re
import sys
import tempfile
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
MAX_BYTES = 32_000
MAX_OPTIONS = 8
OPTION_ID = re.compile(r"^[a-z][a-z0-9_-]{0,63}$")
CONTEXT_KEYS = {"goal", "facts", "constraints", "risks"}


class InputError(ValueError):
    pass


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


def load_context(path: Path, router: Any) -> dict[str, Any]:
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


def advise(context: dict[str, Any], router: Any, db_root: Path) -> dict[str, Any]:
    request_id = str(uuid.uuid4())
    started = datetime.now(timezone.utc)
    payload = {"state": context, "model": router.pinned_model(), "questions": questions(context)}
    record: dict[str, Any] = {"id": request_id, "at": started.isoformat(), "kind": "dynamic_advice", "context": context}
    try:
        response, meta = router.post_json(router.api_url(), payload, router.resolve_api_key()[0], router.DEFAULT_TIMEOUT)
        answer = response.get("answers", {}).get("recommendation", {})
        choice = answer.get("choice")
        allowed = {option["id"] for option in context["options"]}
        if choice not in allowed:
            raise InputError("TypeSafe returned a choice outside the supplied options")
        confidence = answer.get("confidence")
        if not isinstance(confidence, (int, float)) or isinstance(confidence, bool) or not 0 <= confidence <= 1:
            raise InputError("TypeSafe returned an invalid confidence")
        record.update({"choice": choice, "confidence": confidence, "call_id": request_id, "latency_ms": meta["latency_ms"], "http_status": meta["http_status"], "answers": response["answers"], "exit_code": 0})
        router.append_jsonl(router.requests_log_path(started), {"call_id": request_id, "kind": "dynamic_advice", "state": context, "questions": payload["questions"], "answers": response["answers"], "exit_code": 0, "latency_ms": meta["latency_ms"], "ts": started.isoformat()})
    except Exception as error:
        record.update({"error": str(error), "exit_code": 1})
        raise
    finally:
        record_path = write_record(record, db_root)
    return {"advisory": True, "choice": choice, "confidence": confidence, "call_id": request_id, "latency_ms": meta["latency_ms"], "record_path": str(record_path)}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True, type=Path, help="curated decision context JSON")
    parser.add_argument("--router", type=Path, default=Path(os.environ.get("HARNESS_TYPESAFE_ROUTER", str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py"))))
    parser.add_argument("--db-root", type=Path, default=Path(os.environ.get("HARNESS_DB_ROOT", str(ROOT / ".harness-db"))))
    args = parser.parse_args(argv)
    try:
        router = load_router(args.router)
        context = load_context(args.context, router)
        result = advise(context, router, args.db_root)
    except (InputError, OSError, ValueError) as error:
        parser.error(str(error))
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
