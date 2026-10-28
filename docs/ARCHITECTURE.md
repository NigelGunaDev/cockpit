# Architecture

| Owner | Responsibility |
|---|---|
| Original tmux server | Existing terminals, panes and agent processes |
| Private transient tmux UI server | Layout and disposable attachments; native rendering/input |
| Rust supervisor | Native directory/Git discovery, exact target validation, navigation, panels and reconciliation |
| Private SQLite registry | Native conversation identity and enabled/forgotten restore intent |
| Presentation preferences | Attached/selected terminals and layout; no replacement-target authority |
| Embedded Node helper | Short-lived native recovery/Close operations, with no installed project code |

`main.rs` resolves directories, original socket and remembered state. `observe.rs`
reads tmux and Git directly, accepting plain directories and enumerating registered
worktrees. It neither looks for nor executes a project CLI. `runtime.rs` supervises
the private UI server; `layout.rs`, `ui.rs`, `commands.rs`, `palette.rs` and
`presentation.rs` supply the terminal interface. `changes.rs` reads local Git
changes. Terminal scrollback is captured independently of original copy mode.

`session_registry.rs` owns durable saved intent and exact native/runtime/repository
evidence. `agents.rs` validates native files. `recovery.rs` supplies inventory/link/
Forget/Close controls. `restore.rs` runs one transient sequential worker, using
per-entry operation locks and short registry transactions.

`backend.rs` embeds `backend/recovery.cjs` at compile time and invokes Node with a
structured request as a scalar argument. The helper uses built-in modules and
Git/tmux to validate/reopen exact conversations. No npm modules, project installer,
external orchestration CLI or persistent Node service are involved. Entry PATH is
captured before Node/toolchain shims can rewrite it. No native/model prompt is sent.

Close disables remembered intent before exact foreground native-file ownership is
checked by the process operation. Multiple panes, shared windows, changed generations
and uncertain ownership are refused. The underlying tmux process remains independent
of attachment clients. Task protocols and their data are not read or written.

Legacy observation metadata fields remain compatible in serialized structures;
there is no hook-backed status producer or attention receipt service. Session presence
and native-file ownership do not establish model activity, trust or completed work.
