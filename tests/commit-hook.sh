#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
TMP=$(mktemp -d "${TMPDIR:-/tmp}/harness-commit-hook.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
TMP=$(CDPATH='' cd "$TMP" && pwd -P)

mkdir -p "$TMP/repo/.githooks" "$TMP/repo/scripts"
git -C "$TMP/repo" init -q
cp "$ROOT/.githooks/pre-commit" "$TMP/repo/.githooks/pre-commit"
chmod +x "$TMP/repo/.githooks/pre-commit"
cat > "$TMP/repo/scripts/verify.sh" <<'VERIFY'
#!/usr/bin/env sh
printf '%s|%s|%s|%s\n' "$*" "${HARNESS_VERIFY_SCOPE:-unset}" "${GIT_DIR:-unset}" "${GIT_INDEX_FILE:-unset}" > "$VERIFY_LOG"
exit "${VERIFY_EXIT:-0}"
VERIFY
chmod +x "$TMP/repo/scripts/verify.sh"

(cd "$TMP/repo" && GIT_DIR="$TMP/repo/.git" GIT_WORK_TREE="$TMP/repo" GIT_INDEX_FILE="$TMP/repo/.git/index.lock" VERIFY_LOG="$TMP/verify.log" .githooks/pre-commit > "$TMP/pass.out")
[ "$(cat "$TMP/verify.log")" = "--project $TMP/repo|full|unset|unset" ] || {
  printf '%s\n' 'FAIL: hook did not invoke isolated full verification for the repository root.' >&2
  exit 1
}
set +e
(cd "$TMP/repo" && GIT_DIR="$TMP/repo/.git" GIT_WORK_TREE="$TMP/repo" GIT_INDEX_FILE="$TMP/repo/.git/index.lock" VERIFY_LOG="$TMP/verify.log" VERIFY_EXIT=7 .githooks/pre-commit > "$TMP/fail.out" 2>&1)
status=$?
set -e
[ "$status" -eq 7 ] || {
  printf '%s\n' 'FAIL: hook did not propagate the verification failure.' >&2
  exit 1
}
printf '%s\n' 'OK: commit hook runs verification and blocks on failure.'
