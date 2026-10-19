"""Exercise the real installer in a disposable home with a synthetic Git remote."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
ROOT=Path(__file__).resolve().parents[3]

class Installer(unittest.TestCase):
    def test_real_install_preserves_local_additions_and_uses_a_stable_root(self):
        with tempfile.TemporaryDirectory(prefix='harness-install-') as tmp:
            base=Path(tmp).resolve();repo=base/'source';repo.mkdir();home=base/'home';home.mkdir()
            archive=base/'source.tar'
            subprocess.run(['git','-C',str(ROOT),'archive','HEAD','--output',str(archive)],check=True)
            subprocess.run(['tar','-xf',str(archive),'-C',str(repo)],check=True)
            for name in ['scripts/harness','scripts/harness-target.sh','scripts/orchestration_common.py','scripts/install-updater.py']:
                shutil.copy2(ROOT/name,repo/name)
            shutil.copytree(ROOT/'services/harness-updater',repo/'services/harness-updater',ignore=shutil.ignore_patterns('target','__pycache__'),dirs_exist_ok=True)
            for args in [('init','-q'),('config','user.email','test@example.invalid'),('config','user.name','Fixture'),('add','.'),('commit','-qm','Create synthetic updater fixture.')]:
                subprocess.run(['git','-C',str(repo),*args],check=True)
            (repo/'services/harness-updater/target').symlink_to(ROOT/'services/harness-updater/target',target_is_directory=True)
            local=repo/'unpublished-local-work';local.write_text('preserve this')
            db=repo/'.harness-db';db.mkdir();(db/'history').write_text('preserve existing state')
            bin_dir=base/'bin';bin_dir.mkdir();gh=bin_dir/'gh'
            gh.write_text('#!'+sys.executable+'\nimport sys,json\nsha=sys.argv[sys.argv.index("--commit")+1]\nprint(json.dumps([{ "headSha":sha,"status":"completed","conclusion":"success","databaseId":1}]))\n');gh.chmod(0o755)
            codex=home/'.codex';codex.mkdir();(codex/'AGENTS.md').write_text('Preserve human guidance.\n')
            local_hook='python3 '+str(repo/'scripts/slim_session_logs.py')+' --spawn'
            (codex/'hooks.json').write_text(json.dumps({'hooks':{'SessionEnd':[{'hooks':[{'command':local_hook}]}],'OtherEvent':[{'hooks':[{'command':'echo unrelated'}]}]}}))
            environment=dict(os.environ,HOME=str(home),XDG_CONFIG_HOME=str(home/'.config'),HARNESS_CONFIG_HOME=str(home/'.config/harness'),PATH=str(bin_dir)+os.pathsep+os.environ['PATH'],RUSTUP_HOME=os.environ.get('RUSTUP_HOME',str(Path.home()/'.rustup')),CARGO_HOME=os.environ.get('CARGO_HOME',str(Path.home()/'.cargo')))
            subprocess.run([sys.executable,str(repo/'scripts/install-updater.py'),'--repository','example/harness','--url',str(repo),'--no-service'],env=environment,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=120)
            state=home/'.local/share/harness-updater'
            self.assertEqual((home/'.config/harness/root').read_text().strip(),str(state/'current'))
            self.assertEqual(local.read_text(),'preserve this');self.assertEqual((db/'history').read_text(),'preserve existing state')
            hooks=json.loads((codex/'hooks.json').read_text())['hooks']
            self.assertEqual(hooks['OtherEvent'][0]['hooks'][0]['command'],'echo unrelated')
            self.assertEqual(hooks['SessionEnd'][0]['hooks'][0]['command'],local_hook)
            commands=[hook['command'] for groups in hooks.values() for group in groups for hook in group.get('hooks',[])]
            self.assertTrue(any('harness-runtime exec scripts/workflow_gate.py' in command for command in commands))
            cli=home/'.local/bin/harness'
            result=subprocess.run([str(cli),'--help'],env=environment,check=True,capture_output=True,text=True)
            self.assertIn('Usage: harness',result.stdout)
            self.assertIn('Preserve human guidance.',(codex/'AGENTS.md').read_text())

if __name__=='__main__':unittest.main()
