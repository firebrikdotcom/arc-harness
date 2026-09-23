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


if __name__ == "__main__":
    unittest.main()
