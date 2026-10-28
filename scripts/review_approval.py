#!/usr/bin/env python3
"""Issue one-time review receipts from native user-prompt events."""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

from run_paths import records_dir

ROOT = Path(__file__).resolve().parent.parent
RECEIPT_NAME = "review-approval.json"
RECEIPT_TTL_SECONDS = 1800
RUN_ID_PATTERN = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
# A session holds at most one pending request, so a bare "approve" or
# "approved" is unambiguous; a supplied run ID must still match it.
APPROVAL_PATTERN = re.compile(
    r"(?:approved?|approve\s+review(?:\s+([A-Za-z0-9][A-Za-z0-9._-]{0,127}))?)[.!]?\Z",
    re.IGNORECASE,
)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def timestamp_epoch(value) -> float | None:
    if isinstance(value, (int, float)):
        return float(value)
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=timezone.utc)
        return parsed.timestamp()
    except ValueError:
        try:
            return float(value)
        except ValueError:
            return None


def physical_project(path: str | Path) -> Path:
    project = Path(path).resolve()
    if not project.is_dir():
        raise ValueError("review project directory is missing")
    result = subprocess.run(
        ["git", "-C", str(project), "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0 and result.stdout.strip():
        return Path(result.stdout.strip()).resolve()
    return project


def target_database(harness_root: Path, base_db: Path, project: str | Path) -> tuple[Path, Path]:
    """Resolve the same registered target database used by scripts/harness."""
    harness_root = harness_root.resolve()
    base_db = base_db.resolve()
    target = physical_project(project)
    registry = harness_root / "scripts" / "harness-target.sh"
    if registry.is_file():
        env = os.environ.copy()
        env["HARNESS_DB_ROOT"] = str(base_db)
        result = subprocess.run(
            ["sh", str(registry), "lookup", str(target)],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        if result.returncode == 0 and result.stdout.strip():
            target_dir = Path(result.stdout.strip()).resolve()
            state = target_dir / "target.state"
            if state.is_file():
                values = {}
                for line in state.read_text(encoding="utf-8").splitlines():
                    key, separator, value = line.partition("=")
                    if separator:
                        values[key] = value
                recorded = values.get("TARGET_ROOT")
                if recorded:
                    target = Path(recorded).resolve()
                    return target_dir / "db", target
    return base_db, target


def receipt_path(db_root: Path, session_id: str) -> Path:
    if not session_id or len(session_id.encode()) > 256 or "\n" in session_id or "\r" in session_id:
        raise ValueError("native session ID is missing or invalid")
    return records_dir(db_root, session_id) / RECEIPT_NAME


def tree_hash(project: Path, harness_root: Path) -> str:
    command = harness_root / "scripts" / "tree-hash.sh"
    result = subprocess.run(
        ["sh", str(command), str(project)],
        capture_output=True,
        text=True,
        check=False,
    )
    value = result.stdout.strip()
    if result.returncode != 0 or not re.fullmatch(r"(?:[0-9a-f]{40}|files-[0-9a-f]{64})", value):
        raise ValueError("could not compute the current project tree hash")
    return value


def read_state(path: Path) -> dict | None:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) and value.get("schema_version") == 1 else None


def write_state(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    try:
        path.parent.chmod(0o700)
    except OSError:
        pass
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def request_or_check(
    *, db_root: Path, session_id: str, run_id: str, findings: Path,
    project: str | Path, harness_root: Path = ROOT, now: float | None = None,
) -> tuple[bool, str]:
    if not RUN_ID_PATTERN.fullmatch(run_id):
        raise ValueError("run ID is invalid")
    findings = findings.resolve()
    project_root = physical_project(project)
    payload = json.loads(findings.read_text(encoding="utf-8"))
    expected_tree = payload.get("tree_hash") if isinstance(payload, dict) else None
    current_tree = tree_hash(project_root, harness_root.resolve())
    if expected_tree != current_tree:
        raise ValueError("review findings do not match the current project tree")
    key = {
        "run_id": run_id,
        "session_id": session_id,
        "project": str(project_root),
        "findings_path": str(findings),
        "findings_sha256": sha256_file(findings),
        "tree_hash": current_tree,
    }
    path = receipt_path(db_root, session_id)
    current_time = now if now is not None else time.time()
    old = read_state(path)
    same_request = old is not None and all(old.get(name) == value for name, value in key.items())
    fresh = same_request and current_time - float(old.get("requested_at", 0)) <= RECEIPT_TTL_SECONDS
    approved_at = timestamp_epoch(old.get("approved_at")) if old else None
    if (fresh and old.get("status") == "approved" and approved_at is not None
            and approved_at >= float(old.get("requested_at", 0))
            and old.get("approval_source") == "native-user-prompt-hook"
            and re.fullmatch(r"[0-9a-f]{64}", str(old.get("approval_message_sha256", "")))):
        return True, "Native chat approval receipt verified."
    if not (fresh and old.get("status") == "pending"):
        old = {
            "schema_version": 1,
            **key,
            "status": "pending",
            "requested_at": current_time,
        }
        write_state(path, old)
    return False, (
        f"Review approval is pending for run {run_id}; show the independent findings to the user, "
        f"ask them to reply `approved` (or `approve review {run_id}`) in this chat, then retry `harness review submit {findings}`."
    )


def capture_prompt(
    *, harness_root: Path, base_db: Path, project: str | Path,
    session_id: str, prompt: str, prompt_at=None,
) -> bool:
    """Record approval only when called with a native prompt hook's user text."""
    match = APPROVAL_PATTERN.fullmatch(prompt.strip()) if isinstance(prompt, str) else None
    if not match:
        return False
    db_root, project_root = target_database(harness_root, base_db, project)
    path = receipt_path(db_root, session_id)
    request = read_state(path)
    if not request or request.get("status") != "pending":
        return False
    approval_time = time.time() if prompt_at is None else timestamp_epoch(prompt_at)
    if approval_time is None or approval_time < float(request.get("requested_at", 0)):
        return False
    if match.group(1) and request.get("run_id") != match.group(1):
        return False
    if request.get("project") != str(project_root):
        return False
    if time.time() - float(request.get("requested_at", 0)) > RECEIPT_TTL_SECONDS:
        return False
    try:
        if sha256_file(Path(request["findings_path"])) != request.get("findings_sha256"):
            return False
        if tree_hash(project_root, harness_root.resolve()) != request.get("tree_hash"):
            return False
    except (OSError, ValueError, KeyError):
        return False
    request.update({
        "status": "approved",
        "approved_at": approval_time,
        "approval_message_sha256": hashlib.sha256(prompt.strip().encode("utf-8")).hexdigest(),
        "approval_source": "native-user-prompt-hook",
    })
    write_state(path, request)
    return True


def consume(
    *, db_root: Path, session_id: str, run_id: str, findings: Path,
    project: str | Path, harness_root: Path = ROOT,
) -> tuple[bool, str]:
    findings = findings.resolve()
    project_root = physical_project(project)
    path = receipt_path(db_root, session_id)
    state = read_state(path)
    if not state or state.get("status") != "approved":
        return False, "No native chat approval receipt is available for this submission."
    expected = {
        "run_id": run_id,
        "session_id": session_id,
        "project": str(project_root),
        "findings_path": str(findings),
        "findings_sha256": sha256_file(findings),
        "tree_hash": tree_hash(project_root, harness_root.resolve()),
    }
    if any(state.get(name) != value for name, value in expected.items()):
        return False, "The native chat approval receipt does not match this run, findings file, or project tree."
    now = time.time()
    approved_at = timestamp_epoch(state.get("approved_at"))
    if (approved_at is None or approved_at < float(state.get("requested_at", 0))
            or approved_at > now + 5
            or state.get("approval_source") != "native-user-prompt-hook"
            or not re.fullmatch(r"[0-9a-f]{64}", str(state.get("approval_message_sha256", "")))):
        return False, "The native chat approval receipt has no valid native prompt event."
    if now - float(state.get("requested_at", 0)) > RECEIPT_TTL_SECONDS:
        return False, "The native chat approval receipt expired; request approval again."
    state["status"] = "consumed"
    state["consumed_at"] = now
    write_state(path, state)
    return True, "Native chat approval receipt consumed."


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    for command in ("request", "consume"):
        sub = subparsers.add_parser(command)
        sub.add_argument("--db-root", type=Path, required=True)
        sub.add_argument("--session-id", required=True)
        sub.add_argument("--run-id", required=True)
        sub.add_argument("--findings", type=Path, required=True)
        sub.add_argument("--project", required=True)
        sub.add_argument("--harness-root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        command = args.command
        values = vars(args)
        values.pop("command")
        approved, message = (request_or_check if command == "request" else consume)(**values)
        print(message)
        return 0 if approved else 4
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        print(f"Review approval unavailable: {error}", file=sys.stderr)
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
