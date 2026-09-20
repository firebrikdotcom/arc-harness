"""Offline tests for dynamic TypeSafe advice during agent work."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from collections import Counter
from pathlib import Path

from live_context_advice import CASES, context as case_context


ROOT = Path(__file__).resolve().parent.parent
SPEC = importlib.util.spec_from_file_location("context_advice", ROOT / "scripts/context_advice.py")
assert SPEC and SPEC.loader
ADVICE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ADVICE)

CONTEXT = {
    "version": 1,
    "decision": {"question": "Which persistence model best fits this ledger?"},
    "context": {
        "goal": "Maintain an auditable transfer ledger.",
        "facts": ["Transfers can be reversed."],
        "constraints": ["Financial correctness is mandatory."],
    },
    "options": [
        {"id": "crud", "description": "CRUD tables plus an audit log."},
        {"id": "event_sourcing", "description": "Events are the ledger source of truth."},
    ],
}


class FakeRouter:
    Counter = Counter
    DEFAULT_TIMEOUT = 1

    def __init__(self, choice: str = "event_sourcing") -> None:
        self.choice = choice
        self.calls: list[dict] = []

    @staticmethod
    def redact(value, counts):
        def visit(item):
            if isinstance(item, dict):
                for key, child in item.items():
                    if "secret" in key or "password" in key:
                        counts["key_name"] += 1
                    visit(child)
            elif isinstance(item, list):
                for child in item:
                    visit(child)
            elif isinstance(item, str) and "forbidden-token" in item:
                counts["token"] += 1
        visit(value)
        return value

    @staticmethod
    def pinned_model():
        return "fixture"

    @staticmethod
    def api_url():
        return "https://fixture.invalid"

    @staticmethod
    def resolve_api_key():
        return "fixture", "fixture"

    def post_json(self, url, payload, key, timeout):
        self.calls.append(payload)
        return {"answers": {"recommendation": {"choice": self.choice, "confidence": 0.92}}}, {"http_status": 200, "latency_ms": 4}

    @staticmethod
    def requests_log_path(started):
        return Path("/tmp/unused-typesafe-log.jsonl")

    @staticmethod
    def append_jsonl(path, record):
        return None


class ContextAdviceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="harness-context-advice-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.path = self.root / "context.json"

    def write(self, value) -> None:
        self.path.write_text(json.dumps(value))

    def test_dynamic_options_are_sent_and_saved(self) -> None:
        self.write(CONTEXT)
        router = FakeRouter()
        result = ADVICE.advise(ADVICE.load_context(self.path, router), router, self.root / "db")
        self.assertEqual(result["choice"], "event_sourcing")
        self.assertEqual(set(router.calls[0]["questions"]["recommendation"]["criteria"]), {"crud", "event_sourcing"})
        record = Path(result["record_path"])
        self.assertTrue(record.is_file())
        self.assertEqual(record.stat().st_mode & 0o777, 0o600)

    def test_five_unrelated_contexts_are_forwarded_without_a_domain_template(self) -> None:
        for case in CASES:
            with self.subTest(case=case["name"]):
                self.write(case_context(case))
                router = FakeRouter(choice=case["options"][0][0])
                result = ADVICE.advise(ADVICE.load_context(self.path, router), router, self.root / case["name"])
                expected = {identifier for identifier, _ in case["options"]}
                self.assertEqual(set(router.calls[0]["questions"]["recommendation"]["criteria"]), expected)
                self.assertIn(result["choice"], expected)
        implementation = (ROOT / "scripts/context_advice.py").read_text().lower()
        for subject in ("banking", "clinic", "inspection", "storefront", "staff portal"):
            self.assertNotIn(subject, implementation)

    def test_secret_like_context_is_rejected_before_call(self) -> None:
        unsafe = json.loads(json.dumps(CONTEXT))
        unsafe["context"]["goal"] = "Maintain an auditable transfer ledger with forbidden-token."
        self.write(unsafe)
        router = FakeRouter()
        with self.assertRaisesRegex(ADVICE.InputError, "sensitive content"):
            ADVICE.load_context(self.path, router)
        self.assertEqual(router.calls, [])

    def test_unknown_choice_is_rejected(self) -> None:
        self.write(CONTEXT)
        router = FakeRouter(choice="outside_options")
        with self.assertRaisesRegex(ADVICE.InputError, "outside the supplied options"):
            ADVICE.advise(ADVICE.load_context(self.path, router), router, self.root / "db")


if __name__ == "__main__":
    unittest.main()
