import json
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))
os.environ.setdefault("HARNESS_TYPESAFE_BASE_ROUTER", str(Path(__file__).resolve().parent / "fake_router.py"))
import codex_router  # noqa: E402
from context_advice import validated_answers  # noqa: E402

QUESTIONS = {
    "recommendation": {"type": "choice", "instructions": "Pick one.", "criteria": {"grep": "Grep", "retrieval": "Search"}},
    "risk": {"type": "score", "instructions": "Risk.", "criteria": ["low", "mid", "high"]},
    "enough": {"type": "noul", "instructions": "Evidence suffices."},
}
ANSWERS = {"recommendation": {"choice": "retrieval", "confidence": 0.9},
           "risk": {"score": 1, "confidence": 0.8}, "enough": {"noul": 0.7}}


def events(answers=ANSWERS):
    return "\n".join(json.dumps(e) for e in (
        {"type": "thread.started"},
        {"type": "item.completed", "item": {"type": "agent_message", "text": json.dumps({"answers": answers})}},
        {"type": "turn.completed", "usage": {"input_tokens": 100, "output_tokens": 7}}))


class CodexRouterTests(unittest.TestCase):
    def payload(self, model="codex:gpt-test"):
        return {"model": model, "state": {"facts": ["target unfamiliar"]}, "questions": QUESTIONS}

    def test_answers_pass_harness_validation_and_report_model_and_usage(self):
        calls = []

        def runner(argv, **kwargs):
            calls.append((argv, kwargs))
            schema = json.loads(Path(argv[argv.index("--output-schema") + 1]).read_text())
            self.assertEqual(set(schema["properties"]["answers"]["required"]), set(QUESTIONS))
            return subprocess.CompletedProcess(argv, 0, events(), "")

        with mock.patch.dict(os.environ, {"HARNESS_RUN_ID": "x", "CODEX_THREAD_ID": "t", "CODEX_HOME": "/h"}):
            response, meta = codex_router.post_json(codex_router.api_url(), self.payload(), "", 5, runner=runner)
        argv, kwargs = calls[0]
        self.assertEqual(argv[argv.index("-m") + 1], "gpt-test")
        self.assertIn("read-only", argv)
        self.assertNotIn("HARNESS_RUN_ID", kwargs["env"])
        self.assertNotIn("CODEX_THREAD_ID", kwargs["env"])
        self.assertEqual(kwargs["env"]["CODEX_HOME"], "/h")
        self.assertEqual(kwargs["env"]["HARNESS_AUTO_INIT"], "0")
        self.assertIn("target unfamiliar", kwargs["input"])
        self.assertEqual(response["model"], "codex:gpt-test")
        self.assertEqual(response["usage"], {"input_tokens": 100, "output_tokens": 7})
        self.assertEqual(meta["attempts"], 1)
        clean = validated_answers(QUESTIONS, response)
        self.assertEqual(clean["recommendation"]["choice"], "retrieval")
        self.assertEqual(clean["enough"]["probability"], 0.7)

    def test_jev_model_pin_is_refused(self):
        with self.assertRaises(codex_router.BadInputError):
            codex_router.post_json("", self.payload(model="jev-1.13.0"), "", 5, runner=None)

    def test_nested_call_is_refused(self):
        with mock.patch.dict(os.environ, {codex_router.CHILD_ENV: "1"}):
            with self.assertRaises(codex_router.BadInputError):
                codex_router.post_json("", self.payload(), "", 5, runner=None)

    def test_failures_and_timeouts_retry_then_raise_unavailable(self):
        outcomes = [subprocess.TimeoutExpired("codex", 5), subprocess.CompletedProcess([], 1, "", "")]

        def runner(argv, **kwargs):
            outcome = outcomes.pop(0)
            if isinstance(outcome, Exception):
                raise outcome
            return outcome

        with self.assertRaises(codex_router.UnavailableError):
            codex_router.post_json("", self.payload(), "", 5, attempts=2, runner=runner)
        self.assertEqual(outcomes, [])

    def test_missing_answer_is_unavailable(self):
        runner = lambda argv, **kwargs: subprocess.CompletedProcess(argv, 0, '{"type":"turn.completed"}', "")
        with self.assertRaises(codex_router.UnavailableError):
            codex_router.post_json("", self.payload(), "", 5, runner=runner)

    def test_pinned_model_ignores_jev_pin(self):
        with mock.patch.dict(os.environ, {"TYPESAFE_MODEL": "jev-1.13.0"}):
            self.assertEqual(codex_router.pinned_model(), codex_router.DEFAULT_MODEL)
        with mock.patch.dict(os.environ, {"TYPESAFE_MODEL": "codex:gpt-x"}):
            self.assertEqual(codex_router.pinned_model(), "codex:gpt-x")


if __name__ == "__main__":
    unittest.main()
