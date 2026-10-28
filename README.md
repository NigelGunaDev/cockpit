# Cockpit

An independent, experimental WSL terminal workspace. Development is paused;
this repository preserves the app for future maintenance.

Rust/Ratatui supplies navigation and panels. Stock tmux renders existing terminals
through disposable attachments. Cockpit provides a session picker, sidebar,
focused/split/grid layouts, independent scrollback, local Git diffs and remembered
Claude/Codex/Pi conversations. Project discovery uses directories and Git worktrees.
There is no project plugin, workspace CLI, task framework or application daemon to install.

## Requirements

- A regular interactive WSL Linux terminal, outside another tmux client.
- tmux, Git, and Rust/Cargo with edition 2024 support for building.
- Node.js on PATH for remembered-agent recovery and exact Close. The helper is
  embedded in the executable and uses only Node built-ins; no npm installation.
- Agent executables on PATH when reopening their saved conversations.

## Build and launch

```bash
cargo test --locked -- --test-threads=1
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

Create an ordinary terminal session if you do not already have one:

```bash
tmux new-session -d -s work -c /path/to/project
./target/release/cc-cockpit --project /path/to/project
```

Type your chosen agent command in that terminal. Additional terminals can be
created with ordinary tmux commands. Cockpit does not create tasks or branches.
For a user-local binary, run `cargo install --locked --path .`.

`--project` is repeatable. Without it, discovery uses the current directory or
`COCKPIT_PROJECT_DIR`, plus matching remembered projects. Git worktrees are grouped
under their project. Plain directories work for terminal use; native conversation
recovery currently requires a saved Git repository identity and branch.

`--list` lists observations without restoring agents. `--socket PATH` or
`--socket-name NAME` chooses the original tmux server. A missing server is allowed
when enabled remembered conversations can recover it. Use `--help` for arguments.

## Controls

Press **Ctrl+]**, release it, then the next key. Change the leader with `--leader`.

| Keys | Action |
|---|---|
| Leader, `s` | Session picker |
| Leader, Space | Search commands |
| Leader, `b` | Sidebar |
| Leader, `1` / `2` / `4` | Focus / split / grid |
| Leader, Tab / Shift+Tab | Next / previous attached terminal |
| Leader, `v` | Read-only / input control |
| Leader, `[` | Independent terminal scrollback |
| Leader, `g` | Read-only local Git changes |
| Leader, `o` | Remembered sessions |
| Leader, `?` | Controls guide |
| Leader, `d` | Detach, preserving original sessions |

The picker uses Enter to focus/replace and `a` to add a terminal to the layout.
Task creation, protocol approvals, task queues, task-review panels, the old attention
inbox and exported-conversation browser are outside this standalone app.

[Usage](docs/USAGE.md) · [Architecture](docs/ARCHITECTURE.md) ·
[Recovery](docs/RECOVERY.md) · [Development and validation](docs/DEVELOPMENT.md)

## Boundaries

Original tmux sessions own the agent processes. Cockpit owns temporary UI attachments.
Detaching preserves original processes; explicit Close is a separate confirmed action.
Other tmux clients share pane geometry and can provide input. Original settings stay
unchanged. Concurrent modification of original tmux exit behavior is unsupported.

Recovery reopens an exact saved conversation without submitting a prompt or approving
trust/permissions. Restored terminals start read-only. Native exit does not mean Forget.
Physical Windows Terminal and real-agent fidelity, power-cycle recovery and performance
are separate manual checks; disposable shell/synthetic-agent tests do not establish them.

[Baseline import hashes](docs/PROVENANCE.json) describe the original snapshot;
current files intentionally differ. Raw transcripts, databases, runtime manifests,
screenshots and execution logs are excluded. See `LICENSE` for the project licence.
