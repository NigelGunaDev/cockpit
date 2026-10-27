# cc-sessions integration

Cockpit is independently buildable but still depends on a compatible cc-sessions
CLI installed in every project it supervises. Keep that dependency when returning
to this experiment; extraction does not make its lifecycle implementation portable.

The source snapshot is recorded in PROVENANCE.json. The upstream repository is
https://github.com/NigelGunaDev/cc-sessions . To prepare a disposable project, follow
that repository's installer instructions. Avoid modifying real task state to try
the interface.

The Rust supervisor executes the chosen installed project's `sessions/bin/sessions`
from its project directory. Required routes include `workspace snapshot --json`;
attention, task-review and history routes supply their corresponding panels.
Conversation recovery additionally requires `workspace recover` and
`workspace close-session`, including exact Git/native/runtime validation. The
shared implementations remain in cc-sessions rather than being duplicated here.

Explicit `--project` arguments are recommended. Otherwise discovery walks parents
of the canonical current directory for `sessions/bin/sessions`, with the supported
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

The native acceptance drivers and raw historical results were not copied into
this public snapshot. They remain in the original source repository; review and
sanitize them before moving individual drivers here. The Rust unit tests are
preserved with the source and run without copying local conversations or settings.
