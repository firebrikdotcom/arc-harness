#!/usr/bin/env python3
"""The persisted on/off switch for delegating decisions to Jev.

Off (the default), every Jev call runs in shadow mode: the agent's own baseline
is recorded first, Jev answers, the baseline is what happens, and the two are
compared later. On, the harness follows Jev where it has a lever: the task-entry
route selects the launch profile, the session-start route is reported as the
route to follow, and `harness advise` returns Jev's choice as the action.
Deterministic gates (permissions, required checks, known failures, explicit user
choices, irreversible work) decide before any call in either mode, and the
phase-gate checkpoints stay observations because the gates they predict are
mechanical.

The switch lives outside every checkout, in one JSON file:
`$HARNESS_CONFIG_HOME/jev-delegation.json`, defaulting to
`$XDG_CONFIG_HOME/harness/` or `~/.config/harness/`. `HARNESS_JEV_DELEGATION=on|off`
overrides the file for one shell (tests force `off`).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

OVERRIDE_ENV = "HARNESS_JEV_DELEGATION"
HOME_ENV = "HARNESS_CONFIG_HOME"
FILE_NAME = "jev-delegation.json"
DEFAULT_MODEL = "jev-1.13.0"
ON = {"on", "1", "true", "yes", "active"}
OFF = {"off", "0", "false", "no", "shadow"}


class SwitchError(ValueError):
    pass


def config_home() -> Path:
    explicit = os.environ.get(HOME_ENV, "").strip()
    if explicit:
        return Path(explicit)
    xdg = os.environ.get("XDG_CONFIG_HOME", "").strip()
    return (Path(xdg) if xdg else Path.home() / ".config") / "harness"


def config_path() -> Path:
    return config_home() / FILE_NAME


def read_file() -> dict[str, Any]:
    path = config_path()
    try:
        if path.stat().st_size > 4096:
            raise SwitchError(f"delegation switch file exceeds 4096 bytes: {path}")
        data = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise SwitchError(f"cannot read delegation switch {path}: {type(error).__name__}") from error
    if not isinstance(data, dict) or type(data.get("enabled")) is not bool:
        raise SwitchError(f"delegation switch {path} must be an object with a boolean 'enabled'")
    return data


def parse_override(raw: str) -> bool:
    value = raw.strip().lower()
    if value in ON:
        return True
    if value in OFF:
        return False
    raise SwitchError(f"{OVERRIDE_ENV} must be on or off")


def state() -> dict[str, Any]:
    """The effective switch: the env override when set, else the file, else off."""
    stored = read_file()
    override = os.environ.get(OVERRIDE_ENV)
    if override is not None and override.strip():
        enabled, source = parse_override(override), "environment"
    elif stored:
        enabled, source = stored["enabled"], "file"
    else:
        enabled, source = False, "default"
    return {
        "enabled": enabled,
        "mode": "active" if enabled else "shadow",
        "source": source,
        "path": str(config_path()),
        "model": stored.get("model") if isinstance(stored.get("model"), str) else None,
        "changed_at": stored.get("changed_at"),
        "reason": stored.get("reason"),
    }


def enabled() -> bool:
    return state()["enabled"]


def pinned_model() -> str:
    """The exact model active routing requires: the shell's pin, else the switch's."""
    from_env = os.environ.get("TYPESAFE_MODEL", "").strip()
    if from_env and not from_env.endswith("-latest"):
        return from_env
    stored = state()["model"]
    if stored and not stored.endswith("-latest"):
        return stored
    return from_env or "jev-latest"


def write(enabled_flag: bool, reason: str | None, model: str | None) -> dict[str, Any]:
    previous = read_file()
    record = {
        "enabled": enabled_flag,
        "model": model or previous.get("model") or os.environ.get("TYPESAFE_MODEL") or DEFAULT_MODEL,
        "changed_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "reason": reason or None,
    }
    if record["model"].endswith("-latest"):
        record["model"] = DEFAULT_MODEL
    path = config_path()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd, temporary = tempfile.mkstemp(prefix=".jev-delegation-", dir=path.parent)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as stream:
            json.dump(record, stream, sort_keys=True)
            stream.write("\n")
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return record


def describe(current: dict[str, Any]) -> str:
    lines = [f"Jev delegation: {'on' if current['enabled'] else 'off'} ({current['mode']} mode, from {current['source']})"]
    if current["source"] == "environment":
        lines.append(f"  {OVERRIDE_ENV} overrides the file for this shell.")
    lines.append(f"  file: {current['path']}")
    if current["model"]:
        lines.append(f"  model pin: {current['model']}")
    if current["changed_at"]:
        lines.append(f"  changed: {current['changed_at']}" + (f" ({current['reason']})" if current["reason"] else ""))
    if current["enabled"]:
        lines.append("  effect: route and launch select Jev's profile, the session route is the one to follow,")
        lines.append("          advise returns Jev's choice as the action. Deterministic gates still decide first.")
    else:
        lines.append("  effect: every Jev call is a shadow comparison; the agent's baseline is what runs.")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="harness jev", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("action", choices=("on", "off", "status"))
    parser.add_argument("--reason", help="why the switch changed; stored with it")
    parser.add_argument("--model", help=f"exact Jev model pin to store (default: TYPESAFE_MODEL or {DEFAULT_MODEL})")
    parser.add_argument("--json", action="store_true", help="print the effective state as JSON")
    args = parser.parse_args(argv)
    try:
        if args.action in ("on", "off"):
            if args.model and args.model.endswith("-latest"):
                raise SwitchError("the model pin must be exact, not a -latest alias")
            write(args.action == "on", args.reason, args.model)
        current = state()
    except SwitchError as error:
        parser.error(str(error))
    if args.json:
        print(json.dumps(current, sort_keys=True))
    else:
        print(describe(current))
        if args.action != "status" and current["source"] == "environment" and current["enabled"] != (args.action == "on"):
            print(f"  note: the file now says {args.action}, but {OVERRIDE_ENV} wins in this shell.", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
