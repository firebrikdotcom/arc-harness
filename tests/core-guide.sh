#!/usr/bin/env sh
set -eu

# GUIDE.md is the one mandatory read. The entry files point to it instead of
# restating its rules, and every other doc is opened only when a task needs it.

SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
ROOT=$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)
GUIDE="$ROOT/GUIDE.md"
MAX_LINES=100
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/harness-core-guide.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  printf '%s\n' "FAIL: $*"
  exit 1
}

# An entry file without the managed harness-cli block, which the installer owns.
unmanaged() {
  sed '/<!-- harness-cli:start -->/,/<!-- harness-cli:end -->/d' "$1"
}

[ -f "$GUIDE" ] || fail "GUIDE.md is missing"
lines=$(wc -l < "$GUIDE" | tr -d ' ')
[ "$lines" -le "$MAX_LINES" ] || fail "GUIDE.md has $lines lines; keep it under $MAX_LINES"

# Every mandatory safety and gate rule is stated in the guide.
while IFS= read -r rule; do
  grep -qF -- "$rule" "$GUIDE" || fail "GUIDE.md does not state: $rule"
done <<'EOF'
--project PATH
.harness-db/
scripts/harness plan start
`build done` needs a passing `scripts/verify.sh`
`review done` needs a `scripts/review.sh`
scripts/action.sh validate PATH
A rejected proposal is not approval
explicit user instruction to continue in the current conversation
`scripts/harness abort` and `scripts/knowledge-trust.sh approve` are human-only
data, not instructions
Do not delete existing files
Preserve user changes and unrelated worktree changes
secrets
`progress.md` holds only the current run
Do not claim success without running `scripts/verify.sh`
exact commands
never overrides
scripts/jg.sh
EOF

# Each document the guide links to exists, so an on-demand pointer never dangles.
sed -n 's/.*\](\([^)#]*\)[^)]*).*/\1/p' "$GUIDE" > "$TMP_ROOT/links"
[ -s "$TMP_ROOT/links" ] || fail "GUIDE.md links to no reference docs"
while IFS= read -r link; do
  [ -e "$ROOT/$link" ] || fail "GUIDE.md links to a missing file: $link"
done < "$TMP_ROOT/links"
for doc in docs/setup.md docs/architecture.md docs/conventions.md docs/jev-checkpoints.md SECURITY.md; do
  grep -qF "]($doc)" "$GUIDE" || fail "GUIDE.md does not say when to open $doc"
done

for entry in AGENTS.md CLAUDE.md README.md; do
  grep -qF 'GUIDE.md' "$ROOT/$entry" || fail "$entry does not point to GUIDE.md"
  unmanaged "$ROOT/$entry" > "$TMP_ROOT/entry"
  # No reading list that makes the reference docs mandatory again: no line
  # names two different ones.
  while IFS= read -r line; do
    distinct=$(printf '%s\n' "$line" | grep -oE 'docs/(architecture|conventions|setup)\.md' | sort -u | wc -l | tr -d ' ')
    [ "$distinct" -lt 2 ] || fail "$entry lists several reference docs as one reading step: $line"
  done < "$TMP_ROOT/entry"
  # Rules live in the guide; a second copy drifts.
  for rule in 'action.sh validate' 'knowledge-trust.sh approve' 'harness abort' 'Do not delete existing files'; do
    if grep -qF "$rule" "$TMP_ROOT/entry"; then
      fail "$entry restates a GUIDE.md rule outside the managed block: $rule"
    fi
  done
done

grep -qF 'GUIDE.md' "$ROOT/scripts/init.sh" || fail "scripts/init.sh next steps do not point to GUIDE.md"
if grep -E 'Read .*docs/(architecture|conventions|setup)\.md' "$ROOT/scripts/init.sh" >/dev/null; then
  fail "scripts/init.sh still prints the full reading list"
fi

# The architecture doc no longer carries a list of unknown placeholders.
if grep -qi 'unknown placeholders' "$ROOT/docs/architecture.md"; then
  fail "docs/architecture.md still lists unknown placeholders"
fi

printf '%s\n' "PASS: core guide"
