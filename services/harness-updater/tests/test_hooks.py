import importlib.util
import json
import os
from pathlib import Path
import shlex
import tempfile
import unittest
from unittest.mock import patch
ROOT=Path(__file__).resolve().parents[3]
spec=importlib.util.spec_from_file_location('installer',ROOT/'scripts/install-updater.py')
installer=importlib.util.module_from_spec(spec);spec.loader.exec_module(installer)

class Hooks(unittest.TestCase):
    def test_rewrite_pins_nested_scripts_and_preserves_unrelated_hooks(self):
        with tempfile.TemporaryDirectory() as d:
            release=Path(d)/'release with spaces';launcher=Path(d)/'harness runtime'
            (release/'scripts').mkdir(parents=True)
            (release/'scripts/session_hook.py').touch()
            file=Path(d)/'hooks.json'
            file.write_text(json.dumps({'hooks':{'SessionStart':[{'hooks':[{'command':shlex.join(['python3',str(release/'scripts/session_hook.py'),str(release/'scripts/hooks/auto-init.sh')]),'timeout':30},{'command':'echo unrelated'}]}]}}))
            installer.rewrite_hooks(file,release,launcher)
            hooks=json.loads(file.read_text())['hooks']['SessionStart'][0]['hooks']
            self.assertEqual(shlex.split(hooks[0]['command']),[str(launcher),'exec','scripts/session_hook.py','@root/scripts/hooks/auto-init.sh'])
            self.assertEqual(hooks[0]['timeout'],30);self.assertEqual(hooks[1]['command'],'echo unrelated')
    def test_unpublished_local_hook_additions_are_preserved(self):
        with tempfile.TemporaryDirectory() as d:
            base=Path(d);release=base/'release';release.mkdir()
            original='python3 '+str(base/'developer/scripts/slim_session_logs.py')+' --spawn'
            file=base/'hooks.json';file.write_text(json.dumps({'hooks':{'SessionEnd':[{'hooks':[{'command':original,'timeout':5}]}]}}))
            installer.rewrite_hooks(file,release,base/'runtime')
            self.assertEqual(json.loads(file.read_text())['hooks']['SessionEnd'][0]['hooks'][0]['command'],original)

    def test_managed_cli_remains_recognizable_and_enters_the_launcher(self):
        import subprocess
        with tempfile.TemporaryDirectory() as d:
            home=Path(d);bin_dir=home/'.local/bin';bin_dir.mkdir(parents=True)
            state=home/'.local/share/harness-updater';state.mkdir(parents=True)
            (state/'current').symlink_to(ROOT,target_is_directory=True)
            runtime=bin_dir/'harness-runtime';log=home/'called'
            runtime.write_text('#!/bin/sh\n'+'printf \'%s\\n\' "$@" > '+shlex.quote(str(log))+'\nexit 42\n');runtime.chmod(0o755)
            cli=bin_dir/'harness';cli.symlink_to(state/'current/scripts/harness')
            environment=dict(os.environ,HOME=str(home));environment.pop('HARNESS_RUNTIME_ACTIVE',None)
            result=subprocess.run([str(cli),'--help'],env=environment,capture_output=True)
            self.assertEqual(result.returncode,42,result.stderr)
            self.assertEqual(log.read_text().splitlines(),['exec','scripts/harness','--help'])
            self.assertEqual(cli.resolve(),ROOT/'scripts/harness')

    def test_developer_control_plane_is_not_registered_as_a_new_target(self):
        import subprocess
        with tempfile.TemporaryDirectory() as d:
            base=Path(d).resolve();dev=base/'developer';dev.mkdir();db=base/'db';(db/'targets').mkdir(parents=True)
            environment=dict(os.environ,HARNESS_DB_ROOT=str(db),HARNESS_DEVELOPER_ROOT=str(dev))
            script=ROOT/'scripts/harness-target.sh'
            for operation in ('register','lookup'):
                result=subprocess.run([str(script),operation,str(dev)],env=environment,capture_output=True)
                self.assertNotEqual(result.returncode,0,result.stdout)
            self.assertEqual(list((db/'targets').iterdir()),[])

    def test_control_database_does_not_override_target_state(self):
        spec=importlib.util.spec_from_file_location('common',ROOT/'scripts/orchestration_common.py')
        common=importlib.util.module_from_spec(spec);spec.loader.exec_module(common)
        with tempfile.TemporaryDirectory() as d:
            base=Path(d).resolve();dev=base/'dev';dev.mkdir();db=base/'state'
            with patch.dict(os.environ,{'HARNESS_CONTROL_DB_ROOT':str(db),'HARNESS_DB_ROOT':str(db),'HARNESS_DEVELOPER_ROOT':str(dev)}):
                self.assertEqual(common.db_root(dev),db)
                with patch.object(common.subprocess,'run') as run:
                    run.return_value.returncode=0;run.return_value.stdout=str(db/'targets/project/db')
                    self.assertEqual(common.db_root(base/'project'),db/'targets/project/db')
            with patch.dict(os.environ,{'HARNESS_DB_ROOT':str(db/'explicit')},clear=True):
                self.assertEqual(common.db_root(dev),db/'explicit')

if __name__=='__main__':unittest.main()
