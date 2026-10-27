# Architecture

| Component | Owner and responsibility |
|---|---|
| Original tmux server | Existing task sessions, panes and agent processes |
| Private transient tmux UI server | Cockpit layout, disposable original-server attachment clients and native input routing |
| Rust supervisor | Discovery, target validation, focus, layout, panels and local reconciliation |
| Installed workspace CLI | Task/workspace operations, observations, review/history readers, recovery and exact Close |
| Project workflow files | Task lifecycle and approval authority |
| Cockpit SQLite registry | User-local native-conversation restore intent and verified identity; no task lifecycle state |
| Presentation preferences | Selected/pinned terminals and layout; no authority to accept a replacement runtime |

`main.rs` discovers supported projects and the original socket. `observe.rs`
combines CLI snapshots with original tmux identities. `runtime.rs` creates a
private UI server and reconciles runtime/action requests. `tmux.rs` supplies
native command execution; `layout.rs` and `ui.rs` manage layouts and navigation.
`commands.rs`, `palette.rs`, `presentation.rs` and `key_hints.rs` expose controls.

`session_registry.rs` stores private conversation identity and enabled/forgotten
intent. `agents.rs` validates native Claude/Codex/Pi JSONL evidence. `recovery.rs`
provides inventory controls, and `restore.rs` runs a transient sequential worker
through the installed CLI. Short registry transactions do not span agent startup;
per-entry operation locks serialize recovery and Close. Exact ownership evidence
remains separate from trust approval and whether a model is working.

`attention.rs`, `queue.rs`, `review.rs`, `changes.rs` and `history.rs` are supporting
panels. They delegate state changes to the existing CLI. Viewing a terminal is
separate from enabling input. Removing a tile is separate from forgetting or
closing an agent. Completing a task is separate from review approval or merging.

There is no embedded terminal emulator, browser frontend or persistent cockpit
application service. The private UI server is tmux, with original task processes
remaining owned by their existing server. No active session or local state is
part of this repository.
