#!/usr/bin/env python3
"""Bounded hook stdin shared by the policy, audit and failure-budget hooks.

Prompts and tool inputs stay under LIMIT: anything larger is refused. A post-tool
payload carries whatever the tool returned (a generated image arrives as 2-3 MB of
base64), and the hooks only read its status fields, so an oversized result is
reduced to those fields plus the tail of its text output instead of blocking a
call that has already happened.
"""
from __future__ import annotations

import json
import sys

LIMIT = 1_000_000
POST_LIMIT = 64_000_000
POST_EVENTS = {"PostToolUse", "PostToolUseFailure"}
STATUS_KEYS = ("exit_code", "exitCode", "isError", "is_error", "session_id", "duration_ms")
TEXT_KEYS = ("stdout", "stderr", "output")
TEXT_TAIL = 64_000


def slim(payload: dict, size: int) -> dict:
    """The payload without its tool result body; error text keeps its last TEXT_TAIL characters."""
    response = payload.get("tool_response")
    response = response if isinstance(response, dict) else {}
    kept = {key: response[key] for key in STATUS_KEYS
            if type(response.get(key)) in (bool, int) or (isinstance(response.get(key), str) and len(response[key]) <= 256)}
    for key in TEXT_KEYS:
        if isinstance(response.get(key), str):
            kept[key] = response[key][-TEXT_TAIL:]
    kept["omitted_bytes"] = size
    result = {**payload, "tool_response": kept}
    if len(json.dumps(result)) > LIMIT:
        result["tool_input"] = {}
    return result


def read(stream=None) -> str:
    """Hook stdin as JSON text of at most LIMIT characters; ValueError when it cannot be bounded."""
    raw = (stream or sys.stdin).read(POST_LIMIT + 1)
    if len(raw) <= LIMIT:
        return raw
    if len(raw) <= POST_LIMIT:
        try:
            payload = json.loads(raw)
        except ValueError:
            payload = None
        if isinstance(payload, dict) and payload.get("hook_event_name") in POST_EVENTS:
            text = json.dumps(slim(payload, len(raw)))
            if len(text) <= LIMIT:
                return text
    raise ValueError("Hook payload exceeds policy limit")
