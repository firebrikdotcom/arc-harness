#!/usr/bin/env python3
"""Jev at harness seams: the session-start route and the verify history.

Session start asks Jev for the task-entry route over enum-only signals derived
from git and run history; below the configured confidence the main agent's
normal path decides. The phase gates, verify, review, and the tool hooks still
call this script, but their events are deterministic gates, so they neither ask
Jev nor label outcomes; verify-result only appends the private verify history.
The `pilot` event prints the legacy outcome-label report for compatibility.

Nothing here executes, authorizes, waives, or reorders any harness gate. When
HARNESS_JEV_CHECKPOINTS is not "1", every command is a no-op.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent
sys.path.insert(0, str(SCRIPTS))

import context_advice as advice  # noqa: E402
import task_route  # noqa: E402
from run_paths import current_file, records_dir  # noqa: E402

ENABLE_ENV = "HARNESS_JEV_CHECKPOINTS"
DEFAULT_TIMEOUT_SECONDS = "10"
LANGUAGE_BY_EXTENSION = {
    "py": "python", "sh": "shell", "bash": "shell", "zsh": "shell",
    "ts": "javascript", "tsx": "javascript", "js": "javascript", "jsx": "javascript", "vue": "javascript",
    "svelte": "javascript", "css": "stylesheet", "scss": "stylesheet",
    "php": "php", "go": "go", "rs": "rust", "rb": "ruby", "java": "java", "cs": "csharp",
    "kt": "kotlin", "swift": "swift", "dart": "dart",
    "md": "markdown", "json": "config", "yaml": "config", "yml": "config", "toml": "config",
    "tf": "infrastructure", "sql": "sql", "html": "html",
}
AREA_BY_LANGUAGE = {
    "javascript": "frontend", "stylesheet": "frontend", "html": "frontend",
    "python": "backend", "php": "backend", "go": "backend", "rust": "backend", "ruby": "backend",
    "java": "backend", "csharp": "backend", "sql": "backend",
    "kotlin": "mobile", "swift": "mobile", "dart": "mobile",
    "infrastructure": "infrastructure", "shell": "infrastructure",
    "markdown": "docs",
}
NON_SOURCE_LANGUAGES = {"markdown", "config"}
TEST_DIRECTORIES = {"test", "tests", "spec", "specs", "__tests__", "testing"}
TEST_NAME = re.compile(r"^(test_.*|conftest\.py|.*(_test|\.test|_spec|\.spec)\.[a-z0-9]+)$")
CAMEL_TEST_NAME = re.compile(r"[a-z0-9](Test|Tests|Spec)\.[A-Za-z0-9]+$")
VERIFY_HISTORY = "verify-history.jsonl"
VERIFY_HISTORY_KEEP = 100
UNTRACKED_LINE_LIMIT_BYTES = 1_000_000


class Skip(Exception):
    """The checkpoint does not apply here; say why and exit 0."""


def enabled() -> bool:
    import harness_config
    config = harness_config.load()["typesafe"]
    return config["mode"] != "off" and config["checkpoints"] and os.environ.get(ENABLE_ENV, "0") == "1"


def now() -> str:
    return datetime.now(timezone.utc).isoformat()


def bucket(count: int) -> str:
    if count <= 0:
        return "none"
    if count <= 3:
        return "small"
    if count <= 15:
        return "medium"
    return "large"


def git(project: Path, *arguments: str) -> list[str]:
    try:
        result = subprocess.run(["git", "-C", str(project), *arguments], capture_output=True,
                                text=True, timeout=10, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return []
    if result.returncode:
        return []
    return [line for line in result.stdout.splitlines() if line.strip()]


def git_bytes(project: Path, *arguments: str) -> bytes | None:
    try:
        result = subprocess.run(["git", "-C", str(project), *arguments], capture_output=True, timeout=20, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return None if result.returncode else result.stdout


def language_of(path: str) -> str | None:
    name = path.rsplit("/", 1)[-1]
    extension = name.rsplit(".", 1)[-1].lower() if "." in name else ""
    return LANGUAGE_BY_EXTENSION.get(extension)


def language_counts(paths: list[str]) -> Counter:
    counts: Counter = Counter()
    for path in paths:
        language = language_of(path)
        if language:
            counts[language] += 1
    return counts


def is_test_path(path: str) -> bool:
    """A test file by directory or by name; 'latest.py' or 'contest/' are not tests."""
    parts = path.strip("/").split("/")
    if any(part.lower() in TEST_DIRECTORIES for part in parts[:-1]):
        return True
    name = parts[-1]
    return bool(TEST_NAME.match(name.lower()) or CAMEL_TEST_NAME.search(name))


def is_source_path(path: str) -> bool:
    language = language_of(path)
    return bool(language) and language not in NON_SOURCE_LANGUAGES and not is_test_path(path)


def change_shape(dirty: list[str]) -> str:
    """Whether tests moved together with source, not merely whether a test file changed."""
    source = any(is_source_path(path) for path in dirty)
    tests = any(is_test_path(path) for path in dirty)
    if source and tests:
        return "source_and_tests"
    if source:
        return "source_only"
    if tests:
        return "tests_only"
    return "docs_or_config_only" if dirty else "none"


def line_bucket(lines: int) -> str:
    if lines <= 0:
        return "none"
    if lines <= 50:
        return "small"
    if lines <= 300:
        return "medium"
    if lines <= 1500:
        return "large"
    return "very_large"


def porcelain_path(line: str) -> str:
    path = line[3:]
    if " -> " in path:
        path = path.split(" -> ", 1)[1]
    return path.strip('"')


def changed_lines(project: Path, untracked: list[str]) -> int:
    """Added plus deleted lines against HEAD, plus the lines of untracked files (bounded)."""
    numstat = git(project, "diff", "HEAD", "--numstat") if git(project, "rev-parse", "--verify", "-q", "HEAD") else (
        git(project, "diff", "--numstat") + git(project, "diff", "--cached", "--numstat"))
    total = 0
    for line in numstat:
        added, _, rest = line.partition("\t")
        deleted = rest.partition("\t")[0]
        total += (int(added) if added.isdigit() else 0) + (int(deleted) if deleted.isdigit() else 0)
    for path in untracked:
        file = project / path
        try:
            if file.is_file() and file.stat().st_size <= UNTRACKED_LINE_LIMIT_BYTES:
                total += file.read_bytes().count(b"\n")
        except OSError:
            continue
    return total


def ignored_path(path: str) -> bool:
    return path.startswith((".harness-db/", "harness-db/"))


def git_signals(project: Path) -> dict[str, Any]:
    """Enum-only facts about the working tree; no path or content leaves this function."""
    status = git(project, "status", "--porcelain", "--untracked-files=all")
    entries = [(line[:2], porcelain_path(line)) for line in status if len(line) > 3]
    entries = [(code, path) for code, path in entries if not ignored_path(path)]
    dirty = [path for _, path in entries]
    untracked = [path for code, path in entries if code == "??"]
    tracked = git(project, "ls-files") if project.is_dir() else []
    languages = language_counts(dirty) or language_counts(tracked[:2000])
    lowered = [path.lower() for path in dirty]
    lines = changed_lines(project, untracked) if dirty else 0
    return {
        "is_git": bool(git(project, "rev-parse", "--is-inside-work-tree")),
        "dirty_count": len(dirty),
        "dirty_bucket": bucket(len(dirty)),
        "languages": [name for name, _ in languages.most_common(3)],
        "language_counts": dict(languages),
        "tests_changed": any(is_test_path(path) for path in dirty),
        "change_shape": change_shape(dirty),
        "changed_lines_bucket": line_bucket(lines),
        "docs_changed": any(path.endswith(".md") or path.startswith("docs/") for path in lowered),
        "progress_changed": any(path.endswith("progress.md") for path in lowered),
        "config_changed": any(path.endswith((".json", ".yaml", ".yml", ".toml")) for path in lowered),
        "has_tests_dir": any((project / name).is_dir() for name in ("tests", "test", "spec", "__tests__")),
    }


def tree_fingerprint(project: Path) -> str | None:
    """A local sha256 of HEAD, the tracked diff, and untracked file contents; it never leaves this machine."""
    head = git_bytes(project, "rev-parse", "HEAD") or b"no-head"
    diff = git_bytes(project, "diff", "HEAD", "--binary") if head != b"no-head" else (
        (git_bytes(project, "diff", "--binary") or b"") + (git_bytes(project, "diff", "--cached", "--binary") or b""))
    untracked = git_bytes(project, "ls-files", "--others", "--exclude-standard", "-z")
    if diff is None or untracked is None:
        return None
    digest = hashlib.sha256(head + b"\0" + diff)
    for raw in sorted(name for name in untracked.split(b"\0") if name):
        path = raw.decode("utf-8", "surrogateescape")
        if ignored_path(path):
            continue
        digest.update(b"\0" + raw + b"\0")
        try:
            file = project / path
            stat = file.stat()
            if stat.st_size <= UNTRACKED_LINE_LIMIT_BYTES * 5:
                digest.update(hashlib.sha256(file.read_bytes()).digest())
            else:
                digest.update(f"{stat.st_size}:{stat.st_mtime_ns}".encode())
        except OSError:
            digest.update(b"unreadable")
    return digest.hexdigest()[:32]


def area_for(languages: list[str] | dict[str, int], project: Path) -> str:
    """Majority area by file count; ties and unknown languages stay 'other'."""
    if any((project / name).is_dir() for name in ("android", "ios")):
        return "mobile"
    weights = languages if isinstance(languages, dict) else {name: 1 for name in languages}
    votes: Counter = Counter()
    for language, count in weights.items():
        votes[AREA_BY_LANGUAGE.get(language, "other")] += count
    if not votes:
        return "other"
    area, count = votes.most_common(1)[0]
    if list(votes.values()).count(count) > 1:
        return "other"
    return area


def read_kv(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    if not path.is_file():
        return values
    for line in path.read_text(encoding="utf-8").splitlines():
        key, separator, value = line.partition("=")
        if separator:
            values[key] = value
    return values


def run_state(db_root: Path) -> dict[str, str] | None:
    current = current_file(db_root)
    if not current.is_file():
        return None
    run_id = current.read_text(encoding="utf-8").splitlines()[0].strip() if current.read_text(encoding="utf-8").strip() else ""
    state = read_kv(db_root / "runs" / run_id / "state") if run_id else {}
    return state or None


def last_verify(db_root: Path) -> dict[str, str]:
    return read_kv(records_dir(db_root) / "verify.state")


def verify_history(db_root: Path) -> list[dict[str, Any]]:
    """Oldest-first verification results for this target.

    The history file is written by the verify-result event. The single verify.state record
    is appended when it is newer than the history's last line: databases from before the
    history existed, or verifications that ran with checkpoints disabled.
    """
    path = records_dir(db_root) / VERIFY_HISTORY
    entries: list[dict[str, Any]] = []
    if path.is_file():
        for line in path.read_text(encoding="utf-8").splitlines():
            try:
                entry = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(entry, dict) and isinstance(entry.get("epoch"), int) and isinstance(entry.get("exit"), int):
                entries.append(entry)
    record = last_verify(db_root)
    if record.get("RECORD_EPOCH", "").isdigit() and record.get("EXIT", "").isdigit():
        latest = {"epoch": int(record["RECORD_EPOCH"]), "exit": int(record["EXIT"]), "run_id": None, "tree": None}
        # verify.state is rewritten by every verification, so a newer one means verify.sh ran
        # while checkpoints were off and the history missed it.
        if not entries or latest["epoch"] > entries[-1]["epoch"]:
            entries.append(latest)
    return entries


def append_verify_history(db_root: Path, project: Path, state: dict[str, str], exit_code: int) -> None:
    """Append one private history line; only exit, counts, run id, and a local tree hash are kept."""
    record = last_verify(db_root)
    directory = records_dir(db_root)
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    path = directory / VERIFY_HISTORY

    def number(key: str) -> int | None:
        value = record.get(key, "")
        return int(value) if value.isdigit() else None

    entry = {"v": 1, "epoch": number("RECORD_EPOCH") or int(datetime.now(timezone.utc).timestamp()), "exit": exit_code,
             "failures": number("FAILURES"), "ran": number("RAN"), "run_id": state.get("RUN_ID"),
             "tree": tree_fingerprint(project)}
    lines = path.read_text(encoding="utf-8").splitlines() if path.is_file() else []
    lines = [line for line in lines if line.strip()][-(VERIFY_HISTORY_KEEP - 1):] + [json.dumps(entry, sort_keys=True)]
    fd, temporary = tempfile.mkstemp(prefix=".verify-history-", dir=directory)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        os.fchmod(fd, 0o600)
        stream.write("\n".join(lines) + "\n")
    os.replace(temporary, path)


def pilot_lines(db_root: Path) -> list[str]:
    summary = advice.pilot(db_root)
    lines = [f"pilot {summary['labeled']}/{summary['target']} labeled shadow decisions"
             f" ({summary['correct']} correct, {summary['unlabeled_evaluated']} unlabeled, {summary['fallback']} fallback)."]
    unlabeled = advice.pending(db_root, limit=10)["unlabeled"]
    if unlabeled:
        ids = ", ".join(f"{row['family']}:{row['call_id'][:8]}" for row in unlabeled if row.get("call_id"))
        lines.append(f"unlabeled checkpoints (label with harness advise --label CALL_ID ...): {ids}")
    return lines


def session_metadata(project: Path, db_root: Path, signals: dict[str, Any]) -> dict[str, Any]:
    state = run_state(db_root)
    record = last_verify(db_root)
    failing = bool(state and state.get("RUN_STATUS") == "active" and record and record.get("EXIT") not in {"", "0"})
    return {
        "version": 1, "task_kind": "change", "area": area_for(signals["language_counts"], project),
        "proposed_action": "start_routine_agent", "reversibility": "reversible",
        "uncertainty_reason": "scope_unclear", "diff_size": signals["dirty_bucket"],
        "changed_file_count": min(signals["dirty_count"], 10000), "known_failures": 1 if failing else 0,
        "required_checks_pending": False, "approval_required": False, "user_choice_explicit": False,
    }


def session_start(project: Path, db_root: Path, router: Path) -> str:
    signals = git_signals(project)
    if not signals["is_git"]:
        raise Skip("not a git worktree")
    metadata = session_metadata(project, db_root, signals)
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
        json.dump(metadata, handle)
        path = Path(handle.name)
    try:
        # None: active when `harness jev` is on, shadow otherwise.
        route = task_route.route_task(path, project, db_root, router, None, origin="session_routing")
    finally:
        path.unlink(missing_ok=True)
    if route["source"] == "fallback":
        return (f"Jev session route not followed ({route.get('reason')}; confidence {route.get('confidence')}, "
                f"minimum {route.get('min_confidence')}): the main agent decides.")
    if route.get("routing_mode") == "shadow":
        line = "Jev session route observed in shadow mode, not followed: the main agent decides."
        if route.get("delegation_gate"):
            line += f" Delegation is on but held back: {route['delegation_gate']}."
        return line
    if route["source"] == "typesafe":
        return (f"Jev session route: {route['recommendation']} (confidence {route.get('confidence')}, "
                f"minimum {route.get('min_confidence')}); follow it unless a deterministic rule decides.")
    return f"Jev session route: {route['recommendation']} (source {route['source']}); existing rules decide."


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("event", choices=("plan-done", "build-start", "verify-start", "verify-result", "review-handoff",
                                          "run-complete", "tool-repeat", "session-start", "pilot"))
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument("--db-root", type=Path, default=Path(os.environ.get("HARNESS_DB_ROOT", str(ROOT / ".harness-db"))))
    parser.add_argument("--router", type=Path, default=Path(os.environ.get("HARNESS_TYPESAFE_ROUTER", str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py"))))
    parser.add_argument("--exit", type=int, dest="exit_code", help="verification exit code for verify-result")
    parser.add_argument("--verify-exit", type=int, help="verification exit code observed by review")
    parser.add_argument("--repeats", type=int, help="how many times the same command has been issued")
    parser.add_argument("--force", action="store_true", help=f"run even when {ENABLE_ENV} is not 1")
    args = parser.parse_args(argv)
    if not (enabled() or args.force):
        return 0
    os.environ.setdefault("HARNESS_JEV_TIMEOUT", DEFAULT_TIMEOUT_SECONDS)
    os.environ.setdefault("HARNESS_JEV_ATTEMPTS", "1")
    lines: list[str] = []
    try:
        if args.event == "pilot":
            lines = pilot_lines(args.db_root)
        elif args.event == "session-start":
            lines = [session_start(args.project, args.db_root, args.router)]
        else:
            # Phase events are deterministic gates, not judgments: no provider call and
            # no automatic label. Only verify-result keeps the private history; the
            # others print nothing, so the gates that call them stay quiet.
            if args.event == "verify-result":
                if args.exit_code is None:
                    parser.error("verify-result requires --exit")
                # History is kept for every verification, inside a run or not.
                try:
                    append_verify_history(args.db_root, args.project, run_state(args.db_root) or {}, args.exit_code)
                except OSError as error:
                    lines.append(f"verify history not written: {type(error).__name__}")
    except Skip as reason:
        lines = [f"checkpoint skipped: {reason}"]
    except (advice.InputError, task_route.InputError, OSError, ValueError) as error:
        # Checkpoints are shadow observations; a broken checkpoint never fails the caller.
        lines = [f"checkpoint skipped: {type(error).__name__}: {str(error)[:160]}"]
    for line in lines:
        print(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
