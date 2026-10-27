# Workspace CLI integration

Cockpit is independently buildable but requires a compatible workspace CLI in
every project it supervises. A backend implementation and installer are not
included in this repository. Standalone backend support remains future work.

The supervisor executes a project's `sessions/bin/sessions` from that project's
directory. Required routes include `workspace snapshot --json`; attention,
task-review and history routes supply their corresponding panels. Conversation
recovery additionally requires `workspace recover` and `workspace close-session`,
including exact Git, native conversation and runtime validation. These operations
are supplied by the external backend.

Use a disposable project with a compatible installed CLI for evaluation. Explicit
`--project` arguments are recommended. Otherwise discovery walks parents of the
canonical current directory for `sessions/bin/sessions`, with the supported
environment fallback. Enabled remembered projects can join interactive discovery
only when the selected socket and recorded repository identity still match.

Export executable/environment requirements before launch. Shell aliases are not
saved recovery definitions. The CLI preserves entry PATH for native launchers;
agent binaries must be available in that environment. Cockpit never migrates a
Claude conversation into Codex or Pi and does not run Claude lifecycle skills in
other agents.

`--list` is read-only discovery. Interactive startup may reopen enabled saved
conversations through the CLI; it submits no new model prompt. A failure, occupied
name or ambiguous identity is visible rather than silently retargeted. Recovered
attachments start read-only. Explicit Close disables restore intent before asking
the CLI to stop an exactly verified original agent session.

Native acceptance drivers and raw historical results are not included. The Rust
unit tests are preserved with the source and run without copying local
conversations or settings. Baseline import hashes are recorded in PROVENANCE.json.
