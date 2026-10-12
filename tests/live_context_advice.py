"""Opt-in end-to-end checks for five dynamic TypeSafe advice contexts."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / "scripts/harness"
CASES = [
    {
        "name": "banking ledger",
        "decision": "Which persistence design best fits this banking ledger?",
        "goal": "Keep transfers, reversals, balances, and a reconstructible audit trail correct.",
        "options": [("crud", "CRUD tables with an explicit audit log."), ("event_sourcing", "Events are the ledger source of truth.")],
    },
    {
        "name": "clinic appointments",
        "decision": "Which data model best fits this appointment scheduling feature?",
        "goal": "Prevent double booking while supporting clinicians, patients, and recurring appointments.",
        "options": [("relational", "Use relational tables and transactional constraints."), ("document", "Use a document-oriented model for the schedule.")],
    },
    {
        "name": "offline field app",
        "decision": "Which conflict strategy best fits this offline inspection app?",
        "goal": "Synchronize edits made by technicians while devices are disconnected for days.",
        "options": [("operation_log", "Sync an ordered log of operations and resolve conflicts explicitly."), ("last_write_wins", "Sync complete records and keep only the newest edit.")],
    },
    {
        "name": "storefront search",
        "decision": "Which search approach best fits this product catalogue?",
        "goal": "Help shoppers find products by exact part number, title, and common synonyms.",
        "options": [("keyword_index", "Use a conventional keyword and synonym index."), ("semantic_search", "Use vector similarity as the primary search mechanism.")],
    },
    {
        "name": "staff portal authentication",
        "decision": "Which session mechanism best fits this internal staff portal?",
        "goal": "Support immediate access revocation, short sessions, and centralized audit controls.",
        "options": [("server_sessions", "Use server-side sessions with centrally revocable state."), ("signed_tokens", "Use self-contained signed tokens without server session state.")],
    },
]


def context(case: dict) -> dict:
    return {
        "version": 1,
        "decision": {"question": case["decision"]},
        "context": {
            "goal": case["goal"],
            "constraints": ["Choose only from the supplied options.", "No implementation is authorized by this request."],
        },
        "options": [{"id": identifier, "description": description} for identifier, description in case["options"]],
    }


def main() -> int:
    with tempfile.TemporaryDirectory(prefix="harness-live-advice-") as temporary:
        root = Path(temporary)
        environment = os.environ | {"HARNESS_DB_ROOT": str(root / "db"), "TYPESAFE_LOG_DIR": str(root / "logs")}
        for case in CASES:
            path = root / f"{case['name'].replace(' ', '-')}.json"
            path.write_text(json.dumps(context(case)))
            result = subprocess.run([str(CLI), "advise", "--context", str(path)], text=True, capture_output=True, env=environment, check=False)
            if result.returncode:
                print(result.stderr or result.stdout, file=sys.stderr)
                return result.returncode
            answer = json.loads(result.stdout)
            allowed = {identifier for identifier, _ in case["options"]}
            if answer.get("choice") not in allowed or not isinstance(answer.get("confidence"), (int, float)) or not 0 <= answer["confidence"] <= 1:
                print(f"FAIL: {case['name']} returned an invalid dynamic response: {result.stdout}", file=sys.stderr)
                return 1
            record = Path(answer["record_path"])
            if not record.is_file() or record.stat().st_mode & 0o777 != 0o600:
                print(f"FAIL: {case['name']} did not create a private advice record", file=sys.stderr)
                return 1
            print(f"PASS: {case['name']} -> {answer['choice']} ({answer['confidence']:.2f})")
        checkpoint = {
            "version": 2,
            "checkpoint": {"family": "identification", "question_version": "fixture-1", "policy_version": "fixture-1",
                           "baseline_action": "troubleshooting", "bypass_reason": "none"},
            "context": {"goal": "Classify a synthetic documentation summary.",
                        "facts": ["The page describes diagnosing an intermittent connection problem and checking its symptoms."]},
            "questions": {
                "recommendation": {"type": "choice", "instructions": "What is the primary purpose of this page?",
                                   "criteria": {"tutorial": "Teach a new task", "troubleshooting": "Diagnose a problem"}},
                "diagnostic": {"type": "boolean", "instructions": "The page is primarily diagnostic."},
                "specificity": {"type": "score", "instructions": "How specific is the supplied summary?",
                                "criteria": ["No diagnostic detail", "Some symptom detail", "Full reproducible procedure"]},
            },
        }
        path = root / "batch.json"
        path.write_text(json.dumps(checkpoint))
        result = subprocess.run([str(CLI), "advise", "--context", str(path)], text=True,
                                capture_output=True, env=environment, check=False)
        if result.returncode:
            print(result.stderr, file=sys.stderr)
            return result.returncode
        answer = json.loads(result.stdout)
        if answer.get("status") != "evaluated" or answer.get("action") != "troubleshooting" or answer.get("shadow") is not True:
            print(f"FAIL: live shadow batch: {answer}", file=sys.stderr)
            return 1
        record = json.loads(Path(answer["record_path"]).read_text())
        if record.get("model_returned") != record.get("model_requested") or not record.get("usage"):
            print("FAIL: live batch missing model or usage", file=sys.stderr)
            return 1
        print("PASS: live Choice/Score/Boolean batch; baseline preserved, model and usage recorded")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
