#!/usr/bin/env sh
# Install the shared agent-efficiency rules (docs/agent-efficiency.md) into the
# global guides that every interactive Claude Code and Codex session reads.
#
#   scripts/install-efficiency-rules.sh [--home DIR] [--remove]
#
# `<home>/.claude/CLAUDE.md` and `<home>/.codex/AGENTS.md` each get one marked
# `harness-efficiency` block, placed after the `global-harness` block when the
# guide has one and at the end otherwise. A rerun refreshes the block from
# docs/agent-efficiency.md and changes nothing else; --remove deletes it. A
# missing guide is skipped. Project-owned guides are never touched.
# `<home>/.codex/config.toml` gets the helper settings from docs/codex-multi-agent.toml
# (long waits, helpers report once) in a marked block at the end; it is skipped when
# the file already sets that table itself, and restored when Codex rejects the result.
# Exit 2 for usage errors or a missing rules file, 1 when Codex rejects the settings.
set -u

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
HARNESS_ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
RULES="$HARNESS_ROOT/docs/agent-efficiency.md"
CODEX_SETTINGS="$HARNESS_ROOT/docs/codex-multi-agent.toml"
HOME_DIR=${HOME:-}
REMOVE=0
START='<!-- harness-efficiency:start -->'
END='<!-- harness-efficiency:end -->'
GLOBAL_END='<!-- global-harness:end -->'

info() {
  printf '%s\n' "$*"
}

usage() {
  sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
  case "$1" in
    --home)
      [ $# -ge 2 ] || { info "FAIL: --home requires a directory"; exit 2; }
      HOME_DIR=$2
      shift 2
      ;;
    --remove)
      REMOVE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      info "FAIL: unknown argument: $1"
      usage
      exit 2
      ;;
  esac
done

if [ -z "$HOME_DIR" ] || [ ! -d "$HOME_DIR" ]; then
  info "FAIL: home directory does not exist: ${HOME_DIR:-<empty>}"
  exit 2
fi
[ -f "$RULES" ] || { info "FAIL: rules file is missing: $RULES"; exit 2; }
[ -f "$CODEX_SETTINGS" ] || { info "FAIL: Codex settings file is missing: $CODEX_SETTINGS"; exit 2; }

install_guide() {
  target="$1"
  [ -f "$target" ] || { info "skipped: $target (missing)"; return 0; }
  # A guide linked to a shared copy (a synced vault) is edited in place, so the link survives.
  if [ -L "$target" ]; then
    target=$(readlink -f "$target")
  fi
  tmp="$target.tmp.$$"
  # Drop any existing block, then insert a fresh one after the global block (or at the end).
  awk -v start="$START" -v end="$END" -v gend="$GLOBAL_END" -v rules="$RULES" -v remove="$REMOVE" '
    function block() {
      print start
      while ((getline line < rules) > 0) print line
      close(rules)
      print end
      placed = 1
    }
    # One blank line is held back so the blank before an old block goes with it.
    $0 == start { blank = 0; skipping = 1; next }
    $0 == end { skipping = 0; next }
    skipping { next }
    $0 == "" { if (blank) print ""; blank = 1; next }
    {
      if (blank) print ""
      blank = 0
      print
    }
    $0 == gend && !remove { print ""; block() }
    END {
      if (blank) print ""
      if (!remove && !placed) { print ""; block() }
    }
  ' "$target" > "$tmp"
  if cmp -s "$tmp" "$target"; then
    rm -f "$tmp"
    info "unchanged: $target"
  else
    mv "$tmp" "$target"
    if [ "$REMOVE" -eq 1 ]; then info "removed: $target"; else info "refreshed: $target"; fi
  fi
}

# The Codex helper settings live in a TOML block marked with comments.
install_codex_config() {
  target="$HOME_DIR/.codex/config.toml"
  [ -f "$target" ] || { info "skipped: $target (missing)"; return 0; }
  if [ -L "$target" ]; then
    target=$(readlink -f "$target")
  fi
  cstart='# <!-- harness-efficiency:start -->'
  cend='# <!-- harness-efficiency:end -->'
  stripped="$target.tmp.$$"
  awk -v start="$cstart" -v end="$cend" '
    $0 == start { skipping = 1; next }
    $0 == end { skipping = 0; next }
    !skipping { print }
  ' "$target" > "$stripped"
  if [ "$REMOVE" -eq 0 ] && grep -q '^\[features\.multi_agent_v2\]' "$stripped"; then
    rm -f "$stripped"
    info "skipped: $target (sets [features.multi_agent_v2] itself)"
    return 0
  fi
  tmp="$target.new.$$"
  # Trailing blank lines go, so a rerun gives the same file.
  awk '{ lines[NR] = $0 } END { n = NR; while (n > 0 && lines[n] == "") n--; for (i = 1; i <= n; i++) print lines[i] }' \
    "$stripped" > "$tmp"
  rm -f "$stripped"
  if [ "$REMOVE" -eq 0 ]; then
    { printf '\n%s\n' "$cstart"; cat "$CODEX_SETTINGS"; printf '%s\n' "$cend"; } >> "$tmp"
  fi
  if cmp -s "$tmp" "$target"; then
    rm -f "$tmp"
    info "unchanged: $target"
    return 0
  fi
  backup="$target.bak.$$"
  cp -p "$target" "$backup"
  cat "$tmp" > "$target"
  rm -f "$tmp"
  codex_bin=${HARNESS_CODEX_BIN:-$(command -v codex 2>/dev/null || true)}
  if [ -n "$codex_bin" ] && ! CODEX_HOME=$(dirname "$target") "$codex_bin" features list >/dev/null 2>&1; then
    cat "$backup" > "$target"
    rm -f "$backup"
    info "FAIL: Codex rejected the helper settings; $target restored."
    return 1
  fi
  rm -f "$backup"
  if [ "$REMOVE" -eq 1 ]; then info "removed: $target"; else info "refreshed: $target"; fi
}

info "Installing agent-efficiency rules"
info "Rules: $RULES"
info "Home:  $HOME_DIR"
install_guide "$HOME_DIR/.claude/CLAUDE.md"
install_guide "$HOME_DIR/.codex/AGENTS.md"
install_codex_config || exit 1
exit 0
