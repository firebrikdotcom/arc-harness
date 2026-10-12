#!/usr/bin/env python3
"""PostToolUse / PostToolUseFailure hook: stop a run that keeps failing the same way.

For every shell call it reports the outcome to `harness failure` in the payload's
session and project. A failure is keyed by the command and signed by its
normalized error (numbers, hashes, and temporary paths removed), so the same
command failing with the same error twice in a row pauses the run; a different
error restarts the count and a success clears it. Neither the command nor the
error text is stored, only digests and a short first line for the pause notice.
Exits 2 with the pause notice on stderr when the run pauses, else 0.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

from run_paths import session_id

SHELL_TOOLS = {"Bash", "shell", "exec_command", "local_shell", "container.exec"}
VOLATILE = (
    (re.compile(r"/tmp/[^\s:'\"]+"), "/tmp/X"),
    (re.compile(r"\b[0-9a-f]{7,64}\b"), "H"),
    (re.compile(r"\d+(\.\d+)?"), "N"),
    (re.compile(r"[ \t]+"), " "),
)


def normalize(text: str) -> str:
    for pattern, replacement in VOLATILE:
        text = pattern.sub(replacement, text)
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return "\n".join(lines[-20:])


def digest(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def outcome(payload: dict) -> tuple[bool, str]:
    """(failed, error text) from either runtime's post-tool payload."""
    response = payload.get("tool_response")
    response = response if isinstance(response, dict) else {}
    code = response.get("exit_code", response.get("exitCode"))
    failed = (payload.get("hook_event_name") == "PostToolUseFailure" or response.get("is_error") is True
              or response.get("isError") is True or (type(code) is int and code != 0))
    parts = [payload.get("error"), response.get("stderr"), response.get("output"), response.get("stdout")]
    return failed, "\n".join(part for part in parts if isinstance(part, str) and part)


def main() -> int:
    raw = sys.stdin.read(1_000_001)
    if len(raw) > 1_000_000:
        return 0
    payload = json.loads(raw or "{}")
    if payload.get("tool_name") not in SHELL_TOOLS:
        return 0
    tool_input = payload.get("tool_input") if isinstance(payload.get("tool_input"), dict) else {}
    command = tool_input.get("command", tool_input.get("cmd", ""))
    command = " ".join(map(str, command)) if isinstance(command, list) else str(command)
    if not command.strip() or re.search(r"(^|[\s/])harness\s+(continue|status|step|workflow|failure|brief)\b", command):
        return 0
    cwd = payload.get("cwd") or os.getcwd()
    if not os.path.isdir(cwd):
        return 0
    failed, error = outcome(payload)
    key = digest(" ".join(command.split()))
    args = ["failure", "clear", "--command-key", key]
    if failed:
        first = next((line.strip() for line in error.splitlines() if line.strip()), "no error text")
        args = ["failure", "record", "--command-key", key, "--signature", digest(normalize(error)),
                "--detail", first[:200]]
    environment = dict(os.environ)
    sid = session_id(payload.get("session_id"))
    if sid:
        environment["HARNESS_SESSION_ID"] = sid
    harness = Path(__file__).resolve().parent / "harness"
    result = subprocess.run([str(harness), *args], cwd=cwd, env=environment, capture_output=True, text=True, timeout=30)
    if result.returncode == 3:
        # Exit 2 hands the pause notice to the agent with the failed call's result.
        print(result.stdout.strip(), file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, TypeError, subprocess.SubprocessError):
        raise SystemExit(0)
