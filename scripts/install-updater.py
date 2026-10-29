#!/usr/bin/env python3
"""Install the one-way Rust updater and stable launchers on Linux/macOS computers."""
from __future__ import annotations
import argparse
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shlex
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
MARKERS = ('scripts/hooks/auto-init.sh','scripts/hooks/session-route.sh','scripts/hooks/jev-observe.sh',
           'scripts/retrieval-reminder.sh','scripts/workflow_audit.py','scripts/workflow_gate.py',
           'scripts/session_hook.py','scripts/observe_commands.py','scripts/failure_budget.py','scripts/slim_session_logs.py',
           'scripts/worktree_cleanup.py')


def rewrite_hooks(path: Path, release: Path, launcher: Path):
    data = json.loads(path.read_text())
    for groups in data.get('hooks', {}).values():
        for group in groups:
            for hook in group.get('hooks', []):
                command = hook.get('command', '')
                if not any(marker in command for marker in MARKERS):
                    continue
                args = shlex.split(command)
                if args[0] == 'python3':
                    args.pop(0)
                relative = next((m for m in MARKERS if args[0] == m or args[0].endswith('/'+m)), None)
                if relative is None or not (release/relative).is_file():
                    # Keep local unpublished hook additions that this release does not contain.
                    continue
                nested = ['@root/' + Path(a).relative_to(release).as_posix()
                          if a.startswith(str(release) + '/') else a for a in args[1:]]
                hook['command'] = shlex.join([str(launcher), 'exec', relative, *nested])
    path.write_text(json.dumps(data, indent=2) + '\n')


def command(*args, **kwargs):
    return subprocess.run([str(a) for a in args], check=True, **kwargs)


def tool(name):
    path = shutil.which(name)
    if not path:
        raise RuntimeError(f'{name} is required')
    return str(Path(path).absolute())


def write_executable(path: Path, body: str):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(0o755)


def install(args):
    home = Path.home()
    config_home = Path(os.environ.get('XDG_CONFIG_HOME', home/'.config'))
    directory = config_home/'harness'
    directory.mkdir(parents=True, exist_ok=True)
    cfgfile = directory/'updater.json'
    state = home/'.local/share/harness-updater'
    binary = home/'.local/libexec/harness-updater'
    launcher = home/'.local/bin/harness-runtime'
    cli = home/'.local/bin/harness'
    # Preserve developer state and existing machine authentication; no credentials copied.
    cfg = {'repository': args.repository, 'url': args.url, 'branch': args.branch,
           'workflows': args.workflow or ['ci.yml'], 'developer_root': str(ROOT),
           'state': str(state), 'git': tool('git'), 'gh': shutil.which('gh'),
           'python': tool('python3'), 'poll_seconds': args.interval}
    if cfgfile.exists() and json.loads(cfgfile.read_text()) != cfg:
        raise RuntimeError(f'Existing updater settings differ; edit {cfgfile} explicitly before reinstalling')
    command(tool('cargo'), 'build', '--release', '--locked', '--manifest-path', ROOT/'services/harness-updater/Cargo.toml')
    binary.parent.mkdir(parents=True, exist_ok=True)
    temporary = binary.with_suffix('.new')
    shutil.copy2(ROOT/'services/harness-updater/target/release/harness-updater', temporary)
    temporary.replace(binary)
    cfgfile.write_text(json.dumps(cfg, indent=2)+'\n')
    write_executable(launcher, '#!/bin/sh\nexec '+shlex.join([str(binary),'--config',str(cfgfile)])+' "$@"\n')
    command(launcher, 'check')
    current = state/'current'
    if not current.is_symlink():
        raise RuntimeError('No CI-passing release is available yet; hooks unchanged. Rerun after CI passes.')
    release = current.resolve()
    # Existing installers preserve unrelated hook groups and back up originals.
    command(release/'scripts/install-hooks.sh')
    for path in (home/'.codex/hooks.json', home/'.claude/settings.json'):
        rewrite_hooks(path, release, launcher)
    # Follow future activations when the guard compares installed versions.
    (directory/'root').write_text(str(current)+'\n')
    cli_temp = cli.with_suffix('.new')
    if cli_temp.is_symlink():
        cli_temp.unlink()
    cli_temp.symlink_to(state/'current/scripts/harness')
    cli_temp.replace(cli)
    # Point ambient global guidance at managed commands, preserving human prose.
    for path in (home/'.codex/AGENTS.md', home/'.claude/CLAUDE.md'):
        if not path.exists():
            continue
        body = path.read_text()
        managed = '<!-- harness-managed:start -->\n## Managed harness runtime\n\nUse `'+str(cli)+'` for harness commands. Run other harness scripts through `'+str(launcher)+' exec scripts/SCRIPT [ARGS]`. These launchers pin each operation and preserve the existing machine database. The editable checkout is for development; use `'+str(launcher)+' override '+str(ROOT)+'` to test it and `'+str(launcher)+' resume` to return to published versions.\n<!-- harness-managed:end -->'
        pattern = r'<!-- harness-managed:start -->.*?<!-- harness-managed:end -->'
        body = re.sub(pattern, lambda m: managed, body, flags=re.S) if re.search(pattern, body, re.S) else managed+'\n\n'+body
        body = body.replace(str(ROOT)+'/scripts/harness ',str(cli)+' ')
        path.write_text(body)
    if not args.no_service:
        service(cfgfile,binary)
    command(launcher,'status')
    print(f'Installed updater; stable harness CLI: {cli}. New agent sessions load managed hooks.')


def service(config: Path, binary: Path):
    home = Path.home()
    system = platform.system()
    env = {'PATH': os.environ.get('PATH', '/usr/local/bin:/usr/bin:/bin'), 'HOME': str(home)}
    if system == 'Linux':
        target = home/'.config/systemd/user/harness-updater.service'
        target.parent.mkdir(parents=True, exist_ok=True)
        def quote(value):
            return '"'+str(value).replace('\\','\\\\').replace('"','\\"').replace('%','%%')+'"'
        body = '[Unit]\nDescription=Activate verified published harness versions\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart='+quote(binary)+' --config '+quote(config)+' daemon\nRestart=on-failure\nRestartSec=15\n'
        for key,value in env.items():
            body += 'Environment='+quote(key+'='+value)+'\n'
        body += '\n[Install]\nWantedBy=default.target\n'
        target.write_text(body)
        command('systemctl','--user','daemon-reload')
        command('systemctl','--user','enable','harness-updater.service')
        command('systemctl','--user','restart','harness-updater.service')
    elif system == 'Darwin':
        target = home/'Library/LaunchAgents/local.harness.updater.plist'
        target.parent.mkdir(parents=True, exist_ok=True)
        logs = home/'Library/Logs'; logs.mkdir(exist_ok=True)
        target.write_bytes(plistlib.dumps({'Label':'local.harness.updater', 'ProgramArguments':[str(binary),'--config',str(config),'daemon'],
            'RunAtLoad':True,'KeepAlive':True,'ThrottleInterval':15,'EnvironmentVariables':env,
            'StandardOutPath':str(logs/'harness-updater.log'),'StandardErrorPath':str(logs/'harness-updater-error.log')}))
        domain = f'gui/{os.getuid()}'
        subprocess.run(['launchctl','bootout',domain+'/local.harness.updater'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        command('launchctl','bootstrap',domain,target)
    else:
        raise RuntimeError('Automatic services are supported only on Linux and macOS computers')


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--repository',default='firebrikdotcom/arc-harness')
    p.add_argument('--url',default='https://github.com/firebrikdotcom/arc-harness.git')
    p.add_argument('--branch',default='master')
    p.add_argument('--workflow',action='append')
    p.add_argument('--interval',type=int,default=60)
    p.add_argument('--no-service',action='store_true')
    args=p.parse_args()
    try:
        install(args)
    except (RuntimeError,subprocess.CalledProcessError) as e:
        p.exit(1,str(e)+'\n')

if __name__=='__main__':
    main()
