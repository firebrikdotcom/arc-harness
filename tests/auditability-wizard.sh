#!/usr/bin/env sh
set -eu

ROOT=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
WIZARD=$ROOT/tools/auditability-planning-wizard.html

[ -f "$WIZARD" ] || { echo "FAIL: wizard is missing" >&2; exit 1; }

for phrase in \
  'Ten decisions only' \
  'Arc is the event-sourced foundation' \
  'fingerprinted/redacted' \
  'shadow mode' \
  'What single workflow is the first pilot?' \
  'Which facts must the pilot record?' \
  'Which projection should reviewers see first?' \
  'How should events travel between machines?' \
  'retention, and repair rule' \
  'What evidence is required before active routing?' \
  'Download Markdown' \
  'Download JSON' \
  'application/json' \
  'aria-live="polite"' \
  'localStorage' \
  'raw prompts'; do
  grep -Fq "$phrase" "$WIZARD" || { echo "FAIL: missing wizard concern: $phrase" >&2; exit 1; }
done

if grep -Eq '<script[^>]+src=|fetch\(|XMLHttpRequest|WebSocket' "$WIZARD"; then
  echo "FAIL: wizard must not make network requests" >&2
  exit 1
fi

node -e "const fs=require('fs');const h=fs.readFileSync(process.argv[1],'utf8');const m=h.match(/<script>([\\s\\S]*)<\\/script>/);if(!m)throw new Error('missing inline script');new Function(m[1]);if(!m[1].includes('questions.length !== 10'))throw new Error('missing ten-question guard');" "$WIZARD"

echo "PASS: auditability planning wizard content and script parse"
