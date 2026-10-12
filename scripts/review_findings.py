#!/usr/bin/env python3
"""Validate an independent reviewer's findings for the review gate.

The reviewer works in a fresh context from the review packet that scripts/review.sh
writes (contract, verification output, diff) and returns JSON shaped like
schemas/review-findings.schema.json. The findings bind to the exact files reviewed
through tree_hash (scripts/tree-hash.sh), so an edit after the review voids them.

  review_findings.py check --findings PATH --project PATH [--contract PATH]
  review_findings.py template --project PATH [--contract PATH]

Findings may answer the contract's acceptance criteria ("criteria": id, answer,
evidence). The answers are optional, but a criterion answered not_met blocks like
an open major finding: an approval cannot also say the goal was missed. With
--contract, check warns about criteria left unanswered or ids the contract lacks,
and template lists one answer per criterion.

check exits 0 when the verdict is approve, no blocker or major finding is open, and
no criterion is not_met, 1 when the review blocks (or no findings exist), 2 when the
file is malformed, and 3 when the findings cover files that have since changed.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys

SEVERITIES = ("blocker", "major", "minor")
STATUSES = ("open", "fixed", "wontfix")
ANSWERS = ("met", "not_met", "not_applicable")
SCRIPTS = Path(__file__).resolve().parent


def tree_hash(project: str) -> str:
    result = subprocess.run(["sh", str(SCRIPTS / "tree-hash.sh"), project], capture_output=True, text=True)
    return result.stdout.strip()


def text(value, name: str, limit: int = 4000) -> str:
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > limit:
        raise ValueError(f"{name} must be a nonempty string of at most {limit} bytes")
    return value


def validate(data) -> list[dict]:
    if not isinstance(data, dict):
        raise ValueError("findings must be a JSON object")
    unknown = set(data) - {"tree_hash", "reviewer", "verdict", "summary", "findings", "criteria"}
    if unknown:
        raise ValueError("unknown fields: " + ", ".join(sorted(unknown)))
    text(data.get("tree_hash"), "tree_hash", 100)
    text(data.get("reviewer"), "reviewer", 200)
    if data.get("verdict") not in ("approve", "block"):
        raise ValueError("verdict must be approve or block")
    if "summary" in data:
        text(data["summary"], "summary")
    findings = data.get("findings")
    if not isinstance(findings, list):
        raise ValueError("findings must be a list (empty when nothing was found)")
    for index, item in enumerate(findings):
        where = f"findings[{index}]"
        if not isinstance(item, dict):
            raise ValueError(where + " must be an object")
        if item.get("severity") not in SEVERITIES:
            raise ValueError(where + ".severity must be one of " + ", ".join(SEVERITIES))
        if item.get("status", "open") not in STATUSES:
            raise ValueError(where + ".status must be one of " + ", ".join(STATUSES))
        text(item.get("file"), where + ".file", 1000)
        text(item.get("claim"), where + ".claim")
        text(item.get("evidence"), where + ".evidence")
        if item.get("line") is not None and (type(item["line"]) is not int or item["line"] < 1):
            raise ValueError(where + ".line must be a positive integer or null")
    answers = data.get("criteria", [])
    if not isinstance(answers, list):
        raise ValueError("criteria must be a list of answers to the contract's acceptance criteria")
    ids = set()
    for index, item in enumerate(answers):
        where = f"criteria[{index}]"
        if not isinstance(item, dict) or set(item) - {"id", "answer", "evidence"}:
            raise ValueError(where + " holds id, answer, and evidence")
        identifier = text(item.get("id"), where + ".id", 64)
        if identifier in ids:
            raise ValueError(f"criterion {identifier} is answered twice")
        ids.add(identifier)
        if item.get("answer") not in ANSWERS:
            raise ValueError(where + ".answer must be one of " + ", ".join(ANSWERS))
        text(item.get("evidence"), where + ".evidence")
    return findings


def contract_ids(path: Path | None) -> list[str] | None:
    """The contract's acceptance ids in order, or None without a usable contract."""
    if path is None:
        return None
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        return [item["id"] for item in data["acceptance"] if isinstance(item, dict) and isinstance(item.get("id"), str)]
    except (OSError, ValueError, KeyError, TypeError):
        return None


def check(path: Path, project: str, contract: Path | None = None) -> int:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
        findings = validate(data)
    except FileNotFoundError:
        print(f"FAIL: no review findings at {path}.")
        print("Hand the review packet to an independent reviewer and submit its JSON: harness review submit FILE")
        return 1
    except (ValueError, OSError) as error:
        print(f"FAIL: review findings are malformed: {error}")
        return 2
    current = tree_hash(project)
    if data["tree_hash"] != current:
        print(f"FAIL: the review covered tree {data['tree_hash'][:12]}, but the project is now {current[:12]}.")
        print("Run scripts/review.sh again and review the current files.")
        return 3
    answers = data.get("criteria", [])
    expected = contract_ids(contract)
    if expected is not None:
        answered = {item["id"] for item in answers}
        missing = [identifier for identifier in expected if identifier not in answered]
        extra = sorted(answered - set(expected))
        if missing:
            print("WARN: the reviewer did not answer criteria: " + ", ".join(missing))
        if extra:
            print("WARN: the reviewer answered criteria the contract does not have: " + ", ".join(extra))
    open_findings = [item for item in findings if item.get("status", "open") == "open" and item["severity"] in ("blocker", "major")]
    not_met = [item for item in answers if item["answer"] == "not_met"]
    if data["verdict"] != "approve" or open_findings or not_met:
        print(f"FAIL: the reviewer ({data['reviewer']}) verdict is {data['verdict']} with {len(open_findings)} open blocker/major finding(s)"
              f" and {len(not_met)} criterion/criteria not met:")
        for item in open_findings:
            location = item["file"] + (f":{item['line']}" if item.get("line") else "")
            print(f"  [{item['severity']}] {location}: {item['claim']}")
        for item in not_met:
            print(f"  [not met] {item['id']}: {item['evidence']}")
        print("Fix them (or mark each finding fixed/wontfix with a reason in its evidence) and review again.")
        return 1
    minor = sum(1 for item in findings if item.get("status", "open") == "open")
    met = f", {len(answers)} criterion answer(s)" if answers else ""
    print(f"OK: {data['reviewer']} approved tree {current[:12]} ({len(findings)} finding(s), {minor} open minor{met}).")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=("check", "template"))
    parser.add_argument("--findings", type=Path)
    parser.add_argument("--project", required=True)
    parser.add_argument("--contract", type=Path, help="the run's task.json; its acceptance ids are the criteria to answer")
    args = parser.parse_args()
    if args.action == "template":
        template = {"tree_hash": tree_hash(args.project), "reviewer": "WHO REVIEWED (e.g. subagent:code-reviewer)",
                    "verdict": "approve | block", "summary": "One paragraph.",
                    "findings": [{"severity": "blocker | major | minor", "file": "path", "line": 1,
                                  "claim": "What is wrong.", "evidence": "How you know: command output, file:line.",
                                  "status": "open"}]}
        ids = contract_ids(args.contract)
        if ids:
            template["criteria"] = [{"id": identifier, "answer": "met | not_met | not_applicable",
                                     "evidence": "Command and result, test, or file:line."} for identifier in ids]
        print(json.dumps(template, indent=2))
        return 0
    if not args.findings:
        parser.error("check needs --findings")
    return check(args.findings, args.project, args.contract)


if __name__ == "__main__":
    sys.exit(main())
