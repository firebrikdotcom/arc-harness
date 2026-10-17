#!/usr/bin/env python3
"""Durable bounded worker scheduling and explicit native-runtime handoffs."""
from __future__ import annotations
import argparse
import hashlib
import concurrent.futures
import json
import os
import re
import signal
import shlex
import sqlite3
import subprocess
import sys
import time
import uuid
from functools import lru_cache
from pathlib import Path
import harness_config
import audit_transport

TERMINAL = {'completed', 'failed', 'cancelled', 'blocked'}
ACTIVE = {'reserved', 'launching', 'running', 'cancelling'}


def db_path() -> Path:
    return Path(os.environ.get('HARNESS_DISPATCH_DB', str(harness_config.ROOT / '.harness-db/dispatch.sqlite')))


def connection():
    path = db_path()
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    db = sqlite3.connect(path, timeout=30)
    db.row_factory = sqlite3.Row
    db.execute('CREATE TABLE IF NOT EXISTS runs (id TEXT PRIMARY KEY, project TEXT, session TEXT, status TEXT, created REAL, request_id TEXT)')
    db.execute('CREATE TABLE IF NOT EXISTS workers (run TEXT, id TEXT, status TEXT, data TEXT, PRIMARY KEY(run,id))')
    path.chmod(0o600)
    if 'request_id' not in {row[1] for row in db.execute('PRAGMA table_info(runs)')}:
        db.execute('ALTER TABLE runs ADD COLUMN request_id TEXT')
    return db


def session_has_plan(project: Path, session_id: str, request_id: str | None = None) -> bool:
    with connection() as db:
        return db.execute('SELECT 1 FROM runs WHERE project=? AND session=? AND status IN ("active","completed") AND (? IS NULL OR request_id=?)', (str(project.resolve()), session_id, request_id, request_id)).fetchone() is not None


plan_for_session = session_has_plan


def safe_path(project: Path, raw: str) -> str:
    if not isinstance(raw, str) or not raw or Path(raw).is_absolute() or '..' in Path(raw).parts or raw == '.':
        raise ValueError('owned paths must be relative bounded paths without traversal')
    resolved = (project / raw).resolve()
    if not resolved.is_relative_to(project.resolve()):
        raise ValueError('path escapes project through symlink')
    return Path(raw).as_posix().rstrip('/')


def overlap(a: str, b: str) -> bool:
    return a == b or a.startswith(b + '/') or b.startswith(a + '/')


def validate(plan: dict, project: Path, config: dict) -> list[dict]:
    workers = plan.get('workers')
    if set(plan) - {'workers', 'rationale', 'acceptance', 'request_id'} or not isinstance(workers, list) or len(workers) > 32:
        raise ValueError('plan requires 1..32 workers')
    if not workers and (not plan.get('rationale') or not plan.get('acceptance')):
        raise ValueError('main-only plans require rationale and acceptance')
    seen = {}
    for original in workers:
        w = dict(original)
        required = {'id', 'role', 'responsibility', 'acceptance', 'owned_paths', 'context', 'dependencies', 'access', 'profiles'}
        if set(w) != required or not re.fullmatch(r'[a-zA-Z0-9_-]{1,64}', w.get('id', '')) or w['id'] in seen:
            raise ValueError('invalid worker contract or duplicate id')
        for key in ('role', 'responsibility', 'context'):
            if not isinstance(w[key], str) or not w[key].strip():
                raise ValueError('worker text fields must be nonempty')
        if w['access'] not in ('read', 'edit') or not isinstance(w['acceptance'], list) or not w['acceptance'] or any(not isinstance(x, str) or not x.strip() for x in w['acceptance']):
            raise ValueError('worker requires access and acceptance criteria')
        for key in ('dependencies', 'owned_paths', 'profiles'):
            if not isinstance(w[key], list) or any(not isinstance(x, str) for x in w[key]):
                raise ValueError('worker lists must contain strings')
        w['owned_paths'] = [safe_path(project, x) for x in w['owned_paths']]
        if any(not snapshot_allowed(project, path) for path in w['owned_paths']):
            raise ValueError('owned path is protected or sensitive')
        if w['access'] == 'edit' and not w['owned_paths']:
            raise ValueError('editing worker must own paths')
        if not 1 <= len(w['profiles']) <= 8 or len(set(w['profiles'])) != len(w['profiles']):
            raise ValueError('worker requires 1..8 unique registered profiles')
        for p in w['profiles']:
            profile = config['profiles'].get(p)
            if not profile or profile['role'] not in (w['role'], '*'):
                raise ValueError('profile is missing or ineligible for role')
        if len(json.dumps(w).encode()) > config.get('max_context_bytes', 12000):
            raise ValueError('worker contract exceeds bounded context size')
        seen[w['id']] = w
    visiting, done = set(), set()
    def visit(key):
        if key in visiting:
            raise ValueError('dependency cycle')
        if key not in seen:
            raise ValueError('unknown dependency')
        if key in done:
            return
        visiting.add(key)
        for dep in seen[key]['dependencies']:
            visit(dep)
        visiting.remove(key)
        done.add(key)
    for key in seen:
        visit(key)
    edits = [w for w in seen.values() if w['access'] == 'edit']
    for i, a in enumerate(edits):
        for b in edits[i + 1:]:
            if any(overlap(x, y) for x in a['owned_paths'] for y in b['owned_paths']):
                raise ValueError('editing ownership overlaps')
    return list(seen.values())


def emit(event, run, w=None):
    w = w or {}
    profile = w.get('profile', {})
    payload = {k: w.get(k) for k in ('queued_at', 'started_at', 'finished_at', 'duration_ms', 'queue_ms', 'context_bytes', 'input_tokens', 'cached_input_tokens', 'output_tokens', 'total_tokens', 'cost_usd', 'exit_code', 'decisions')}
    payload.update(run_id=run['id'], worker_id=w.get('id'), parent_session_id=run['session'], role=w.get('role'), status=w.get('status', run['status']))
    payload.update({k: profile.get(k) for k in ('provider', 'model', 'effort')})
    try:
        audit_transport.emit('orchestration.' + event, payload, flush_now=False)
    except (OSError, ValueError, sqlite3.Error):
        pass


def save(db, run_id, w):
    db.execute('UPDATE workers SET status=?,data=? WHERE run=? AND id=?', (w['status'], json.dumps(w), run_id, w['id']))


def get_run(db, run_id):
    row = db.execute('SELECT * FROM runs WHERE id=?', (run_id,)).fetchone()
    if row is None:
        raise ValueError('unknown run')
    return dict(row)


def workers(db, run_id):
    return [json.loads(row['data']) for row in db.execute('SELECT data FROM workers WHERE run=? ORDER BY rowid', (run_id,))]


def create(plan: dict, project: Path, session_id: str) -> dict:
    config = harness_config.load()['dispatch']
    if not config['enabled']:
        return {'status': 'disabled'}
    project = project.resolve(strict=True)
    validated = validate(plan, project, config)
    import workflow_audit
    with workflow_audit.context_db() as workflow_db:
        workflow_context = workflow_audit.load(workflow_db, session_id)
    if not plan.get('request_id'):
        plan = {**plan, 'request_id': workflow_context.get('request_id')}
    run = {'id': uuid.uuid4().hex, 'project': str(project), 'session': session_id, 'status': 'active', 'created': time.time(), 'request_id': plan.get('request_id')}
    # Selection uses only enum metadata, never task text or repository paths.
    for w in validated:
        candidates = w['profiles']
        choice = candidates[0]
        w['decisions'] = 0
        if len(candidates) > 1:
            from decision_checkpoint import evaluate, claim, outcome, resolve
            state = {'contract_hash': hashlib.sha256(json.dumps(w, sort_keys=True).encode()).hexdigest()}
            action_hash = hashlib.sha256(json.dumps(candidates).encode()).hexdigest()
            scope = {'kind': 'dispatch', 'run_id': run['id'], 'worker_id': w['id'], 'stage': 'profile', 'action_hash': action_hash, **{key: workflow_context.get(key) for key in ('request_id', 'request_seq', 'plan_revision')}}
            receipt = evaluate(project=project, session_id=session_id, family='reasoning_allocation', goal='select_worker_profile', facts=['access_' + w['access']], choices={p: ' '.join(str(config['profiles'][p].get(k, 'unknown')) for k in ('provider', 'model', 'effort')) for p in candidates}, scope=scope, state=state)
            if receipt.get('needs_reasoning'):
                # Below the confidence threshold the main agent decides: its first-listed profile.
                receipt = resolve(project=project, identifier=receipt['id'], session_id=session_id, action=choice,
                                  reason='main agent listed this profile first', scope=scope, state=state)
            if receipt.get('action') in candidates:
                choice = receipt['action']
            w['decisions'] = 1
            w['decision_id'] = receipt.get('id')
            claim(project=project, identifier=receipt['id'], session_id=session_id, scope=scope, state=state, action_hash=action_hash)
            outcome(project=project, identifier=receipt['id'], session_id=session_id, exit_code=0)
        w.update(profile=config['profiles'][choice], profile_id=choice, status='queued', queued_at=time.time(), context_bytes=len(json.dumps(w).encode()))
    with connection() as db:
        db.execute('INSERT INTO runs (id,project,session,status,created,request_id) VALUES (:id,:project,:session,:status,:created,:request_id)', run)
        for w in validated:
            db.execute('INSERT INTO workers VALUES (?,?,?,?)', (run['id'], w['id'], w['status'], json.dumps(w)))
            emit('worker_planned', run, w)
    emit('run_created', run)
    return {'run_id': run['id'], 'status': 'active'}


def git(project, *args, **kwargs):
    return subprocess.run(['git', '-C', str(project), *args], check=True, capture_output=True, **kwargs).stdout


def snapshot_allowed(project, relative):
    import permit
    parts = Path(relative).parts
    if any(re.search(r'(?i)(^\.env($|\.)|(^|[-_.])(secret|secrets|credential|credentials|private|id_rsa|id_ed25519)($|[-_.])|\.(pem|key|p12|pfx|keystore)$)', part) for part in parts):
        return False
    if any(part in ('.harness-db', '.git', '.codex', '.claude', '.agents') or part.startswith('.harness-dispatch-') for part in parts):
        return False
    rules_path, rules = permit.load_rules(project)
    if permit.judge_subject(relative, 'file', project, rules, rules_path):
        return False
    try:
        safe_path(project, relative)
    except ValueError:
        return False
    return not (project / relative).is_symlink()


def apply_patch(run, worker, target):
    if worker['status'] != 'completed' or not worker.get('patch'):
        raise ValueError('integration requires a verified completed editing worker')
    data = Path(worker['patch']).read_bytes()
    if hashlib.sha256(data).hexdigest() != worker.get('patch_hash'):
        raise ValueError('patch artifact changed since verification')
    for path in worker.get('changed_paths', []):
        safe_path(target, path)
        if not snapshot_allowed(target, path) or not any(path == owned or path.startswith(owned + '/') for owned in worker['owned_paths']):
            raise ValueError('patch contains protected or unowned paths')
    if data:
        git(target, 'apply', '--check', '--binary', '-', input=data)
        git(target, 'apply', '--binary', '-', input=data)


def integrate(run_id, worker_id):
    with connection() as db:
        db.execute('BEGIN IMMEDIATE')
        run = get_run(db, run_id)
        worker = next(w for w in workers(db, run_id) if w['id'] == worker_id)
        apply_patch(run, worker, Path(run['project']))
        worker['integrated_at'] = time.time()
        save(db, run_id, worker)
    return {'status': 'integrated', 'worker_id': worker_id, 'project': run['project']}


def prepare(run, w):
    project = Path(run['project'])
    if w['access'] == 'read':
        w['workspace'] = str(project)
        return
    import shutil
    destination = project.parent / (project.name + '-dispatch-' + run['id'][:10] + '-' + w['id'])
    git(project, 'worktree', 'add', '--no-checkout', '--detach', str(destination), 'HEAD')
    w['workspace'] = str(destination)
    # Build an allowlisted index from current files, never checkout or stage secrets.
    git(destination, 'read-tree', '--empty')
    candidates = git(project, 'ls-files', '--cached', '--others', '--exclude-standard', '-z').split(b'\0')
    copied = []
    for raw in candidates:
        if not raw:
            continue
        relative = os.fsdecode(raw)
        if not snapshot_allowed(project, relative):
            continue
        source, target = project / relative, destination / relative
        if not source.is_file():
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        copied.append(relative)
    for relative in copied:
        git(destination, 'add', '--', relative)
    git(destination, '-c', 'user.name=Harness', '-c', 'user.email=harness@localhost', 'commit', '--allow-empty', '-m', 'Snapshot permitted current workspace files')
    with connection() as dep_db:
        dependencies = {item['id']: item for item in workers(dep_db, run['id'])}
    for dependency in w['dependencies']:
        predecessor = dependencies[dependency]
        if predecessor.get('patch'):
            apply_patch(run, predecessor, destination)
            for path in predecessor.get('changed_paths', []):
                git(destination, 'add', '--', path)
    if w['dependencies']:
        git(destination, '-c', 'user.name=Harness', '-c', 'user.email=harness@localhost', 'commit', '--allow-empty', '-m', 'Apply verified worker dependencies')
    w['base_commit'] = git(destination, 'rev-parse', 'HEAD').decode().strip()


def message(run, w):
    return '\n'.join([f"Project: {w['workspace']}", f"Role: {w['role']}", f"Responsibility: {w['responsibility']}", f"Access: {w['access']}; owned paths: {json.dumps(w['owned_paths'])}", 'You are not alone. Preserve other workers changes. Work only in this workspace and owned paths. Do not alter secrets.', 'Acceptance: ' + json.dumps(w['acceptance']), 'Bounded context: ' + w['context'], 'Verified dependency reports: ' + json.dumps(w.get('dependency_evidence', [])), 'Return only a JSON report: status must be completed or failed; acceptance must contain one object per criterion in the original order, copying each criterion string exactly, with nonempty evidence and boolean passed; include nonempty top-level evidence and artifacts array. Completion requires actual verification.'])


def next_workers(run_id):
    config = harness_config.load()['dispatch']
    if not config['enabled']:
        return {'status': 'disabled', 'workers': []}
    previous = status(run_id)
    if previous['run']['status'] != 'active':
        return {'run_id': run_id, 'workers': [], 'runtime_actions': previous['runtime_actions']}
    output = []
    with connection() as db:
        db.execute('BEGIN IMMEDIATE')
        run = get_run(db, run_id)
        all_workers = workers(db, run_id)
        by_id = {w['id']: w for w in all_workers}
        capacity = config.get('max_workers', 3) - sum(w['status'] in ACTIVE for w in all_workers)
        for w in all_workers:
            if w['status'] != 'queued':
                continue
            deps = [by_id[d]['status'] for d in w['dependencies']]
            if any(s in TERMINAL and s != 'completed' for s in deps):
                w['status'] = 'blocked'
                save(db, run_id, w)
                emit('worker_failed', run, w)
                continue
            if capacity <= 0 or any(s != 'completed' for s in deps):
                continue
            try:
                prepare(run, w)
            except (OSError, subprocess.SubprocessError, ValueError) as error:
                w.update(status='failed', error=type(error).__name__, finished_at=time.time())
                save(db, run_id, w)
                emit('worker_failed', run, w)
                continue
            w['dependency_evidence'] = [{'worker_id': dependency, 'evidence': str(by_id[dependency].get('result', {}).get('evidence', ''))[:1200], 'patch': by_id[dependency].get('patch')} for dependency in w['dependencies']]
            w['context_bytes'] = len(message(run, w).encode())
            if w['context_bytes'] > config.get('max_context_bytes', 12000):
                w.update(status='failed', error='prepared worker context exceeds configured byte bound', finished_at=time.time())
                save(db, run_id, w)
                emit('worker_failed', run, w)
                continue
            w.update(status='reserved', started_at=time.time())
            w['queue_ms'] = int((w['started_at'] - w['queued_at']) * 1000)
            save(db, run_id, w)
            capacity -= 1
            item = {'worker_id': w['id'], 'provider': w['profile']['provider'], 'workspace': w['workspace']}
            if item['provider'] == 'native':
                item['tool'] = 'collaboration.spawn_agent'
                item['arguments'] = {'task_name': 'dispatch_' + w['id'], 'fork_turns': 'none', 'agent_type': w['profile'].get('agent_type', w['role']), 'model': w['profile']['model'], 'reasoning_effort': w['profile']['effort'], 'message': message(run, w)}
            else:
                item['tool'] = 'exec_command'
                item['arguments'] = {'cmd': shlex.join([sys.executable, str(Path(__file__).resolve()), 'execute', '--run', run_id, '--worker', w['id']]), 'yield_time_ms': 1000}
            output.append(item)
    return {'run_id': run_id, 'workers': output, 'runtime_actions': previous['runtime_actions']}


def bind(run_id, worker_id, agent_id):
    with connection() as db:
        db.execute('BEGIN IMMEDIATE')
        run = get_run(db, run_id)
        w = next(w for w in workers(db, run_id) if w['id'] == worker_id)
        if w['status'] != 'reserved' or w['profile']['provider'] != 'native' or not agent_id:
            raise ValueError('native worker must be reserved before binding')
        w.update(status='running', agent_id=agent_id)
        save(db, run_id, w)
        emit('worker_started', run, w)
    return {'status': 'running', 'worker_id': worker_id}


def finish(run_id, worker_id, result):
    with connection() as db:
        db.execute('BEGIN IMMEDIATE')
        run = get_run(db, run_id)
        w = next(w for w in workers(db, run_id) if w['id'] == worker_id)
        if w['status'] not in ACTIVE or (w['profile']['provider'] == 'native' and (w['status'] != 'running' or not w.get('agent_id'))):
            raise ValueError('worker is not active')
        accepted = result.get('acceptance', [])
        valid = isinstance(accepted, list) and len(accepted) == len(w['acceptance']) and all(isinstance(a, dict) and a.get('criterion') == criterion and a.get('passed') is True and isinstance(a.get('evidence'), str) and a['evidence'].strip() for a, criterion in zip(accepted, w['acceptance']))
        if result.get('status') == 'completed' and (not valid or not result.get('evidence')):
            raise ValueError('completion requires evidence for every acceptance criterion')
        w.update(status='completed' if result.get('status') == 'completed' else 'failed', result=result, finished_at=time.time())
        w['duration_ms'] = int((w['finished_at'] - w['started_at']) * 1000)
        for key in ('input_tokens', 'cached_input_tokens', 'output_tokens', 'total_tokens', 'cost_usd', 'exit_code'):
            w[key] = result.get(key)
        if w['access'] == 'edit':
            workspace = Path(w['workspace'])
            for raw in git(workspace, 'ls-files', '--others', '--exclude-standard', '-z').split(b'\0'):
                if raw:
                    relative = os.fsdecode(raw)
                    if snapshot_allowed(workspace, relative):
                        git(workspace, 'add', '-N', '--', relative)
            changed = git(workspace, 'diff', '--name-only', w['base_commit']).decode().splitlines()
            if any(not any(p == owned or p.startswith(owned + '/') for owned in w['owned_paths']) for p in changed):
                w['status'] = 'failed'
                w['error'] = 'worker changed paths outside ownership'
            patch = git(workspace, 'diff', '--binary', w['base_commit'])
            artifact = db_path().parent / 'dispatch-artifacts' / run_id / (worker_id + '.patch')
            artifact.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            artifact.write_bytes(patch)
            artifact.chmod(0o600)
            w['patch'] = str(artifact)
            w['patch_hash'] = hashlib.sha256(patch).hexdigest()
            w['changed_paths'] = changed
        save(db, run_id, w)
        emit('worker_completed' if w['status'] == 'completed' else 'worker_failed', run, w)
    return status(run_id)


def status(run_id):
    with connection() as db:
        db.execute('BEGIN IMMEDIATE')
        run = get_run(db, run_id)
        items = workers(db, run_id)
        timeout = harness_config.load()['dispatch'].get('timeout_seconds', 900)
        for w in items:
            if w['status'] == 'launching':
                supervisor = process_identity(w.get('supervisor_pid'))
                if not supervisor or supervisor['start'] != w.get('supervisor_start'):
                    w.update(status='failed', error='external launch supervisor exited before process publication', finished_at=time.time())
                    save(db, run_id, w)
                    emit('worker_failed', run, w)
            if w['profile']['provider'] != 'native' and w['status'] == 'running' and w.get('pid'):
                identity = process_identity(w['pid'])
                if identity and identity['start'] == w.get('process_start'):
                    w.pop('process_missing_at', None)
                    if time.time() - w['started_at'] > timeout + 2:
                        terminate_worker(w)
                        w.update(status='failed', error='external worker exceeded timeout after supervisor loss', finished_at=time.time(), exit_code=124)
                        emit('worker_failed', run, w)
                elif not group_members(w['pid']):
                    if time.time() - w.setdefault('process_missing_at', time.time()) > 2:
                        w.update(status='failed', error='external worker exited without supervisor report', finished_at=time.time())
                        emit('worker_failed', run, w)
                else:
                    w['error'] = 'supervisor leader lost; group remains alive and capacity stays reserved'
                save(db, run_id, w)
            if w['profile']['provider'] == 'native' and w['status'] in {'reserved', 'running'} and time.time() - w['started_at'] > timeout:
                w.update(status='cancelling' if w.get('agent_id') else 'failed', terminal_after_cancel='failed', error='native handoff timed out; parent must execute interrupt action', exit_code=124)
                if w.get('agent_id'):
                    w['runtime_action'] = {'tool': 'collaboration.interrupt_agent', 'arguments': {'target': w['agent_id']}}
                save(db, run_id, w)
                emit('worker_failed', run, w)
        if items and all(w['status'] in TERMINAL for w in items) and run['status'] == 'active':
            run['status'] = 'completed' if all(w['status'] == 'completed' for w in items) else 'failed'
            db.execute('UPDATE runs SET status=? WHERE id=?', (run['status'], run_id))
            emit('run_completed', run)
    return {'run': run, 'workers': items, 'runtime_actions': [w['runtime_action'] for w in items if w.get('runtime_action')]}


def process_identity(pid):
    try:
        fields = Path('/proc', str(pid), 'stat').read_text().rsplit(')', 1)[1].split()
        return {'start': fields[19], 'group': int(fields[2]), 'state': fields[0]}
    except (OSError, ValueError, IndexError):
        return None


def group_members(group):
    members = {}
    for entry in Path('/proc').iterdir():
        if entry.name.isdigit():
            identity = process_identity(int(entry.name))
            if identity and identity['group'] == group and identity['state'] != 'Z':
                members[int(entry.name)] = identity['start']
    return members


def terminate_worker(w):
    pid = w.get('pid')
    if not pid:
        return
    identity = process_identity(pid)
    if not identity or identity['start'] != w.get('process_start'):
        return  # Never signal a reused process ID.
    original = group_members(pid)
    try:
        os.killpg(pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + 1
    while time.monotonic() < deadline and group_members(pid):
        time.sleep(.02)
    remaining = group_members(pid)
    if any(original.get(member) == started for member, started in remaining.items()):
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + 1
    while time.monotonic() < deadline and group_members(pid):
        time.sleep(.02)
    if group_members(pid):
        raise ValueError('worker process group is still alive; capacity remains reserved')


def cancel(run_id, worker_id=None, acknowledged=False, evidence=None):
    handoffs = []
    with connection() as db:
        db.execute('BEGIN IMMEDIATE')
        run = get_run(db, run_id)
        for w in workers(db, run_id):
            if (worker_id is None or w['id'] == worker_id) and w['status'] not in TERMINAL:
                if w.get('pid'):
                    terminate_worker(w)
                if w.get('agent_id') and not acknowledged:
                    handoff = {'tool': 'collaboration.interrupt_agent', 'arguments': {'target': w['agent_id']}}
                    handoffs.append(handoff)
                    w.update(status='cancelling', runtime_action=handoff)
                    save(db, run_id, w)
                    continue
                if w.get('agent_id') and (w['status'] != 'cancelling' or not evidence):
                    raise ValueError('native cancellation acknowledgement requires pending cancellation and actual tool evidence')
                w.update(status=w.get('terminal_after_cancel', 'cancelled'), finished_at=time.time())
                w.pop('runtime_action', None)
                if evidence:
                    w['cancel_evidence'] = evidence
                save(db, run_id, w)
                emit('worker_cancelled', run, w)
    return {'status': status(run_id)['run']['status'], 'handoffs': handoffs}


@lru_cache(maxsize=1)
def codex_optional_features():
    try:
        result = subprocess.run(['codex', 'features', 'list'], capture_output=True, text=True, timeout=5, check=False)
        if result.returncode == 0:
            available = {line.split()[0] for line in result.stdout.splitlines() if line.split()}
            return [name for name in ('apps', 'plugins', 'memories') if name in available]
    except (OSError, subprocess.SubprocessError):
        pass
    return []


def default_command(profile, access):
    if profile['provider'] == 'codex':
        sandbox = 'read-only' if access == 'read' else 'workspace-write'
        # Restrictive inherited/operator sandbox settings must never be widened.
        if os.environ.get('HARNESS_PARENT_SANDBOX') == 'read-only' or os.environ.get('CODEX_SANDBOX_MODE') == 'read-only':
            sandbox = 'read-only'
        try:
            import tomllib
            file = Path(os.environ.get('CODEX_HOME', str(Path.home() / '.codex'))) / 'config.toml'
            if file.exists() and tomllib.loads(file.read_text()).get('sandbox_mode') == 'read-only':
                sandbox = 'read-only'
        except (OSError, ValueError):
            pass  # The CLI still validates and loads its authoritative user config.
        command = ['codex', 'exec', '--json', '--sandbox', sandbox, '--model', profile['model'], '-c', 'model_reasoning_effort="' + profile['effort'] + '"', '-c', 'skills.include_instructions=false']
        for feature in codex_optional_features():
            command.extend(['--disable', feature])
        return [*command, '--', '{prompt}']
    command = ['claude', '-p', '--output-format', 'stream-json', '--verbose', '--model', profile['model'], '--effort', profile['effort']]
    if access == 'read':
        command.extend(['--tools', 'Read'])
    return [*command, '--', '{prompt}']


def external(run_id, worker_id):
    if not Path('/proc/self/stat').exists():
        raise ValueError('external worker supervision requires Linux /proc process identity support')
    if not harness_config.load()['dispatch']['enabled']:
        raise ValueError('role dispatch is disabled; reserved external worker was not launched')
    with connection() as db:
        db.execute('BEGIN IMMEDIATE')
        run = get_run(db, run_id)
        w = next(w for w in workers(db, run_id) if w['id'] == worker_id)
        if w['status'] != 'reserved' or w['profile']['provider'] == 'native':
            raise ValueError('external worker must be reserved; another supervisor may already own it')
        identity = process_identity(os.getpid())
        w.update(status='launching', launch_token=uuid.uuid4().hex, supervisor_pid=os.getpid(), supervisor_start=identity['start'] if identity else None)
        save(db, run_id, w)
    try:
        _external_owned(run_id, worker_id, run, w)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        with connection() as db:
            current = next(x for x in workers(db, run_id) if x['id'] == worker_id)
        if current['status'] not in TERMINAL and current.get('launch_token') == w['launch_token']:
            finish(run_id, worker_id, {'status': 'failed', 'evidence': type(error).__name__})


def parse_terminal_report(text):
    if not isinstance(text, str):
        return None
    candidates = [text.strip(), *re.findall(r'```(?:json)?\s*\n(.*?)```', text, flags=re.DOTALL)]
    result = None
    for candidate in candidates:
        try:
            value = json.loads(candidate)
            if isinstance(value, dict) and 'status' in value:
                result = value
        except ValueError:
            continue
    return result


def _external_owned(run_id, worker_id, run, w):
    profile = w['profile']
    prompt = message(run, w)
    report = db_path().parent / 'dispatch-artifacts' / run_id / (worker_id + '.result.json')
    report.parent.mkdir(parents=True, exist_ok=True)
    local_report = Path(w['workspace']) / ('.harness-dispatch-report-' + uuid.uuid4().hex + '.json')
    command = profile.get('command')
    if command is None:
        command = default_command(profile, w['access'])
    schema_bytes = 0
    if profile['provider'] == 'claude' and not profile.get('command'):
        schema = {'type': 'object', 'additionalProperties': False,
                  'properties': {'status': {'type': 'string', 'enum': ['completed', 'failed']},
                                 'evidence': {'type': 'string', 'minLength': 1},
                                 'artifacts': {'type': 'array', 'items': {'type': 'string'}},
                                 'acceptance': {'type': 'array', 'minItems': len(w['acceptance']), 'maxItems': len(w['acceptance']), 'items': {'type': 'object', 'additionalProperties': False, 'properties': {'criterion': {'type': 'string', 'enum': w['acceptance']}, 'evidence': {'type': 'string', 'minLength': 1}, 'passed': {'type': 'boolean'}}, 'required': ['criterion', 'evidence', 'passed']}}},
                  'required': ['status', 'evidence', 'artifacts', 'acceptance']}
        separator = command.index('--')
        schema_json = json.dumps(schema, separators=(',', ':'))
        schema_bytes = len(schema_json.encode())
        command[separator:separator] = ['--json-schema', schema_json]
    replacements = {'{prompt}': prompt + '\nReturn only the terminal JSON report as your final response. Optional report file: ' + str(local_report), '{project}': w['workspace'], '{result}': str(local_report), '{model}': profile['model'], '{effort}': profile['effort']}
    w['context_bytes'] = len(replacements['{prompt}'].encode()) + schema_bytes
    if w['context_bytes'] > harness_config.load()['dispatch'].get('max_context_bytes', 12000):
        finish(run_id, worker_id, {'status': 'failed', 'evidence': 'external final worker packet exceeds configured context byte bound'})
        return
    with connection() as packet_db:
        packet_db.execute('BEGIN IMMEDIATE')
        current = next(x for x in workers(packet_db, run_id) if x['id'] == worker_id)
        if current['status'] != 'launching' or current.get('launch_token') != w['launch_token']:
            return
        current['context_bytes'] = w['context_bytes']
        save(packet_db, run_id, current)
        w = current
    argv = [replacements.get(arg, arg) for arg in command]
    env = {k: v for k, v in os.environ.items() if k not in ('CODEX_THREAD_ID', 'CLAUDE_SESSION_ID', 'HARNESS_SESSION_ID', 'CLAUDECODE', 'HARNESS_RUN_ID')}
    worker_state = Path(w['workspace']) / '.harness-dispatch-state'
    worker_state.mkdir(exist_ok=True, mode=0o700)
    env.update(HARNESS_DB_ROOT=str(worker_state), HARNESS_WORKFLOW_STATE=str(worker_state / 'workflow.sqlite'), HARNESS_AUDIT_OUTBOX=str(worker_state / 'audit-outbox.sqlite'), HARNESS_DISPATCH_DB=str(worker_state / 'dispatch.sqlite'))
    launch_state = report.with_suffix('.launch-state.json')
    launch_command = report.with_suffix('.argv.json')
    launch_state.write_text(json.dumps({'version': 1, 'task_kind': 'change' if w['access'] == 'edit' else 'research', 'area': 'other', 'proposed_action': 'start_routine_agent', 'reversibility': 'reversible', 'uncertainty_reason': 'none'}))
    launch_command.write_text(json.dumps(argv))
    launch_state.chmod(0o600)
    launch_command.chmod(0o600)
    argv = [str(harness_config.ROOT / 'scripts/harness'), 'launch', '--state', str(launch_state), '--project', w['workspace'], '--default-command', str(launch_command)]
    output = report.with_suffix('.jsonl')
    code = -1
    usage = {}
    try:
        with output.open('w') as stream:
            output.chmod(0o600)
            with connection() as db:
                db.execute('BEGIN IMMEDIATE')
                current = next(x for x in workers(db, run_id) if x['id'] == worker_id)
                if current['status'] != 'launching' or current.get('launch_token') != w['launch_token']:
                    return
                # Keep the lease locked across process creation and PID publication.
                process = subprocess.Popen(argv, cwd=w['workspace'], env=env, stdin=subprocess.DEVNULL, stdout=stream, stderr=subprocess.STDOUT, start_new_session=True)
                identity = process_identity(process.pid)
                current.update(status='running', pid=process.pid, process_start=identity['start'] if identity else None)
                w = current
                save(db, run_id, w)
                emit('worker_started', run, w)
            try:
                code = process.wait(timeout=harness_config.load()['dispatch'].get('timeout_seconds', 900))
            except subprocess.TimeoutExpired:
                terminate_worker(w)
                process.wait()
                code = 124
        result = None
        for line in output.read_text().splitlines():
            try:
                event = json.loads(line)
                candidate = event.get('usage') or {}
                if isinstance(candidate, dict):
                    for key in ('input_tokens', 'cached_input_tokens', 'output_tokens', 'total_tokens'):
                        if type(candidate.get(key)) is int:
                            usage[key] = candidate[key]
                    if type(candidate.get('cache_read_input_tokens')) is int:
                        usage['cached_input_tokens'] = candidate['cache_read_input_tokens']
                    if 'input_tokens' in candidate and any(key in candidate for key in ('cache_creation_input_tokens', 'cache_read_input_tokens')):
                        usage['input_tokens'] = sum(candidate.get(key, 0) for key in ('input_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'))
                    if 'input_tokens' in usage and 'output_tokens' in usage and 'total_tokens' not in usage:
                        usage['total_tokens'] = sum(candidate.get(key, 0) for key in ('input_tokens', 'output_tokens', 'cache_creation_input_tokens', 'cache_read_input_tokens'))
                if type(event.get('total_cost_usd')) in (int, float):
                    usage['cost_usd'] = event['total_cost_usd']
                if isinstance(event.get('structured_output'), dict):
                    result = event['structured_output']
                text = event.get('result')
                if event.get('type') == 'item.completed' and isinstance(event.get('item'), dict):
                    text = event['item'].get('text')
                if not isinstance(event.get('structured_output'), dict):
                    candidate_result = parse_terminal_report(text)
                    if candidate_result:
                        result = candidate_result
                if event.get('status') in ('completed', 'failed') and 'acceptance' in event:
                    result = event
            except (ValueError, AttributeError, TypeError):
                continue
        if local_report.is_symlink():
            raise ValueError('worker report cannot be a symlink')
        if local_report.exists():
            result = json.loads(local_report.read_text())
            if not local_report.is_symlink():
                local_report.unlink()
        result = result or {'status': 'failed', 'evidence': 'No terminal acceptance report returned'}
        report.write_text(json.dumps(result))
        report.chmod(0o600)
        if code != 0:
            result['status'] = 'failed'
    except (OSError, ValueError) as error:
        result = {'status': 'failed', 'evidence': type(error).__name__}
    result.update(usage, exit_code=code)
    try:
        finish(run_id, worker_id, result)
    except ValueError:
        with connection() as db:
            current = next(x for x in workers(db, run_id) if x['id'] == worker_id)
        if current['status'] not in TERMINAL:
            finish(run_id, worker_id, {'status': 'failed', 'evidence': 'Invalid terminal acceptance report', 'exit_code': code})


def run_workers(run_id):
    with concurrent.futures.ThreadPoolExecutor(max_workers=harness_config.load()['dispatch'].get('max_workers', 3)) as pool:
        while True:
            ready = next_workers(run_id)
            if ready.get('status') == 'disabled':
                return ready
            native = [w for w in ready['workers'] if w['provider'] == 'native']
            futures = [pool.submit(external, run_id, w['worker_id']) for w in ready['workers'] if w['provider'] != 'native']
            for future in futures:
                future.result()
            if native:
                return {'run_id': run_id, 'native_handoffs': native}
            current = status(run_id)
            if current['run']['status'] != 'active' or not ready['workers']:
                return current


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=('create', 'next', 'status', 'run', 'bind', 'complete', 'cancel', 'message', 'integrate', 'execute'))
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--session-id', default=os.environ.get('HARNESS_SESSION_ID') or os.environ.get('CODEX_THREAD_ID') or os.environ.get('CLAUDE_SESSION_ID'))
    parser.add_argument('--plan', type=Path)
    parser.add_argument('--run')
    parser.add_argument('--worker')
    parser.add_argument('--agent-id')
    parser.add_argument('--result', type=Path)
    parser.add_argument('--text')
    parser.add_argument('--acknowledged', action='store_true')
    parser.add_argument('--evidence')
    args = parser.parse_args()
    try:
        if args.action == 'create':
            if not args.plan or not args.session_id:
                raise ValueError('create requires --plan and exact --session-id')
            value = create(json.loads(args.plan.read_text()), args.project, args.session_id)
        else:
            if not args.run:
                raise ValueError('--run is required')
            if args.action == 'next':
                value = next_workers(args.run)
            elif args.action == 'status':
                value = status(args.run)
            elif args.action == 'run':
                value = run_workers(args.run)
            elif args.action == 'execute':
                external(args.run, args.worker)
                value = status(args.run)
            elif args.action == 'bind':
                value = bind(args.run, args.worker, args.agent_id)
            elif args.action == 'complete':
                value = finish(args.run, args.worker, json.loads(args.result.read_text()))
            elif args.action == 'integrate':
                value = integrate(args.run, args.worker)
            elif args.action == 'cancel':
                value = cancel(args.run, args.worker, args.acknowledged, args.evidence)
            else:
                with connection() as db:
                    w = next(w for w in workers(db, args.run) if w['id'] == args.worker)
                if not w.get('agent_id') or w['status'] != 'running' or not args.text:
                    raise ValueError('message requires a running bound native worker and --text')
                value = {'tool': 'collaboration.send_message', 'arguments': {'target': w['agent_id'], 'message': args.text}}
        print(json.dumps(value, indent=2))
    except (ValueError, OSError, StopIteration, sqlite3.Error) as error:
        parser.error(str(error))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
