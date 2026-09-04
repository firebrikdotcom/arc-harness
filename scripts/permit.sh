#!/usr/bin/env sh
set -eu

# Denylist check for commands and write paths. Used by scripts/action.sh after
# schema validation and by the phase guard hook on every real tool call, so the
# same rules apply to what the agent proposes and to what it actually does.
#
#   permit.sh check --command "STRING" [--project PATH]
#   permit.sh check --path PATH         [--project PATH]
#   permit.sh rules                     [--project PATH]
#
# Exit 0 allowed, 1 denied, 2 usage or environment error.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-$HARNESS_ROOT}"

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/permit.sh check (--command STRING | --path PATH) [--project PATH]"
  info "       scripts/permit.sh rules [--project PATH]"
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

[ "$#" -ge 1 ] || { usage; exit 2; }
COMMAND=$1
shift
KIND=""
SUBJECT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --command) [ "$#" -ge 2 ] || { info "FAIL: --command requires a string."; exit 2; }; KIND='command'; SUBJECT=$2; shift 2 ;;
    --path) [ "$#" -ge 2 ] || { info "FAIL: --path requires a path."; exit 2; }; KIND=path; SUBJECT=$2; shift 2 ;;
    --project) [ "$#" -ge 2 ] || { info "FAIL: --project requires a path."; exit 2; }; PROJECT_ROOT=$2; shift 2 ;;
    *) info "FAIL: unknown argument: $1"; usage; exit 2 ;;
  esac
done

[ -d "$PROJECT_ROOT" ] || { info "FAIL: project root does not exist: $PROJECT_ROOT"; exit 2; }
PROJECT_ROOT=$(CDPATH='' cd "$PROJECT_ROOT" && pwd -P)

if [ -f "$PROJECT_ROOT/.harness-denylist" ]; then
  RULES="$PROJECT_ROOT/.harness-denylist"
else
  RULES="$HARNESS_ROOT/schemas/denylist.default"
fi
[ -f "$RULES" ] || { info "FAIL: no denylist found at $RULES"; exit 2; }

case "$COMMAND" in
  rules)
    info "Rules: $RULES"
    grep -E '^(command|path) ' "$RULES" || true
    exit 0
    ;;
  check) ;;
  *) info "FAIL: unknown command: $COMMAND"; usage; exit 2 ;;
esac
[ -n "$KIND" ] || { info "FAIL: check needs --command or --path."; exit 2; }

# A write path inside the project is judged relative to the project root.
if [ "$KIND" = "path" ]; then
  case "$SUBJECT" in
    "$PROJECT_ROOT"/*) SUBJECT=${SUBJECT#"$PROJECT_ROOT"/} ;;
  esac
fi

if has_cmd python3; then
  python3 - "$RULES" "$KIND" "$SUBJECT" <<'PY'
import re
import sys

rules_path, kind, subject = sys.argv[1], sys.argv[2], sys.argv[3]
with open(rules_path, encoding="utf-8") as handle:
    for number, line in enumerate(handle, 1):
        line = line.rstrip("\n")
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        rule_kind, _, pattern = line.partition(" ")
        if rule_kind != kind:
            continue
        try:
            if re.search(pattern, subject):
                sys.stdout.write("DENY: %s matches %s:%d: %s\n" % (kind, rules_path, number, pattern))
                sys.exit(1)
        except re.error as error:
            sys.stderr.write("FAIL: bad regex at %s:%d: %s\n" % (rules_path, number, error))
            sys.exit(2)
sys.stdout.write("OK: %s allowed\n" % kind)
PY
elif has_cmd node; then
  node - "$RULES" "$KIND" "$SUBJECT" <<'JS'
const fs = require('fs');
const [rulesPath, kind, subject] = process.argv.slice(2);
const lines = fs.readFileSync(rulesPath, 'utf8').split('\n');
for (let index = 0; index < lines.length; index += 1) {
  const line = lines[index];
  if (!line.trim() || line.trimStart().startsWith('#')) continue;
  const space = line.indexOf(' ');
  const ruleKind = space === -1 ? line : line.slice(0, space);
  const pattern = space === -1 ? '' : line.slice(space + 1);
  if (ruleKind !== kind) continue;
  let regex;
  try {
    regex = new RegExp(pattern);
  } catch (error) {
    process.stderr.write(`FAIL: bad regex at ${rulesPath}:${index + 1}: ${error.message}\n`);
    process.exit(2);
  }
  if (regex.test(subject)) {
    process.stdout.write(`DENY: ${kind} matches ${rulesPath}:${index + 1}: ${pattern}\n`);
    process.exit(1);
  }
}
process.stdout.write(`OK: ${kind} allowed\n`);
JS
else
  info "FAIL: python3 or node is required to evaluate the denylist."
  exit 2
fi
