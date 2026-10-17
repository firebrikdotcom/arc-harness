"""Offline stand-in for the TypeSafe router used by harness tests.

Importable like the real skill (redact, post_json, pinned_model, ...).
Behaviour is driven by environment variables: FAKE_ROUTER_CHOICE (choice key),
FAKE_ROUTER_CONFIDENCE (choice confidence, default 0.9), FAKE_ROUTER_BOOL
(Boolean probability), FAKE_ROUTER_FAIL=1 (transport failure),
FAKE_ROUTER_LOG_DIR (JSONL logs), FAKE_STATE_PATH (where to record the request).
"""

from __future__ import annotations

import json
import os
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
    if os.environ.get("FAKE_STATE_PATH"):
        Path(os.environ["FAKE_STATE_PATH"]).write_text(json.dumps(payload))
    answers = {}
    for name, question in payload["questions"].items():
        kind = question["type"]
        if kind == "choice":
            keys = list(question["criteria"])
            wanted = os.environ.get("FAKE_ROUTER_CHOICE")
            choice = wanted if wanted in keys else keys[0]
            answers[name] = {"type": "choice", "choice": choice,
                             "confidence": float(os.environ.get("FAKE_ROUTER_CONFIDENCE", "0.9"))}
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
