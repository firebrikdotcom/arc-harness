#!/usr/bin/env sh
set -eu

# Trust gate for a project's knowledge/ folder. The folder can carry
# instructions and hooks, so nothing in it is followed until a human approves
# its exact content once per machine. Any later change needs a new approval.
#
#   knowledge-trust.sh check   [--project PATH]   exit 0 trusted, 1 untrusted, 2 usage
#   knowledge-trust.sh approve [--project PATH]   record the current content as trusted
#   knowledge-trust.sh status  [--project PATH]

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
HARNESS_DB_ROOT="${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}"
PROJECT_ROOT="${HARNESS_TARGET_ROOT:-.}"

info() {
  printf '%s\n' "$*"
}

usage() {
  info "Usage: scripts/knowledge-trust.sh check|approve|status [--project PATH]"
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

sha256() {
  if has_cmd shasum; then
    shasum -a 256 | cut -d ' ' -f 1
  elif has_cmd sha256sum; then
    sha256sum | cut -d ' ' -f 1
  else
    info "FAIL: shasum or sha256sum is required." >&2
    exit 2
  fi
}

if [ "$#" -eq 0 ]; then
  usage
  exit 2
fi
COMMAND=$1
shift
while [ "$#" -gt 0 ]; do
  case "$1" in
    --project)
      [ "$#" -ge 2 ] || { info "FAIL: --project requires a path."; exit 2; }
      PROJECT_ROOT=$2
      shift 2
      ;;
    *)
      info "FAIL: unknown argument: $1"
      usage
      exit 2
      ;;
  esac
done

[ -d "$PROJECT_ROOT" ] || { info "FAIL: project root does not exist: $PROJECT_ROOT"; exit 2; }
PROJECT_ROOT=$(CDPATH='' cd "$PROJECT_ROOT" && pwd -P)
KNOWLEDGE="$PROJECT_ROOT/knowledge"
TRUST_DIR="$HARNESS_DB_ROOT/trust"
TRUST_FILE="$TRUST_DIR/$(printf '%s' "$PROJECT_ROOT" | sha256).state"

# Hash every file's path and content so a rename or edit changes the result.
content_hash() {
  (
    cd "$KNOWLEDGE" && find . -type f | LC_ALL=C sort | while IFS= read -r file; do
      printf '%s\n' "$file"
      cat "$file"
    done
  ) | sha256
}

approved_hash() {
  [ -f "$TRUST_FILE" ] && sed -n 's/^HASH=//p' "$TRUST_FILE" | tail -n 1 || true
}

case "$COMMAND" in
  check)
    if [ ! -d "$KNOWLEDGE" ]; then
      info "OK: no knowledge/ folder in $PROJECT_ROOT."
      exit 0
    fi
    current=$(content_hash)
    approved=$(approved_hash)
    if [ -n "$approved" ] && [ "$current" = "$approved" ]; then
      info "OK: knowledge/ is trusted (approved $(sed -n 's/^APPROVED_AT=//p' "$TRUST_FILE"))."
      exit 0
    fi
    if [ -z "$approved" ]; then
      info "UNTRUSTED: knowledge/ in $PROJECT_ROOT has never been approved on this machine."
    else
      info "UNTRUSTED: knowledge/ in $PROJECT_ROOT changed since it was approved."
    fi
    info "Review its files, then a human runs: scripts/knowledge-trust.sh approve --project $PROJECT_ROOT"
    exit 1
    ;;
  approve)
    if [ ! -d "$KNOWLEDGE" ]; then
      info "FAIL: no knowledge/ folder in $PROJECT_ROOT."
      exit 2
    fi
    mkdir -p "$TRUST_DIR"
    {
      printf 'PROJECT_ROOT=%s\n' "$PROJECT_ROOT"
      printf 'HASH=%s\n' "$(content_hash)"
      printf 'APPROVED_AT=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      printf 'FILES=%s\n' "$(find "$KNOWLEDGE" -type f | wc -l | tr -d ' ')"
    } > "$TRUST_FILE.tmp.$$"
    mv "$TRUST_FILE.tmp.$$" "$TRUST_FILE"
    info "OK: knowledge/ approved for $PROJECT_ROOT. Record: $TRUST_FILE"
    ;;
  status)
    if [ ! -d "$KNOWLEDGE" ]; then
      info "knowledge/: absent"
    elif [ -f "$TRUST_FILE" ]; then
      cat "$TRUST_FILE"
      if [ "$(content_hash)" = "$(approved_hash)" ]; then
        info "STATE=trusted"
      else
        info "STATE=changed"
      fi
    else
      info "STATE=never-approved"
    fi
    ;;
  *)
    info "FAIL: unknown command: $COMMAND"
    usage
    exit 2
    ;;
esac
