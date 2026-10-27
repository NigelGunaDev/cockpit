//! Read-only terminal controls guide. No task or attachment operations.
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

const NOTES: &[(&str, &str)] = &[
    (
        "Original tmux prefix",
        "Passes through. Your original settings stay unchanged.",
    ),
    (
        "Picker search",
        "Press /, type a filter, then Esc to finish searching.",
    ),
    (
        "Filtered results",
        "Enter attaches; a adds a tile. Esc closes the picker.",
    ),
    (
        "Mouse in terminal tiles",
        "First click focuses; another click reaches the app.",
    ),
    (
        "Mouse in panels",
        "Click selects rows/fields; wheel scrolls. Enter performs actions.",
    ),
    (
        "Read only",
        "Typing is suppressed. Use leader then v for control.",
    ),
    (
        "Disconnected tile",
        "Select a live session explicitly; no automatic replacement.",
    ),
    (
        "Inbox navigation",
        "/ search (Ctrl+u clears); n next unresolved; t type; v receipts; p project; ] older / [ latest.",
    ),
    (
        "Session organization",
        "t toggles projects/attention-first; e cycles observed activity, including unknown and stale.",
    ),
    (
        "History limitations",
        "Earlier alternate-screen frames may be unavailable.",
    ),
    (
        "Session groups",
        "Tab selects groups; Left/Right folds/expands. Up/Down selects terminals.",
    ),
    (
        "Status counts",
        "A: unresolved/unacknowledged. * means incomplete attention evidence.",
    ),
];

pub fn leader_label(leader: &str) -> String {
    leader
        .strip_prefix("C-")
        .map(|key| format!("Ctrl+{key}"))
        .unwrap_or_else(|| leader.to_owned())
}

#[derive(Default)]
pub struct Help {
    state: ListState,
}

impl Help {
    pub fn key(&mut self, key: KeyCode, rows: u16) {
        let cursor = self.state.selected().unwrap_or(0);
        let page = usize::from(rows.saturating_sub(7) / 2).max(1);
        let last = crate::commands::COMMANDS.len() + NOTES.len() - 1;
        let next = match key {
            KeyCode::Up | KeyCode::Char('k') => cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => cursor.saturating_add(1).min(last),
            KeyCode::PageUp => cursor.saturating_sub(page),
            KeyCode::PageDown => cursor.saturating_add(page).min(last),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => cursor,
        };
        self.state.select(Some(next));
    }

    pub fn draw(&mut self, frame: &mut Frame, leader: &str) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
        ])
        .split(crate::presentation::body(frame.area()));
        let label = leader_label(leader);
        frame.render_widget(
            Paragraph::new(format!("Press {label}, release, then press the shortcut."))
                .wrap(Wrap { trim: false })
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Cockpit controls"),
                ),
            areas[0],
        );
        self.state.select(Some(self.state.selected().unwrap_or(0)));
        let commands = crate::commands::COMMANDS.iter().map(|command| {
            (
                format!(
                    "{} · {}",
                    crate::commands::shortcut(command, leader),
                    command.label
                ),
                command.description.to_owned(),
            )
        });
        let items = commands
            .chain(
                NOTES
                    .iter()
                    .map(|(label, description)| ((*label).into(), (*description).into())),
            )
            .map(|(key, description)| {
                ListItem::new(vec![
                    Line::from(key),
                    Line::from(format!("  {description}")),
                ])
            });
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().borders(Borders::ALL).title("Shortcuts"))
                .highlight_style(crate::presentation::selected()),
            areas[1],
            &mut self.state,
        );
        frame.render_widget(
            Paragraph::new("↑/↓ j/k scroll · PgUp/PgDn · Home/End · Esc close\nTyping here never reaches the task terminal.")
                .wrap(Wrap { trim: false }),
            areas[2],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn custom_leader_and_scrolling_expose_controls_on_small_screens() {
        let mut help = Help::default();
        let mut terminal = Terminal::new(TestBackend::new(72, 14)).unwrap();
        let text = |terminal: &Terminal<TestBackend>| {
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        terminal.draw(|frame| help.draw(frame, "C-a")).unwrap();
        assert!(text(&terminal).contains("Ctrl+a"));
        assert!(text(&terminal).contains("Esc close"));
        assert!(!text(&terminal).contains("Ctrl+]"));
        help.key(KeyCode::End, 14);
        terminal.draw(|frame| help.draw(frame, "C-a")).unwrap();
        assert!(text(&terminal).contains("Status counts"));
        help.key(KeyCode::Home, 14);
        assert_eq!(help.state.selected(), Some(0));
        help.key(KeyCode::Up, 14);
        assert_eq!(help.state.selected(), Some(0));
        help.key(KeyCode::PageDown, 14);
        assert!(help.state.selected().unwrap() > 0);
    }
}
