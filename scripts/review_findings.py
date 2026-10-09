#!/usr/bin/env python3
"""Validate an independent reviewer's findings for the review gate.

The reviewer works in a fresh context from the review packet that scripts/review.sh
writes (contract, verification output, diff) and returns JSON shaped like
schemas/review-findings.schema.json. The findings bind to the exact files reviewed
through tree_hash (scripts/tree-hash.sh), so an edit after the review voids them.

  review_findings.py check --findings PATH --project PATH
  review_findings.py template --project PATH

check exits 0 when the verdict is approve and no blocker or major finding is open,
1 when the review blocks or is stale, 2 when the file is malformed.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys

SEVERITIES = ("blocker", "major", "minor")
STATUSES = ("open", "fixed", "wontfix")
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
    unknown = set(data) - {"tree_hash", "reviewer", "verdict", "summary", "findings"}
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
    return findings


def check(path: Path, project: str) -> int:
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
        return 1
    open_findings = [item for item in findings if item.get("status", "open") == "open" and item["severity"] in ("blocker", "major")]
    if data["verdict"] != "approve" or open_findings:
        print(f"FAIL: the reviewer ({data['reviewer']}) verdict is {data['verdict']} with {len(open_findings)} open blocker/major finding(s):")
        for item in open_findings:
            location = item["file"] + (f":{item['line']}" if item.get("line") else "")
            print(f"  [{item['severity']}] {location}: {item['claim']}")
        print("Fix them (or mark each fixed/wontfix with a reason in its evidence) and review again.")
        return 1
    minor = sum(1 for item in findings if item.get("status", "open") == "open")
    print(f"OK: {data['reviewer']} approved tree {current[:12]} ({len(findings)} finding(s), {minor} open minor).")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=("check", "template"))
    parser.add_argument("--findings", type=Path)
    parser.add_argument("--project", required=True)
    args = parser.parse_args()
    if args.action == "template":
        print(json.dumps({"tree_hash": tree_hash(args.project), "reviewer": "WHO REVIEWED (e.g. subagent:code-reviewer)",
                          "verdict": "approve | block", "summary": "One paragraph.",
                          "findings": [{"severity": "blocker | major | minor", "file": "path", "line": 1,
                                        "claim": "What is wrong.", "evidence": "How you know: command output, file:line.",
                                        "status": "open"}]}, indent=2))
        return 0
    if not args.findings:
        parser.error("check needs --findings")
    return check(args.findings, args.project)


if __name__ == "__main__":
    sys.exit(main())
