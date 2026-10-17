#!/usr/bin/env python3
"""Durable bounded decisions and single-use action receipts (not a permission gate)."""
from __future__ import annotations
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import time
import uuid
import context_advice as advice
import harness_config
from audit_transport import emit


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()).hexdigest()


def directory(project):
    from orchestration_common import db_root
    root = db_root(project) / 'decisions'
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    return root


def save(root, row):
    temp = root / ('.' + str(uuid.uuid4()))
    with temp.open('x') as stream:
        os.chmod(temp, 0o600)
        json.dump(row, stream, sort_keys=True, allow_nan=False)
    temp.replace(root / (row['id'] + '.json'))


def telemetry(kind, row):
    keys = ('id', 'parent_session_id', 'family', 'mode', 'status', 'action', 'confidence', 'min_confidence', 'needs_reasoning', 'escalation_reason', 'action_source',
            'cache_hit', 'duration_ms', 'latency_ms', 'input_tokens', 'output_tokens', 'total_tokens',
            'state_build_ms', 'fallback_reason', 'action_taken', 'exit_code', 'outcome', 'action_duration_ms')
    try:
        emit('orchestration.' + kind, {'decision_id': row['id'], 'usage': {k: row.get(k) for k in ('input_tokens', 'output_tokens', 'total_tokens')}, **{k: row.get(k) for k in keys}}, flush_now=False)
    except Exception:
        pass  # Collection is best-effort and never changes action policy.


def evaluate(*, project: Path, session_id: str, family: str, goal: str, facts: list[str],
             choices: dict[str, str], scope: dict, state: dict, baseline: str | None = None,
             bypass: str | None = None, router_path: Path | None = None) -> dict:
    began = time.monotonic()
    settings = harness_config.load()['typesafe']
    mode = settings['mode']
    if family not in advice.FAMILIES:
        raise ValueError('Invalid decision family')
    if bypass and bypass not in advice.BYPASSES - {'none'}:
        raise ValueError('Invalid deterministic bypass')
    if not session_id or not isinstance(scope, dict) or not isinstance(state, dict):
        raise ValueError('Session, scope and state are required')
    if not isinstance(choices, dict) or not 2 <= len(choices) <= advice.MAX_OPTIONS:
        raise ValueError('Supply two to eight bounded choices')
    for key, description in choices.items():
        if not isinstance(key, str) or not advice.OPTION_ID.fullmatch(key):
            raise ValueError('Invalid choice id')
        advice.text_field(description, 'choice')
    advice.text_field(goal, 'goal', 4000)
    advice.strings(facts, 'facts')
    context = {'version': 2, 'checkpoint': {'family': family, 'question_version': 'decision-1',
        'policy_version': 'decision-2', 'bypass_reason': bypass or 'none'},
        'context': {'goal': goal, 'facts': facts}, 'questions': {'recommendation': {
            'type': 'choice', 'instructions': 'Choose the best bounded next action using the supplied facts.',
            'criteria': choices}}}
    state_hash = digest(state)
    cache_key = digest([str(project.resolve()), session_id, family, context, scope, state_hash, mode, settings['model'], settings.get('min_confidence', 0.70)])
    root = directory(project)
    # Serialize identical evaluations, including outages, without a workflow DB transaction.
    with (root / ('.cache-' + cache_key + '.lock')).open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        matches = []
        for path in root.glob('*.json'):
            old = json.loads(path.read_text())
            if old.get('cache_key') == cache_key and old['expires_at'] > time.time() and old['status'] != 'rejected':
                matches.append(old)
        # A settled decision (Jev's accepted choice or a resolution) wins over an open escalation.
        matches.sort(key=lambda old: (bool(old.get('needs_reasoning')), old['status'] == 'pending'))
        for old in matches[:1]:
                result = {k: v for k, v in old.items() if k not in ('claimed_at', 'action_hash', 'outcome', 'outcome_at')}
                if result['status'] == 'pending':
                    result.update(status='fallback', fallback_reason='unfinished_previous_attempt', escalation_reason='unfinished_previous_attempt', needs_reasoning=True, action='normal_reasoning')
                result.update(id=str(uuid.uuid4()), cached_from=old['id'], cache_hit=True, latency_ms=0,
                              input_tokens=None, output_tokens=None, total_tokens=None,
                              state_build_ms=round((time.monotonic()-began)*1000),
                              duration_ms=round((time.monotonic()-began)*1000))
                save(root, result)
                telemetry('decision_evaluated', result)
                return result
        row = {'id': str(uuid.uuid4()), 'parent_session_id': session_id, 'family': family, 'mode': mode,
            'status': 'pending', 'action': 'normal_reasoning', 'scope': scope, 'choices': choices,
            'confidence': None, 'min_confidence': settings.get('min_confidence', 0.70),
            'needs_reasoning': True, 'action_source': 'escalation', 'model': settings['model'],
            'state_hash': state_hash, 'state': state, 'cache_key': cache_key, 'cache_hit': False,
            'expires_at': time.time() + settings.get('decision_ttl_seconds', 300), 'call_id': None,
            'latency_ms': None, 'input_tokens': None, 'output_tokens': None, 'total_tokens': None,
            'state_build_ms': round((time.monotonic()-began)*1000)}
        save(root, row)  # A crash/provider outage leaves a durable pending attempt.
        if bypass or mode == 'off':
            row.update(status='bypassed', fallback_reason=bypass or 'typesafe_disabled')
            if bypass and baseline in choices:
                row.update(action=baseline, needs_reasoning=False, action_source='deterministic')
        else:
            try:
                router = advice.load_router(router_path or Path(os.environ.get('HARNESS_TYPESAFE_ROUTER',
                    str(Path.home() / '.agents/skills/typesafe-routing/scripts/route.py'))))
                advice.validate_context(context, router)
                result = advice.advise(context, router, root.parent, delegate=(mode == 'active'))
                row.update(call_id=result['call_id'], status=result['status'], latency_ms=result['latency_ms'],
                           fallback_reason=result.get('fallback_reason'), confidence=result.get('confidence'))
                record = json.loads(Path(result['record_path']).read_text())
                usage = record.get('usage') or {}
                row.update({k: usage.get(k) for k in ('input_tokens', 'output_tokens', 'total_tokens')})
                choice = ((result.get('answers') or {}).get('recommendation') or {}).get('choice')
                confidence = result.get('confidence', ((result.get('answers') or {}).get('recommendation') or {}).get('confidence'))
                row['confidence'] = confidence
                if mode == 'active' and result['status'] == 'evaluated' and choice in choices:
                    if advice.finite(confidence, 0, 1) and confidence >= row['min_confidence']:
                        row.update(action=choice, needs_reasoning=False, action_source='jev')
                    else:
                        row.update(status='fallback', fallback_reason='missing_confidence' if confidence is None else 'low_confidence')
            except advice.InputError:
                row.update(status='rejected', fallback_reason='invalid_context')
                save(root, row)
                raise
            except Exception as error:
                row.update(status='fallback', fallback_reason=type(error).__name__)
        row['escalation_reason'] = (row.get('fallback_reason') or ('shadow_mode' if mode == 'shadow' else 'no_recommendation')) if row['needs_reasoning'] else None
        row['duration_ms'] = round((time.monotonic()-began)*1000)
        save(root, row)
        telemetry('decision_evaluated', row)
        return row


def current_settings(row):
    settings = harness_config.load()['typesafe']
    return (row['mode'] == settings['mode'] and row.get('model') == settings['model']
            and row.get('min_confidence') == settings.get('min_confidence', 0.70))


def resolve(*, project: Path, identifier: str, session_id: str, action: str, reason: str,
            scope: dict | None = None, state: dict | None = None) -> dict:
    """Record a single agent judgment after escalation; never replace an accepted Jev choice."""
    advice.text_field(reason, 'reason')
    root = directory(project)
    identifier = str(uuid.UUID(identifier))
    import workflow_audit
    with workflow_audit.context_db() as db:
        current = workflow_audit.load(db, session_id)
    with (root / '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        row = json.loads((root / (identifier + '.json')).read_text())
        if (row['parent_session_id'] != session_id or row['expires_at'] <= time.time()
                or row.get('claimed_at') or not row.get('needs_reasoning')
                or row['status'] not in ('fallback', 'bypassed', 'evaluated') or not current_settings(row)):
            raise ValueError('Resolution requires a fresh, unresolved escalation in this session')
        if current.get('request_id') and any(row['scope'].get(key) != current.get(key)
                for key in ('request_id', 'request_seq', 'plan_revision')):
            raise ValueError('Decision scope does not match the current prompt and plan revision')
        original_state = row.get('state', {})
        if ('evidence_revision' in original_state and original_state['evidence_revision'] != current.get('evidence_revision', 0)
                or 'active_todo_id' in original_state and original_state['active_todo_id'] != current.get('active_todo_id')):
            raise ValueError('Decision evidence or active task changed; evaluate current state')
        if scope is not None and scope != row['scope'] or state is not None and digest(state) != row['state_hash']:
            raise ValueError('Decision state or scope has changed; evaluate current state')
        if action not in row['choices']:
            raise ValueError('Resolution action must be one of the original bounded choices')
        # The source request remains identifiable; copies through evaluate retain the same cache key.
        row.update(status='resolved', action=action, needs_reasoning=False,
                   action_source='agent_after_escalation', resolution_reason=reason, resolved_at=time.time())
        save(root, row)
        telemetry('decision_resolved', row)
        return row


def claim(*, project: Path, identifier: str, session_id: str, scope: dict, state: dict, action_hash: str) -> dict:
    root = directory(project)
    identifier = str(uuid.UUID(identifier))
    import workflow_audit
    with workflow_audit.context_db() as db:
        current = workflow_audit.load(db, session_id)
    if current.get('request_id') and any(scope.get(key) != current.get(key)
            for key in ('request_id', 'request_seq', 'plan_revision')):
        raise ValueError('Decision scope does not match the current prompt and plan revision')
    with (root / '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        row = json.loads((root / (identifier + '.json')).read_text())
        if (row['parent_session_id'] != session_id or row['scope'] != scope or row['state_hash'] != digest(state)
                or row['expires_at'] <= time.time() or row['status'] in ('pending', 'rejected') or row.get('claimed_at')
                or row.get('needs_reasoning') or not current_settings(row)):
            raise ValueError('Decision receipt is stale, mismatched or already consumed; evaluate current state')
        if scope.get('action_hash') != action_hash:
            raise ValueError('Decision action fingerprint mismatch')
        row.update(claimed_at=time.time(), action_hash=action_hash)
        save(root, row)
        return row


def outcome(*, project: Path, identifier: str, session_id: str, exit_code=None, duration_ms=None) -> dict:
    root = directory(project)
    identifier = str(uuid.UUID(identifier))
    with (root / '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        row = json.loads((root / (identifier + '.json')).read_text())
        if row['parent_session_id'] != session_id or not row.get('claimed_at') or row.get('outcome'):
            raise ValueError('Outcome requires an unreported action in this session')
        if exit_code is not None and type(exit_code) is not int:
            raise ValueError('Exit code must be integer or unknown')
        if duration_ms is not None and not advice.finite(duration_ms):
            raise ValueError('Duration must be non-negative or unknown')
        row.update(outcome={'exit_code': exit_code, 'duration_ms': duration_ms, 'label': 'unknown'},
                   outcome_at=time.time())
        save(root, row)
        telemetry('decision_outcome', {**row, 'action_duration_ms': duration_ms, 'action_taken': row['action'], 'exit_code': exit_code, 'outcome': 'unknown'})
        return row


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=('evaluate', 'propose', 'consume', 'claim', 'outcome', 'resolve', 'list', 'status'))
    parser.add_argument('--project', type=Path, default=Path.cwd())
    parser.add_argument('--input', type=Path)
    parser.add_argument('--id')
    parser.add_argument('--session-id', required=True)
    parser.add_argument('--scope', type=json.loads, default={})
    parser.add_argument('--state', type=json.loads, default={})
    parser.add_argument('--action-hash')
    parser.add_argument('--action')
    parser.add_argument('--reason')
    parser.add_argument('--exit-code', type=int)
    parser.add_argument('--duration-ms', type=float)
    args = parser.parse_args(argv)
    try:
        common = {'project': args.project, 'session_id': args.session_id}
        if args.command in ('evaluate', 'propose'):
            data = advice.read_json(args.input)
            result = evaluate(**common, **data)
        elif args.command in ('consume', 'claim'):
            result = claim(**common, identifier=args.id, scope=args.scope, state=args.state, action_hash=args.action_hash)
        elif args.command == 'resolve':
            result = resolve(**common, identifier=args.id, action=args.action, reason=args.reason,
                             scope=args.scope or None, state=args.state or None)
        elif args.command == 'outcome':
            result = outcome(**common, identifier=args.id, exit_code=args.exit_code, duration_ms=args.duration_ms)
        else:
            result = [json.loads(p.read_text()) for p in directory(args.project).glob('*.json')]
            result = [r for r in result if r['parent_session_id'] == args.session_id]
        print(json.dumps(result, sort_keys=True))
    except (ValueError, OSError, TypeError) as error:
        parser.error(str(error))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
