#!/usr/bin/env python3
"""Bounded task contracts: what a run must deliver, what it must not touch, and
the commands that decide when it is done (schemas/task.schema.json).

`harness contract set FILE` stores a contract in the run; `plan done` requires one
(or an explicit, recorded waiver), `build done` runs every acceptance command, and
`review done` refuses a change that touched a non-goal path.

  task_contract.py validate FILE
  task_contract.py criteria FILE --project PATH
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


def run_criteria(data: dict, project: Path) -> int:
    if "waived" in data:
        print(f"Contract waived: {data['waived']}")
        return 0
    failed = 0
    for item in data["acceptance"]:
        if "command" not in item:
            print(f"REVIEW: {item['id']}: {item['criterion']} (no command; the reviewer judges it)")
            continue
        command = shlex.join(item["command"])
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
    parser.add_argument("action", choices=("validate", "criteria", "nongoals", "show"))
    parser.add_argument("file", type=Path)
    parser.add_argument("--project", type=Path, default=Path(os.getcwd()))
    parser.add_argument("--base", default="")
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
    project = args.project.resolve()
    if args.action == "criteria":
        return run_criteria(data, project)
    return check_non_goals(data, project, args.base)


if __name__ == "__main__":
    sys.exit(main())
