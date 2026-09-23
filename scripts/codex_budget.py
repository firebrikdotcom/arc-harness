#!/usr/bin/env python3
"""Measure and enforce a Codex thread token budget through App Server."""

from __future__ import annotations

import argparse
import json
import os
import selectors
import subprocess
import sys
import time
from pathlib import Path
from typing import Any


EXIT_BUDGET = 3
TERMINAL_GOAL_STATUSES = {"paused", "blocked", "usageLimited", "budgetLimited", "complete"}


class BudgetError(RuntimeError):
    """Raised when the budget controller cannot safely continue."""


class AppServer:
    """Minimal newline-delimited JSON-RPC client for `codex app-server proxy`."""

    def __init__(self, codex: str, request_timeout: float) -> None:
        self.request_timeout = request_timeout
        self.next_id = 0
        self.active_turns: dict[str, str] = {}
        self.read_buffer = b""
        self.process = subprocess.Popen(
            [codex, "app-server", "proxy"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
        )
        if self.process.stdin is None or self.process.stdout is None:
            raise BudgetError("Codex App Server did not provide stdin/stdout pipes")
        self.selector = selectors.DefaultSelector()
        self.selector.register(self.process.stdout, selectors.EVENT_READ)
        self.call(
            "initialize",
            {
                "clientInfo": {
                    "name": "harness_budget",
                    "title": "Harness Budget",
                    "version": "1",
                }
            },
        )
        self.notify("initialized", {})

    def send(self, message: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        try:
            payload = (json.dumps(message, separators=(",", ":")) + "\n").encode()
            self.process.stdin.write(payload)
            self.process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise BudgetError(f"Codex App Server connection closed: {error}") from error

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self.send({"method": method, "params": params})

    def call(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self.next_id += 1
        request_id = self.next_id
        self.send({"method": method, "id": request_id, "params": params})
        deadline = time.monotonic() + self.request_timeout
        while True:
            message = self.receive(deadline)
            self.observe(message)
            if message.get("id") != request_id:
                continue
            if "error" in message:
                error = message["error"]
                detail = error.get("message", error) if isinstance(error, dict) else error
                raise BudgetError(f"Codex App Server rejected {method}: {detail}")
            result = message.get("result", {})
            if not isinstance(result, dict):
                raise BudgetError(f"Codex App Server returned an invalid {method} result")
            return result

    def receive(self, deadline: float) -> dict[str, Any]:
        assert self.process.stdout is not None
        while b"\n" not in self.read_buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not self.selector.select(remaining):
                raise BudgetError("timed out waiting for Codex App Server")
            chunk = os.read(self.process.stdout.fileno(), 65536)
            if not chunk:
                detail = ""
                if self.process.stderr is not None:
                    detail = self.process.stderr.read().decode(errors="replace").strip()
                suffix = f": {detail}" if detail else ""
                raise BudgetError(f"Codex App Server exited unexpectedly{suffix}")
            self.read_buffer += chunk
        raw_line, self.read_buffer = self.read_buffer.split(b"\n", 1)
        try:
            message = json.loads(raw_line.decode())
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise BudgetError(f"Codex App Server returned invalid JSON: {error}") from error
        if not isinstance(message, dict):
            raise BudgetError("Codex App Server returned a non-object message")
        return message

    def observe(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        params = message.get("params")
        if not isinstance(params, dict):
            return
        thread_id = params.get("threadId")
        if not isinstance(thread_id, str):
            return
        if method == "turn/started":
            turn = params.get("turn")
            if isinstance(turn, dict) and isinstance(turn.get("id"), str):
                self.active_turns[thread_id] = turn["id"]
        elif method in {"thread/tokenUsage/updated", "thread/goal/updated"}:
            turn_id = params.get("turnId")
            if isinstance(turn_id, str):
                self.active_turns[thread_id] = turn_id
        elif method == "turn/completed":
            turn = params.get("turn")
            if isinstance(turn, dict) and turn.get("id") == self.active_turns.get(thread_id):
                self.active_turns.pop(thread_id, None)

    def close(self) -> None:
        self.selector.close()
        if self.process.stdin is not None:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2)


def goal_for_thread(server: AppServer, thread_id: str) -> dict[str, Any] | None:
    goal = server.call("thread/goal/get", {"threadId": thread_id}).get("goal")
    if goal is not None and not isinstance(goal, dict):
        raise BudgetError("Codex App Server returned an invalid thread goal")
    return goal


def set_budget(
    server: AppServer,
    thread_id: str,
    token_budget: int,
    objective: str,
) -> dict[str, Any]:
    existing = goal_for_thread(server, thread_id)
    params: dict[str, Any] = {
        "threadId": thread_id,
        "status": "active",
        "tokenBudget": token_budget,
    }
    if existing is None:
        params["objective"] = objective
    elif existing.get("status") != "active":
        raise BudgetError(
            f"thread {thread_id} has goal status {existing.get('status')}; "
            "resume or start a new goal deliberately before attaching a budget"
        )
    result = server.call("thread/goal/set", params)
    goal = result.get("goal")
    if not isinstance(goal, dict):
        raise BudgetError("Codex App Server did not return the updated goal")
    return goal


def discover_thread(
    server: AppServer,
    cwd: Path,
    started_at: int,
    timeout: float,
    interval: float,
    excluded_thread: str | None,
) -> str:
    deadline = time.monotonic() + timeout
    expected_cwd = str(cwd.resolve())
    while True:
        result = server.call(
            "thread/list",
            {
                "cwd": expected_cwd,
                "limit": 20,
                "sortKey": "created_at",
                "sortDirection": "desc",
                "sourceKinds": ["appServer"],
            },
        )
        candidates = []
        for thread in result.get("data", []):
            if not isinstance(thread, dict) or thread.get("id") == excluded_thread:
                continue
            if thread.get("cwd") != expected_cwd:
                continue
            status = thread.get("status")
            if not isinstance(status, dict) or status.get("type") == "notLoaded":
                continue
            created_at = thread.get("createdAt")
            if isinstance(created_at, int) and created_at >= started_at - 1:
                candidates.append(thread)
        if candidates:
            candidates.sort(key=lambda item: (item["createdAt"], item["id"]), reverse=True)
            return str(candidates[0]["id"])
        if time.monotonic() >= deadline:
            raise BudgetError(f"no new Codex thread appeared for {expected_cwd} within {timeout:g}s")
        time.sleep(interval)


def active_turn(server: AppServer, thread_id: str) -> str | None:
    cached = server.active_turns.get(thread_id)
    if cached:
        return cached
    result = server.call("thread/read", {"threadId": thread_id, "includeTurns": True})
    thread = result.get("thread")
    if not isinstance(thread, dict):
        raise BudgetError("Codex App Server did not return the requested thread")
    turns = thread.get("turns", [])
    if not isinstance(turns, list):
        raise BudgetError("Codex App Server returned invalid thread turns")
    for turn in reversed(turns):
        if isinstance(turn, dict) and turn.get("status") == "inProgress" and isinstance(turn.get("id"), str):
            return turn["id"]
    return None


def report_tokens(harness: Path, db_root: Path, delta: int) -> bool:
    if delta <= 0:
        return False
    environment = os.environ.copy()
    environment["HARNESS_DB_ROOT"] = str(db_root)
    result = subprocess.run(
        [str(harness), "step", "--tokens", str(delta), "--note", "Codex token-budget meter"],
        env=environment,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode not in {0, EXIT_BUDGET}:
        detail = (result.stderr or result.stdout).strip()
        raise BudgetError(f"harness rejected a {delta}-token update: {detail or result.returncode}")
    if delta > 0 and os.environ.get("HARNESS_AUDIT_ENABLED") == "1":
        emitter = harness.parent / "audit_emit.py"
        route_record = os.environ.get("HARNESS_ROUTE_RECORD")
        command = [sys.executable, str(emitter), "token", "--delta", str(delta)]
        if route_record:
            command.extend(["--record", route_record])
        subprocess.run(command, env=environment, capture_output=True, check=False)
    return result.returncode == EXIT_BUDGET


def mark_budget_limited(server: AppServer, thread_id: str) -> None:
    server.call("thread/goal/set", {"threadId": thread_id, "status": "budgetLimited"})


def watch_budget(
    server: AppServer,
    thread_id: str,
    token_budget: int,
    initial_used: int,
    interval: float,
    harness: Path | None,
    db_root: Path | None,
) -> int:
    previous = initial_used
    while True:
        goal = goal_for_thread(server, thread_id)
        if goal is None:
            raise BudgetError(f"thread goal disappeared for {thread_id}")
        used = goal.get("tokensUsed")
        if not isinstance(used, int) or used < 0:
            raise BudgetError("Codex App Server returned an invalid tokensUsed value")
        harness_paused = False
        if harness is not None and db_root is not None:
            harness_paused = report_tokens(harness, db_root, max(0, used - previous))
        previous = max(previous, used)
        if harness_paused or used >= token_budget or goal.get("status") == "budgetLimited":
            turn_id = active_turn(server, thread_id)
            if turn_id is not None:
                server.call("turn/interrupt", {"threadId": thread_id, "turnId": turn_id})
            if not harness_paused and goal.get("status") != "budgetLimited":
                mark_budget_limited(server, thread_id)
            print(
                json.dumps(
                    {
                        "status": "harnessPaused" if harness_paused else "budgetLimited",
                        "thread_id": thread_id,
                        "turn_id": turn_id,
                        "tokens_used": used,
                        "token_budget": token_budget,
                    },
                    sort_keys=True,
                )
            )
            return EXIT_BUDGET
        if goal.get("status") in TERMINAL_GOAL_STATUSES:
            print(json.dumps({"status": goal["status"], "thread_id": thread_id, "tokens_used": used}, sort_keys=True))
            return 0
        time.sleep(interval)


def positive_float(value: str) -> float:
    try:
        number = float(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be a number") from error
    if number <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def non_negative_int(value: str) -> int:
    try:
        number = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if number < 0:
        raise argparse.ArgumentTypeError("must be non-negative")
    return number


def parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--thread", help="existing Codex thread ID")
    target.add_argument("--discover-cwd", type=Path, help="wait for a newly created thread in this directory")
    parser.add_argument("--tokens", type=non_negative_int, required=True)
    parser.add_argument("--objective", default="Complete the current task within the harness token budget.")
    parser.add_argument("--watch", action="store_true", help="keep measuring and interrupt at the cap")
    parser.add_argument("--interval", type=positive_float, default=5.0)
    parser.add_argument("--discover-timeout", type=positive_float, default=60.0)
    parser.add_argument("--started-at", type=non_negative_int, default=int(time.time()))
    parser.add_argument("--exclude-thread")
    parser.add_argument("--harness", type=Path)
    parser.add_argument("--db-root", type=Path)
    parser.add_argument("--codex", default="codex")
    parser.add_argument("--request-timeout", type=positive_float, default=30.0)
    args = parser.parse_args(argv)
    if (args.harness is None) != (args.db_root is None):
        parser.error("--harness and --db-root must be supplied together")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    server: AppServer | None = None
    try:
        server = AppServer(args.codex, args.request_timeout)
        thread_id = args.thread
        if thread_id is None:
            thread_id = discover_thread(
                server,
                args.discover_cwd,
                args.started_at,
                args.discover_timeout,
                args.interval,
                args.exclude_thread,
            )
        goal = set_budget(server, thread_id, args.tokens, args.objective)
        if not args.watch:
            print(json.dumps({"thread_id": thread_id, "goal": goal}, sort_keys=True))
            return 0
        used = goal.get("tokensUsed", 0)
        if not isinstance(used, int) or used < 0:
            raise BudgetError("Codex App Server returned an invalid tokensUsed value")
        return watch_budget(
            server,
            thread_id,
            args.tokens,
            used,
            args.interval,
            args.harness,
            args.db_root,
        )
    except (BudgetError, OSError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 2
    finally:
        if server is not None:
            server.close()


if __name__ == "__main__":
    raise SystemExit(main())
