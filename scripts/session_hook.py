#!/usr/bin/env python3
"""Bind a native lifecycle payload before invoking an existing shell hook."""

import os
from pathlib import Path
import shlex
import subprocess
import sys
import json

from run_paths import session_id


def main() -> int:
    raw = sys.stdin.read(1_000_001)
    if len(raw) > 1_000_000:
        return 0
    payload = json.loads(raw or "{}")
    sid = session_id(payload.get("session_id"))
    environment = dict(os.environ)
    if sid:
        environment["HARNESS_SESSION_ID"] = sid
        env_file = environment.get("CLAUDE_ENV_FILE")
        if env_file and payload.get("hook_event_name") == "SessionStart":
            with open(env_file, "a") as output:
                output.write("\nexport HARNESS_SESSION_ID=" + shlex.quote(sid) + "\n")
    script = Path(sys.argv[1]).resolve()
    allowed = Path(__file__).resolve().parent / "hooks" / "auto-init.sh"
    if script != allowed:
        raise ValueError("unsupported native session hook")
    return subprocess.run(["sh", str(script)], input=raw, text=True, env=environment).returncode


if __name__ == "__main__":
    raise SystemExit(main())
