# Development and validation

Development is paused. Preserve native terminal fidelity and exact input ownership
when returning to this app. Rust unit tests live beside implementation under `src/`;
`backend/tests/recovery.test.cjs` covers the self-contained native process helper.
No project installation or task framework is required.

## Checks

```bash
cargo test --locked -- --test-threads=1
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
node --test backend/tests/recovery.test.cjs
python3 tests/terminal_smoke.py --binary target/release/cc-cockpit
python3 tests/native_recovery_smoke.py --binary target/release/cc-cockpit
```

Integration drivers use private temporary tmux servers, fixture Git projects and
explicit test registries/preferences. The terminal driver copies the binary away
from its source checkout and exercises real PTY input, layouts and reopening in a
project with no installed tooling. Recovery uses a synthetic Codex-shaped process
holding its own fixture conversation file open, with no model request or real agent.
Reports default to /tmp and raw output must stay outside the public repository.

## Executed standalone results

The standalone build passed 68 Rust unit tests, formatting, all-target Clippy with
warnings denied, and a locked release build using Rust/Cargo 1.99.1. Thirteen native
helper tests passed, including exact identity, Git changes, launcher shims,
replacement/shared-target refusal, idempotence, concurrent recovery and failed launch.
Seven real-PTY terminal checks passed without installed project tooling. Six synthetic
native-agent checks passed through the actual Rust supervisor and embedded helper:
registration, server-loss recovery read-only, duplicate-free reopen, exact Close,
forgotten-session exclusion and preservation of native history/Git/project files.

These are executed fixture checks, not real Claude/Codex/Pi compatibility, physical
Windows Terminal or PC reboot acceptance. Historical real Codex observations predate
the standalone refactor and do not validate the new packaging by themselves.

## Remaining manual validation

Use disposable conversations with each installed agent version. Check Shift+Enter,
multiline editing, native prefixes, clipboard/paste, Unicode/IME, mouse/wheel,
concurrent clients, resizing, clean/abrupt closure, and deliberate Close exclusion
across a coordinated WSL/PC restart. Record actual resource measurements instead
of treating synthetic tests as production performance. Do not auto-approve trust
or permissions, replay interrupted commands, or treat session presence as work done.
