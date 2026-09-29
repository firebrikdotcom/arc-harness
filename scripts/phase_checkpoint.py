#!/usr/bin/env python3
"""Emit and resolve automatic Jev shadow checkpoints at harness seams.

The phase gates, verify, review, and the tool hooks call this script so that
every harness run produces shadow checkpoints and independently labeled
outcomes without the agent authoring JSON by hand. Each checkpoint records the
action the harness was going to take anyway, asks Jev a bounded question over
enum-only signals derived from git, run, and verification history, and is later
labeled from a deterministic oracle: the verify exit code, the first verify
after build start, whether the run looped back to an earlier phase, or, for a
repeated shell command, whether that command recurred before its phase ended.

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

ENABLE_ENV = "HARNESS_JEV_CHECKPOINTS"
POLICY_VERSION = "shadow-1"
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
GO_ACTIONS = {"proceed_to_build", "ready_for_handoff"}
HOLD_ACTIONS = {"refine_plan", "needs_more_work"}
REPEAT_GO_ACTIONS = {"retry_same_command"}
REPEAT_HOLD_ACTIONS = {"change_approach", "gather_more_evidence"}
NON_SOURCE_LANGUAGES = {"markdown", "config"}
TEST_DIRECTORIES = {"test", "tests", "spec", "specs", "__tests__", "testing"}
TEST_NAME = re.compile(r"^(test_.*|conftest\.py|.*(_test|\.test|_spec|\.spec)\.[a-z0-9]+)$")
CAMEL_TEST_NAME = re.compile(r"[a-z0-9](Test|Tests|Spec)\.[A-Za-z0-9]+$")
VERIFY_HISTORY = "verify-history.jsonl"
VERIFY_HISTORY_KEEP = 100
HISTORY_WINDOW = 5
UNTRACKED_LINE_LIMIT_BYTES = 1_000_000


class Skip(Exception):
    """The checkpoint does not apply here; say why and exit 0."""


def enabled() -> bool:
    return os.environ.get(ENABLE_ENV) == "1"


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
    current = db_root / "runs" / "current"
    if not current.is_file():
        return None
    run_id = current.read_text(encoding="utf-8").splitlines()[0].strip() if current.read_text(encoding="utf-8").strip() else ""
    state = read_kv(db_root / "runs" / run_id / "state") if run_id else {}
    return state or None


def require_active_run(db_root: Path) -> dict[str, str]:
    state = run_state(db_root)
    if not state or state.get("RUN_STATUS") != "active":
        raise Skip("no active harness run")
    return state


def as_int(value: Any) -> int:
    """An integer field of a pending item; anything malformed counts as zero."""
    if isinstance(value, bool):
        return 0
    if isinstance(value, int):
        return value
    return int(value) if isinstance(value, str) and value.isdigit() else 0


def int_value(state: dict[str, str], key: str) -> int:
    value = state.get(key, "0")
    return int(value) if value.isdigit() else 0


def last_verify(db_root: Path) -> dict[str, str]:
    return read_kv(db_root / "records" / "verify.state")


def verify_history(db_root: Path) -> list[dict[str, Any]]:
    """Oldest-first verification results for this target.

    The history file is written by the verify-result event. The single verify.state record
    is appended when it is newer than the history's last line: databases from before the
    history existed, or verifications that ran with checkpoints disabled.
    """
    path = db_root / "records" / VERIFY_HISTORY
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
    directory = db_root / "records"
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


def in_run(entry: dict[str, Any], state: dict[str, str]) -> bool:
    if entry.get("run_id"):
        return entry["run_id"] == state.get("RUN_ID")
    started = int_value(state, "RUN_STARTED_EPOCH")
    return bool(started) and entry.get("epoch", 0) >= started


def verify_facts(db_root: Path, state: dict[str, str], project: Path | None = None) -> list[str]:
    """Verification history across runs and within this run, as counts and yes/no only."""
    history = verify_history(db_root)
    if not history:
        return ["Verification history for this target: none recorded yet."]
    recent = history[-HISTORY_WINDOW:]
    passed = sum(1 for entry in recent if entry["exit"] == 0)
    latest = history[-1]
    latest_passed = latest["exit"] == 0
    streak = 0
    for entry in reversed(history):
        if (entry["exit"] == 0) != latest_passed:
            break
        streak += 1
    facts = [f"Verification history for this target (last {len(recent)}): {passed} passed, {len(recent) - passed} failed; "
             f"the most recent {'passed' if latest_passed else 'failed'} ({min(streak, HISTORY_WINDOW)}{'+' if streak > HISTORY_WINDOW else ''} in a row)."]
    this_run = [entry for entry in history if in_run(entry, state)]
    if this_run:
        failed = sum(1 for entry in this_run if entry["exit"] != 0)
        facts.append(f"Verifications in this run so far: {bucket(len(this_run))} count, {failed} failed.")
    else:
        facts.append("Verifications in this run so far: none.")
    current = tree_fingerprint(project) if project is not None and latest.get("tree") else None
    changed = "unknown" if current is None else ("no" if current == latest["tree"] else "yes")
    facts.append(f"Working tree changed since the most recent verification: {changed}.")
    return facts


def run_history_facts(db_root: Path, state: dict[str, str]) -> list[str]:
    """How previous harness runs of this target ended; statuses and loop counts only."""
    runs = db_root / "runs"
    current = state.get("RUN_ID", "")
    finished: list[tuple[int, int]] = []
    aborted = unfinished = 0
    for path in sorted(runs.glob("*/state")) if runs.is_dir() else []:
        other = read_kv(path)
        if not other or other.get("RUN_ID", path.parent.name) == current:
            continue
        status = other.get("RUN_STATUS")
        if status == "complete":
            finished.append((int_value(other, "RUN_STARTED_EPOCH"), int_value(other, "LOOPS_USED")))
        elif status == "aborted":
            aborted += 1
        else:
            unfinished += 1
    facts = [f"Previous runs of this target: {bucket(len(finished))} completed, {bucket(aborted)} aborted, "
             f"{bucket(unfinished)} left unfinished."]
    if finished:
        recent = [loops for _, loops in sorted(finished)[-HISTORY_WINDOW:]]
        facts.append(f"Of the last {len(recent)} completed runs, {sum(1 for loops in recent if loops > 0)} re-entered an earlier phase.")
    return facts


def retrieval_facts(db_root: Path, state: dict[str, str]) -> list[str]:
    """Whether scripts/jg.sh ran a semantic retrieval in this run; counts only, never questions or paths."""
    directory = db_root / "retrieval"
    run_id = state.get("RUN_ID", "")
    searches = 0
    complete = 0
    for path in sorted(directory.glob("*.state")) if directory.is_dir() else []:
        record = read_kv(path)
        if record.get("RECORD_KIND") != "jevgrep" or not run_id or record.get("RUN_ID") != run_id:
            continue
        searches += 1
        if record.get("COMPLETE") == "yes":
            complete += 1
    if not searches:
        return ["Semantic retrieval (jevgrep) used in this run: no."]
    return [f"Semantic retrieval (jevgrep) used in this run: yes ({bucket(searches)} count, complete results: {complete})."]


def base_facts(signals: dict[str, Any], state: dict[str, str]) -> list[str]:
    languages = ", ".join(signals["languages"]) or "none detected"
    return [
        f"Dirty file count bucket: {signals['dirty_bucket']}.",
        f"Changed line count bucket: {signals['changed_lines_bucket']}.",
        f"Languages touched: {languages}.",
        f"Change shape: {signals['change_shape']} (whether tests changed together with source).",
        f"Documentation changed: {'yes' if signals['docs_changed'] else 'no'}.",
        f"Project has a test directory: {'yes' if signals['has_tests_dir'] else 'no'}.",
        f"Harness steps used so far: {bucket(int_value(state, 'STEPS_USED'))}.",
        f"Phase loops used so far: {int_value(state, 'LOOPS_USED')}.",
    ]


def common_facts(signals: dict[str, Any], state: dict[str, str], db_root: Path, project: Path | None) -> list[str]:
    """The facts every automatic checkpoint shares: tree shape, history, and retrieval use."""
    return (base_facts(signals, state) + verify_facts(db_root, state, project)
            + run_history_facts(db_root, state) + retrieval_facts(db_root, state))


CONSTRAINTS = [
    "Required checks, permissions, failures, and completion gates are deterministic and cannot be waived.",
    "Answer only from the supplied facts; missing evidence is unknown, not false.",
]


def checkpoint(family: str, question_version: str, baseline: str, goal: str, facts: list[str],
               questions: dict[str, Any], risks: list[str] | None = None) -> dict[str, Any]:
    context: dict[str, Any] = {"goal": goal, "facts": facts, "constraints": CONSTRAINTS}
    if risks:
        context["risks"] = risks
    return {
        "version": 2,
        "checkpoint": {"family": family, "question_version": question_version, "policy_version": POLICY_VERSION,
                       "baseline_action": baseline, "bypass_reason": "none", "state_build_ms": None},
        "context": context,
        "questions": questions,
    }


LOOP_QUESTION = "This run will re-enter an earlier phase (another plan or build pass) before it completes."


def build_plan_done(signals: dict[str, Any], state: dict[str, str], db_root: Path,
                    project: Path | None = None) -> tuple[dict, str]:
    # phase-plan-2: the progress.md questions of phase-plan-1 are gone because interactive
    # targets never write progress.md; the question now matches its oracle, a later loop.
    return checkpoint(
        "handoff_assessment", "phase-plan-2", "proceed_to_build",
        "Assess whether this run can go from planning to building without a later loop back to an earlier phase.",
        common_facts(signals, state, db_root, project),
        {
            "recommendation": {"type": "choice", "instructions": "Should the run proceed to build now?",
                               "criteria": {"proceed_to_build": "Build now; the run is unlikely to loop back to planning or to another build pass.",
                                            "refine_plan": "Rework is likely; refine the approach before building."}},
            "loop_likely": {"type": "boolean", "instructions": LOOP_QUESTION},
        },
        risks=["Starting the build on an approach that needs rework costs another plan or build pass."],
    ), "run_complete"


def build_build_start(signals: dict[str, Any], state: dict[str, str], db_root: Path,
                      project: Path | None = None) -> tuple[dict, str]:
    return checkpoint(
        "reasoning_allocation", "phase-build-2", "routine",
        "Allocate reasoning effort for the build phase that is starting.",
        common_facts(signals, state, db_root, project),
        {
            "recommendation": {"type": "choice", "instructions": "How much reasoning does this build phase need before its first verification passes?",
                               "criteria": {"routine": "Routine implementation; the first verification is expected to pass.",
                                            "targeted_check": "Run one targeted check before implementing further.",
                                            "deep_reasoning": "Deeper investigation is needed; the first verification is likely to fail."}},
            "rework_risk": {"type": "score", "instructions": "How much rework would a wrong allocation cause?",
                            "criteria": ["Minor: one short fix", "Moderate: one failed verify cycle", "Major: the change needs redesign"]},
        },
        risks=["Underestimating effort produces a failed verification cycle."],
    ), "first_verify"


def build_verify_start(signals: dict[str, Any], state: dict[str, str], db_root: Path,
                       project: Path | None = None) -> tuple[dict, str]:
    return checkpoint(
        "evidence_assessment", "verify-predict-2", "run_full_verification",
        "Predict whether the pending changes will pass the project's verification checks.",
        common_facts(signals, state, db_root, project),
        {
            "recommendation": {"type": "choice", "instructions": "What should happen next?",
                               "criteria": {"run_full_verification": "Run full verification now; it is expected to pass.",
                                            "fix_before_verifying": "Failures are likely; more fixes are needed first."}},
            "will_pass": {"type": "boolean", "instructions": "The pending changes will pass the project's verification checks."},
        },
    ), "verify_result"


def build_review_handoff(signals: dict[str, Any], state: dict[str, str], db_root: Path, verify_exit: int,
                         project: Path | None = None) -> tuple[dict, str]:
    # review-handoff-2: docs_and_progress_current is replaced by the loop question its oracle measures.
    baseline = "ready_for_handoff" if verify_exit == 0 else "needs_more_work"
    facts = common_facts(signals, state, db_root, project) + [f"Verification during review {'passed' if verify_exit == 0 else 'failed'}."]
    return checkpoint(
        "handoff_assessment", "review-handoff-2", baseline,
        "Assess whether the reviewed change is ready to hand off without another build loop.",
        facts,
        {
            "recommendation": {"type": "choice", "instructions": "Is the change ready to hand off?",
                               "criteria": {"ready_for_handoff": "Ready; no further build loop is expected.",
                                            "needs_more_work": "Another build loop is expected before handoff."}},
            "loop_likely": {"type": "boolean", "instructions": "Another build pass will be needed before this run completes."},
        },
    ), "run_complete"


def command_digests(db_root: Path, run_id: str | None) -> list[str]:
    """The observe hook's per-run checksum log (one line per non-harness shell command)."""
    if not run_id:
        return []
    path = db_root / "runs" / run_id / "command-digests"
    try:
        return [line.strip() for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    except OSError:
        return []


def repeated_digest(digests: list[str], repeats: int) -> tuple[str | None, int]:
    """The latest digest whose count reaches exactly `repeats`, and its 1-based position in the log.

    The hook appends the repeated command and then calls this script, but a parallel shell
    call may append another digest in between, so the last line is not necessarily it.
    """
    seen: Counter = Counter()
    found: tuple[str | None, int] = (digests[-1] if digests else None, len(digests))
    for position, digest in enumerate(digests, start=1):
        seen[digest] += 1
        if seen[digest] == repeats:
            found = (digest, position)
    return found


def build_tool_repeat(signals: dict[str, Any], state: dict[str, str], repeats: int, db_root: Path | None = None,
                      project: Path | None = None, digests: list[str] | None = None) -> tuple[dict, str]:
    history = common_facts(signals, state, db_root, project) if db_root is not None else base_facts(signals, state)
    if digests is None:
        digests = command_digests(db_root, state.get("RUN_ID")) if db_root is not None else []
    distinct = len(set(digests))
    facts = history + [f"The same shell command has now been issued {repeats} times in this run.",
                       f"Shell commands recorded in this run: {bucket(len(digests))} count, {bucket(distinct)} distinct.",
                       f"Current phase: {state.get('CURRENT_PHASE', 'unknown')}."]
    return checkpoint(
        "progress_assessment", "tool-repeat-2", "retry_same_command",
        "Assess whether repeating the same command is still making progress.",
        facts,
        {
            "recommendation": {"type": "choice", "instructions": "What is the most productive next step?",
                               "criteria": {"retry_same_command": "Run the same command again.",
                                            "change_approach": "Try a different investigation or fix.",
                                            "gather_more_evidence": "Read more evidence before acting."}},
            "stuck": {"type": "boolean", "instructions": "The agent is repeating an approach that is not producing progress."},
        },
    ), "tool_repeat"


def write_pending(db_root: Path, result: dict[str, Any], context: dict[str, Any], resolver: str, state: dict[str, str],
                  extra: dict[str, Any] | None = None) -> None:
    if resolver == "none" or result.get("status") != "evaluated":
        return
    directory = db_root / "advice-pending"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(directory, 0o700)
    answers = result.get("answers") or {}
    compact = {}
    for key, answer in answers.items():
        if answer.get("type") == "boolean":
            compact[key] = answer.get("probability")
        else:
            compact[key] = answer.get(answer.get("type"))
    payload = {
        "call_id": result["call_id"], "family": context["checkpoint"]["family"],
        "question_version": context["checkpoint"]["question_version"], "resolver": resolver,
        "run_id": state.get("RUN_ID"), "created_at": now(),
        "baseline_action": context["checkpoint"]["baseline_action"],
        "recommendation": compact.get("recommendation"), "answers": compact,
        "loops_at": int_value(state, "LOOPS_USED"),
        **(extra or {}),
    }
    target = directory / f"{result['call_id']}.json"
    fd, temporary = tempfile.mkstemp(prefix=".pending-", dir=directory)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        os.fchmod(fd, 0o600)
        json.dump(payload, stream, sort_keys=True)
        stream.write("\n")
    os.replace(temporary, target)


def pending_items(db_root: Path, resolver: str) -> list[tuple[Path, dict[str, Any]]]:
    directory = db_root / "advice-pending"
    items = []
    for path in sorted(directory.glob("*.json")) if directory.is_dir() else []:
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if isinstance(data, dict) and data.get("resolver") == resolver:
            items.append((path, data))
    return items


def drop_corrupt_pending(db_root: Path) -> list[str]:
    """Remove pending files that are not a JSON object; write_pending only ever writes objects."""
    directory = db_root / "advice-pending"
    lines = []
    for path in sorted(directory.glob("*.json")) if directory.is_dir() else []:
        try:
            data = json.loads(path.read_text(encoding="utf-8"))
        except OSError:
            continue
        except ValueError:
            data = None
        if not isinstance(data, dict):
            path.unlink(missing_ok=True)
            lines.append("dropped a corrupt pending oracle file")
    return lines


def label_verify(item: dict[str, Any], exit_code: int) -> tuple[str, str]:
    passed = exit_code == 0
    probability = item.get("answers", {}).get("will_pass")
    if isinstance(probability, (int, float)):
        predicted = probability >= 0.5
    else:
        predicted = item.get("recommendation") == "run_full_verification"
    if predicted == passed:
        label = "correct"
    elif predicted and not passed:
        label = "under_escalated"
    else:
        label = "over_escalated"
    return label, f"Verification exited {exit_code}; Jev predicted pass with probability {probability if probability is not None else 'unknown'}."


def label_allocation(item: dict[str, Any], exit_code: int) -> tuple[str, str]:
    recommendation = item.get("recommendation")
    if exit_code == 0:
        label = "correct" if recommendation == "routine" else "over_escalated"
    else:
        label = "correct" if recommendation == "deep_reasoning" else "under_escalated"
    return label, f"The first verification after build start exited {exit_code}; Jev recommended {recommendation}."


def label_handoff(item: dict[str, Any], loops_now: int) -> tuple[str, str]:
    looped = loops_now > as_int(item.get("loops_at"))
    recommendation = item.get("recommendation")
    if recommendation in GO_ACTIONS:
        label = "under_escalated" if looped else "correct"
    elif recommendation in HOLD_ACTIONS:
        label = "correct" if looped else "over_escalated"
    else:
        label = "unknown"
    return label, f"The run {'re-entered an earlier phase' if looped else 'completed without re-entering an earlier phase'} after this handoff; Jev recommended {recommendation}."


def label_tool_repeat(item: dict[str, Any], stuck: bool | None, reason: str) -> tuple[str, str]:
    """stuck: the command recurred or the run looped; not stuck: the phase and run ended cleanly."""
    recommendation = item.get("recommendation")
    if stuck is None:
        label = "unknown"
    elif recommendation in REPEAT_HOLD_ACTIONS:
        label = "correct" if stuck else "over_escalated"
    elif recommendation in REPEAT_GO_ACTIONS:
        label = "under_escalated" if stuck else "correct"
    else:
        label = "unknown"
    return label, f"{reason} Jev recommended {recommendation}."


def tool_repeat_outcome(db_root: Path, item: dict[str, Any], state: dict[str, str], ended: bool) -> tuple[bool | None, str] | None:
    """Mechanical oracle for tool-repeat-2; None means the evidence is not in yet."""
    digests = command_digests(db_root, item.get("run_id"))
    index = item.get("digest_index")
    digest = item.get("digest")
    if isinstance(index, int) and digest and digest in digests[index:]:
        return True, "The same command was issued again after the checkpoint."
    if int_value(state, "LOOPS_USED") > as_int(item.get("loops_at")):
        return True, "The run re-entered an earlier phase after the checkpoint."
    if not ended:
        return None
    phase = str(item.get("phase_at") or "").upper()
    if phase in {"PLAN", "BUILD", "REVIEW"} and state.get(f"PHASE_{phase}") == "done":
        return False, "The command did not recur, the phase it repeated in completed, and the run ended without a loop."
    return None, "The run ended before the phase in which the command repeated completed."


def run_ended(state: dict[str, str], current_run: str | None) -> bool:
    return state.get("RUN_ID") != current_run or state.get("RUN_STATUS") in {"complete", "aborted"}


def item_run_state(db_root: Path, item: dict[str, Any]) -> dict[str, str]:
    return read_kv(db_root / "runs" / str(item.get("run_id")) / "state") if item.get("run_id") else {}


ITEM_ERRORS = (advice.InputError, OSError, ValueError, KeyError, TypeError, AttributeError)


def label_item(db_root: Path, router: Any, path: Path, item: dict[str, Any], compute) -> str | None:
    """Label one pending item from compute() -> (label, evidence), or None while its oracle is pending.

    Any failure, in the oracle or in recording (for example, an advice record removed by operator
    retention), drops the item with a line, so it can neither recur on every event nor block others.
    """
    call_id = str(item.get("call_id") or "unknown")
    try:
        result = compute()
        if result is None:
            return None
        label, evidence = result
        outcome = {"call_id": item["call_id"], "action_taken": item.get("baseline_action") or "baseline",
                   "outcome": label, "evidence": evidence}
        advice.label_outcome(outcome, router, db_root)
        line = f"labeled {item.get('family')}/{item.get('question_version')} {label} ({call_id[:8]})"
    except ITEM_ERRORS as error:
        line = f"could not label {call_id[:8]}: {type(error).__name__}"
    path.unlink(missing_ok=True)
    return line


def resolve_tool_repeats(db_root: Path, router: Any, current: dict[str, str], ending: bool = False) -> list[str]:
    """Label every tool-repeat checkpoint whose outcome is now known, in this run or an earlier one."""
    lines = []
    current_run = current.get("RUN_ID")
    for path, item in pending_items(db_root, "tool_repeat"):
        def compute(item: dict[str, Any] = item) -> tuple[str, str] | None:
            same = item.get("run_id") == current_run
            state = current if same else item_run_state(db_root, item)
            # A run other than the current one has ended: completed, aborted, or superseded.
            ended = not same or ending or run_ended(state, current_run)
            outcome = tool_repeat_outcome(db_root, item, state, ended)
            return None if outcome is None else label_tool_repeat(item, *outcome)
        line = label_item(db_root, router, path, item, compute)
        if line:
            lines.append(line)
    return lines


def resolve_stale(db_root: Path, router: Any, current: dict[str, str]) -> list[str]:
    """Pending oracles left by a run that ended without reaching them are labeled from that run alone.

    Without this, a later run's verification or loop count would label another run's prediction.
    Pending items have always carried their run id; only an item with an empty one stays unscoped.
    """
    lines = []
    current_run = current.get("RUN_ID")
    for resolver in ("verify_result", "first_verify", "run_complete"):
        for path, item in pending_items(db_root, resolver):
            if not item.get("run_id") or item.get("run_id") == current_run:
                continue

            def compute(item: dict[str, Any] = item, resolver: str = resolver) -> tuple[str, str]:
                state = item_run_state(db_root, item)
                if resolver == "run_complete" and (state.get("RUN_STATUS") == "complete"
                                                   or int_value(state, "LOOPS_USED") > as_int(item.get("loops_at"))):
                    return label_handoff(item, int_value(state, "LOOPS_USED"))
                return "unknown", "The run ended before this checkpoint's oracle was observed."
            lines.append(label_item(db_root, router, path, item, compute) or "")
    return [line for line in lines if line]


def resolve(db_root: Path, router: Any, resolver: str, labeler, *arguments: Any, run_id: str | None = None) -> list[str]:
    lines = []
    for path, item in pending_items(db_root, resolver):
        if run_id and item.get("run_id") and item.get("run_id") != run_id:
            # Defensive: resolve_stale normally labels other runs' items first, but it may have failed.
            continue
        lines.append(label_item(db_root, router, path, item, lambda item=item: labeler(item, *arguments)) or "")
    return [line for line in lines if line]


def emit(context: dict[str, Any], resolver: str, router: Any, db_root: Path, state: dict[str, str],
         extra: dict[str, Any] | None = None) -> str:
    result = advice.advise(advice.validate_context(context, router), router, db_root)
    write_pending(db_root, result, context, resolver, state, extra)
    family = context["checkpoint"]["family"]
    version = context["checkpoint"]["question_version"]
    if result["status"] != "evaluated":
        return f"{family}/{version} {result['status']} ({result.get('fallback_reason') or 'no evaluation'}); baseline kept."
    recommendation = (result.get("answers") or {}).get("recommendation", {}).get("choice")
    agreement = "agrees with" if recommendation == context["checkpoint"]["baseline_action"] else "differs from"
    return f"{family}/{version} shadow recommendation {recommendation} {agreement} baseline {context['checkpoint']['baseline_action']} ({result['call_id'][:8]}); baseline kept."


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
        route = task_route.route_task(path, project, db_root, router, "shadow")
    finally:
        path.unlink(missing_ok=True)
    observed = route.get("observed_recommendation") or route.get("recommendation")
    summary = advice.pilot(db_root)
    return (f"Jev shadow route for this session: {observed} (source {route['source']}, shadow; existing rules decide). "
            f"Pilot: {summary['labeled']}/{summary['target']} labeled shadow decisions.")


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
            if args.event == "verify-result":
                if args.exit_code is None:
                    parser.error("verify-result requires --exit")
                # History is kept for every verification, inside a run or not, before any skip.
                try:
                    append_verify_history(args.db_root, args.project, run_state(args.db_root) or {}, args.exit_code)
                except OSError as error:
                    lines.append(f"verify history not written: {type(error).__name__}")
            state = require_active_run(args.db_root)
            router = advice.load_router(args.router)
            ending = args.event == "run-complete"
            # Resolving earlier oracles must never suppress this event's own checkpoint,
            # and one resolver failing must not stop the other.
            for resolver_step in (lambda: drop_corrupt_pending(args.db_root),
                                  lambda: resolve_stale(args.db_root, router, state),
                                  lambda: resolve_tool_repeats(args.db_root, router, state, ending=ending)):
                try:
                    lines += resolver_step()
                except ITEM_ERRORS as error:
                    lines.append(f"pending oracles not resolved: {type(error).__name__}")
            run_id = state.get("RUN_ID")
            if args.event == "verify-result":
                lines += resolve(args.db_root, router, "verify_result", label_verify, args.exit_code, run_id=run_id)
                lines += resolve(args.db_root, router, "first_verify", label_allocation, args.exit_code, run_id=run_id)
            elif ending:
                lines += resolve(args.db_root, router, "run_complete", label_handoff, int_value(state, "LOOPS_USED"), run_id=run_id)
                lines += pilot_lines(args.db_root)
            else:
                signals = git_signals(args.project)
                extra = None
                if args.event == "plan-done":
                    context, resolver = build_plan_done(signals, state, args.db_root, args.project)
                elif args.event == "build-start":
                    context, resolver = build_build_start(signals, state, args.db_root, args.project)
                elif args.event == "verify-start":
                    context, resolver = build_verify_start(signals, state, args.db_root, args.project)
                elif args.event == "review-handoff":
                    if args.verify_exit is None:
                        parser.error("review-handoff requires --verify-exit")
                    context, resolver = build_review_handoff(signals, state, args.db_root, args.verify_exit, args.project)
                else:
                    if args.repeats is None:
                        parser.error("tool-repeat requires --repeats")
                    # One snapshot of the hook's log serves both the facts and the oracle position.
                    digests = command_digests(args.db_root, state.get("RUN_ID"))
                    digest, position = repeated_digest(digests, args.repeats)
                    extra = {"digest": digest, "digest_index": position, "phase_at": state.get("CURRENT_PHASE")}
                    context, resolver = build_tool_repeat(signals, state, args.repeats, args.db_root, args.project, digests)
                lines += [emit(context, resolver, router, args.db_root, state, extra)]
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
