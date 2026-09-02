#!/usr/bin/env sh
set -eu

SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH= cd "$SCRIPT_DIR/.." && pwd -P)

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/action.sh validate PATH"
  info ""
  info "Validates a proposed action JSON file against schemas/action.schema.json."
  info "Does not execute the action."
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

validate_with_python() {
  python3 - "$1" <<'PY'
import json
import sys

path = sys.argv[1]
allowed = {
    "run_command": ("version", "type", "reason", "command", "cwd", "timeout_s"),
    "write_file": ("version", "type", "reason", "path", "content"),
}


def fail(message):
    sys.stderr.write("FAIL: %s\n" % message)
    sys.exit(2)


def is_int(value):
    return isinstance(value, int) and not isinstance(value, bool)


def is_number(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def is_relative_path(value):
    if not value or value.startswith("/"):
        return False
    return ".." not in value.split("/")


try:
    with open(path, encoding="utf-8") as handle:
        data = json.load(handle)
except json.JSONDecodeError as error:
    fail("invalid JSON: %s" % error)
except OSError as error:
    fail("cannot read action file: %s" % error)

if not isinstance(data, dict):
    fail("action must be a JSON object")

if "version" not in data:
    fail("missing version")
if not is_int(data["version"]) or data["version"] != 1:
    fail("version must be 1")

action_type = data.get("type")
if action_type not in allowed:
    fail("unknown type: %s" % action_type)

extra = [key for key in data if key not in allowed[action_type]]
if extra:
    fail("unknown field: %s" % extra[0])

if "reason" in data and not isinstance(data["reason"], str):
    fail("reason must be a string")

if action_type == "run_command":
    if "command" not in data:
        fail("run_command requires command")
    command = data["command"]
    if not isinstance(command, list) or not command:
        fail("command must be a non-empty array of strings")
    if any(not isinstance(item, str) or item == "" for item in command):
        fail("command must be a non-empty array of strings")
    if "cwd" in data and (not isinstance(data["cwd"], str) or data["cwd"] == ""):
        fail("cwd must be a non-empty string")
    if "timeout_s" in data:
        timeout = data["timeout_s"]
        if not is_number(timeout) or timeout <= 0:
            fail("timeout_s must be a positive number")
elif action_type == "write_file":
    if "path" not in data:
        fail("write_file requires path")
    file_path = data["path"]
    if not isinstance(file_path, str) or not is_relative_path(file_path):
        fail("path must be a relative path without ..")
    if "content" in data and not isinstance(data["content"], str):
        fail("content must be a string")

sys.stdout.write("PASS: action valid (type=%s)\n" % action_type)
PY
}

validate_with_node() {
  node - "$1" <<'JS'
const fs = require('fs');

const path = process.argv[2];
const allowed = {
  run_command: ['version', 'type', 'reason', 'command', 'cwd', 'timeout_s'],
  write_file: ['version', 'type', 'reason', 'path', 'content'],
};

function fail(message) {
  process.stderr.write(`FAIL: ${message}\n`);
  process.exit(2);
}

function isRelativePath(value) {
  if (!value || value.startsWith('/')) {
    return false;
  }
  return !value.split('/').includes('..');
}

let data;
try {
  data = JSON.parse(fs.readFileSync(path, 'utf8'));
} catch (error) {
  fail(`invalid JSON: ${error.message}`);
}

if (data === null || typeof data !== 'object' || Array.isArray(data)) {
  fail('action must be a JSON object');
}

if (!Object.prototype.hasOwnProperty.call(data, 'version')) {
  fail('missing version');
}
if (!Number.isInteger(data.version) || data.version !== 1) {
  fail('version must be 1');
}

const actionType = data.type;
if (!Object.prototype.hasOwnProperty.call(allowed, actionType)) {
  fail(`unknown type: ${actionType}`);
}

const extra = Object.keys(data).find((key) => !allowed[actionType].includes(key));
if (extra) {
  fail(`unknown field: ${extra}`);
}

if (Object.prototype.hasOwnProperty.call(data, 'reason') && typeof data.reason !== 'string') {
  fail('reason must be a string');
}

if (actionType === 'run_command') {
  if (!Object.prototype.hasOwnProperty.call(data, 'command')) {
    fail('run_command requires command');
  }
  const command = data.command;
  if (!Array.isArray(command) || command.length === 0) {
    fail('command must be a non-empty array of strings');
  }
  if (command.some((item) => typeof item !== 'string' || item === '')) {
    fail('command must be a non-empty array of strings');
  }
  if (Object.prototype.hasOwnProperty.call(data, 'cwd') && (typeof data.cwd !== 'string' || data.cwd === '')) {
    fail('cwd must be a non-empty string');
  }
  if (Object.prototype.hasOwnProperty.call(data, 'timeout_s')) {
    const timeout = data.timeout_s;
    if (typeof timeout !== 'number' || !Number.isFinite(timeout) || timeout <= 0) {
      fail('timeout_s must be a positive number');
    }
  }
} else if (actionType === 'write_file') {
  if (!Object.prototype.hasOwnProperty.call(data, 'path')) {
    fail('write_file requires path');
  }
  if (typeof data.path !== 'string' || !isRelativePath(data.path)) {
    fail('path must be a relative path without ..');
  }
  if (Object.prototype.hasOwnProperty.call(data, 'content') && typeof data.content !== 'string') {
    fail('content must be a string');
  }
}

process.stdout.write(`PASS: action valid (type=${actionType})\n`);
JS
}

if [ "$#" -eq 0 ]; then
  usage
  exit 2
fi

case "$1" in
  --help|-h)
    usage
    exit 0
    ;;
  validate)
    if [ "$#" -lt 2 ]; then
      info "FAIL: validate requires a path."
      usage
      exit 2
    fi
    ACTION_FILE="$2"
    if [ ! -f "$ACTION_FILE" ]; then
      info "FAIL: action file does not exist: $ACTION_FILE"
      exit 2
    fi
    if [ ! -f "$HARNESS_ROOT/schemas/action.schema.json" ]; then
      info "FAIL: missing schemas/action.schema.json in harness root."
      exit 2
    fi
    if has_cmd python3; then
      validate_with_python "$ACTION_FILE"
    elif has_cmd node; then
      validate_with_node "$ACTION_FILE"
    else
      info "FAIL: python3 or node is required to validate action JSON."
      exit 2
    fi
    ;;
  *)
    info "FAIL: unknown argument: $1"
    usage
    exit 2
    ;;
esac
