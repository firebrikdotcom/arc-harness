#!/usr/bin/env sh
# Machine-local registry of harness targets.
#
# A target is any project the harness operates on: a git worktree, a plain
# checkout, or a bare directory. Registration writes only under the ignored
# harness database (HARNESS_DB_ROOT/targets/<id>/), never inside the target,
# so tracked files in the project stay untouched. Each target owns a private
# database directory for runs, gate records, routes, and checkpoints.
#
#   scripts/harness-target.sh root [PATH]        project root for PATH (git toplevel, else PATH)
#   scripts/harness-target.sh id PATH            stable registry id for the project root of PATH
#   scripts/harness-target.sh register PATH      register (or refresh) the target; prints its registry directory
#   scripts/harness-target.sh lookup PATH        registry directory of the registered target containing PATH
#   scripts/harness-target.sh db-root PATH       private database directory of that target
#   scripts/harness-target.sh fingerprint PATH   hash of the project's bootstrap inputs (manifests and lockfiles)
#   scripts/harness-target.sh list               registered targets
#
# Exit 1 when a lookup finds nothing or a path cannot be a target ($HOME, /,
# or the harness root itself); exit 2 for usage errors.
set -eu

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
DB_ROOT=${HARNESS_DB_ROOT:-$HARNESS_ROOT/.harness-db}
TARGETS_DIR=$DB_ROOT/targets
BOOTSTRAP_INPUTS="Makefile package.json package-lock.json pnpm-lock.yaml yarn.lock composer.json composer.lock go.mod go.sum Cargo.toml Cargo.lock npm-shrinkwrap.json bun.lock bun.lockb"

info() {
  printf '%s\n' "$*"
}

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
}

has_cmd() {
  command -v "$1" >/dev/null 2>&1
}

now_iso() {
  date -u +%Y-%m-%dT%H:%M:%SZ
}

# hash_stdin  Hex digest of stdin using the strongest available tool.
hash_stdin() {
  if has_cmd sha256sum; then
    sha256sum | cut -d' ' -f1
  elif has_cmd shasum; then
    shasum -a 256 | cut -d' ' -f1
  else
    cksum | cut -d' ' -f1
  fi
}

absolute_dir() {
  [ -d "$1" ] || return 1
  CDPATH='' cd "$1" 2>/dev/null && pwd -P
}

# target_root PATH  The project root that contains PATH.
target_root() {
  _dir=$(absolute_dir "$1") || return 1
  if has_cmd git; then
    _top=$(git -C "$_dir" rev-parse --show-toplevel 2>/dev/null || :)
    if [ -n "$_top" ]; then
      absolute_dir "$_top"
      return 0
    fi
  fi
  printf '%s\n' "$_dir"
}

# target_kind ROOT  worktree (linked git worktree), repo (main checkout), or dir.
target_kind() {
  if [ -f "$1/.git" ]; then
    printf 'worktree\n'
  elif [ -d "$1/.git" ]; then
    printf 'repo\n'
  else
    printf 'dir\n'
  fi
}

# target_id ROOT  <basename>-<12 hex chars of the root path hash>.
target_id() {
  _base=$(printf '%s' "$(basename "$1")" | tr -c 'A-Za-z0-9._-' '_' | cut -c1-48)
  _hash=$(printf '%s' "$1" | hash_stdin | cut -c1-12)
  printf '%s-%s\n' "$_base" "$_hash"
}

# registrable ROOT  Refuses the home directory, the filesystem root, and the harness root.
registrable() {
  case "$1" in
    /|"${HOME:-/nonexistent}"|"$HARNESS_ROOT") return 1 ;;
  esac
  return 0
}

state_value() {
  sed -n "s/^$2=//p" "$1" 2>/dev/null | head -n 1
}

cmd_root() {
  target_root "${1:-.}"
}

cmd_id() {
  _root=$(target_root "$1") || { info "FAIL: not a directory: $1"; return 2; }
  target_id "$_root"
}

cmd_register() {
  _root=$(target_root "$1") || { info "FAIL: not a directory: $1"; return 2; }
  registrable "$_root" || { info "FAIL: not a registrable target: $_root"; return 1; }
  _id=$(target_id "$_root")
  _dir=$TARGETS_DIR/$_id
  mkdir -p "$_dir/db"
  _state=$_dir/target.state
  _registered=$(state_value "$_state" REGISTERED_AT)
  [ -n "$_registered" ] || _registered=$(now_iso)
  {
    printf 'TARGET_ID=%s\n' "$_id"
    printf 'TARGET_ROOT=%s\n' "$_root"
    printf 'TARGET_KIND=%s\n' "$(target_kind "$_root")"
    printf 'REGISTERED_AT=%s\n' "$_registered"
    printf 'LAST_SEEN_AT=%s\n' "$(now_iso)"
  } > "$_state.tmp.$$"
  mv "$_state.tmp.$$" "$_state"
  printf '%s\n' "$_dir"
}

# cmd_lookup PATH  Walks up from PATH to the nearest registered target root.
cmd_lookup() {
  _dir=$(absolute_dir "$1") || return 1
  [ -d "$TARGETS_DIR" ] || return 1
  while :; do
    _candidate=$TARGETS_DIR/$(target_id "$_dir")
    if [ -f "$_candidate/target.state" ] && [ "$(state_value "$_candidate/target.state" TARGET_ROOT)" = "$_dir" ]; then
      printf '%s\n' "$_candidate"
      return 0
    fi
    [ "$_dir" != "/" ] || return 1
    _dir=$(dirname "$_dir")
  done
}

cmd_db_root() {
  _dir=$(cmd_lookup "$1") || return 1
  printf '%s/db\n' "$_dir"
}

cmd_fingerprint() {
  _root=$(target_root "$1") || { info "FAIL: not a directory: $1"; return 2; }
  {
    for _f in $BOOTSTRAP_INPUTS; do
      if [ -f "$_root/$_f" ]; then
        printf '%s\n' "$_f"
        cat "$_root/$_f"
      fi
    done
  } | hash_stdin
}

cmd_list() {
  [ -d "$TARGETS_DIR" ] || return 0
  for _state in "$TARGETS_DIR"/*/target.state; do
    [ -f "$_state" ] || continue
    printf '%s\t%s\t%s\t%s\n' "$(state_value "$_state" TARGET_ID)" "$(state_value "$_state" TARGET_KIND)" "$(state_value "$_state" LAST_SEEN_AT)" "$(state_value "$_state" TARGET_ROOT)"
  done
}

[ "$#" -ge 1 ] || { usage; exit 2; }
COMMAND=$1
shift
case "$COMMAND" in
  root) cmd_root "${1:-.}" ;;
  id|register|lookup|db-root|fingerprint)
    [ "$#" -eq 1 ] || { info "FAIL: $COMMAND requires exactly one PATH."; exit 2; }
    case "$COMMAND" in
      id) cmd_id "$1" ;;
      register) cmd_register "$1" ;;
      lookup) cmd_lookup "$1" ;;
      db-root) cmd_db_root "$1" ;;
      fingerprint) cmd_fingerprint "$1" ;;
    esac
    ;;
  list) cmd_list ;;
  -h|--help|help) usage ;;
  *) info "FAIL: unknown command: $COMMAND"; usage; exit 2 ;;
esac
