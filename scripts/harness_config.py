#!/usr/bin/env python3
"""Shared runtime switches; strict JSON with brief _doc fields, never secrets."""
from __future__ import annotations

import argparse
import copy
import fcntl
import json
import os
import tempfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
DEFAULTS = {
    "version": 1,
    "_doc": "Machine-wide runtime switches. No credentials. CLI and dashboard preserve other sections.",
    "typesafe": {
        "_doc": "mode: off = no calls; shadow = compare only; active = follow decisions within existing rules.",
        "mode": "shadow",
        "model": "jev-1.13.0",
        "checkpoints": True,
        "enforce_decisions": False,
        "_doc_enforce_decisions": "Require decision records at covered native execution seams; internal judgments use harness decision explicitly.",
        "decision_ttl_seconds": 300,
        "_doc_decision_ttl_seconds": "Reuse only unchanged decision states within this many seconds; consumed action receipts remain single-use.",
        "_doc_checkpoints": "Enable automatic phase/session checkpoints; required checks always remain enforced.",
    },
    "dispatch": {
        "_doc": "Enable role-scoped dispatch. False keeps execution with the parent and prevents new worker launches.",
        "enabled": False,
        "max_workers": 3,
        "_doc_max_workers": "Maximum simultaneous native and external workers per run.",
        "max_context_bytes": 12000,
        "_doc_max_context_bytes": "Maximum UTF-8 bytes in each worker's role packet; do not include full conversation history.",
        "timeout_seconds": 900,
        "_doc_timeout_seconds": "Stop timed-out external processes and require native parent cancellation at this deadline.",
        "profiles": {
            "native_scan": {"provider": "native", "model": "gpt-6-luna", "effort": "high", "role": "explorer", "agent_type": "explorer"},
            "native_build": {"provider": "native", "model": "gpt-6.1-sol", "effort": "medium", "role": "implementer", "agent_type": "worker"},
            "native_review": {"provider": "native", "model": "gpt-6.1-sol", "effort": "high", "role": "reviewer", "agent_type": "code-reviewer"},
            "codex_build": {"provider": "codex", "model": "gpt-6.1-sol", "effort": "medium", "role": "implementer"},
            "claude_review": {"provider": "claude", "model": "sonnet", "effort": "medium", "role": "reviewer"}
        },
        "_doc_profiles": "Eligible role/model/backend combinations. Native needs parent tools; command providers use operator-configured argv only.",
    },
    "telemetry": {
        "_doc": "Dashboard collection only; does not enable TypeSafe or dispatch. Unavailable service queues events locally.",
        "enabled": True,
        "jev": True,
        "workflow": True,
        "workflow_prompts": False,
        "_doc_jev": "Collect decision, routing and agent measurements.",
        "_doc_workflow": "Collect session, task, todo and tool lifecycle events.",
        "_doc_workflow_prompts": "Store full future user prompts; requires workflow collection. Saved history is retained.",
    },
    "sites": {
        "_doc": "Firebrik Sites publishing for every agent (harness sites). The token stays in token_file (0600), never here.",
        "enabled": True,
        "url": "https://sites.firebrik.com",
        "_doc_url": "Sites host; its MCP endpoint is URL/mcp/sites.",
        "token_file": "~/.config/firebrik-sites/token",
        "_doc_token_file": "File holding a personal API token from Settings > API tokens; FIREBRIK_SITES_TOKEN overrides it.",
    },
}
SECTIONS = ("typesafe", "dispatch", "telemetry", "sites")


class ConfigError(ValueError):
    pass


def config_home() -> Path:
    if os.environ.get("HARNESS_CONFIG_HOME", "").strip():
        return Path(os.environ["HARNESS_CONFIG_HOME"])
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "harness"


def path() -> Path:
    # Legacy path override remains useful to isolated installations and old tests.
    return Path(os.environ.get("HARNESS_CONFIG") or os.environ.get("HARNESS_AUDIT_SETTINGS") or config_home() / "config.json")


def read_json(file: Path) -> dict[str, Any]:
    try:
        if file.stat().st_size > 32_000:
            raise ConfigError(f"configuration exceeds 32000 bytes: {file}")
        value = json.loads(file.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ConfigError(f"cannot read configuration {file}: {type(error).__name__}") from error
    if not isinstance(value, dict):
        raise ConfigError(f"configuration must be an object: {file}")
    return value


def validate(value: dict[str, Any]) -> dict[str, Any]:
    if type(value.get("version")) is not int or value["version"] != 1:
        raise ConfigError("configuration version must be 1")
    allowed = {"version", *SECTIONS}
    if any(k not in allowed and not k.startswith("_doc") for k in value):
        raise ConfigError("unknown configuration section")
    fields = {"typesafe": {"mode", "model", "checkpoints", "changed_at", "reason", "enforce_decisions", "decision_ttl_seconds"},
              "dispatch": {"enabled", "max_workers", "max_context_bytes", "timeout_seconds", "profiles"}, "telemetry": {"enabled", "jev", "workflow", "workflow_prompts"},
              "sites": {"enabled", "url", "token_file"}}
    for section, keys in fields.items():
        part = value.get(section)
        if not isinstance(part, dict) or any(k not in keys and not k.startswith("_doc") for k in part):
            raise ConfigError(f"invalid configuration section: {section}")
    typesafe = value["typesafe"]
    if typesafe.get("mode") not in ("off", "shadow", "active"):
        raise ConfigError("typesafe.mode must be off, shadow or active")
    model = typesafe.get("model")
    if not isinstance(model, str) or not model.strip() or model.endswith("-latest"):
        raise ConfigError("typesafe.model must be an exact model pin")
    for section, keys in {"typesafe": ["checkpoints", "enforce_decisions"], "dispatch": ["enabled"],
                          "telemetry": ["enabled", "jev", "workflow", "workflow_prompts"], "sites": ["enabled"]}.items():
        for key in keys:
            if type(value[section].get(key)) is not bool:
                raise ConfigError(f"{section}.{key} must be boolean")
    for section, limits in {"typesafe": {"decision_ttl_seconds": (1, 3600)},
                            "dispatch": {"max_workers": (1, 16), "max_context_bytes": (1000, 32000), "timeout_seconds": (1, 86400)}}.items():
        for key, (low, high) in limits.items():
            number = value[section].get(key)
            if type(number) is not int or not low <= number <= high:
                raise ConfigError(f"{section}.{key} must be an integer from {low} to {high}")
    sites = value["sites"]
    url = sites.get("url")
    if not isinstance(url, str) or len(url) > 200 or not (url.startswith("https://") or url.startswith(("http://localhost", "http://127.0.0.1"))) \
            or "@" in url or "?" in url or any(c.isspace() for c in url):
        raise ConfigError("sites.url must be an https URL (http only for localhost) without credentials or query")
    token_file = sites.get("token_file")
    if not isinstance(token_file, str) or not token_file.strip() or len(token_file) > 500 or not token_file.startswith(("/", "~/")):
        raise ConfigError("sites.token_file must be an absolute or ~/ path to the token file, not the token")
    profiles = value["dispatch"].get("profiles")
    if not isinstance(profiles, dict) or not 1 <= len(profiles) <= 16:
        raise ConfigError("dispatch.profiles must contain 1 to 16 profiles")
    for name, profile in profiles.items():
        if not isinstance(name, str) or not name.replace("_", "").replace("-", "").isalnum() or len(name) > 64:
            raise ConfigError("invalid dispatch profile name")
        if not isinstance(profile, dict) or set(profile) - {"provider", "model", "effort", "role", "agent_type", "command", "_doc"}:
            raise ConfigError(f"invalid dispatch profile: {name}")
        if profile.get("provider") not in ("native", "codex", "claude", "command"):
            raise ConfigError(f"invalid dispatch provider: {name}")
        for key in ("model", "role", "effort"):
            if not isinstance(profile.get(key), str) or not profile[key].strip() or len(profile[key]) > 200:
                raise ConfigError(f"profile {name}.{key} must be a bounded non-empty string")
        if profile["effort"] not in ("low", "medium", "high", "xhigh", "max", "ultra"):
            raise ConfigError(f"invalid dispatch effort: {name}")
        if profile["provider"] == "command":
            command = profile.get("command")
            if not isinstance(command, list) or not command or any(not isinstance(v, str) or not v or len(v) > 2000 for v in command):
                raise ConfigError(f"profile {name}.command must be an argv array")
            if any("dangerously" in item or item.startswith("--ignore-") for item in command):
                raise ConfigError("dispatch command cannot bypass runtime permissions or rules")
    return value


def legacy_collection(value: dict[str, Any]) -> dict[str, bool]:
    if any(type(value.get(k)) is not bool for k in ("jev", "workflow")) or type(value.get("workflow_prompts", False)) is not bool:
        raise ConfigError("invalid audit collection settings")
    return {k: value.get(k, False) for k in ("jev", "workflow", "workflow_prompts")}


def load(file: Path | None = None) -> dict[str, Any]:
    file = file or path()
    value = copy.deepcopy(DEFAULTS)
    if file.exists():
        stored = read_json(file)
        if "version" not in stored and "jev" in stored:
            value["telemetry"].update(legacy_collection(stored))
            return validate(value)
        # Sections added after a file was written start from their defaults.
        if "sites" not in stored:
            stored["sites"] = copy.deepcopy(DEFAULTS["sites"])
        for section in SECTIONS:
            if isinstance(stored.get(section), dict):
                if section == "dispatch" and stored[section].get("_doc") == "Reserved for automatic role dispatch; false keeps work with the main agent. Dispatcher not implemented yet.":
                    stored[section]["_doc"] = DEFAULTS[section]["_doc"]
                for key, default in DEFAULTS[section].items():
                    if key not in stored[section]:
                        stored[section][key] = copy.deepcopy(default)
        return validate(stored)
    # Read-only migration fallback; explicit init persists one authoritative file.
    legacy = config_home() / "jev-delegation.json"
    if legacy.exists():
        stored = read_json(legacy)
        if type(stored.get("enabled")) is not bool:
            raise ConfigError("legacy delegation switch must contain boolean enabled")
        value["typesafe"].update({k: stored[k] for k in ("model", "reason", "changed_at") if k in stored})
        value["typesafe"]["mode"] = "active" if stored["enabled"] else "shadow"
    legacy_audit = ROOT / ".harness-db/audit-settings.json"
    # Custom homes isolate tests and other installations from this machine's settings.
    if "HARNESS_CONFIG_HOME" not in os.environ and "HARNESS_CONFIG" not in os.environ and legacy_audit.exists():
        value["telemetry"].update(legacy_collection(read_json(legacy_audit)))
    # Legacy unpinned state stays readable so routing can report its existing
    # activation holdback; persistence still requires validation in update().
    return value


def update(section: str | None = None, changes: dict[str, Any] | None = None, file: Path | None = None) -> dict[str, Any]:
    file = file or path()
    file.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (file.parent / (file.name + ".lock")).open("a") as lock:
        os.chmod(lock.name, 0o600)
        fcntl.flock(lock, fcntl.LOCK_EX)
        value = load(file)
        if section:
            if section not in SECTIONS:
                raise ConfigError("unknown settings section")
            value[section].update(changes or {})
        validate(value)
        fd, temporary = tempfile.mkstemp(prefix=".config-", dir=file.parent)
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                json.dump(value, stream, indent=2)
                stream.write("\n")
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, file)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    return value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("show", "init", "update"))
    parser.add_argument("--file", type=Path)
    parser.add_argument("--section", choices=SECTIONS)
    parser.add_argument("--values", help="JSON object containing only changed fields")
    args = parser.parse_args()
    try:
        if args.action == "update":
            changes = json.loads(args.values or "null")
            if not args.section or not isinstance(changes, dict):
                raise ConfigError("update requires --section and --values JSON object")
            value = update(args.section, changes, args.file)
        else:
            value = load(args.file) if args.action == "show" else update(file=args.file)
        print(json.dumps(value, indent=2))
    except (ConfigError, OSError, json.JSONDecodeError) as error:
        parser.error(str(error))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
