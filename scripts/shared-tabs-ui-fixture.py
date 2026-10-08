#!/usr/bin/python3
"""Prepare an isolated shared-tabs bundle in /tmp for direct launch and Cua inspection."""
import json, os, plistlib, shutil, subprocess, tempfile, time, uuid
from pathlib import Path

fixture = Path(tempfile.mkdtemp(prefix='riwork-shared-tabs-', dir='/tmp'))
home, runtime, root = fixture/'home', fixture/'runtime', fixture/'project'
for directory in [home, runtime, root]: directory.mkdir(mode=0o700)
bundle = fixture/'Shared Tabs Check.app'
shutil.copytree(Path('target/debug/RiWork.app'), bundle)
macos = bundle/'Contents/MacOS'
# Keep a real Mach-O executable for direct bundle launch. Launch with env -i
# HOME="$HOME" PATH="$PATH" RIWORK_HOME=<home> RIWORK_RUNTIME_DIR=<runtime>
# open -n <bundle> --args <root>, then inspect only that process through Cua.
info_path = bundle/'Contents/Info.plist'
info = plistlib.loads(info_path.read_bytes())
identifier = 'dev.riwork.shared-tabs.check.'+uuid.uuid4().hex[:8]
info.update(CFBundleIdentifier=identifier, CFBundleName='Shared Tabs Check', CFBundleExecutable='riwork')
info_path.write_bytes(plistlib.dumps(info))
clean = {key:value for key,value in os.environ.items() if not key.startswith(('RIWORK_', 'TMUX'))}
clean.update(RIWORK_HOME=str(home), RIWORK_RUNTIME_DIR=str(runtime))
binary = str(macos/'riwork')
def cli(*args, env=clean):
    return json.loads(subprocess.check_output([binary,*args,'--json'],env=env))
project = cli('project','add',str(root)); project_id = project['id']
user, orchestrator = str(uuid.uuid4()), str(uuid.uuid4())
for chat_id,title,scope in [(user,'User chat',None),(orchestrator,'Project orchestrator',{'scope':'project','project_id':project_id})]:
    chat = {'id':chat_id,'provider':'codex','project_id':project_id,'cwd':str(root),'title':title,'user_title':title,'created_at_unix':int(time.time()),'state':{'state':'stopped'},'approval_mode':'supervised'}
    if scope: chat['orchestrator']=scope
    directory=home/'chats'/chat_id; directory.mkdir(parents=True,mode=0o700)
    (directory/'info.json').write_text(json.dumps(chat))
    (directory/'events.jsonl').write_text(json.dumps({'chat_id':chat_id,'seq':1,'event':{'event':'info','info':chat}})+'\n')
worker=cli('shell','create','--project',project_id,env={**clean,'RIWORK_CHAT_ID':user})
cli('tabs','list','--project',project_id)
layout={'layout':{'pane':1},'panes':{'1':{'tabs':[{'kind':'panel','panel':'shells'}],'active_tab_key':'panel:shells'}},'active_pane':1,'locked_panes':[],'panels_initialized':True,'sidebar_visible':False,'window_size':{'width':1100,'height':800}}
(home/'layouts.json').write_text(json.dumps({'schema_version':1,'projects':{project_id:layout}}))
(home/'settings.json').write_text(json.dumps({'schema_version':1,'theme':'ri_work','tab_close_behavior':'ask','orchestrator_mode':'chat'}))
subprocess.run(['codesign','--force','--sign','-',str(bundle)],check=True)
metadata={'fixture':str(fixture),'home':str(home),'runtime':str(runtime),'project':project_id,'root':str(root),'bundle_id':identifier,'bundle':str(bundle),'user_chat':user,'orchestrator':orchestrator,'worker':worker['id']}
(fixture/'fixture.json').write_text(json.dumps(metadata,indent=2))
Path('/tmp/shared-tabs-fixture.json').write_text(json.dumps(metadata,indent=2))
print(json.dumps(metadata,indent=2))
