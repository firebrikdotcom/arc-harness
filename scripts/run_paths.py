"""Select session-owned run pointers and check records without adopting legacy runs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


def run_owner(sid: str) -> str:
    """A Codex subagent works inside its root thread's run; its rollout names that root."""
    if not UUID.fullmatch(sid):
        return sid
    home = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
    for rollout in (home / "sessions").glob(f"*/*/*/rollout-*-{sid}.jsonl"):
        try:
            with rollout.open() as handle:
                meta = json.loads(handle.readline(1_000_000)).get("payload", {})
        except (OSError, ValueError, AttributeError):
            return sid
        root = meta.get("session_id")
        if meta.get("id") == sid and meta.get("thread_source") == "subagent" and isinstance(root, str) and UUID.fullmatch(root):
            return root
        return sid
    return sid


def session_id(value: str | None = None) -> str | None:
    result = (value or os.environ.get("HARNESS_SESSION_ID") or os.environ.get("CODEX_THREAD_ID")
              or os.environ.get("CLAUDE_SESSION_ID") or os.environ.get("CLAUDE_CODE_SESSION_ID"))
    if result and (not isinstance(result, str) or len(result.encode()) > 256 or not result.strip() or "\n" in result or "\r" in result):
        raise ValueError("invalid session ID")
    return result


def session_key(value: str | None = None) -> str | None:
    # Runs and check records belong to the root session; todo and audit state stay per agent (session_id).
    sid = session_id(value)
    return hashlib.sha256(run_owner(sid).encode()).hexdigest() if sid else None


def current_file(db_root: Path, value: str | None = None) -> Path:
    key = session_key(value)
    return db_root / "runs" / "sessions" / key / "current" if key else db_root / "runs" / "current"


def records_dir(db_root: Path, value: str | None = None) -> Path:
    key = session_key(value)
    return db_root / "records" / "sessions" / key if key else db_root / "records"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", choices=("current", "records", "key"))
    parser.add_argument("db_root", type=Path)
    parser.add_argument("--session-id")
    args = parser.parse_args()
    try:
        print({"current": current_file, "records": records_dir, "key": lambda root, sid: session_key(sid) or ""}[args.kind](args.db_root, args.session_id))
    except ValueError as error:
        parser.error(str(error))
