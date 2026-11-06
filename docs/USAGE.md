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
not model activity. The standalone build has no project-hook status or persistent
attention inbox. Historical observation fields are not a promise of live evidence.

Leader then `o` manages remembered native conversations; see [recovery](RECOVERY.md).
New terminals can be created using ordinary tmux commands. Task/protocol creation,
approvals, queue management and task-review panels are not part of this app.
