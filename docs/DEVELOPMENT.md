# Development and validation

Development is paused. Preserve the existing terminal and workflow authority
boundaries before adding features. Start with the root Cargo commands in README.
All preserved Rust unit tests live beside the implementation under `src/`.

## Historical evidence

Before extraction, the integration merge passed 96 Rust tests and the shared
framework's JavaScript suite. Earlier disposable real-Codex checks demonstrated
exact-conversation recovery following a user-initiated WSL restart, read-only
restoration, repeat opening without duplicates, cancelled/confirmed exact Close,
and deliberate-close exclusion after ordinary reopening/private tmux server loss.
Those measurements are historical, not newly rerun by copying this repository.
No synthetic performance measurement establishes real multi-agent responsiveness.

Outstanding manual checks include deliberate-close exclusion across another full
WSL restart or PC power cycle, physical Windows Terminal keyboard/mouse/clipboard
fidelity, and real Claude/Pi recovery. Native exit still does not automatically
Forget; use explicit Close/Forget for restart exclusion.

## Returning to this project

1. Use a disposable Git project with a compatible installed workspace CLI.
2. Run the preserved unit tests, formatting, Clippy and locked release build.
3. Validate a focused native attachment, detach/reopen and exact target loss.
4. Exercise splits/grid, native concurrent clients, sizing, scrolling and input.
5. Validate recovery separately for each installed agent version and toolchain.
6. Record actual results and resource measurements; retain failed attempts locally.

Use separate tmux sockets and test conversations. Leave real tasks and original
settings alone. Never automatically approve agent trust/permissions or fabricate
workflow completion to get through a test.

## Extraction checks (2026-10-28)

Executed from this separate checkout using rustc 1.99.1 and Cargo 1.99.1:

- All 96 Rust unit tests passed, with one test thread.
- Formatting check passed.
- All-target Clippy passed with warnings denied.
- Locked offline release build passed using cached dependencies.
- All 28 source/manifest file hashes matched the original snapshot.
- Relative Markdown links resolved; allowlisted contents were screened for common
  credentials, private paths and personal identifiers. The only matched email was
  the reserved `test@example.invalid` fixture. This review is not a guarantee.

No live terminal, agent, reboot or production performance test was rerun during
extraction. Raw build/test output remains local and is not published. The original
acceptance drivers depend on the original fixture layout and need deliberate
adaptation before they can be runnable here.
