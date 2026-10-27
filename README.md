# Cockpit

An experimental WSL terminal workspace for parallel agent sessions. Development
is paused; this repository preserves the implementation for possible future work.

Rust/Ratatui provides navigation and supporting panels. Stock tmux renders the
interactive terminals through disposable attachments to existing tmux sessions.
It supports focused, split and four-terminal layouts, saved selections, terminal
history, attention, task review and remembered Claude/Codex/Pi conversations.

**This is a separate source repository, not a standalone orchestration system.**
It requires compatible cc-sessions installations in the projects it observes.
The executable remains `cc-cockpit`; no binary or runtime-state rename was made.

## Build and check

Use WSL Linux with Rust/Cargo, a C build toolchain for bundled SQLite, tmux and
Node.js. Cargo edition 2024 is required; the extraction is tested using the local
Rust toolchain recorded in [development notes](docs/DEVELOPMENT.md).

```bash
cargo test --locked -- --test-threads=1
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
```

The binary is `target/release/cc-cockpit`. For a user-local installation:

```bash
cargo install --locked --path .
```

## Launch

From a regular interactive WSL terminal, outside tmux, use a project with a
compatible installed cc-sessions CLI and existing original tmux sessions:

```bash
cc-cockpit --project /path/to/project --list
cc-cockpit --project /path/to/project
```

The first command lists observations without attaching or restoring sessions.
The interactive command can restore enabled remembered conversations. It sends
no continuation prompt; recovered attachments start read-only. Specify
`--socket /absolute/path/to/socket` or `--socket-name NAME` when needed. `--help`
explains the launch arguments. Project discovery and the CLI requirements are
covered in [integration notes](docs/INTEGRATION.md).

The default cockpit leader is **Ctrl+]**, released before the next key:

| Keys | Action |
|---|---|
| Leader, `s` | Session picker |
| Leader, Space | Command search |
| Leader, `?` | Controls guide |
| Leader, `b` | Sidebar |
| Leader, `1` / `2` / `4` | Focus / split / grid |
| Leader, `o` | Remembered sessions |
| Leader, `d` | Detach cockpit, keeping original agents running |

[Full controls](docs/USAGE.md) · [Architecture](docs/ARCHITECTURE.md) ·
[Integration](docs/INTEGRATION.md) · [Development and validation](docs/DEVELOPMENT.md)

## Status and limits

Original tmux sessions own their agents. Cockpit owns temporary UI attachments;
closing cockpit does not close the original agents. Native pane geometry is
shared with other tmux clients. Input ownership checks cover ordinary target
loss/replacement; concurrent changes to original tmux exit behavior are an
explicitly unsupported case. Cockpit does not modify original tmux settings.

Recovery needs exact native conversation and workspace evidence. Task completion
and native agent exit do not imply Forget. Explicit Close/Forget controls saved
restore intent. Full WSL-restart exclusion of a deliberately closed conversation,
physical terminal fidelity across all agents, and real Claude/Pi recovery remain
unverified. See the development notes before treating this as a daily driver.

## Provenance and public contents

[The extraction manifest](docs/PROVENANCE.json) identifies the source revision and
hashes of unchanged Rust files. This is a clean snapshot, not an import of the
original Git history. Raw session transcripts, SQLite databases, prompts, runtime
manifests, screenshots, test execution logs and historical evidence are excluded.
Original development history and evidence remain in the cc-sessions repository.

See `LICENSE`, `LICENSE-cc-sessions` and `NOTICE` for attribution.
