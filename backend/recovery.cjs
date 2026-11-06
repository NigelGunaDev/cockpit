// Native agent identity and foreground resume arguments. No workflow state writes.
const fs = require('fs');
const os = require('os');
const path = require('path');
const { spawnSync } = require('child_process');

function nativeSession(agent, file, cwd, expectedId, stateRoot) {
    if (!['claude', 'codex', 'pi'].includes(agent)) throw new Error('Unsupported agent');
    if (![file, cwd, stateRoot].every(v => typeof v === 'string' && path.isAbsolute(v))) throw new Error('Native file, store and workspace must be absolute');
    const before = fs.lstatSync(file);
    if (!before.isFile() || before.uid !== process.getuid()) throw new Error('Native session must be an existing same-user regular file');
    const fd = fs.openSync(file, fs.constants.O_RDONLY | fs.constants.O_NOFOLLOW | fs.constants.O_NONBLOCK);
    let rows;
    try {
        const opened = fs.fstatSync(fd);
        if (!opened.isFile() || before.ino !== opened.ino || before.dev !== opened.dev) throw new Error('Native session changed');
        const bytes = Buffer.alloc(256 * 1024);
        const count = fs.readSync(fd, bytes, 0, bytes.length, 0);
        rows = bytes.subarray(0, count).toString('utf8').split('\n').slice(0, 1024).flatMap(line => { try { return [JSON.parse(line)]; } catch { return []; } });
    } finally { fs.closeSync(fd); }
    const row = agent === 'codex' ? rows.find(r => r.type === 'session_meta')?.payload : agent === 'pi' ? (rows[0]?.type === 'session' ? rows[0] : null) : rows.find(r => r.sessionId && r.cwd);
    const id = agent === 'claude' ? row?.sessionId : row?.id;
    if (typeof id !== 'string' || !id.trim() || id.startsWith('-') || Buffer.byteLength(id) > 128 || /[\x00-\x1f\x7f]/.test(id) || ['.', '..'].includes(id) || id !== expectedId) throw new Error('Native session ID does not match');
    if (!row?.cwd || !path.isAbsolute(row.cwd) || fs.realpathSync(row.cwd) !== fs.realpathSync(cwd)) throw new Error('Native conversation belongs to another workspace');
    file = fs.realpathSync(file); stateRoot = fs.realpathSync(stateRoot);
    if (agent === 'claude' && (path.basename(file) !== `${id}.jsonl` || path.dirname(path.dirname(path.dirname(file))) !== stateRoot || path.basename(path.dirname(path.dirname(file))) !== 'projects')) throw new Error('Expected native Claude projects file');
    if (agent === 'codex' && (!path.basename(file).endsWith(`${id}.jsonl`) || !file.startsWith(path.join(stateRoot, 'sessions') + path.sep))) throw new Error('Expected native Codex sessions file');
    if (agent === 'pi' && path.dirname(file) !== stateRoot) throw new Error('Pi native storage directory changed');
    return { kind: agent, native_session_id: id, native_file: file, native_cwd: fs.realpathSync(cwd), state_root: stateRoot };
}

const CALLER_VARIABLES = ['CLAUDE_PROJECT_DIR', 'CLAUDE_CODE_SESSION_ID', 'CLAUDE_CODE_TASK_LIST_ID', 'CLAUDECODE', 'TMUX', 'TMUX_PANE', 'CODEX_THREAD_ID', 'CODEX_SESSION_ID', 'CODEX_INTERNAL_ORIGINATOR_OVERRIDE', 'CI', 'GITHUB_ACTIONS', 'GITHUB_WORKFLOW', 'CONTINUOUS_INTEGRATION', 'GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE', 'GIT_COMMON_DIR', 'GIT_CONFIG_COUNT', 'GIT_CONFIG_GLOBAL'];
function launcher(identity) {
    const program = identity.kind;
    // Node toolchain shims may rewrite PATH; the CLI wrapper captures its entry PATH.
    const launchPath = process.env.COCKPIT_LAUNCH_PATH || process.env.PATH || '';
    const executable = launchPath.split(path.delimiter).map(dir => path.join(dir, program)).find(file => { try { fs.accessSync(file, fs.constants.X_OK); return fs.statSync(file).isFile(); } catch { return false; } });
    if (!executable) throw new Error(`${program} is not on PATH; restore preserves your configured agent environment`);
    const args = identity.kind === 'claude' ? ['--resume', identity.native_session_id] : identity.kind === 'codex' ? ['resume', identity.native_session_id, '--no-daemon'] : ['--session', identity.native_file];
    const vars = [`PATH=${launchPath}`, `HOME=${os.homedir()}`, `PROJECT_DIR=${identity.native_cwd}`];
    if (identity.kind === 'claude') vars.push(`CLAUDE_CONFIG_DIR=${identity.state_root}`);
    if (identity.kind === 'codex') vars.push(`CODEX_HOME=${identity.state_root}`);
    return ['env', ...CALLER_VARIABLES.flatMap(v => ['-u', v]), ...vars, path.resolve(executable), ...args];
}
function startTime(pid) {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    return stat.slice(stat.lastIndexOf(')') + 1).trim().split(/\s+/)[19];
}
// npm/mise installs run Claude's native binary as claude.exe: the agent itself, not a shim.
const withoutExe = name => name.replace(/\.exe$/, '');
function processEvidence(target, identity) {
    const queue = [target.pane_pid]; let visited = 0; const candidates = [];
    while (queue.length && ++visited <= 128) {
        const pid = queue.pop();
        try {
            const argv = fs.readFileSync(`/proc/${pid}/cmdline`).toString().split('\0').filter(Boolean);
            const base = withoutExe(path.basename(argv[0] || ''));
            const kind = ['claude','codex','pi'].includes(base) ? base : ['node','bun','nodejs'].includes(base) ? (argv[1] || '').includes('/@anthropic-ai/claude-code/') ? 'claude' : (argv[1] || '').includes('/@openai/codex/') ? 'codex' : (argv[1] || '').includes('/pi-coding-agent/') ? 'pi' : null : null;
            if (kind) {
                const exact = kind === identity.kind && (kind === 'claude' ? ['--resume','-r'].some(flag => argv[argv.indexOf(flag)+1] === identity.native_session_id && argv.includes(flag)) : kind === 'codex' ? argv.includes('resume') && argv[argv.indexOf('resume')+1] === identity.native_session_id && argv.includes('--no-daemon') : argv.includes('--session') && argv[argv.indexOf('--session')+1] === identity.native_file);
                let opened = false;
                for (const fd of fs.readdirSync(`/proc/${pid}/fd`).slice(0, 1024)) { try { if (fs.readlinkSync(`/proc/${pid}/fd/${fd}`) === identity.native_file) opened = true; } catch {} }
                const nodeWrapper = ['node','bun','nodejs'].includes(base) && kind === 'codex';
                // Toolchain shims can retain argv[0] (e.g. "codex") while their
                // executable differs. Follow their children, not a real agent's
                // tool processes. No toolchain-specific path is required.
                const executable = withoutExe(path.basename(fs.readlinkSync(`/proc/${pid}/exe`)));
                const children = nodeWrapper || (base === kind && executable !== kind)
                    ? fs.readFileSync(`/proc/${pid}/task/${pid}/children`, 'utf8').trim().split(/\s+/).filter(Boolean).map(Number)
                    : [];
                const wrapper = nodeWrapper || children.length > 0;
                candidates.push({ exact, opened, pid, wrapper });
                if (wrapper) queue.push(...children);
                continue;
            }
            queue.push(...fs.readFileSync(`/proc/${pid}/task/${pid}/children`, 'utf8').trim().split(/\s+/).filter(Boolean).map(Number));
        } catch {}
    }
    if (queue.length || candidates.some(c => !c.exact) || candidates.filter(c=>!c.wrapper).length > 1 || !candidates.length) return { exact: false, opened: false };
    return { exact: true, opened: candidates.some(c => c.opened) };
}

const crypto = require('crypto');
function resolveGitCommonDirectory(root) {
    return fs.realpathSync(execute('git', ['-C',root,'rev-parse','--path-format=absolute','--git-common-dir']));
}
function readWorkspaceRoots(root) {
    const project_root=fs.realpathSync(execute('git',['-C',root,'rev-parse','--show-toplevel']));
    const workspaces=execute('git',['-C',root,'worktree','list','--porcelain']).split('\n')
        .filter(line=>line.startsWith('worktree ')).slice(0,64)
        .flatMap(line=>{try{return [{root:fs.realpathSync(line.slice(9))}];}catch{return [];}});
    return {project_root,workspaces};
}
function execute(program, args, options = {}) {
    const env = { ...process.env, GIT_OPTIONAL_LOCKS: '0' };
    for (const key of CALLER_VARIABLES) delete env[key];
    const result = spawnSync(program, args, { env, encoding:'utf8', timeout:5000, maxBuffer:1024*1024, stdio:['ignore','pipe','pipe'], ...options });
    if (result.error || result.status !== 0) throw new Error(result.error?.message || (result.stderr || result.stdout || `exit ${result.status}`).trim());
    return result.stdout.trimEnd();
}
function parse(args) {
    const allowed = ['root','cwd','agent','session-id','state-root','session-file','tmux-name','socket','socket-name','common-dir','common-device','common-inode','branch','expected-key'];
    const values = {};
    for (let i=0; i<args.length; i+=2) {
        const key = args[i]?.slice(2);
        if (!args[i]?.startsWith('--') || !allowed.includes(key) || values[key] !== undefined || !args[i+1] || args[i+1].includes('\0')) throw new Error('Invalid recovery argument; check the saved conversation settings');
        values[key] = args[i+1];
    }
    return values;
}
function socketOf(values) {
    if (values.socket && values['socket-name']) throw new Error('Choose socket or socket-name');
    let file = values.socket;
    if (!file) {
        const name = values['socket-name'] || 'default';
        if (!/^[A-Za-z0-9_-]+$/.test(name)) throw new Error('Invalid socket name');
        file = path.join(process.env.TMUX_TMPDIR || '/tmp', `tmux-${process.getuid()}`, name);
    }
    if (!path.isAbsolute(file) || Buffer.byteLength(file) >= 104) throw new Error('Choose an absolute short Unix socket');
    const parent = path.dirname(file);
    if (!fs.existsSync(parent) && path.basename(parent) === `tmux-${process.getuid()}`) fs.mkdirSync(parent, {mode:0o700});
    const meta = fs.lstatSync(parent);
    if (!meta.isDirectory() || meta.uid !== process.getuid() || (meta.mode & 0o077)) throw new Error('Tmux socket parent must be private and same-user');
    if (fs.existsSync(file)) {
        const socket = fs.lstatSync(file);
        if (!socket.isSocket() || socket.uid !== process.getuid()) throw new Error('Expected same-user Unix socket');
    }
    return file;
}
function inventory(socket) {
    // A stale same-user socket after server loss is allowed; other failures are not an empty inventory.
    if (!fs.existsSync(socket)) return [];
    let text;
    try { text = execute('tmux', ['-S',socket,'list-panes','-a','-F','#{pid}\t#{session_id}\t#{session_created}\t#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_current_path}']); }
    catch (error) { if (/no server running|failed to connect|Connection refused/.test(error.message)) return []; throw error; }
    if (execute('tmux', ['-S',socket,'show-option','-gqv','@cockpit-control'])) throw new Error('Cannot recover into a transient cockpit UI server');
    return text.split('\n').filter(Boolean).map(line => {
        const [server,session_id,session_created,session_name,window_id,pane_id,pid,dead,cwd] = line.split('\t');
        if (!/^\$\d+$/.test(session_id) || !/^%\d+$/.test(pane_id) || !/^@\d+$/.test(window_id)) throw new Error('Invalid tmux inventory');
        const server_pid=Number(server), pane_pid=Number(pid), server_start=startTime(server_pid), pane_start=dead === '1' ? '' : startTime(pane_pid);
        return { key:`${server_pid}:${server_start}:${session_id}:${session_created}:${pane_id}:${pane_pid}:${pane_start}`, socket,server_pid,server_start,session_id,session_created,session_name,window_id,pane_id,pane_pid,pane_start,cwd,dead:dead==='1',project:null,workspace:null,task:null,claude_session_id:null,association:'unknown' };
    });
}
function context(values, projectRoot) {
    if (process.platform !== 'linux') throw new Error('Session recovery requires WSL/Linux');
    if (!values.root || !path.isAbsolute(values.root)) throw new Error('An existing absolute workspace root is required');
    const root = fs.realpathSync(values.root);
    const roots = readWorkspaceRoots(projectRoot);
    if (!roots.workspaces.some(w => w.root === root)) throw new Error('Workspace is not registered with this project');
    const common = resolveGitCommonDirectory(root);
    const meta = fs.statSync(common, {bigint:true});
    if (common !== values['common-dir'] || String(meta.dev) !== values['common-device'] || String(meta.ino) !== values['common-inode']) throw new Error('Repository identity changed; recovery refused');
    const branch = execute('git', ['-C',root,'symbolic-ref','--short','HEAD']);
    if (!values.branch || branch !== values.branch) throw new Error('Workspace branch changed; recovery refused');
    const cwd = fs.realpathSync(values.cwd || root);
    if (cwd !== root && !cwd.startsWith(root + path.sep)) throw new Error('Native cwd is outside workspace');
    const identity = nativeSession(values.agent,values['session-file'],cwd,values['session-id'],values['state-root']);
    if (!/^[A-Za-z0-9][A-Za-z0-9_-]{0,127}$/.test(values['tmux-name'] || '')) throw new Error('Unsupported tmux session name; select an existing session manually');
    const socket = socketOf(values);
    const receipt = crypto.createHash('sha256').update(JSON.stringify(identity)).digest('hex');
    return { root, identity, socket, receipt, name:values['tmux-name'], project:roots.project_root };
}
function receiptOf(socket,target) { return execute('tmux',['-S',socket,'show-option','-qv','-t',target.session_id,'@cc-cockpit-conversation']); }
function result(operation, outcome, ctx, target=null, error=null, evidence=null) {
    return { operation, ok:!['failed','conflict'].includes(outcome),outcome,workspace:ctx?.root,agent:ctx?.identity.kind,native_session_id:ctx?.identity.native_session_id,target,readiness:evidence?.opened ? 'native-file-open' : 'unconfirmed',warnings:[...(target && !evidence?.opened ? ['Terminal is available. Loaded conversation/trust is not confirmed; no prompt was submitted.'] : []), ...(outcome === 'uncertain' && error ? [error] : [])],error:outcome === 'uncertain' ? null : error };
}
function sameCwd(a,b) {try{return fs.realpathSync(a)===b;}catch{return false;}}
function recover(values, projectRoot) {
    const ctx = context(values,projectRoot);
    let current = inventory(ctx.socket);
    const matches = current.filter(t => !t.dead && sameCwd(t.cwd, ctx.identity.native_cwd) && processEvidence(t,ctx.identity).exact);
    const marked = current.filter(t => receiptOf(ctx.socket,t) === ctx.receipt);
    if (matches.length > 1 || marked.length > 1) return result('RestoreSession','conflict',ctx,null,'Multiple terminals claim this conversation');
    if (matches.length === 1) {
        const t = matches[0]; t.project=ctx.project; t.workspace=ctx.root;
        return result('RestoreSession','existing',ctx,t,null,processEvidence(t,ctx.identity));
    }
    if (marked.length) return result('RestoreSession','uncertain',ctx,marked[0],'A previous launch exists but its native process is unconfirmed; inspect it before retrying');
    if (current.some(t => t.session_name === ctx.name)) return result('RestoreSession','conflict',ctx,null,'Tmux name is occupied by a different or unverified conversation');
    const tmuxArgs = ['-S',ctx.socket,'new-session','-d','-s',ctx.name,'-c',ctx.identity.native_cwd,...launcher(ctx.identity),';','set-option','-t',ctx.name,'@cc-cockpit-conversation',ctx.receipt,';','set-option','-w','-t',`${ctx.name}:`,'remain-on-exit','on'];
    try { execute('tmux',tmuxArgs); }
    catch (error) {
        // Another CLI may have won new-session. Reconcile its durable tmux receipt; never blindly spawn again.
        current=inventory(ctx.socket);
        const winner=current.filter(t=>receiptOf(ctx.socket,t)===ctx.receipt);
        if (winner.length===1) return result('RestoreSession','uncertain',ctx,winner[0],`Launch result unconfirmed: ${error.message}`);
        return result('RestoreSession','conflict',ctx,null,error.message);
    }
    const deadline=Date.now()+2500;
    while (Date.now()<deadline) {
        current=inventory(ctx.socket);
        const target=current.find(t=>t.session_name===ctx.name && receiptOf(ctx.socket,t)===ctx.receipt);
        if (!target) return result('RestoreSession','uncertain',ctx,null,'Launched terminal disappeared; no automatic retry');
        if (target.dead) return result('RestoreSession','failed',ctx,target,'Agent exited during startup. Dead pane retained for inspection.');
        const evidence=processEvidence(target,ctx.identity);
        if (evidence.exact && evidence.opened) {
            target.project=ctx.project;target.workspace=ctx.root;
            return result('RestoreSession','started',ctx,target,null,evidence);
        }
        Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,100);
    }
    const target=current.find(t=>t.session_name===ctx.name)||null;
    if(target && processEvidence(target,ctx.identity).exact) {target.project=ctx.project;target.workspace=ctx.root;return result('RestoreSession','started',ctx,target,null,processEvidence(target,ctx.identity));}
    return result('RestoreSession','uncertain',ctx,target,'Native process identity not confirmed; terminal retained, no automatic retry');
}
function close(values, projectRoot) {
    const ctx=context(values,projectRoot);
    const current=inventory(ctx.socket);
    const target=current.find(t=>t.key===values['expected-key']);
    if (!target) return result('CloseSession',current.some(t=>t.session_name===ctx.name)?'conflict':'already-ended',ctx,null,current.some(t=>t.session_name===ctx.name)?'Terminal changed; nothing closed':null);
    if (target.session_name!==ctx.name || target.dead || current.filter(t=>t.session_id===target.session_id).length!==1 || !processEvidence(target,ctx.identity).exact || !processEvidence(target,ctx.identity).opened) return result('CloseSession','conflict',ctx,null,'Close requires confirmed native-file ownership in a single-pane foreground session; use Forget or native exit while loading is unconfirmed');
    if (execute('tmux',['-S',ctx.socket,'list-windows','-t',target.session_id,'-F','#{window_linked}'])!=='0') return result('CloseSession','conflict',ctx,null,'Shared windows cannot be closed from cockpit');
    // Evaluate identity inside the receiving server, then close its immutable session ID.
    const condition=`#{&&:#{==:#{pid},${target.server_pid}},#{&&:#{==:#{session_created},${target.session_created}},#{==:#{pane_pid},${target.pane_pid}}}}`;
    const reply=execute('tmux',['-S',ctx.socket,'if-shell','-F','-t',target.pane_id,condition,`kill-session -t ${target.session_id}`, 'display-message -p conflict']);
    if (reply==='conflict') return result('CloseSession','conflict',ctx,null,'Terminal changed; nothing closed');
    return result('CloseSession','closed',ctx);
}
function handleWorkspaceRecovery(args,jsonOutput=false,projectRoot=process.cwd(),closing=false) {
    let answer;
    try { const values=parse(args);answer=closing?close(values,projectRoot):recover(values,projectRoot); }
    catch(error) { answer={operation:closing?'CloseSession':'RestoreSession',ok:false,outcome:'failed',error:error.message,target:null,warnings:[]}; }
    if (!jsonOutput && !answer.ok) throw new Error(answer.error || 'Session operation refused');
    return jsonOutput?answer:`${answer.operation}: ${answer.outcome}${answer.error?` — ${answer.error}`:''}${answer.target?`\nTerminal: ${answer.target.session_name}`:''}`;
}

module.exports={nativeSession,launcher,startTime,processEvidence,CALLER_VARIABLES,handleWorkspaceRecovery,parse,socketOf,inventory,recover,close};
if (process.argv[1] === '--cockpit-operation') {
    try {
        const request=JSON.parse(process.argv[2]);
        if (!Array.isArray(request.args) || !['recover','close-session'].includes(request.operation)) throw new Error('Unsupported native operation');
        const answer=handleWorkspaceRecovery(request.args,true,request.project,request.operation==='close-session');
        process.stdout.write(JSON.stringify(answer));
        if (!answer.ok) process.exitCode=1;
    } catch(error) { process.stdout.write(JSON.stringify({ok:false,error:error.message}));process.exitCode=1; }
}
