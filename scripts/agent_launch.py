#!/usr/bin/env python3
"""Launch one command profile selected by the task-entry route."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

from task_route import InputError, ROOT, route_task


def configured_token_budget(environment: dict[str, str]) -> int | None:
    value = environment.get("HARNESS_BUDGET_TOKENS", "").strip()
    if value in {"", "unknown"}:
        return None
    if not value.isdigit():
        raise InputError("HARNESS_BUDGET_TOKENS must be a non-negative integer or 'unknown'")
    return int(value)


def command_runs_codex(command: list[str]) -> bool:
    return Path(command[0]).name == "codex"


def audit_agent_completion(route: dict, profile: str, exit_code: int, environment: dict[str, str]) -> None:
    if environment.get("HARNESS_AUDIT_ENABLED") != "1":
        return
    emitter = ROOT / "scripts" / "audit_emit.py"
    command = [
        sys.executable,
        str(emitter),
        "agent",
        "--record",
        route["record_path"],
        "--profile",
        profile,
        "--exit-code",
        str(exit_code),
    ]
    for name, option in (
        ("HARNESS_AGENT_INPUT_TOKENS", "--input-tokens"),
        ("HARNESS_AGENT_OUTPUT_TOKENS", "--output-tokens"),
        ("HARNESS_AGENT_TOTAL_TOKENS", "--total-tokens"),
    ):
        if environment.get(name, "").isdigit():
            command.extend([option, environment[name]])
    try:
        result = subprocess.run(command, env=environment, capture_output=True, text=True, timeout=3, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return
    if result.stderr.strip():
        print(result.stderr.strip(), file=sys.stderr)


def read_command(path: Path | None) -> list[str] | None:
    if path is None:
        return None
    try:
        if path.stat().st_size > 4096:
            raise InputError(f"command file exceeds 4096 bytes: {path}")
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise InputError(f"cannot read command file {path}: {error}") from error
    if not isinstance(value, list) or not value or any(not isinstance(item, str) or not item for item in value):
        raise InputError(f"command file must contain a non-empty JSON argv array: {path}")
    return value


def selected_profile(route: dict, mode: str | None) -> str | None:
    recommendation = route["recommendation"]
    if route["source"] == "typesafe" and (route.get("routing_mode") or route.get("mode") or mode) == "shadow":
        return "default"
    return {
        "ask_user": None,
        "proceed": "routine",
        "targeted_check": "targeted",
        "reasoning_model": "deep",
    }.get(recommendation, "default")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", required=True, type=Path)
    parser.add_argument("--project", type=Path, default=Path.cwd())
    parser.add_argument("--mode", choices=("shadow", "active"), default=None,
                        help="default: active when 'harness jev status' is on, otherwise shadow")
    parser.add_argument("--router", type=Path, default=Path(os.environ.get("HARNESS_TYPESAFE_ROUTER", str(Path.home() / ".agents/skills/typesafe-routing/scripts/route.py"))))
    parser.add_argument("--db-root", type=Path, default=Path(os.environ.get("HARNESS_DB_ROOT", str(ROOT / ".harness-db"))))
    parser.add_argument("--agent", choices=("codex", "claude", "agy", "opencode"), help="use this executable as the default command")
    parser.add_argument("--default-command", type=Path, help="JSON argv array for the existing launch path")
    parser.add_argument("--routine-command", type=Path)
    parser.add_argument("--targeted-command", type=Path)
    parser.add_argument("--deep-command", type=Path)
    parser.add_argument("--dry-run", action="store_true", help="route and show the selected command without launching")
    args = parser.parse_args(argv)
    if not args.project.is_dir():
        parser.error(f"project does not exist: {args.project}")
    if not args.agent and not args.default_command:
        parser.error("provide --agent or --default-command")
    try:
        commands = {
            "default": read_command(args.default_command) or [args.agent],
            "routine": read_command(args.routine_command),
            "targeted": read_command(args.targeted_command),
            "deep": read_command(args.deep_command),
        }
        route = route_task(args.state, args.project, args.db_root, args.router, args.mode)
    except (InputError, OSError) as error:
        parser.error(str(error))
    profile = selected_profile(route, args.mode)
    if profile is None:
        print(json.dumps({"launched": False, "reason": "user input or authorization is required", "route": route}, sort_keys=True))
        return 3
    command = commands[profile] or commands["default"]
    output = {"launched": not args.dry_run, "profile": profile if commands[profile] else "default", "command": command, "route": route}
    if args.dry_run:
        print(json.dumps(output, sort_keys=True))
        return 0
    environment = os.environ.copy()
    environment["HARNESS_ROUTE_RECORD"] = route["record_path"]
    if route.get("call_id"):
        environment["HARNESS_ROUTE_CALL_ID"] = route["call_id"]
    try:
        token_budget = configured_token_budget(environment)
    except InputError as error:
        parser.error(str(error))
    if command_runs_codex(command) and token_budget is not None:
        parser.error(
            "a direct codex CLI launch cannot be interrupted through a separate App Server; "
            "start an App Server-owned thread and use 'harness budget --thread ID --tokens N --watch'"
        )
    os.chdir(args.project)
    try:
        result = subprocess.run(command, env=environment, check=False)
    except OSError as error:
        parser.error(f"cannot launch {command[0]}: {error}")
    audit_agent_completion(route, output["profile"], result.returncode, environment)
    if route.get("source") == "typesafe" and route.get("call_id"):
        emitter = ROOT / "scripts" / "audit_emit.py"
        subprocess.run(
            [
                sys.executable,
                str(emitter),
                "outcome",
                "--record",
                route["record_path"],
                "--outcome",
                "unknown",
                "--route-taken",
                output["profile"],
                "--evidence",
                "Agent completion is observable; route correctness still requires independent review.",
                "--avoided",
                "normal reasoning path",
            ],
            env=environment,
            check=False,
        )
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
