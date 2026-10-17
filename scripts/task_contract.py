#!/usr/bin/env python3
"""Bounded task contracts: what a run must deliver, what it must not touch, and
the commands that decide when it is done (schemas/task.schema.json).

`harness contract set FILE` stores a contract in the run; `plan done` requires one
(or an explicit, recorded waiver), `build done` runs every acceptance command (a
`make TARGET` that only repeats the checks of the passing verify run on the same
files is counted from that run instead of running twice), and
`review done` refuses a change that touched a non-goal path.

  task_contract.py validate FILE
  task_contract.py criteria FILE --project PATH [--verified VERIFY_RECORD]
  task_contract.py checklist FILE
  task_contract.py nongoals FILE --project PATH --base REV
  task_contract.py show FILE

Exit 0 ok, 1 a criterion failed or a non-goal was touched, 2 malformed or refused.
"""
from __future__ import annotations

import argparse
import fnmatch
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

import permit

FIELDS = {"goal", "deliverables", "constraints", "non_goals", "acceptance", "waived"}


def text(value, name: str, limit: int = 2000) -> str:
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > limit:
        raise ValueError(f"{name} must be a nonempty string of at most {limit} bytes")
    return value


def strings(value, name: str, minimum: int = 0) -> list[str]:
    if not isinstance(value, list) or len(value) < minimum:
        raise ValueError(f"{name} must be a list with at least {minimum} item(s)")
    return [text(item, f"{name}[{index}]") for index, item in enumerate(value)]


def validate(data) -> dict:
    if not isinstance(data, dict):
        raise ValueError("a contract is a JSON object")
    unknown = set(data) - FIELDS
    if unknown:
        raise ValueError("unknown fields: " + ", ".join(sorted(unknown)))
    if "waived" in data:
        if set(data) != {"waived"}:
            raise ValueError("a waiver holds only its reason")
        text(data["waived"], "waived")
        return data
    text(data.get("goal"), "goal")
    strings(data.get("deliverables"), "deliverables", 1)
    strings(data.get("constraints", []), "constraints")
    non_goals = data.get("non_goals")
    if not isinstance(non_goals, list):
        raise ValueError("non_goals must be a list (empty only when nothing is off limits)")
    for index, item in enumerate(non_goals):
        if not isinstance(item, dict) or set(item) - {"description", "paths"}:
            raise ValueError(f"non_goals[{index}] holds description and optional paths")
        text(item.get("description"), f"non_goals[{index}].description")
        strings(item.get("paths", []), f"non_goals[{index}].paths")
    acceptance = data.get("acceptance")
    if not isinstance(acceptance, list) or not acceptance:
        raise ValueError("acceptance must list at least one criterion")
    ids = set()
    for index, item in enumerate(acceptance):
        if not isinstance(item, dict) or set(item) - {"id", "criterion", "command"}:
            raise ValueError(f"acceptance[{index}] holds id, criterion, and optional command")
        identifier = text(item.get("id"), f"acceptance[{index}].id", 64)
        if identifier in ids:
            raise ValueError(f"acceptance id {identifier} is repeated")
        ids.add(identifier)
        text(item.get("criterion"), f"acceptance[{index}].criterion")
        if "command" in item:
            strings(item["command"], f"acceptance[{index}].command", 1)
    if not any("command" in item for item in acceptance):
        raise ValueError("at least one acceptance criterion needs a command, so a machine decides when the task is done")
    return data


def load(path: Path) -> dict:
    return validate(json.loads(path.read_text(encoding="utf-8")))


def read_record(path: Path | None) -> dict[str, str]:
    """KEY=VALUE run record; the last value of a repeated key wins."""
    if not path or not path.is_file():
        return {}
    fields = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        key, sep, value = line.partition("=")
        if sep:
            fields[key] = value
    return fields


def make_dry_run(project: Path, targets: list[str]) -> set[str] | None:
    """The recipe lines `make -n` would run, without make's own status lines."""
    try:
        result = subprocess.run(["make", "--no-print-directory", "-n", *targets], cwd=project,
                                capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    return {line for line in result.stdout.splitlines() if line.strip() and not line.startswith(("make:", "make["))}


def proved_by_verify(command: list[str], project: Path, record: dict[str, str]) -> str | None:
    """A `make TARGET` criterion whose recipe only repeats checks the passing verify
    run already ran on these exact files needs no second run. Returns when that
    verify run was recorded, or None when the criterion must run."""
    if len(command) != 2 or command[0] != "make" or command[1].startswith("-"):
        return None
    if record.get("EXIT") != "0" or record.get("FAILURES") != "0":
        return None
    if not record.get("PROJECT_ROOT") or Path(record["PROJECT_ROOT"]).resolve() != project:
        return None
    tree = record.get("TREE_HASH", "")
    targets = [name.split(":", 1)[1] for name in record.get("PASSED_CHECKS", "").split() if name.startswith("make:")]
    if not tree or tree == "none" or not targets:
        return None
    wanted, proved = make_dry_run(project, [command[1]]), make_dry_run(project, targets)
    if not wanted or proved is None or not wanted <= proved:
        return None
    current = subprocess.run(["sh", str(Path(__file__).resolve().parent / "tree-hash.sh"), str(project)],
                             capture_output=True, text=True, timeout=120)
    if current.returncode != 0 or current.stdout.strip() != tree:
        return None
    return record.get("RECORD_AT", "the last verify run")


def run_criteria(data: dict, project: Path, verified: Path | None = None) -> int:
    if "waived" in data:
        print(f"Contract waived: {data['waived']}")
        return 0
    record = read_record(verified)
    failed = 0
    for item in data["acceptance"]:
        if "command" not in item:
            print(f"REVIEW: {item['id']}: {item['criterion']} (no command; the reviewer judges it)")
            continue
        command = shlex.join(item["command"])
        proved_at = proved_by_verify(item["command"], project, record) if record else None
        if proved_at:
            print(f"PASS: {item['id']}: {item['criterion']} (same checks as the passing verify run at {proved_at} on these files; not run again)")
            continue
        denial = permit.check_command(command, project, str(project))
        if denial:
            print(f"FAIL: {item['id']}: command refused by the denylist: {denial}")
            failed += 1
            continue
        try:
            result = subprocess.run(item["command"], cwd=project, capture_output=True, text=True,
                                    timeout=int(os.environ.get("HARNESS_CRITERION_TIMEOUT", "900")))
        except (OSError, subprocess.TimeoutExpired) as error:
            failed += 1
            print(f"FAIL: {item['id']}: {item['criterion']} ({command} did not run: {type(error).__name__}: {error})")
            continue
        if result.returncode == 0:
            print(f"PASS: {item['id']}: {item['criterion']}")
        else:
            failed += 1
            tail = (result.stdout + result.stderr).strip().splitlines()[-5:]
            print(f"FAIL: {item['id']}: {item['criterion']} ({command} exited {result.returncode})")
            for line in tail:
                print("    " + line)
    return 1 if failed else 0


def plain(value: str) -> str:
    """One terminal-safe line: control characters become spaces, so a contract
    printed for a reviewer cannot move the cursor or recolour the terminal."""
    return " ".join("".join(" " if ord(ch) < 32 or 127 <= ord(ch) < 160 else ch for ch in value).split())


def checklist(data: dict) -> int:
    """Prints every acceptance criterion for the reviewer to answer, and says
    which ones only the reviewer checks (they carry no command)."""
    if "waived" in data:
        print(f"Contract waived: {plain(data['waived'])}")
        print("Judge the change against that reason; there are no criteria to answer.")
        return 0
    for item in data["acceptance"]:
        if "command" in item:
            how = "command passed at build done: " + plain(shlex.join(item["command"])) + "; check that it tests this"
        else:
            how = "no command: only the reviewer checks this"
        print(f"- {plain(item['id'])}: {plain(item['criterion'])} [{how}]")
    print("Answer each criterion as met, not_met, or not_applicable, with evidence "
          "(command and result, test, or file:line), in the findings' \"criteria\" list.")
    return 0


def changed_files(project: Path, base: str) -> list[str]:
    def git(*args):
        return subprocess.run(["git", "-C", str(project), *args], capture_output=True, text=True, check=True).stdout.splitlines()
    files = git("diff", "--name-only", "--no-renames", base, "--") if base else git("diff", "--name-only", "HEAD", "--")
    files += git("ls-files", "--others", "--exclude-standard")
    return sorted(set(files))


def check_non_goals(data: dict, project: Path, base: str) -> int:
    if "waived" in data:
        return 0
    try:
        files = changed_files(project, base)
    except (subprocess.CalledProcessError, OSError):
        print("SKIP: non-goal paths not checked (not a git work tree).")
        return 0
    touched = []
    for item in data["non_goals"]:
        for pattern in item.get("paths", []):
            touched += [(path, pattern, item["description"]) for path in files
                        if fnmatch.fnmatch(path, pattern) or path.startswith(pattern.rstrip("/") + "/")]
    for path, pattern, description in touched:
        print(f"FAIL: {path} matches non-goal {pattern} ({description})")
    if not touched:
        print(f"OK: {len(files)} changed file(s), none in a non-goal path.")
    return 1 if touched else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=("validate", "criteria", "checklist", "nongoals", "show"))
    parser.add_argument("file", type=Path)
    parser.add_argument("--project", type=Path, default=Path(os.getcwd()))
    parser.add_argument("--base", default="")
    parser.add_argument("--verified", type=Path, help="passing verify record that may stand in for identical make checks")
    args = parser.parse_args()
    try:
        data = load(args.file)
    except FileNotFoundError:
        print(f"FAIL: no contract at {args.file}.")
        return 2
    except (ValueError, OSError) as error:
        print(f"FAIL: the contract is invalid: {error}")
        return 2
    if args.action == "validate":
        print("OK: contract waived." if "waived" in data else f"OK: contract with {len(data['acceptance'])} acceptance criteria.")
        return 0
    if args.action == "show":
        print(json.dumps(data, indent=2))
        return 0
    if args.action == "checklist":
        return checklist(data)
    project = args.project.resolve()
    if args.action == "criteria":
        return run_criteria(data, project, args.verified)
    return check_non_goals(data, project, args.base)


if __name__ == "__main__":
    sys.exit(main())
