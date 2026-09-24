"""Offline tests for dynamic TypeSafe advice during agent work."""

from __future__ import annotations

import importlib.util
import json
import os
import tempfile
import unittest
import unittest.mock
import copy
import subprocess
import sys
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
        # Fixture checkpoints must never reach the machine's Arc audit service.
        environment = unittest.mock.patch.dict(os.environ, {"HARNESS_AUDIT_ENABLED": "0"})
        environment.start()
        self.addCleanup(environment.stop)
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
        result = ADVICE.advise(ADVICE.load_context(self.path, router), router, self.root / "db")
        self.assertEqual(result["status"], "fallback")
        self.assertNotIn("choice", result)


    def checkpoint(self):
        return {
            "version": 2,
            "checkpoint": {"family": "tool_selection", "question_version": "1", "policy_version": "shadow-1",
                           "baseline_action": "inspect", "bypass_reason": "none", "state_build_ms": 12},
            "context": {"goal": "Choose the next bounded investigation step.", "facts": ["Evidence is incomplete."]},
            "questions": {
                "recommendation": {"type": "choice", "instructions": "Which next step resolves the missing evidence?",
                                   "criteria": {"inspect": "Inspect the current evidence", "trace": "Trace a related call"}},
                "impact": {"type": "score", "instructions": "Assess impact", "criteria": ["Local", "Broad"]},
                "sufficient": {"type": "boolean", "instructions": "The evidence is sufficient"},
            },
        }

    def batch_router(self, response=None):
        router = FakeRouter()
        expected = response or {"model": "fixture", "usage": {"input_tokens": 10, "output_tokens": 3}, "answers": {
            "recommendation": {"choice": "trace", "confidence": .8, "probabilities": {"inspect": .1, "trace": .9}},
            "impact": {"score": .4, "confidence": .2, "probabilities": {"0": .6, "1": .4}},
            "sufficient": {"noul": .05},
        }}
        def post(url, payload, key, timeout):
            router.calls.append(payload)
            records = list((self.root / "db" / "advice").glob("*.json"))
            self.assertTrue(any(json.loads(p.read_text())["status"] == "pending" for p in records))
            self.assertNotIn("checkpoint", payload["state"])
            self.assertNotIn("baseline_action", payload["state"])
            self.assertEqual(payload["questions"]["sufficient"]["type"], "noul")
            return copy.deepcopy(expected), {"latency_ms": 4, "http_status": 200}
        router.post_json = post
        return router

    def evaluate(self, context=None, router=None):
        self.write(context or self.checkpoint())
        router = router or self.batch_router()
        return ADVICE.advise(ADVICE.load_context(self.path, router), router, self.root / "db")

    def test_batch_shadow_preserves_baseline_and_distinct_measures(self):
        router = self.batch_router()
        result = self.evaluate(router=router)
        self.assertEqual(len(router.calls), 1)
        self.assertEqual(result["action"], "inspect")
        self.assertEqual(result["answers"]["recommendation"]["choice"], "trace")
        self.assertEqual(result["answers"]["sufficient"]["probability"], .05)
        self.assertEqual(result["answers"]["impact"]["score"], .4)
        self.assertEqual(result["status"], "evaluated")
        record = json.loads(Path(result["record_path"]).read_text())
        self.assertEqual(record["model_returned"], "fixture")
        self.assertEqual(record["usage"]["input_tokens"], 10)

    def test_all_deterministic_bypasses_avoid_network(self):
        for reason in ADVICE.BYPASSES - {"none"}:
            context = self.checkpoint()
            context["checkpoint"]["bypass_reason"] = reason
            context["questions"] = {}
            router = FakeRouter()
            result = self.evaluate(context, router)
            self.assertEqual(result["status"], "bypassed")
            self.assertEqual(result["action"], "inspect")
            self.assertEqual(router.calls, [])

    def test_missing_distributions_remain_unknown(self):
        response = {"model": "fixture", "answers": {
            "recommendation": {"choice": "trace", "confidence": .8},
            "impact": {"score": .4, "confidence": .2}, "sufficient": {"noul": .05}}}
        result = self.evaluate(router=self.batch_router(response))
        self.assertEqual(result["status"], "evaluated")
        self.assertIsNone(result["answers"]["impact"]["probabilities"])
        self.assertIsNone(json.loads(Path(result["record_path"]).read_text())["usage"])

    def test_invalid_answers_drift_and_transport_use_fallback(self):
        valid = {"model": "fixture", "answers": {
            "recommendation": {"choice": "trace", "confidence": .8},
            "impact": {"score": .4, "confidence": .2}, "sufficient": {"noul": .05}}}
        cases = []
        for field, value in [("confidence", float("nan")), ("choice", "unknown"),
                             ("probabilities", {"trace": 1}), ("confidence", True)]:
            response = copy.deepcopy(valid)
            response["answers"]["recommendation"][field] = value
            cases.append(response)
        response = copy.deepcopy(valid); response["answers"]["sufficient"]["noul"] = 2; cases.append(response)
        response = copy.deepcopy(valid); response["answers"]["impact"]["score"] = 2; cases.append(response)
        response = copy.deepcopy(valid); del response["answers"]["impact"]; cases.append(response)
        response = copy.deepcopy(valid); response["model"] = "different"; cases.append(response)
        for response in cases:
            with self.subTest(response=response):
                result = self.evaluate(router=self.batch_router(response))
                self.assertEqual(result["status"], "fallback")
                self.assertEqual(result["action"], "inspect")
        router = self.batch_router()
        def fail(*args):
            raise TimeoutError("forbidden-token")
        router.post_json = fail
        result = self.evaluate(router=router)
        self.assertEqual(result["status"], "fallback")
        self.assertNotIn("forbidden-token", Path(result["record_path"]).read_text())

    def test_input_size_versions_and_secret_rejected(self):
        for mutate in [lambda c: c.update(version=True),
                       lambda c: c["checkpoint"].update(baseline_action=""),
                       lambda c: c["context"].update(goal="forbidden-token"),
                       lambda c: c.update(questions={})]:
            c = self.checkpoint(); mutate(c); self.write(c)
            with self.assertRaises(ADVICE.InputError):
                ADVICE.load_context(self.path, FakeRouter())
        self.path.write_text(" " * (ADVICE.MAX_BYTES + 1))
        with self.assertRaisesRegex(ADVICE.InputError, "exceeds"):
            ADVICE.load_context(self.path, FakeRouter())

    def test_outcomes_are_linked_once_and_reported_by_cohort(self):
        first = self.evaluate()
        context = self.checkpoint(); context["checkpoint"]["policy_version"] = "shadow-2"
        self.evaluate(context)
        outcome = {"call_id": first["call_id"], "action_taken": "inspect", "outcome": "incorrect",
                   "evidence": "Independent inspection established that the original step resolved the question.",
                   "total_decision_ms": 30, "baseline_ms": 20, "rework_ms": 0}
        p = self.root / "outcome.json"; p.write_text(json.dumps(outcome))
        ADVICE.record_outcome(p, FakeRouter(), self.root / "db")
        with self.assertRaisesRegex(ADVICE.InputError, "already recorded"):
            ADVICE.record_outcome(p, FakeRouter(), self.root / "db")
        report = ADVICE.report(self.root / "db")
        self.assertFalse(report["automatic_promotion"])
        self.assertEqual(len(report["cohorts"]), 2)
        group = next(g for g in report["cohorts"] if g["labeled"])
        self.assertEqual(group["accuracy"], 0)
        self.assertEqual(group["disagreements"], 1)
        self.assertEqual(group["paired_delta_ms"], 10)
        self.assertEqual(group["tokens"], 13)
        unlabeled = next(g for g in report["cohorts"] if not g["labeled"])
        self.assertIsNone(unlabeled["accuracy"])
        self.assertIsNone(unlabeled["paired_delta_ms"])
        output = subprocess.run([sys.executable, str(ROOT / "scripts/context_advice.py"), "--report",
                                 "--router", "/missing", "--db-root", str(self.root / "db")],
                                check=True, capture_output=True, text=True)
        self.assertEqual(len(json.loads(output.stdout)["cohorts"]), 2)

    def test_flag_form_checkpoint_pending_and_label_round_trip(self):
        router = ROOT / "tests" / "fake_router.py"
        db = self.root / "db"
        env = {**os.environ, "FAKE_ROUTER_LOG_DIR": str(self.root / "logs"), "HARNESS_AUDIT_ENABLED": "0"}
        cli = [sys.executable, str(ROOT / "scripts/context_advice.py"), "--db-root", str(db), "--router", str(router)]
        quick = subprocess.run(cli + ["--family", "tool_selection", "--baseline", "inspect",
                                      "--goal", "Choose the next read-only step to locate a handler.",
                                      "--fact", "The graph result is stale.", "--constraint", "Coverage rules remain mandatory.",
                                      "--choice", "inspect=Read the current candidate source.", "--choice", "trace=Trace a related caller.",
                                      "--boolean", "sufficient=The supplied evidence establishes the handler location.",
                                      "--score", "impact=How much investigation would a wrong step waste?:One short read|Several steps"],
                               env=env, capture_output=True, text=True, check=False)
        self.assertEqual(quick.returncode, 0, quick.stderr)
        result = json.loads(quick.stdout)
        self.assertEqual((result["status"], result["shadow"], result["action"]), ("evaluated", True, "inspect"))
        self.assertEqual(set(result["answers"]), {"recommendation", "sufficient", "impact"})
        record = json.loads(Path(result["record_path"]).read_text())
        self.assertEqual((record["family"], record["question_version"], record["policy_version"]), ("tool_selection", "quick-1", "shadow-1"))
        pending = json.loads(subprocess.run(cli + ["--pending"], env=env, capture_output=True, text=True, check=True).stdout)
        self.assertEqual([row["call_id"] for row in pending["unlabeled"]], [result["call_id"]])
        label = subprocess.run(cli + ["--label", result["call_id"], "--outcome", "correct", "--action-taken", "inspect",
                                      "--evidence", "The source read located the handler."], env=env, capture_output=True, text=True, check=False)
        self.assertEqual(label.returncode, 0, label.stderr)
        report = json.loads(subprocess.run(cli + ["--report"], env=env, capture_output=True, text=True, check=True).stdout)
        self.assertEqual((report["pilot"]["labeled"], report["pilot"]["correct"], report["pilot"]["remaining"]), (1, 1, 29))
        self.assertEqual(json.loads(subprocess.run(cli + ["--pending"], env=env, capture_output=True, text=True, check=True).stdout)["unlabeled_total"], 0)
        incomplete = subprocess.run(cli + ["--label", result["call_id"], "--outcome", "correct"], env=env, capture_output=True, text=True, check=False)
        self.assertEqual(incomplete.returncode, 2)
        self.assertIn("--label requires", incomplete.stderr)
        malformed = subprocess.run(cli + ["--family", "tool_selection", "--baseline", "x", "--goal", "g", "--choice", "no-separator"],
                                   env=env, capture_output=True, text=True, check=False)
        self.assertEqual(malformed.returncode, 2)

    def test_timeout_and_attempts_environment_is_validated(self):
        with unittest.mock.patch.dict(os.environ, {"HARNESS_JEV_TIMEOUT": "nope"}):
            with self.assertRaises(ADVICE.InputError):
                ADVICE.request_timeout(FakeRouter())
        with unittest.mock.patch.dict(os.environ, {"HARNESS_JEV_TIMEOUT": "2.5", "HARNESS_JEV_ATTEMPTS": "1"}):
            self.assertEqual(ADVICE.request_timeout(FakeRouter()), 2.5)
            self.assertEqual(ADVICE.request_options(), {"attempts": 1})
        with unittest.mock.patch.dict(os.environ, {"HARNESS_JEV_ATTEMPTS": "9"}):
            with self.assertRaises(ADVICE.InputError):
                ADVICE.request_options()

    def test_fallback_cannot_be_labeled_correct(self):
        context = self.checkpoint(); context["checkpoint"]["bypass_reason"] = "user_choice"
        result = self.evaluate(context)
        self.write({"call_id": result["call_id"], "action_taken": "inspect", "outcome": "correct", "evidence": "Checked"})
        with self.assertRaisesRegex(ADVICE.InputError, "no usable"):
            ADVICE.record_outcome(self.path, FakeRouter(), self.root / "db")


if __name__ == "__main__":
    unittest.main()
