#!/usr/bin/env python3
"""Observe repeated command digests within the exact native session's run."""

import json
import os
from pathlib import Path
import subprocess
import sys

from run_paths import current_file, session_id


def main() -> int:
    raw = sys.stdin.read(1_000_001)
    if os.environ.get("HARNESS_JEV_CHECKPOINTS", "0") != "1" or len(raw) > 1_000_000:
        return 0
    payload = json.loads(raw or "{}")
    if payload.get("tool_name") not in ("Bash", "exec_command", "functions.exec_command"):
        return 0
    cwd = Path(payload.get("cwd") or ".").resolve()
    tool_input = payload.get("tool_input") or {}
    command = str(tool_input.get("command", tool_input.get("cmd", ""))).strip()
    if not cwd.is_dir() or not command or "harness " in command:
        return 0
    root = Path(__file__).resolve().parent.parent
    environment = dict(os.environ)
    sid = session_id(payload.get("session_id"))
    if sid:
        environment["HARNESS_SESSION_ID"] = sid
    db = Path(environment.get("HARNESS_DB_ROOT", root / ".harness-db"))
    lookup = subprocess.run([str(root / "scripts/harness-target.sh"), "lookup", str(cwd)],
                            capture_output=True, text=True, env=environment)
    project = root
    if lookup.returncode == 0:
        target = Path(lookup.stdout.strip())
        db = target / "db"
        values = dict(line.split("=", 1) for line in (target / "target.state").read_text().splitlines() if "=" in line)
        project = Path(values["TARGET_ROOT"])
    pointer = current_file(db, sid)
    if not pointer.is_file():
        return 0
    run = db / "runs" / pointer.read_text().strip()
    state = dict(line.split("=", 1) for line in (run / "state").read_text().splitlines() if "=" in line)
    if state.get("RUN_STATUS") != "active":
        return 0
    digest = subprocess.run(["cksum"], input=command, text=True, capture_output=True, check=True).stdout.split()[0]
    history = run / "command-digests"
    with history.open("a") as output:
        output.write(digest + "\n")
    repeats = history.read_text().splitlines().count(digest)
    if repeats == int(environment.get("HARNESS_JEV_REPEAT_THRESHOLD", "3")):
        subprocess.run([sys.executable, str(root / "scripts/phase_checkpoint.py"), "tool-repeat",
                        "--repeats", str(repeats), "--project", str(project), "--db-root", str(db)],
                       env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, TypeError, AttributeError, subprocess.SubprocessError):
        raise SystemExit(0)
