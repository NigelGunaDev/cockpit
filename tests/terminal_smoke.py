from pathlib import Path
import os, json, subprocess, tempfile, shutil, pty, fcntl, termios, struct, select, time, uuid, hashlib

import argparse
parser=argparse.ArgumentParser()
parser.add_argument('--binary',type=Path,required=True)
parser.add_argument('--report',type=Path,default=Path('/tmp/cockpit-terminal-smoke.json'))
options=parser.parse_args()
SOURCE=Path(__file__).resolve().parents[1]
BINARY=options.binary.resolve()
report = {'status': 'running', 'source_commit': subprocess.check_output(['git','-C',str(SOURCE),'rev-parse','HEAD'],text=True).strip(), 'binary_sha256': hashlib.sha256(BINARY.read_bytes()).hexdigest(), 'source_dirty': bool(subprocess.check_output(['git','-C',str(SOURCE),'status','--porcelain'])), 'checks': {}, 'limits': ['Disposable shell terminals, not real Claude/Codex/Pi', 'No physical Windows Terminal, mouse/clipboard/IME or reboot check']}
env = dict(os.environ)
env.update(TERM='xterm-256color', COLORTERM='truecolor', GIT_AUTHOR_NAME='Cockpit test', GIT_AUTHOR_EMAIL='test@example.invalid', GIT_COMMITTER_NAME='Cockpit test', GIT_COMMITTER_EMAIL='test@example.invalid')
for key in ['TMUX','TMUX_PANE','CLAUDECODE','CLAUDE_PROJECT_DIR','CLAUDE_CODE_SESSION_ID','PROJECT_DIR','GIT_DIR','GIT_WORK_TREE','GIT_INDEX_FILE','GIT_COMMON_DIR']:
    env.pop(key, None)
process = None
master = None
output = bytearray()

with tempfile.TemporaryDirectory(prefix='cp-live-') as temp:
    root = Path(temp); root.chmod(0o700)
    project=root/'project';project.mkdir()
    binary_copy=root/'cc-cockpit';shutil.copyfile(BINARY,binary_copy);binary_copy.chmod(0o700);BINARY=binary_copy
    (project/'README.md').write_text('# Disposable Cockpit verification\n')
    (project/'.gitignore').write_text('sessions/\n.claude/\n')
    for args in [['init','-q','-b','main'],['add','README.md','.gitignore'],['commit','-q','-m','Disposable fixture']]:
        subprocess.run(['git','-C',str(project),*args],env=env,check=True,capture_output=True)
    socket = root/'original.sock'; preferences = root/'preferences.json'; registry = root/'registry.sqlite3'
    def tmux(*args, ui=None, check=True):
        result = subprocess.run(['tmux','-S',str(ui or socket),'-f','/dev/null',*args],env=env,text=True,capture_output=True,timeout=8)
        if check: assert result.returncode == 0, result.stderr
        return result.stdout.strip()
    panes = {}
    for name in ['one','two']:
        pane,pid = tmux('new-session','-d','-P','-F','#{pane_id}|#{pane_pid}','-s',name,'-c',str(project),'-x','160','-y','44','bash --noprofile --norc').split('|')
        panes[name] = (pane,int(pid))
    def hashes():
        return {str(p.relative_to(project)):hashlib.sha256(p.read_bytes()).hexdigest() for p in (project/'sessions').rglob('*') if p.is_file()}
    before = hashes()
    before_git = subprocess.check_output(['git','-C',str(project),'status','--porcelain'],env=env)
    args = [str(BINARY),'--project',str(project),'--socket',str(socket),'--preferences',str(preferences),'--registry',str(registry)]
    def drain():
        if master is None:return
        while select.select([master],[],[],0)[0]:
            try:
                chunk = os.read(master,65536)
                if not chunk:break
                output.extend(chunk)
            except OSError:break
    def wait(fn, seconds=20):
        end=time.monotonic()+seconds
        while time.monotonic()<end:
            drain()
            if process is not None and process.poll() is not None:
                raise AssertionError('Cockpit exited: '+bytes(output[-3500:]).decode(errors='replace'))
            try:
                value=fn()
                if value:return value
            except (FileNotFoundError,json.JSONDecodeError):pass
            time.sleep(.04)
        raise AssertionError('Timed out waiting for smoke condition')
    def manifest():return json.loads(runfile.read_text())
    def start(name, first=False):
        global process,master,output,runfile
        output=bytearray(); directory=root/name;runfile=directory/'run.json'
        master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',44,160,0,0));os.set_blocking(master,False)
        process=subprocess.Popen(args+['--run-dir',str(directory)]+(['--session','one'] if first else []),cwd=SOURCE,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True)
        os.close(slave)
        m=wait(lambda: (lambda r:r if r.get('ui_server_pid') and r.get('tiles') and r.get('selected') else None)(manifest()))
        wait(lambda:tmux('list-clients','-F','#{client_pid}',ui=m['ui_socket'],check=False))
        wait(lambda:tmux('list-clients','-F','#{client_pid}',check=False))
        time.sleep(.3)
        return manifest()
    def action(op,key=None):
        command=[str(BINARY),'__action','--run',str(runfile),'--op',op]
        if key:command+=['--select',key]
        result=subprocess.run(command,env=env,capture_output=True,text=True,timeout=15)
        assert result.returncode==0,result.stderr
    def leader(key):
        m=manifest();os.write(master,b'\x1d')
        wait(lambda:'1' in tmux('list-clients','-F','#{client_prefix}',ui=m['ui_socket']).splitlines())
        os.write(master,key)
    def enter_marker(name,label,paste=False):
        marker=label+uuid.uuid4().hex[:10];command="printf '%s%s\\n' '"+label+"' '"+marker[len(label):]+"'"
        data=command.encode()
        if paste:data=b'\x1b[200~'+data+b'\x1b[201~'
        os.write(master,data+b'\r')
        wait(lambda:marker in tmux('capture-pane','-p','-S','-100','-t',panes[name][0]))
        other='two' if name=='one' else 'one'
        assert marker not in tmux('capture-pane','-p','-S','-100','-t',panes[other][0])
    def stop():
        global process,master
        leader(b'd');process.wait(timeout=12);assert process.returncode==0
        os.close(master);master=None;process=None
    try:
        listing=subprocess.run(args+['--list'],cwd=SOURCE,env=env,text=True,capture_output=True,timeout=20)
        assert listing.returncode==0,listing.stderr
        discovered=json.loads(listing.stdout)
        assert {t['session_name'] for t in discovered['targets']}=={'one','two'}
        assert discovered['errors']==[],discovered['errors']
        assert not (project/'sessions').exists() and not (project/'.claude').exists()
        assert not registry.exists() and not preferences.exists()
        report['checks']['native_discovery_without_project_installation']=True
        m=start('first',first=True)
        wait(lambda:manifest()['control'])
        enter_marker('one','FIRST-')
        report['checks']['focused_native_input_reaches_only_selected_terminal']=True
        offset=len(output);leader(b's')
        wait(lambda:b'Sessions' in bytes(output[offset:]) and b'search' in bytes(output[offset:]))
        os.write(master,b'\x1b');time.sleep(.3)
        enter_marker('one','AFTER-PICKER-')
        report['checks']['native_session_picker_opens_and_returns_to_terminal']=True
        two=next(t['key'] for t in manifest()['targets'] if t['session_name']=='two')
        def presentation(op, key=None):
            command=[str(BINARY),'presentation',op,'--run',str(runfile)]
            if key: command += ['--key',key]
            reply=subprocess.run(command,env=env,capture_output=True,text=True,timeout=20)
            assert reply.returncode==0,reply.stderr
            return json.loads(reply.stdout)
        before_selection=manifest()['selected']
        original_control=manifest()['control']
        opened=presentation('open',two)
        assert opened['version']==1 and opened['selected']==before_selection
        assert manifest()['control']==original_control
        assert len(presentation('open',two)['views'])==2
        enter_marker('one','BACKGROUND-OPEN-')
        presentation('close',two)
        assert len(manifest()['tiles'])==1 and manifest()['selected']==before_selection
        assert int(tmux('display-message','-p','-t',panes['two'][0],'#{pane_pid}'))==panes['two'][1]
        presentation('open',two)
        focused=presentation('focus',two)
        assert focused['selected']==two and focused['host_raised'] is False
        report['checks']['presentation_background_idempotence_focus_and_view_only_close']=True
        wait(lambda:manifest()['selected']==two and len(manifest()['tiles'])==2)
        if not manifest()['control']:action('toggle-control')
        wait(lambda:manifest()['control'])
        enter_marker('two','UNICODE-λ世界-',paste=True)
        action('split');wait(lambda:manifest()['view']=='split' and sum(t['visible'] for t in manifest()['tiles'])==2)
        leader(b'\t');wait(lambda:manifest()['selected']!=two)
        enter_marker('one','FOCUS-SWITCH-')
        report['checks']['two_live_terminals_split_focus_unicode_bracketed_paste']=True
        first_pids={name:int(tmux('display-message','-p','-t',pane,'#{pane_pid}')) for name,(pane,_) in panes.items()}
        stop()
        assert all(int(tmux('display-message','-p','-t',pane,'#{pane_pid}'))==first_pids[name] for name,(pane,_) in panes.items())
        report['checks']['clean_detach_keeps_original_processes']=True
        m=start('second')
        assert len(m['tiles'])==2 and m['view']=='split'
        assert {t['pane_pid'] for t in m['targets']}==set(first_pids.values())
        selected=next(t for t in m['targets'] if t['key']==m['selected'])['session_name']
        if not m['control']:action('toggle-control')
        wait(lambda:manifest()['control']);enter_marker(selected,'REATTACHED-')
        stop()
        report['checks']['reopen_saved_layout_and_interactive_reattachment_without_duplicates']=True
        assert hashes()==before
        assert not (project/'sessions').exists() and not (project/'.claude').exists()
        assert subprocess.check_output(['git','-C',str(project),'status','--porcelain'],env=env)==before_git
        report['checks']['no_project_installation_or_workflow_state_created']=True
        report['status']='passed'
    except Exception as error:
        import traceback
        report.update(status='failed',error=str(error),traceback=traceback.format_exc(),last_screen=bytes(output[-3500:]).decode(errors='replace'))
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
        options.report.write_text(json.dumps(report,indent=2)+'\n')
        print(json.dumps(report,indent=2))
assert report['status']=='passed'
