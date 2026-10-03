#!/usr/bin/env sh
# Shared selector; Node keeps the CLI usable when Python is unavailable.
set -eu
SCRIPT_DIR=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
if command -v python3 >/dev/null 2>&1; then
  exec python3 "$SCRIPT_DIR/run_paths.py" "$@"
fi
if command -v node >/dev/null 2>&1; then
  exec node - "$@" <<'JS'
const path = require('path');
const crypto = require('crypto');
const [kind, root, flag, value] = process.argv.slice(2);
if (!['current', 'records', 'key'].includes(kind) || !root ||
    (flag !== undefined && (flag !== '--session-id' || value === undefined))) process.exit(2);
const sid = value || process.env.HARNESS_SESSION_ID || process.env.CODEX_THREAD_ID || process.env.CLAUDE_SESSION_ID;
if (sid && (Buffer.byteLength(sid) > 256 || !sid.trim() || /[\r\n]/.test(sid))) {
  process.stderr.write('invalid session ID\n'); process.exit(2);
}
const key = sid ? crypto.createHash('sha256').update(sid).digest('hex') : '';
let result;
if (kind === 'key') result = key;
else if (kind === 'current') result = key ? path.join(root, 'runs', 'sessions', key, 'current') : path.join(root, 'runs', 'current');
else result = key ? path.join(root, 'records', 'sessions', key) : path.join(root, 'records');
process.stdout.write(result + '\n');
JS
fi
printf '%s\n' 'FAIL: python3 or node is required for session run selection.' >&2
exit 2
