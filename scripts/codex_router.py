#!/usr/bin/env python3
"""Decision router that answers harness questions with `codex exec` on the signed-in Codex plan.

A drop-in for the TypeSafe router (Jev): point HARNESS_TYPESAFE_ROUTER at this
file and set TYPESAFE_MODEL=codex:<model>. Redaction and request logging come
from the TypeSafe router so both providers apply the same input rules. Each
answer uses the Codex plan's usage allowance instead of a TypeSafe API key.
"""
from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

MODEL_PREFIX = "codex:"
DEFAULT_MODEL = MODEL_PREFIX + "gpt-6.1-sol"
DEFAULT_EFFORT = "low"
# codex exec takes 5 to 15 seconds; HARNESS_JEV_TIMEOUT still overrides this.
DEFAULT_TIMEOUT = 60.0
API_URL = "codex-exec:local"
CHILD_ENV = "HARNESS_CODEX_DECISION_CHILD"
# Keep the login (CODEX_HOME) but drop anything that ties the child to a parent session or run.
DROPPED_ENV_PREFIXES = ("HARNESS_", "CLAUDE")
DROPPED_ENV = {"CLAUDECODE", "CODEX_THREAD_ID", "CODEX_SESSION_ID", "CODEX_CI", "CODEX_SANDBOX",
               "CODEX_SANDBOX_NETWORK_DISABLED"}

PROMPT = """You are a decision service. Answer every question in `questions` using only `state`.
Do not run commands, read files, or ask for more input.
- choice: pick exactly one criterion key.
- score: pick the integer index of the matching criterion (0 is the first).
- noul: give the probability from 0 to 1 that the statement is true.
- confidence: your probability from 0 to 1 that the answer is right.
Reply with the JSON object the output schema requires and nothing else.

"""


def _load_typesafe() -> Any:
    path = Path(os.environ.get("HARNESS_TYPESAFE_BASE_ROUTER",
                               str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py")))
    spec = importlib.util.spec_from_file_location("harness_codex_router_base", path)
    if spec is None or spec.loader is None or not path.is_file():
        raise ImportError(f"TypeSafe router missing (needed for redaction and logs): {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


_base = _load_typesafe()
Counter = _base.Counter
redact = _base.redact
append_jsonl = _base.append_jsonl
requests_log_path = _base.requests_log_path


class BadInputError(Exception):
    """The request cannot succeed as sent; retrying will not help."""


class UnavailableError(Exception):
    """codex exec failed, timed out, or returned no usable answer."""


def api_url() -> str:
    return API_URL


def pinned_model() -> str:
    value = os.environ.get("TYPESAFE_MODEL", "").strip()
    return value if value.startswith(MODEL_PREFIX) else DEFAULT_MODEL


def resolve_api_key() -> tuple[str, str]:
    # codex exec uses its own login; there is no key to pass.
    return "", "codex-login"


def answer_schema(questions: dict[str, Any]) -> dict[str, Any]:
    properties = {}
    for key, question in questions.items():
        kind = question.get("type")
        if kind == "choice":
            fields = {"choice": {"type": "string", "enum": list(question["criteria"])}}
        elif kind == "score":
            fields = {"score": {"type": "integer", "enum": list(range(len(question["criteria"])))}}
        elif kind == "noul":
            fields = {"noul": {"type": "number"}}
        else:
            raise BadInputError(f"unsupported question type: {kind}")
        if kind != "noul":
            fields["confidence"] = {"type": "number"}
        properties[key] = {"type": "object", "additionalProperties": False,
                           "required": list(fields), "properties": fields}
    return {"type": "object", "additionalProperties": False, "required": ["answers"],
            "properties": {"answers": {"type": "object", "additionalProperties": False,
                                       "required": list(properties), "properties": properties}}}


def child_env() -> dict[str, str]:
    env = {k: v for k, v in os.environ.items()
           if k not in DROPPED_ENV and not k.startswith(DROPPED_ENV_PREFIXES)}
    env.update({"HARNESS_AUTO_INIT": "0", CHILD_ENV: "1"})
    return env


def parse_events(stdout: str) -> tuple[dict[str, Any], dict[str, int] | None]:
    message, usage = None, None
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(event, dict):
            continue
        item = event.get("item")
        if event.get("type") == "item.completed" and isinstance(item, dict) and item.get("type") == "agent_message":
            message = item.get("text")
        elif event.get("type") == "turn.completed" and isinstance(event.get("usage"), dict):
            raw = event["usage"]
            usage = {k: raw[k] for k in ("input_tokens", "output_tokens") if type(raw.get(k)) is int}
    if not isinstance(message, str):
        raise UnavailableError("codex exec returned no answer")
    try:
        result = json.loads(message)
    except json.JSONDecodeError as error:
        raise UnavailableError(f"codex exec answer is not JSON: {error}") from error
    if not isinstance(result, dict) or not isinstance(result.get("answers"), dict):
        raise UnavailableError("unexpected response shape (no `answers` object)")
    return result, usage


def post_json(url: str, payload: dict, api_key: str, timeout: float, attempts: int = 1,
              runner: Any = subprocess.run) -> tuple[dict, dict]:
    """Answer one TypeSafe-shaped request with codex exec; the signature mirrors the TypeSafe router."""
    if os.environ.get(CHILD_ENV):
        raise BadInputError("refusing a nested Codex decision call")
    model = payload.get("model")
    if not isinstance(model, str) or not model.startswith(MODEL_PREFIX) or model == MODEL_PREFIX:
        raise BadInputError(f"the Codex router needs TYPESAFE_MODEL={MODEL_PREFIX}<model>, got {model!r}")
    questions = payload.get("questions")
    if not isinstance(questions, dict) or not questions:
        raise BadInputError("no questions")
    prompt = PROMPT + json.dumps({"state": payload.get("state"), "questions": questions}, indent=1)
    effort = os.environ.get("HARNESS_CODEX_DECISION_EFFORT", DEFAULT_EFFORT)
    last_error = "no attempt was made"
    with tempfile.TemporaryDirectory(prefix="harness-codex-decision-") as scratch:
        schema = Path(scratch) / "schema.json"
        schema.write_text(json.dumps(answer_schema(questions)))
        workdir = Path(scratch) / "work"
        workdir.mkdir()
        argv = [os.environ.get("HARNESS_CODEX_BIN", "codex"), "exec", "--ephemeral", "--skip-git-repo-check",
                "--ignore-user-config", "--ignore-rules", "-s", "read-only", "-C", str(workdir),
                "-m", model[len(MODEL_PREFIX):], "-c", f"model_reasoning_effort={effort}",
                "--output-schema", str(schema), "--json", "-"]
        for attempt in range(1, max(1, attempts) + 1):
            started = time.monotonic()
            try:
                done = runner(argv, input=prompt, capture_output=True, text=True, timeout=timeout,
                              env=child_env(), cwd=workdir)
            except subprocess.TimeoutExpired:
                last_error = f"codex exec timed out after {timeout:g}s"
                continue
            except OSError as error:
                raise UnavailableError(f"cannot run codex: {type(error).__name__}") from error
            if done.returncode != 0:
                last_error = f"codex exec exited {done.returncode}"
                continue
            result, usage = parse_events(done.stdout)
            meta = {"http_status": None, "latency_ms": int((time.monotonic() - started) * 1000), "attempts": attempt}
            return {"answers": result["answers"], "model": model, "usage": usage}, meta
    raise UnavailableError(f"Codex decision failed after {max(1, attempts)} attempt(s): {last_error}")
