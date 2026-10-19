"""Real Git repositories and subprocesses exercise the installed Rust CLI."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT=Path(__file__).resolve().parents[3]
MANIFEST=ROOT/'services/harness-updater/Cargo.toml'
BINARY=MANIFEST.parent/'target/debug/harness-updater'

class Updater(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory(prefix='harness-updater-')
        self.addCleanup(self.temp.cleanup)
        self.base=Path(self.temp.name).resolve()
        self.repo=self.base/'remote';self.repo.mkdir()
        self.dev=self.base/'developer';self.dev.mkdir()
        (self.dev/'local-change').write_text('keep my unpublished work')
        self.state=self.base/'managed';self.state.mkdir()
        self.git('init','-b','master')
        self.git('config','user.email','test@example.invalid');self.git('config','user.name','Test')
        self.script('scripts/harness','#!/bin/sh\nexit 0\n')
        self.script('scripts/install-hooks.sh','#!/bin/sh\nexit 0\n')
        self.commit()
        self.answers=self.base/'ci.json';self.ci(True)
        gh=self.base/'gh'
        gh.write_text('#!'+shutil.which('python3')+'\nimport json,sys\nfrom pathlib import Path\np=Path('+repr(str(self.answers))+')\nr=json.loads(p.read_text());sha=sys.argv[sys.argv.index("--commit")+1]\nfor x in r:\n if x["headSha"]=="MATCH":x["headSha"]=sha\nprint(json.dumps(r))\n')
        gh.chmod(0o755)
        self.cfg=self.base/'config.json'
        self.config={'repository':'example/harness','url':str(self.repo),'branch':'master','workflows':['ci.yml'],'developer_root':str(self.dev),'state':str(self.state),'git':shutil.which('git'),'gh':str(gh),'python':shutil.which('python3'),'poll_seconds':10}
        self.save()
    def save(self):self.cfg.write_text(json.dumps(self.config))
    def git(self,*args):return subprocess.check_output(['git','-C',str(self.repo),*args],stderr=subprocess.DEVNULL,text=True).strip()
    def script(self,name,body):
        p=self.repo/name;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(body);p.chmod(0o755)
    def commit(self):
        self.git('add','.');self.git('commit','-m','Update fixture');return self.git('rev-parse','HEAD')
    def ci(self,passed):self.answers.write_text(json.dumps([{'headSha':'MATCH','databaseId':1,'status':'completed','conclusion':'success' if passed else 'failure'}]))
    def run_cli(self,*args,ok=True):
        p=subprocess.run([str(BINARY),'--config',str(self.cfg),*args],text=True,capture_output=True,timeout=20)
        if ok:self.assertEqual(p.returncode,0,p.stderr)
        else:self.assertNotEqual(p.returncode,0,p.stdout)
        return p
    def current(self):return (self.state/'current').resolve()
    def test_activates_exact_commit_preserving_dirty_source(self):
        before=(self.dev/'local-change').read_bytes();sha=self.git('rev-parse','HEAD')
        self.run_cli('check');self.assertEqual(self.current().name,sha)
        self.assertEqual((self.dev/'local-change').read_bytes(),before)
        self.assertEqual(list(self.dev.iterdir()),[self.dev/'local-change'])
        self.run_cli('check');self.assertEqual(len(list((self.state/'releases').iterdir())),1)
    def test_failed_missing_or_wrong_commit_ci_never_activates(self):
        self.ci(False);self.run_cli('check');self.assertFalse((self.state/'current').exists())
        for answers in ([],[{'headSha':'0'*40,'databaseId':2,'status':'completed','conclusion':'success'}]):
            self.answers.write_text(json.dumps(answers));self.run_cli('check');self.assertFalse((self.state/'current').exists())
    def test_latest_rerun_and_every_workflow_must_pass(self):
        self.answers.write_text(json.dumps([{'headSha':'MATCH','databaseId':1,'status':'completed','conclusion':'success'},{'headSha':'MATCH','databaseId':2,'status':'in_progress','conclusion':''}]))
        self.run_cli('check');self.assertFalse((self.state/'current').exists())
        gh=Path(self.config['gh']);gh.write_text('#!'+shutil.which('python3')+'\nimport sys,json\nsha=sys.argv[sys.argv.index("--commit")+1]\nprint(json.dumps([] if "second.yml" in sys.argv else [{"headSha":sha,"databaseId":3,"status":"completed","conclusion":"success"}]))\n')
        self.config['workflows']=['ci.yml','second.yml'];self.save();self.run_cli('check');self.assertFalse((self.state/'current').exists())
    def test_update_then_rollback_pauses_until_resume(self):
        self.run_cli('check');first=self.current()
        self.script('version','two');second=self.commit();self.run_cli('check')
        self.assertEqual(self.current().name,second);self.assertEqual((self.state/'previous').resolve(),first)
        self.run_cli('rollback');self.assertEqual(self.current(),first)
        self.run_cli('check');self.assertEqual(self.current(),first)
        self.run_cli('resume');self.run_cli('check');self.assertEqual(self.current().name,second)
    def test_override_runs_local_source_and_pauses_activation(self):
        self.run_cli('check');old=self.current()
        (self.dev/'scripts').mkdir();p=self.dev/'scripts/harness';p.write_text('#!/bin/sh\necho LOCAL\n');p.chmod(0o755)
        self.run_cli('override',str(self.dev));self.assertEqual(self.run_cli('exec','scripts/harness').stdout.strip(),'LOCAL')
        self.script('version','two');self.commit();self.run_cli('check');self.assertEqual(self.current(),self.dev)
        self.run_cli('resume');self.assertEqual(self.current(),old);self.run_cli('check');self.assertNotEqual(self.current(),old)
    def test_running_operation_defers_activation(self):
        self.script('scripts/hold','#!/bin/sh\ntouch "$1"\nsleep 2\n');self.commit();self.run_cli('check');old=self.current()
        ready=self.base/'ready'
        child=subprocess.Popen([str(BINARY),'--config',str(self.cfg),'exec','scripts/hold',str(ready)])
        self.addCleanup(lambda: child.poll() is None and child.kill())
        for _ in range(100):
            if ready.exists():break
            time.sleep(.02)
        self.assertTrue(ready.exists())
        self.script('version','two');sha=self.commit();self.run_cli('check');self.assertEqual(self.current(),old)
        child.wait(timeout=5);self.run_cli('check');self.assertEqual(self.current().name,sha)
    def test_launcher_keeps_database_locations_and_resolves_nested_paths(self):
        self.script('scripts/probe.py','import os,sys,json\nprint(json.dumps({"db":os.environ["HARNESS_DB_ROOT"],"workflow":os.environ["HARNESS_WORKFLOW_STATE"],"dispatch":os.environ["HARNESS_DISPATCH_DB"],"root":os.environ["HARNESS_ROOT"],"nested":sys.argv[1]}))\n')
        self.commit();self.run_cli('check')
        p=self.run_cli('exec','scripts/probe.py','@root/scripts/harness');data=json.loads(p.stdout)
        self.assertEqual(data['db'],str(self.dev/'.harness-db'))
        self.assertEqual(data['workflow'],str(self.dev/'.harness-db/workflow-state.sqlite'))
        self.assertEqual(data['dispatch'],str(self.dev/'.harness-db/dispatch.sqlite'))
        self.assertEqual(data['root'],str(self.current()))
        self.assertEqual(data['nested'],str(self.current()/'scripts/harness'))
    def test_invalid_script_and_corrupt_config_refused(self):
        self.run_cli('check');self.run_cli('exec','../remote/scripts/harness',ok=False)
        self.config['state']=str(self.dev/'managed');self.save();self.run_cli('check',ok=False)
    def test_provider_or_fetch_failure_preserves_active_release(self):
        self.run_cli('check');old=self.current();self.script('version','two');self.commit()
        Path(self.config['gh']).write_text('#!/bin/sh\nexit 1\n');self.run_cli('check',ok=False);self.assertEqual(self.current(),old)
        self.assertEqual(json.loads((self.state/'status.json').read_text())['stage'],'error')
    def test_existing_git_url_rewrite_is_respected(self):
        config=self.base/'gitconfig'
        config.write_text('[url "'+str(self.repo)+'"]\n    insteadOf = alias://harness\n')
        self.config['url']='alias://harness';self.save()
        with patch.dict(os.environ,{'GIT_CONFIG_GLOBAL':str(config)}):
            self.run_cli('check')
        self.assertEqual(self.current().name,self.git('rev-parse','HEAD'))
    def test_failed_smoke_never_activates(self):
        self.run_cli('check');old=self.current();self.script('scripts/harness','#!/bin/sh\nexit 3\n');self.commit()
        self.run_cli('check',ok=False);self.assertEqual(self.current(),old)
    def test_concurrent_pollers_serialize(self):
        commands=[[str(BINARY),'--config',str(self.cfg),'check']]*4
        children=[subprocess.Popen(cmd,stdout=subprocess.PIPE,stderr=subprocess.PIPE) for cmd in commands]
        for child in children:
            _,error=child.communicate(timeout=20);self.assertEqual(child.returncode,0,error)
        self.assertEqual(self.current().name,self.git('rev-parse','HEAD'))

if __name__=='__main__':
    subprocess.run(['cargo','build','--locked','--manifest-path',str(MANIFEST)],check=True)
    unittest.main()
