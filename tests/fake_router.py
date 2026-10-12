"""Offline stand-in for the TypeSafe router used by harness tests.

Importable like the real skill (redact, post_json, pinned_model, ...) and
runnable as the task-entry CLI. Behaviour is driven by environment variables:
FAKE_ROUTER_CHOICE (choice key), FAKE_ROUTER_BOOL (Boolean probability),
FAKE_ROUTER_FAIL=1 (transport failure), FAKE_ROUTER_LOG_DIR (JSONL logs).
"""

from __future__ import annotations

import json
import os
import sys
import tempfile
from collections import Counter  # noqa: F401 - part of the router interface
from datetime import datetime
from pathlib import Path

DEFAULT_TIMEOUT = 1.0


def redact(value, counts=None):
    counts = counts if counts is not None else Counter()

    def visit(item):
        if isinstance(item, dict):
            for key, child in item.items():
                if any(part in str(key).lower() for part in ("secret", "password", "token", "api_key")):
                    counts["key_name"] += 1
                visit(child)
        elif isinstance(item, list):
            for child in item:
                visit(child)
        elif isinstance(item, str) and "forbidden-token" in item:
            counts["token"] += 1

    visit(value)
    return value


def pinned_model() -> str:
    return os.environ.get("FAKE_MODEL", "fixture")


def api_url() -> str:
    return "https://fixture.invalid"


def resolve_api_key():
    return "fixture", "fixture"


def post_json(url, payload, key, timeout, **_options):
    if os.environ.get("FAKE_ROUTER_FAIL") == "1":
        raise ConnectionError("fixture transport failure")
    answers = {}
    for name, question in payload["questions"].items():
        kind = question["type"]
        if kind == "choice":
            keys = list(question["criteria"])
            wanted = os.environ.get("FAKE_ROUTER_CHOICE")
            choice = wanted if wanted in keys else keys[0]
            answers[name] = {"type": "choice", "choice": choice, "confidence": 0.9}
        elif kind == "score":
            answers[name] = {"type": "score", "score": 1, "confidence": 0.8}
        else:
            answers[name] = {"type": "noul", "noul": float(os.environ.get("FAKE_ROUTER_BOOL", "0.8"))}
    return {"answers": answers, "model": pinned_model(), "usage": {"input_tokens": 20, "output_tokens": 4}}, {"latency_ms": 3}


def requests_log_path(started: datetime) -> Path:
    directory = Path(os.environ.get("FAKE_ROUTER_LOG_DIR", tempfile.gettempdir()))
    return directory / f"fake-router-{started:%Y-%m-%d}.jsonl"


def append_jsonl(path: Path, record: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(record, sort_keys=True, default=str) + "\n")


def main() -> int:
    state = json.load(sys.stdin)
    if os.environ.get("FAKE_STATE_PATH"):
        Path(os.environ["FAKE_STATE_PATH"]).write_text(json.dumps({"state": state, "args": sys.argv[1:]}))
    observed = os.environ.get("FAKE_RECOMMENDATION", "proceed")
    recommendation = "reasoning_model" if "--shadow" in sys.argv else observed
    print(json.dumps({"call_id": "fixture-call", "policy": {"recommendation": recommendation, "observed_recommendation": observed,
                      "reason": "fixture"}, "model": {"requested": pinned_model(), "returned": pinned_model(), "drift": False},
                      "usage": {"input_tokens": 12, "output_tokens": 2}, "latency_ms": 5}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
