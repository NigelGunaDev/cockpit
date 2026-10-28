//! Built-in UI actions. Workflow decisions remain in the existing adapters.
use crate::{model::Run, navigation};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dispatch {
    Panel(&'static str),
    Operation(&'static str),
    Detach,
    LiteralLeader,
}

pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    pub keywords: &'static str,
    pub keys: &'static [&'static str],
    pub description: &'static str,
    pub dispatch: Dispatch,
}

macro_rules! command {
    ($id:literal, $label:literal, $words:literal, [$($key:literal),*], $description:literal, $dispatch:expr) => {
        Command { id: $id, label: $label, keywords: $words, keys: &[$($key),*], description: $description, dispatch: $dispatch }
    };
}

pub const COMMANDS: &[Command] = &[
    command!(
        "palette",
        "Search commands",
        "actions menu palette",
        ["Space"],
        "Find an action and its shortcut.",
        Dispatch::Panel("palette")
    ),
    command!(
        "sessions",
        "Show sessions",
        "terminal switch picker search",
        ["s", "/"],
        "Enter focuses/replaces; a adds a tile.",
        Dispatch::Panel("sessions")
    ),
    command!(
        "last-session",
        "Previous session",
        "last recent back switch",
        ["p"],
        "Return to the previous valid visited terminal.",
        Dispatch::Operation("last-session")
    ),
    command!(
        "session-memory",
        "Remembered sessions",
        "restore registry persistence agent codex pi remember reopen retry close forget memory",
        ["o"],
        "Remember conversations for reopening; retry, close or forget them.",
        Dispatch::Panel("session-memory")
    ),
    command!(
        "sidebar",
        "Toggle sidebar",
        "project navigation",
        ["b"],
        "Show/hide sidebar; it opens with keyboard focus.",
        Dispatch::Operation("sidebar")
    ),
    command!(
        "focus-layout",
        "Focus one terminal",
        "layout zoom",
        ["1"],
        "Keep other attachments alive.",
        Dispatch::Operation("focus-layout")
    ),
    command!(
        "split",
        "Split two terminals",
        "layout side by side",
        ["2"],
        "Show two pinned terminals side by side.",
        Dispatch::Operation("split")
    ),
    command!(
        "grid",
        "Show terminal grid",
        "layout four",
        ["4"],
        "Show up to four pinned terminals.",
        Dispatch::Operation("grid")
    ),
    command!(
        "next",
        "Next pinned terminal",
        "focus cycle",
        ["Tab"],
        "Focus the next pinned tile.",
        Dispatch::Operation("next")
    ),
    command!(
        "previous",
        "Previous pinned terminal",
        "focus cycle",
        ["BTab"],
        "Focus the previous pinned tile.",
        Dispatch::Operation("previous")
    ),
    command!(
        "focus-left",
        "Focus terminal left",
        "visible tile direction navigation",
        ["Left"],
        "Focus a visible neighboring tile; original tmux bindings stay unchanged.",
        Dispatch::Operation("focus-left")
    ),
    command!(
        "focus-right",
        "Focus terminal right",
        "visible tile direction navigation",
        ["Right"],
        "Focus a visible neighboring tile; original tmux bindings stay unchanged.",
        Dispatch::Operation("focus-right")
    ),
    command!(
        "focus-up",
        "Focus terminal up",
        "visible tile direction navigation",
        ["Up"],
        "Focus a visible neighboring tile; original tmux bindings stay unchanged.",
        Dispatch::Operation("focus-up")
    ),
    command!(
        "focus-down",
        "Focus terminal down",
        "visible tile direction navigation",
        ["Down"],
        "Focus a visible neighboring tile; original tmux bindings stay unchanged.",
        Dispatch::Operation("focus-down")
    ),
    command!(
        "toggle-control",
        "Toggle view / control",
        "input read only writable",
        ["v"],
        "Explicitly change input mode for the selected terminal.",
        Dispatch::Operation("toggle-control")
    ),
    command!(
        "history",
        "Terminal scrollback history",
        "output capture independent",
        ["["],
        "Independent history: arrows, PgUp/PgDn, Home/End.",
        Dispatch::Panel("history")
    ),
    command!(
        "changes",
        "Git changes",
        "diff review staged branch",
        ["g"],
        "Read-only changes; Tab staged, b compare base.",
        Dispatch::Panel("changes")
    ),
    command!(
        "remove",
        "Remove cockpit tile",
        "unpin close",
        ["x"],
        "Remove this tile; the original task survives.",
        Dispatch::Operation("remove")
    ),
    command!(
        "detach",
        "Detach cockpit",
        "quit exit close",
        ["d"],
        "Close the cockpit; original task processes survive.",
        Dispatch::Detach
    ),
    command!(
        "help",
        "Controls guide",
        "help shortcuts keys",
        ["?"],
        "Show controls; Esc closes the guide.",
        Dispatch::Panel("help")
    ),
    command!(
        "literal-leader",
        "Forward literal leader",
        "terminal input",
        [],
        "Send one literal leader key to the controlled terminal.",
        Dispatch::LiteralLeader
    ),
];

pub fn get(id: &str) -> Result<&'static Command, String> {
    COMMANDS
        .iter()
        .find(|command| command.id == id)
        .ok_or_else(|| "Unknown cockpit command".into())
}

pub fn shortcut(command: &Command, leader: &str) -> String {
    let leader = crate::help::leader_label(leader);
    if command.dispatch == Dispatch::LiteralLeader {
        return format!("{leader} twice");
    }
    let keys: Vec<_> = command
        .keys
        .iter()
        .map(|key| match *key {
            "BTab" => "Shift+Tab",
            "H" => "Shift+H",
            "R" => "Shift+R",
            other => other,
        })
        .collect();
    format!("{leader} then {}", keys.join(" or "))
}

impl Command {
    pub fn contextual(&self) -> bool {
        !matches!(
            self.id,
            "palette"
                | "sessions"
                | "help"
                | "attention"
                | "queue"
                | "create"
                | "saved-history"
                | "review"
                | "session-memory"
                | "detach"
        )
    }

    pub fn needs_live_target(&self) -> bool {
        matches!(
            self.id,
            "history" | "changes" | "toggle-control" | "literal-leader"
        )
    }
}

/// Advisory presentation check; runtime validates again before acting.
pub fn unavailable(command: &Command, run: &Run) -> Option<String> {
    let tile = run
        .tiles
        .iter()
        .find(|tile| Some(&tile.target.key) == run.selected.as_ref());
    if command.id == "last-session" {
        return navigation::previous_key(run).err();
    }
    if matches!(command.id, "queue" | "create" | "saved-history" | "review")
        && run.projects.is_empty()
    {
        return Some("No registered project is available".into());
    }
    if command.contextual() && tile.is_none() {
        return Some("Select a terminal first".into());
    }
    if command.needs_live_target()
        && tile.is_some_and(|tile| {
            tile.disconnected.is_some()
                || !run
                    .targets
                    .iter()
                    .any(|target| target.key == tile.target.key && !target.dead)
        })
    {
        return Some("Selected terminal is disconnected or exited; select a live session".into());
    }
    if matches!(command.id, "split" | "grid" | "next" | "previous") && run.tiles.len() < 2 {
        return Some("Add another terminal with the session picker's a key".into());
    }
    if command.dispatch == Dispatch::LiteralLeader && !run.control {
        return Some("Selected terminal is view-only".into());
    }
    None
}

#[derive(Serialize, Deserialize)]
pub struct Selection {
    pub id: String,
    pub target: Option<String>,
}

pub fn validate_selection(selection: &Selection, run: &Run) -> Result<&'static Command, String> {
    let command = get(&selection.id)?;
    if command.contextual() && selection.target != run.selected {
        return Err("Selected terminal changed; reopen commands and choose again".into());
    }
    if let Some(reason) = unavailable(command, run) {
        return Err(reason);
    }
    Ok(command)
}

pub fn matching(query: &str) -> Vec<&'static Command> {
    let mut matches: Vec<_> = COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, command)| {
            !matches!(command.dispatch, Dispatch::LiteralLeader) && command.id != "palette"
        })
        .filter_map(|(index, command)| {
            navigation::match_score(query, &[command.label, command.keywords])
                .map(|score| (score, index, command))
        })
        .collect();
    matches.sort_by_key(|(score, index, _)| (*score, *index));
    matches.into_iter().map(|(_, _, command)| command).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn definitions_have_unique_ids_keys_and_searchable_actions() {
        for retired in ["queue", "create", "review", "attention", "saved-history"] {
            assert!(get(retired).is_err());
        }
        let mut ids = HashSet::new();
        let mut keys = HashSet::new();
        for command in COMMANDS {
            assert!(ids.insert(command.id));
            for key in command.keys {
                assert!(keys.insert(key));
            }
        }
        for (query, id) in [
            ("history", "history"),
            ("remember", "session-memory"),
            ("split", "split"),
            ("changes", "changes"),
        ] {
            assert!(matching(query).iter().any(|command| command.id == id));
        }
        assert_eq!(
            shortcut(get("palette").unwrap(), "C-a"),
            "Ctrl+a then Space"
        );
        assert_eq!(
            shortcut(get("session-memory").unwrap(), "C-]"),
            "Ctrl+] then o"
        );
    }

    #[test]
    fn availability_and_execution_context_are_rechecked() {
        let mut run = navigation::fixture();
        let command = get("changes").unwrap();
        assert!(
            unavailable(command, &run)
                .unwrap()
                .contains("Select a terminal")
        );
        let target = navigation::target("old", None, None);
        run.selected = Some(target.key.clone());
        run.targets.push(target.clone());
        run.tiles.push(crate::model::Tile {
            target,
            pane: "%9".into(),
            disconnected: None,
            visible: true,
        });
        let selection = Selection {
            id: "changes".into(),
            target: run.selected.clone(),
        };
        assert!(validate_selection(&selection, &run).is_ok());
        run.selected = Some("new".into());
        assert!(
            validate_selection(&selection, &run)
                .err()
                .unwrap()
                .contains("changed")
        );
        run.selected = Some("old".into());
        run.targets.clear();
        assert!(validate_selection(&selection, &run).is_err());
        assert!(get("task-approve").is_err());
    }
}
