"""Standalone recovery acceptance using one synthetic native agent and private tmux."""
from pathlib import Path
import argparse, os, subprocess, tempfile, shutil, json, sqlite3, pty, fcntl, termios, struct, select, time, hashlib

parser=argparse.ArgumentParser()
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--report',type=Path,default=Path('/tmp/cockpit-native-recovery-smoke.json'))
options=parser.parse_args()
binary=options.binary.resolve()
source=Path(__file__).resolve().parents[1]
report={'status':'running','checks':{},'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'limits':['Synthetic Codex-shaped process, no real agent or model request','Private server loss, not WSL/PC reboot']}
env=dict(os.environ)
env.update(TERM='xterm-256color',COLORTERM='truecolor',GIT_AUTHOR_NAME='Test',GIT_AUTHOR_EMAIL='test@example.invalid',GIT_COMMITTER_NAME='Test',GIT_COMMITTER_EMAIL='test@example.invalid')
for key in ['TMUX','TMUX_PANE','CLAUDECODE','CLAUDE_PROJECT_DIR','CLAUDE_CODE_SESSION_ID','COCKPIT_PROJECT_DIR','PROJECT_DIR','CODEX_THREAD_ID','CODEX_SESSION_ID','GIT_DIR','GIT_COMMON_DIR','GIT_WORK_TREE','GIT_INDEX_FILE']:
    env.pop(key,None)
process=None;master=None;output=bytearray();runfile=None
with tempfile.TemporaryDirectory(prefix='cp-restore-') as temp:
    root=Path(temp);root.chmod(0o700)
    project=root/'project';project.mkdir()
    store=root/'store';native=store/'sessions/2026/10/28/rollout-standalone-id.jsonl';native.parent.mkdir(parents=True)
    native.write_text(json.dumps({'type':'session_meta','payload':{'id':'standalone-id','cwd':str(project)}})+'\n')
    original_bytes=native.read_bytes()
    for args in [['init','-q','-b','main'],['commit','-q','--allow-empty','-m','Disposable fixture']]:
        subprocess.run(['git','-C',str(project),*args],check=True,env=env,capture_output=True)
    bin_dir=root/'bin';bin_dir.mkdir()
    fake=root/'native.py';fake.write_text('import time\nf=open('+repr(str(native))+',"r")\nprint("SAVED-CONVERSATION-READY",flush=True)\nwhile True: time.sleep(1)\n')
    launcher=bin_dir/'codex';launcher.write_text('#!/bin/bash\nexec -a codex /usr/bin/python3 '+repr(str(fake))+' "$@"\n');launcher.chmod(0o700)
    copied=bin_dir/'cc-cockpit';shutil.copyfile(binary,copied);copied.chmod(0o700);binary=copied
    env['PATH']=str(bin_dir)+os.pathsep+env['PATH'];env['CODEX_HOME']=str(store)
    socket=root/'original.sock';registry=root/'saved.sqlite3';preferences=root/'preferences.json'
    arguments=[str(binary),'--project',str(project),'--socket',str(socket),'--registry',str(registry),'--preferences',str(preferences)]
    def tmux(*args,ui=None,check=True):
        result=subprocess.run(['tmux','-S',str(ui or socket),'-f','/dev/null',*args],env=env,text=True,capture_output=True,timeout=8)
        if check:assert result.returncode==0,result.stderr
        return result.stdout.strip()
    def drain():
        if master is None:return
        while select.select([master],[],[],0)[0]:
            try:
                data=os.read(master,65536)
                if not data:break
                output.extend(data)
            except OSError:break
    def manifest():return json.loads(runfile.read_text())
    def wait(fn,seconds=30):
        end=time.monotonic()+seconds
        while time.monotonic()<end:
            drain()
            if process is not None and process.poll() is not None:raise AssertionError('Unexpected exit: '+bytes(output[-3000:]).decode(errors='replace'))
            try:
                value=fn()
                if value:return value
            except (FileNotFoundError,json.JSONDecodeError,sqlite3.OperationalError):pass
            time.sleep(.04)
        raise AssertionError('Timed out; current recovery='+json.dumps(manifest().get('recovery',{})))
    def row():
        db=sqlite3.connect('file:'+str(registry)+'?mode=ro',uri=True)
        try:
            rows=list(db.execute('select id,revision,enabled,data from sessions'))
            assert len(rows)==1,rows
            return {'id':rows[0][0],'revision':rows[0][1],'enabled':bool(rows[0][2]),'data':json.loads(rows[0][3])}
        finally:db.close()
    def start(name,initial=False):
        global process,master,output,runfile
        output=bytearray();directory=root/name;runfile=directory/'run.json'
        master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',40,150,0,0));os.set_blocking(master,False)
        process=subprocess.Popen(arguments+['--run-dir',str(directory)]+(['--session','native'] if initial else []),cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
        os.close(slave)
        m=wait(lambda:(lambda r:r if r.get('ui_server_pid') and r.get('tiles') else None)(manifest()))
        wait(lambda:tmux('list-clients','-F','#{client_pid}',ui=m['ui_socket'],check=False))
        time.sleep(.3);return manifest()
    def leader(key):
        m=manifest();os.write(master,b'\x1d')
        wait(lambda:'1' in tmux('list-clients','-F','#{client_prefix}',ui=m['ui_socket']).splitlines())
        os.write(master,key)
    def stop():
        global process,master
        leader(b'd');process.wait(timeout=12);assert process.returncode==0
        os.close(master);master=None;process=None
    def gone(pid):
        try:return Path(f'/proc/{pid}/stat').read_text().rsplit(')',1)[1].split()[0]=='Z'
        except FileNotFoundError:return True
    try:
        first=tmux('new-session','-d','-P','-F','#{pane_pid}','-s','native','-c',str(project),str(launcher),'resume','standalone-id','--no-daemon')
        m=start('first',True)
        wait(lambda:manifest()['recovery']['entries'])
        saved=row();assert saved['enabled']
        assert saved['data']['identity']['native_session_id']=='standalone-id'
        assert not (project/'sessions').exists() and not (project/'.claude').exists()
        report['checks']['exact_native_registration_without_project_installation']=True
        server=m['targets'][0]['server_pid'];stop()
        tmux('kill-server');wait(lambda:gone(server))
        m=start('restored')
        m=wait(lambda:(lambda r:r if any(j['loaded'] for j in r['recovery']['jobs']) else None)(manifest()))
        assert len(m['targets'])==1 and m['targets'][0]['pane_pid']!=int(first)
        assert not m['control']
        target=m['targets'][0]
        wait(lambda:'SAVED-CONVERSATION-READY' in tmux('capture-pane','-p','-S','-100','-t',target['pane_id']))
        assert row()['enabled'] and row()['data']['target']['key']==target['key']
        report['checks']['bundled_recovery_after_server_loss_exact_loaded_read_only']=True
        stop();m=start('repeat')
        assert m['targets'][0]['pane_pid']==target['pane_pid']
        assert m['recovery']['jobs']==[] and len(m['targets'])==1
        report['checks']['repeat_reopen_reuses_process_without_duplicate']=True
        saved=row();request=json.dumps({'id':saved['id'],'revision':saved['revision'],'key':target['key']})
        result=subprocess.run([str(binary),'__action','--run',str(runfile),'--op','registry-close','--select',request],env=env,text=True,capture_output=True,timeout=15)
        assert result.returncode==0,result.stderr
        wait(lambda:any(j['status']=='closed' for j in manifest()['recovery']['jobs']))
        assert not row()['enabled'] and gone(target['pane_pid'])
        report['checks']['close_disables_saved_intent_and_stops_only_verified_session']=True
        stop()
        tmux('new-session','-d','-s','unrelated','-c',str(project),'sleep','60')
        m=start('excluded')
        assert {t['session_name'] for t in m['targets']}=={'unrelated'}
        assert m['recovery']['jobs']==[] and not row()['enabled']
        stop()
        report['checks']['forgotten_native_conversation_stays_closed']=True
        assert native.read_bytes()==original_bytes
        assert not (project/'sessions').exists() and not (project/'.claude').exists()
        assert subprocess.check_output(['git','-C',str(project),'status','--porcelain'],env=env)==b''
        report['checks']['history_git_and_absent_workflow_state_preserved']=True
        report['status']='passed'
    except Exception as error:
        import traceback
        report.update(status='failed',error=str(error),traceback=traceback.format_exc(),last_screen=bytes(output[-3000:]).decode(errors='replace'))
    finally:
        if master is not None:
            try:os.close(master)
            except OSError:pass
        if process is not None:
            try:process.wait(timeout=8)
            except subprocess.TimeoutExpired:process.terminate();process.wait(timeout=8)
        for path in root.glob('*/run.json'):
            try:tmux('kill-server',ui=json.loads(path.read_text())['ui_socket'],check=False)
            except Exception:pass
        tmux('kill-server',check=False)
        options.report.write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report,indent=2))
assert report['status']=='passed'
