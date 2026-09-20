#!/usr/bin/env python3
"""Launch one command profile selected by the task-entry route."""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

from task_route import InputError, ROOT, route_task


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


def selected_profile(route: dict, mode: str) -> str | None:
    recommendation = route["recommendation"]
    if route["source"] == "typesafe" and mode == "shadow":
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
    parser.add_argument("--mode", choices=("shadow", "active"), default="shadow")
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
    os.chdir(args.project)
    try:
        os.execvpe(command[0], command, environment)
    except OSError as error:
        parser.error(f"cannot launch {command[0]}: {error}")
    return 0  # pragma: no cover: exec replaces this process


if __name__ == "__main__":
    raise SystemExit(main())
