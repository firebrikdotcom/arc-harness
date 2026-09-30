#!/usr/bin/env sh
set -eu

# Every `uses:` in a workflow must name a full 40-character commit SHA, because
# a tag or branch can be moved to different code without any change here.
# Local actions (./path) and digest-pinned Docker images are allowed.

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
tmpdir=$(mktemp -d "${TMPDIR:-/tmp}/harness-ci-pins.XXXXXX")
trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM

# Prints each unpinned reference as FILE:LINE: VALUE and exits 1 if any exist,
# 2 if the files contain no `uses:` at all. Only the block form `uses: REF` is
# parsed; any other line with a `uses` key (a flow mapping, a quoted key) fails
# as unparsed rather than being skipped. That also rejects `uses:` text inside
# a run: script, which is the safe direction for this check.
check_pins() {
  awk '
    /^[[:space:]]*#/ { next }
    /^[[:space:]]*(-[[:space:]]+)?uses:/ {
      found = 1
      value = $0
      sub(/^[[:space:]]*(-[[:space:]]+)?uses:[[:space:]]*/, "", value)
      sub(/[[:space:]]+#.*$/, "", value)
      sub(/[[:space:]]+$/, "", value)
      gsub(/^["\047]|["\047]$/, "", value)
      if (value ~ /^\.\//) next
      # Interval expressions such as {40} are missing from some awks (mawk),
      # so the hex length is checked separately.
      at = index(value, "@")
      name = substr(value, 1, at - 1)
      ref = substr(value, at + 1)
      if (at > 0 && name ~ /^docker:\/\/[^@]+$/ && ref ~ /^sha256:[0-9a-f]+$/ && length(ref) == 71) next
      if (at > 0 && name ~ /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.\/-]+$/ && ref ~ /^[0-9a-f]+$/ && length(ref) == 40) next
      printf "%s:%d: %s\n", FILENAME, FNR, value
      bad = 1
      next
    }
    /(^|[{,[:space:]])["\047]?uses["\047]?[[:space:]]*:/ {
      found = 1
      printf "%s:%d: unparsed uses: %s\n", FILENAME, FNR, $0
      bad = 1
    }
    END {
      if (!found) exit 2
      exit bad
    }
  ' "$@"
}

expect_rejected() {
  label=$1
  shift
  printf '%s\n' "jobs:" "  x:" "    steps:" "$@" > "$tmpdir/case.yml"
  status=0
  check_pins "$tmpdir/case.yml" > "$tmpdir/out" 2>&1 || status=$?
  [ "$status" -eq 1 ] || {
    printf '%s\n' "FAIL: $label returned $status, not 1"
    cat "$tmpdir/out"
    exit 1
  }
}

# Rejected, and the offending line is reported as unparsed.
expect_unparsed() {
  label=$1
  expect_rejected "$@"
  grep -q 'case.yml:5: unparsed uses:' "$tmpdir/out" || {
    printf '%s\n' "FAIL: $label did not report line 5 as unparsed"
    cat "$tmpdir/out"
    exit 1
  }
}

expect_accepted() {
  label=$1
  shift
  printf '%s\n' "jobs:" "  x:" "    steps:" "$@" > "$tmpdir/case.yml"
  check_pins "$tmpdir/case.yml" > "$tmpdir/out" 2>&1 || {
    printf '%s\n' "FAIL: $label was rejected"
    cat "$tmpdir/out"
    exit 1
  }
}

sha=11d5960a326750d5838078e36cf38b85af677262

expect_accepted 'a full SHA with a version comment' "      - uses: actions/checkout@$sha # v4.4.0"
expect_accepted 'a quoted full SHA' "        uses: 'actions/checkout@$sha'"
expect_accepted 'a subdirectory action at a full SHA' "      - uses: github/codeql-action/init@$sha"
expect_accepted 'a local action' '      - uses: ./.github/actions/setup'
expect_rejected 'a major tag' '      - uses: actions/checkout@v4'
expect_rejected 'a tag hidden behind a SHA comment' "      - uses: actions/checkout@v4 # $sha"
expect_rejected 'a short SHA' '      - uses: actions/checkout@11d5960'
expect_rejected 'a branch' '      - uses: actions/checkout@main'
expect_rejected 'an uppercase SHA' "      - uses: actions/checkout@$(printf '%s' "$sha" | tr 'a-f' 'A-F')"
expect_rejected 'a missing ref' '      - uses: actions/checkout'
expect_rejected 'a Docker tag' '      - uses: docker://alpine:3.20'
expect_rejected 'a pinned step next to a floating one' \
  "      - uses: actions/checkout@$sha" \
  '      - uses: actions/setup-node@v4'

# Forms the parser does not read must fail even when a pinned step is present,
# so a floating ref cannot hide behind the no-`uses:` guard.
expect_unparsed 'a flow-mapping step next to a pinned one' \
  "      - uses: actions/checkout@$sha" \
  '      - {uses: actions/setup-node@v4}'
expect_unparsed 'a flow mapping with the key second' \
  "      - uses: actions/checkout@$sha" \
  '      - {name: Node, uses: actions/setup-node@v4}'
expect_unparsed 'a double-quoted key next to a pinned one' \
  "      - uses: actions/checkout@$sha" \
  '      - "uses": actions/setup-node@v4'
expect_unparsed 'a single-quoted key next to a pinned one' \
  "      - uses: actions/checkout@$sha" \
  "        'uses': actions/setup-node@v4"
expect_unparsed 'a flow mapping holding a full SHA' \
  "      - uses: actions/checkout@$sha" \
  "      - {uses: actions/setup-node@$sha}"
expect_accepted 'comments and run steps next to a pinned step' \
  '      # - uses: actions/checkout@v4' \
  "      - uses: actions/checkout@$sha" \
  '      - run: make test' \
  '        # uses: actions/setup-node@v4'

# The guard is not per file: a flow-form file next to a pinned file still fails.
printf '%s\n' 'jobs:' '  x:' '    steps:' "      - uses: actions/checkout@$sha" > "$tmpdir/a.yml"
printf '%s\n' 'jobs:' '  y:' '    steps:' '      - {uses: actions/setup-node@v4}' > "$tmpdir/b.yml"
status=0
check_pins "$tmpdir/a.yml" "$tmpdir/b.yml" > "$tmpdir/out" 2>&1 || status=$?
if [ "$status" -ne 1 ] || ! grep -q 'b.yml:4: unparsed uses:' "$tmpdir/out"; then
  printf '%s\n' "FAIL: a flow-form file next to a pinned file returned $status"
  cat "$tmpdir/out"
  exit 1
fi

# A file with no `uses:` must not pass silently: that would hide a parser miss.
printf '%s\n' 'jobs: {}' > "$tmpdir/empty.yml"
status=0
check_pins "$tmpdir/empty.yml" > "$tmpdir/out" 2>&1 || status=$?
[ "$status" -eq 2 ] || {
  printf '%s\n' "FAIL: a workflow with no uses: returned $status, not 2"
  exit 1
}

expect_accepted 'a digest-pinned Docker image' \
  "      - uses: docker://alpine@sha256:$sha${sha%????????????????}"
expect_rejected 'a short Docker digest' "      - uses: docker://alpine@sha256:$sha"

# The real workflows.
for file in "$ROOT"/.github/workflows/*.yml "$ROOT"/.github/workflows/*.yaml; do
  [ -f "$file" ] || continue
  if [ -z "${have_workflow:-}" ]; then
    set --
    have_workflow=1
  fi
  set -- "$@" "$file"
done
[ -n "${have_workflow:-}" ] || {
  printf '%s\n' 'FAIL: no workflow files found under .github/workflows'
  exit 1
}
check_pins "$@" || {
  printf '%s\n' 'FAIL: pin every workflow action to a full commit SHA (see README.md, CI)'
  exit 1
}

printf '%s\n' 'PASS: every CI action is pinned to a full commit SHA'
