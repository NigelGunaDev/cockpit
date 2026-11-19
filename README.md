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
Task creation, protocol approvals, task queues, task-review panels and the
exported-conversation browser are outside this standalone app.

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

## Presentation API

`cc-cockpit presentation inspect --run /absolute/run.json` prints one JSON object:
API version 1, `ok`, the live supervisor identity (`instance`), the original tmux
server (`socket`), its targets, private views, selection, reported `activity` and
tile capacity. Consumers should pin the instance identity and target key; never
infer a target from its display label.

Use `presentation open|focus|close --run PATH --key KEY` for an existing original
tmux target. Open is idempotent and creates a background attachment without
changing selection or input control. Focus requires an existing view; it never
starts or recovers an agent. Close removes only an unselected, hidden view and
preserves the original process. Visible or selected views are closed interactively.

A refusal prints `{"version":1,"ok":false,"error":{"code","message"}}` and exits
nonzero. Switch on `code`:

| Code | Meaning |
|---|---|
| `invalid_request` | Missing or unknown arguments |
| `run_unavailable` | No private, readable manifest at `--run` |
| `host_stale` | The Cockpit that wrote the manifest has exited |
| `target_stale` | The key no longer names a live terminal on the original server |
| `view_missing` | Focus asked for a target that has no view |
| `capacity` | All four tiles are occupied; no view is replaced |
| `leader_conflict` | The original session uses Cockpit's leader as its prefix |
| `view_visible` | Close asked for a visible or selected view |
| `timeout` | The supervisor did not answer |
| `failed` | Anything else; see `message` |

`open --status-file /absolute/status.json` also registers the agent status file of
that terminal; see [Usage](docs/USAGE.md#agent-activity). Responses report
`host_raised: false`: this selects within Cockpit and does not raise whichever
terminal application contains it. Launch with `--run-dir` for a predictable
private manifest location; a directory left by a Cockpit that exited is reclaimed
at the next launch, and one in use is refused. CC Sessions configures that path as
its presentation adapter's `run` option. Internal `__action` commands remain
implementation details.
