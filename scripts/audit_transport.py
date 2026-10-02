"""Persisted collection switches and retryable local telemetry delivery."""
from __future__ import annotations
import json
import os
import sqlite3
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAX_RECORD_BYTES = 32_000

def settings_path() -> Path:
    return Path(os.environ.get("HARNESS_AUDIT_SETTINGS", ROOT / ".harness-db/audit-settings.json"))

def category(event_type: str) -> str:
    return "workflow" if event_type.startswith("workflow.") else "jev"

def enabled(event_type: str = "jev.route") -> bool:
    # Explicit process-level opt-out remains a hard stop.
    if os.environ.get("HARNESS_AUDIT_ENABLED") == "0":
        return False
    path = settings_path()
    if path.exists():
        config = json.loads(path.read_text())
        if any(type(config.get(key)) is not bool for key in ("jev", "workflow")):
            raise ValueError("invalid audit collection settings")
        if type(config.get("workflow_prompts", False)) is not bool:
            raise ValueError("invalid prompt collection setting")
        if event_type == "workflow.prompt_recorded":
            return config["workflow"] and config.get("workflow_prompts", False)
        return config[category(event_type)]
    return event_type != "workflow.prompt_recorded" and os.environ.get("HARNESS_AUDIT_ENABLED") == "1"

def audit_url() -> str:
    return os.environ.get("HARNESS_AUDIT_URL", "http://127.0.0.1:18080").rstrip("/")

def outbox_path() -> Path:
    return Path(os.environ.get("HARNESS_AUDIT_OUTBOX", ROOT / ".harness-db/audit-outbox.sqlite"))

def connection() -> sqlite3.Connection:
    path = outbox_path()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    db = sqlite3.connect(path, timeout=5)
    path.chmod(0o600)
    db.execute("CREATE TABLE IF NOT EXISTS outbox (id TEXT PRIMARY KEY, event_type TEXT NOT NULL, body TEXT NOT NULL, destination TEXT NOT NULL)")
    return db

def flush() -> bool:
    """Retry until the queue is empty or a transport error occurs (bounded time)."""
    deadline = time.monotonic() + 2
    with connection() as db:
        rows = db.execute("SELECT id,event_type,body,destination FROM outbox ORDER BY rowid").fetchall()
        for event_id, event_type, body, destination in rows:
            if not enabled(event_type):
                # Turning collection off also discards undelivered events of that kind.
                db.execute("DELETE FROM outbox WHERE id=?", (event_id,))
                continue
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            request = urllib.request.Request(destination + "/api/audit/events", data=body.encode(),
                headers={"Content-Type": "application/json", "Accept": "application/json"}, method="POST")
            try:
                with urllib.request.urlopen(request, timeout=remaining) as response:
                    if not 200 <= response.status < 300:
                        return False
            except (OSError, urllib.error.URLError) as error:
                print(f"AUDIT DELIVERY QUEUED: {type(error).__name__}", file=sys.stderr)
                return False
            db.execute("DELETE FROM outbox WHERE id=?", (event_id,))
            db.commit()
    return True

def emit(event_type: str, payload: dict, *, flush_now: bool = True) -> bool:
    if not enabled(event_type):
        return True
    body = json.dumps({"id": str(uuid.uuid4()), "event_type": event_type, "payload": payload},
                      separators=(",", ":"), allow_nan=False)
    if len(body.encode()) > MAX_RECORD_BYTES:
        raise ValueError("audit event is too large")
    event_id = json.loads(body)["id"]
    with connection() as db:
        db.execute("INSERT INTO outbox VALUES (?,?,?,?)", (event_id, event_type, body, audit_url()))
    return flush() if flush_now else True
