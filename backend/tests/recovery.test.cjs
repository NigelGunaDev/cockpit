const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('fs');
const os=require('os');
const path=require('path');
const {spawnSync}=require('child_process');
const {nativeSession,launcher,CALLER_VARIABLES}=require('../recovery.cjs');
const {recover,close,inventory,handleWorkspaceRecovery}=require('../recovery.cjs');
function run(program,args,options={}) {const r=spawnSync(program,args,{encoding:'utf8',...options});assert.equal(r.status,0,r.stderr||r.error?.message);return r.stdout.trim();}
function fixture(t) {
    const root=fs.mkdtempSync(path.join(os.tmpdir(),'workspace recovery-'));
    const project=path.join(root,'project');fs.mkdirSync(project);
    run('git',['-C',project,'init','-q','-b','main']);run('git',['-C',project,'-c','user.name=Test','-c','user.email=test@example.invalid','commit','--allow-empty','-qm','seed']);
    const store=path.join(root,'codex'),file=path.join(store,'sessions','2026','10','19','rollout-test-id.jsonl');fs.mkdirSync(path.dirname(file),{recursive:true});
    fs.writeFileSync(file,JSON.stringify({type:'session_meta',payload:{id:'test-id',cwd:project}})+'\n');
    const socket=path.join(root,'original.sock');fs.chmodSync(root,0o700);
    const bin=path.join(root,'bin');fs.mkdirSync(bin);const python=path.join(root,'agent.py');
    fs.writeFileSync(python,`import os,sys,time,json\nf=open(${JSON.stringify(file)},'r')\nprint('RESTORED:'+str(sys.argv),flush=True)\nwhile True: time.sleep(1)\n`);
    fs.writeFileSync(path.join(bin,'codex'),`#!/bin/bash\nexec -a codex /usr/bin/python3 '${python}' "$@"\n`,{mode:0o755});
    const previousHome=process.env.HOME;process.env.HOME=path.join(root,'home');fs.mkdirSync(process.env.HOME);
    const previousPath=process.env.PATH;process.env.PATH=bin+path.delimiter+previousPath;
    t.after(()=>{spawnSync('tmux',['-S',socket,'kill-server']);process.env.PATH=previousPath;process.env.HOME=previousHome;fs.rmSync(root,{recursive:true,force:true});});
    const common=run('git',['-C',project,'rev-parse','--absolute-git-dir']);const meta=fs.statSync(common);
    const values={root:project,cwd:project,agent:'codex','session-id':'test-id','state-root':store,'session-file':file,'tmux-name':'task-a',socket,'common-dir':common,'common-device':String(meta.dev),'common-inode':String(meta.ino),branch:'main'};
    return {root,project,store,file,socket,values};
}
const linux={skip:process.platform!=='linux'};
test('native adapters validate exact ID/cwd/type/existence without initializing missing sessions',linux,t=>{
    const f=fixture(t);const id=nativeSession('codex',f.file,f.project,'test-id',f.store);
    assert.deepEqual(launcher(id).slice(-3),['resume','test-id','--no-daemon']);
    assert.throws(()=>nativeSession('codex',f.file,f.project,'other',f.store),/ID/);
    assert.throws(()=>nativeSession('codex',f.file,f.root,'test-id',f.store),/workspace/);
    assert.throws(()=>nativeSession('pi',path.join(f.root,'missing.jsonl'),f.project,'id',f.root));
    assert.equal(fs.existsSync(path.join(f.root,'missing.jsonl')),false);
    const symlink=path.join(f.root,'link.jsonl');fs.symlinkSync(f.file,symlink);assert.throws(()=>nativeSession('codex',symlink,f.project,'test-id',f.store),/regular/);
    assert.ok(CALLER_VARIABLES.includes('CODEX_THREAD_ID'));assert.ok(CALLER_VARIABLES.includes('CLAUDECODE'));
});
test('CLI refuses changed repository/branch or unsupported agent without Git/workflow writes',linux,t=>{
    const f=fixture(t);const before=run('git',['-C',f.project,'status','--porcelain']);
    assert.throws(()=>recover({...f.values,'common-inode':'0'},f.project),/identity/);
    assert.throws(()=>recover({...f.values,branch:'feature/other'},f.project),/branch/);
    assert.throws(()=>recover({...f.values,agent:'other'},f.project),/agent/);
    assert.equal(before,run('git',['-C',f.project,'status','--porcelain']));assert.equal(fs.existsSync(f.socket),false);
});
test('real private tmux: exact native launch, idempotent reconnect and precise Close',linux,t=>{
    const f=fixture(t);const first=recover(f.values,f.project);assert.equal(first.outcome,'started',JSON.stringify(first));assert.equal(first.readiness,'native-file-open');
    const pid=first.target.pane_pid;const again=recover(f.values,f.project);assert.equal(again.outcome,'existing');assert.equal(again.target.pane_pid,pid);assert.equal(inventory(f.socket).length,1);
    const closed=close({...f.values,'expected-key':first.target.key},f.project);assert.equal(closed.outcome,'closed',JSON.stringify(closed));assert.equal(close({...f.values,'expected-key':first.target.key},f.project).outcome,'already-ended');
    assert.equal(fs.existsSync(f.file),true);assert.equal(run('git',['-C',f.project,'status','--porcelain']),'');
});
function addProcessShim(f, { children = 1, childId } = {}) {
    const shim=path.join(f.root,'shim.py');
    const args=childId ? `[${JSON.stringify('resume')},${JSON.stringify(childId)},${JSON.stringify('--no-daemon')}]` : 'sys.argv[1:]';
    fs.writeFileSync(shim,`import subprocess,sys\nchildren=[subprocess.Popen(['codex',${JSON.stringify(path.join(f.root,'agent.py'))}]+${args},executable='/usr/bin/python3') for _ in range(${children})]\nfor child in children: child.wait()\n`);
    fs.writeFileSync(path.join(f.root,'bin','codex'),`#!/bin/bash\nexec -a codex /usr/bin/python3 '${shim}' "$@"\n`,{mode:0o755});
}
test('toolchain shim retaining agent argv exposes its native child file ownership and exact Close',linux,t=>{
    const f=fixture(t);addProcessShim(f);
    const first=recover(f.values,f.project);assert.equal(first.outcome,'started',JSON.stringify(first));assert.equal(first.readiness,'native-file-open');
    const again=recover(f.values,f.project);assert.equal(again.outcome,'existing');assert.equal(again.target.pane_pid,first.target.pane_pid);
    assert.equal(close({...f.values,'expected-key':first.target.key},f.project).outcome,'closed');
    assert.equal(fs.existsSync(f.file),true);
});
test('matching shim arguments cannot hide a different native child conversation',linux,t=>{
    const f=fixture(t);addProcessShim(f,{childId:'other-id'});
    const first=recover(f.values,f.project);assert.equal(first.outcome,'uncertain',JSON.stringify(first));assert.equal(first.readiness,'unconfirmed');
    assert.equal(close({...f.values,'expected-key':first.target.key},f.project).outcome,'conflict');assert.equal(inventory(f.socket).length,1);
});
test('multiple native children beneath a shim retain ambiguous ownership and refuse Close',linux,t=>{
    const f=fixture(t);addProcessShim(f,{children:2});
    const first=recover(f.values,f.project);assert.equal(first.outcome,'uncertain',JSON.stringify(first));assert.equal(first.readiness,'unconfirmed');
    assert.equal(close({...f.values,'expected-key':first.target.key},f.project).outcome,'conflict');assert.equal(inventory(f.socket).length,1);
});
// A native claude.exe whose tool child is Claude-named: following that child would make ownership ambiguous.
function claudeExeTree(t,{shim}) {
    const root=fs.mkdtempSync(path.join(os.tmpdir(),'claude exe-'));const binary=path.join(root,'claude.exe');
    fs.copyFileSync('/bin/bash',binary);fs.chmodSync(binary,0o755);
    const native=[binary,'-c','(exec -a claude sleep 60) & wait','claude.exe'];
    const script=path.join(root,'claude');
    fs.writeFileSync(script,`#!/bin/bash\n${native.map(a=>`'${a}'`).join(' ')} "$@"\n`,{mode:0o755});
    const argv=shim?['/bin/bash','-c',`exec -a claude /bin/bash '${script}' "$@"`,'shim','--resume','claude-id']:[...native,'--resume','claude-id'];
    const child=require('child_process').spawn(argv[0],argv.slice(1),{detached:true,stdio:'ignore'});
    t.after(()=>{try{process.kill(-child.pid,'SIGKILL');}catch{}fs.rmSync(root,{recursive:true,force:true});});
    const tree=pid=>{let out=[];try{for(const c of fs.readFileSync(`/proc/${pid}/task/${pid}/children`,'utf8').trim().split(/\s+/).filter(Boolean))out.push(Number(c),...tree(c));}catch{}return out;};
    const comm=pid=>{try{return fs.readFileSync(`/proc/${pid}/comm`,'utf8').trim();}catch{return '';}};
    for(let n=0;n<200&&!tree(child.pid).some(p=>comm(p)==='sleep');n++)Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,25);
    assert.ok(tree(child.pid).some(p=>comm(p)==='sleep'),'process tree did not start');
    return {pane_pid:child.pid};
}
for(const shim of [false,true]) test(`native claude.exe ${shim?'beneath a claude shell shim ':''}is the agent, not a shim whose tool children are followed`,linux,t=>{
    const {processEvidence}=require('../recovery.cjs');
    const target=claudeExeTree(t,{shim});const identity={kind:'claude',native_session_id:'claude-id',native_file:'/nonexistent/claude-id.jsonl'};
    assert.deepEqual(processEvidence(target,identity),{exact:true,opened:false});
    assert.deepEqual(processEvidence(target,{...identity,native_session_id:'other-id'}),{exact:false,opened:false});
});
test('replacement target and occupied name cannot be closed or displaced',linux,t=>{
    const f=fixture(t);const first=recover(f.values,f.project);assert.equal(first.outcome,'started',JSON.stringify(first));
    run('tmux',['-S',f.socket,'kill-session','-t',first.target.session_id]);
    run('tmux',['-S',f.socket,'new-session','-d','-s','task-a','-c',f.project,'sleep','60']);
    assert.equal(close({...f.values,'expected-key':first.target.key},f.project).outcome,'conflict');
    assert.equal(recover(f.values,f.project).outcome,'conflict');assert.equal(inventory(f.socket).length,1);
});
test('structured recovery errors do not initialize workflow state',linux,t=>{
    const f=fixture(t);const answer=handleWorkspaceRecovery(['--root',f.project],true,f.project);
    assert.equal(answer.operation,'RestoreSession');assert.equal(answer.ok,false);assert.equal(fs.existsSync(path.join(f.project,'sessions','sessions-state.json')),false);
});

test('uncertain marked launch is retained without respawn and shared panes refuse Close',linux,t=>{
    const f=fixture(t);const first=recover(f.values,f.project);assert.equal(first.outcome,'started');
    run('tmux',['-S',f.socket,'split-window','-d','-h','-t',first.target.pane_id,'sleep','60']);
    assert.equal(close({...f.values,'expected-key':first.target.key},f.project).outcome,'conflict');
    for(const pane of inventory(f.socket).filter(p=>p.pane_id!==first.target.pane_id))run('tmux',['-S',f.socket,'kill-pane','-t',pane.pane_id]);
    run('tmux',['-S',f.socket,'respawn-pane','-k','-t',first.target.pane_id,'sleep','60']);
    const retry=recover(f.values,f.project);assert.equal(retry.outcome,'uncertain');assert.equal(retry.readiness,'unconfirmed');assert.equal(inventory(f.socket).length,1);
    assert.equal(inventory(f.socket)[0].pane_pid,retry.target.pane_pid);
});
test('concurrent standalone CLI recovery creates one original session',linux,async t=>{
    const f=fixture(t);const {spawn}=require('child_process');
    const modulePath=path.resolve(__dirname,'../recovery.cjs');
    const code=`process.stdout.write(JSON.stringify(require(${JSON.stringify(modulePath)}).recover(JSON.parse(process.argv[1]),process.argv[2])))`;
    const invoke=()=>new Promise((resolve,reject)=>{const child=spawn(process.execPath,['-e',code,JSON.stringify(f.values),f.project],{env:process.env});let out='',err='';child.stdout.on('data',b=>out+=b);child.stderr.on('data',b=>err+=b);child.on('error',reject);child.on('exit',status=>status===0?resolve(JSON.parse(out)):reject(new Error(err)));});
    const answers=await Promise.all([invoke(),invoke()]);assert.equal(inventory(f.socket).length,1);
    assert.ok(answers.some(a=>['started','existing','uncertain'].includes(a.outcome)),JSON.stringify(answers));
    const again=recover(f.values,f.project);assert.equal(again.outcome,'existing');
});
test('agent launch failure keeps a diagnostic pane and never retries implicitly',linux,t=>{
    const f=fixture(t);fs.writeFileSync(path.join(f.root,'bin','codex'),'#!/bin/sh\necho intentional-startup-failure\nexit 7\n',{mode:0o755});
    const result=recover(f.values,f.project);assert.equal(result.outcome,'failed',JSON.stringify(result));
    const panes=inventory(f.socket);assert.equal(panes.length,1);assert.equal(panes[0].dead,true);
    const again=recover(f.values,f.project);assert.equal(again.outcome,'uncertain');assert.equal(inventory(f.socket)[0].pane_id,panes[0].pane_id);
});
