"""Offline coverage for the Arc telemetry bridge."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
EMITTER = ROOT / "scripts" / "audit_emit.py"


class CaptureHandler(BaseHTTPRequestHandler):
    events: list[dict] = []

    def do_POST(self) -> None:  # noqa: N802 - stdlib handler API
        length = int(self.headers["Content-Length"])
        self.events.append(json.loads(self.rfile.read(length)))
        self.send_response(201)
        self.end_headers()

    def log_message(self, *_args: object) -> None:
        return


class AuditEmitterTests(unittest.TestCase):
    def setUp(self) -> None:
        CaptureHandler.events = []
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), CaptureHandler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.temp = tempfile.TemporaryDirectory(prefix="harness-audit-emitter-")
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.record = Path(self.temp.name) / "route.json"
        self.record.write_text(json.dumps({
            "id": "route-1",
            "call_id": None,
            "source": "typesafe",
            "recommendation": "default",
            "observed_recommendation": "proceed",
            "mode": "shadow",
            "routing_mode": "shadow",
            "usage": {"input_tokens": 12, "output_tokens": 3},
            "latency_ms": 7,
        }))
        self.env = {
            **os.environ,
            "HARNESS_AUDIT_ENABLED": "1",
            "HARNESS_AUDIT_SETTINGS": str(Path(self.temp.name) / "settings.json"),
            "HARNESS_AUDIT_OUTBOX": str(Path(self.temp.name) / "outbox.sqlite"),
            "HARNESS_AUDIT_URL": f"http://127.0.0.1:{self.server.server_port}",
        }

    def run_emitter(self, *args: str) -> None:
        result = subprocess.run(
            [sys.executable, str(EMITTER), *args],
            env=self.env,
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_route_and_agent_events_keep_measurements_compact(self) -> None:
        self.run_emitter("route", "--record", str(self.record))
        self.run_emitter(
            "agent",
            "--record", str(self.record),
            "--profile", "routine",
            "--exit-code", "0",
            "--input-tokens", "100",
            "--output-tokens", "25",
        )
        self.assertEqual(len(CaptureHandler.events), 2)
        route = CaptureHandler.events[0]["payload"]
        agent = CaptureHandler.events[1]["payload"]
        self.assertEqual((route["jev_input_tokens"], route["jev_output_tokens"]), (12, 3))
        self.assertEqual(agent["agent_total_tokens"], 125)
        self.assertEqual(agent["token_measurement_status"], "provided")
        self.assertNotIn("evidence", json.dumps(CaptureHandler.events))

    def test_checkpoint_events_carry_facts_but_no_context(self) -> None:
        advice = Path(self.temp.name) / "advice.json"
        advice.write_text(json.dumps({
            "call_id": "11111111-1111-4111-8111-111111111111", "family": "evidence_assessment",
            "question_version": "verify-predict-1", "policy_version": "shadow-1", "question_hash": "abc",
            "status": "evaluated", "fallback_reason": None, "shadow": True, "baseline_action": "run_full_verification",
            "context": {"goal": "PRIVATE GOAL TEXT", "facts": ["PRIVATE FACT"]},
            "questions": {"recommendation": {}, "will_pass": {}},
            "answers": {"recommendation": {"type": "choice", "choice": "run_full_verification", "confidence": 0.9}},
            "usage": {"input_tokens": 30, "output_tokens": 5}, "latency_ms": 40, "model_requested": "jev-1.13.0", "model_returned": "jev-1.13.0",
        }))
        outcome = Path(self.temp.name) / "outcome.json"
        outcome.write_text(json.dumps({"call_id": "11111111-1111-4111-8111-111111111111", "outcome": "correct",
                                       "action_taken": "run_full_verification", "evidence": "Verification exited 0."}))
        self.run_emitter("checkpoint", "--record", str(advice))
        self.run_emitter("checkpoint-outcome", "--record", str(advice), "--outcome-record", str(outcome))
        self.assertEqual([event["event_type"] for event in CaptureHandler.events], ["jev.checkpoint", "jev.checkpoint_outcome"])
        checkpoint, labeled = (event["payload"] for event in CaptureHandler.events)
        self.assertEqual((checkpoint["family"], checkpoint["recommendation"], checkpoint["question_count"], checkpoint["jev_input_tokens"]),
                         ("evidence_assessment", "run_full_verification", 2, 30))
        self.assertEqual((labeled["outcome"], labeled["action_taken"]), ("correct", "run_full_verification"))
        self.assertEqual(len(labeled["evidence_sha256"]), 64)
        serialized = json.dumps(CaptureHandler.events)
        for private in ("PRIVATE", "Verification exited"):
            self.assertNotIn(private, serialized)


if __name__ == "__main__":
    unittest.main()
