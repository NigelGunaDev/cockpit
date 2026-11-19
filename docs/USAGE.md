# Terminal workspace usage

Start in a regular WSL terminal outside tmux. Choose existing original sessions and
pass project directories with `--project` if needed. No files need to be installed
in the project. The original server and its settings remain yours.

## Navigate and arrange

The default leader is Ctrl+] followed by a separate shortcut key. Leader then
Space searches commands; `s` opens the picker, and `b` toggles the sidebar. In the
picker, arrows/j/k select, `/` searches, Enter focuses/replaces, `a` adds a tile,
and Escape closes. Clicking selects a row; Enter performs attachment.

Use leader then `1`, `2` or `4` for focused, split or grid layout. Leader then
Tab/Shift+Tab or arrows changes the focused visible terminal. Leader then `p`
returns to the previous valid selection. Leader then `x` removes a tile, leaving
its original process running. Leader then `d` detaches the whole cockpit.

Keep enough room for each terminal: layouts require at least 60 columns by 12 rows
per container, plus borders/status. Small windows fall back to a focused view.
Original pane geometry is shared with other native tmux clients.

## Input and scrollback

Leader then `v` explicitly toggles read-only/input mode. Input ownership is
validated before handoff; ordinary target loss disconnects rather than following
a reused name. First click on an inactive tile focuses it; a subsequent click is
forwarded. Original tmux prefixes pass through. Configure a different cockpit
leader rather than silently altering original bindings.

Leader then `[` opens independently captured scrollback. Use arrows, PgUp/PgDn,
Home/End and `r` to recapture; Escape returns. Alternate-screen applications may
not expose earlier frames. Leader then `g` opens bounded read-only local Git
changes, with staged/base-comparison modes. These panels do not type into agents.

## Session state

Labels distinguish live, exited and disconnected terminals. Native presence is
not model activity: Cockpit shows activity only for terminals whose status file a
presentation client registered.

## Agent activity

`presentation open --status-file PATH` registers a JSON file for a terminal. Each
second Cockpit reads it and accepts it only when its `owner` (`pid` and process
`start`) is the terminal's pane process or one of its descendants, and its
`boot_id` or `owner_boot`, when present, is the current boot. `status` maps to:

| `status` | Shown as |
|---|---|
| `working`, `busy` | ◐ working |
| `waiting` (`detail` `permission_prompt` or another prompt) | × waiting: permission / question |
| `idle`, `stop_observed` | ○ idle, or ✓ done until you select that terminal |
| `errored`, `failed` | ! errored |
| `exited`, `closed` | - session ended |
| anything else, or an unverified file | · activity unknown, with the reason |

A working or waiting state not refreshed for five minutes is marked stale. In the
picker, `t` sorts terminals by attention (waiting, errored, done, working, idle)
and `e` filters by activity. Terminals that need you keep their activity line in
compact layouts. Registrations end when their terminal leaves the original server.

Leader then `o` manages remembered native conversations; see [recovery](RECOVERY.md).
New terminals can be created using ordinary tmux commands. Task/protocol creation,
approvals, queue management and task-review panels are not part of this app.
