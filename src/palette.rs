//! Searchable built-in actions in the existing native popup, not a shell prompt.
use crate::{
    commands,
    interaction::{self, Hits},
    runtime, ui,
};
use crossterm::event::{
    self, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
    MouseEventKind,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, List, ListState, Paragraph, Wrap},
};
use std::{io, path::Path, time::Duration};

#[derive(Default)]
struct Palette {
    query: String,
    selected: Option<&'static str>,
    message: String,
}

impl Palette {
    fn items(&self) -> Vec<&'static commands::Command> {
        commands::matching(&self.query)
    }

    fn filter_changed(&mut self) {
        self.selected = self.items().first().map(|command| command.id);
        self.message.clear();
    }

    fn move_selection(&mut self, delta: isize) {
        let items = self.items();
        if items.is_empty() {
            self.selected = None;
            return;
        }
        let index = items
            .iter()
            .position(|command| Some(command.id) == self.selected)
            .unwrap_or(0);
        let next = index.saturating_add_signed(delta).min(items.len() - 1);
        self.selected = Some(items[next].id);
        self.message.clear();
    }

    fn draw(&self, frame: &mut Frame, run: &crate::model::Run) -> Hits<&'static str> {
        let mut hits = Hits::new(frame.area());
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(5),
        ])
        .split(crate::presentation::body(frame.area()));
        frame.render_widget(
            Paragraph::new(self.query.as_str()).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Search commands — type an action"),
            ),
            areas[0],
        );
        let items = self.items();
        let gap = crate::presentation::list_gap(frame.area());
        let rows = items.iter().map(|command| {
            let reason = commands::unavailable(command, run);
            let detail = reason.as_deref().unwrap_or(command.description);
            crate::presentation::separated(
                vec![
                    Line::from(format!(
                        "{} · {}",
                        command.label,
                        commands::shortcut(command, &run.leader)
                    )),
                    Line::from(format!(
                        "  {}{detail}",
                        if reason.is_some() {
                            "Unavailable: "
                        } else {
                            ""
                        }
                    )),
                ],
                areas[1].width.saturating_sub(2),
                gap,
            )
            .style(Style::default().fg(if reason.is_some() {
                Color::DarkGray
            } else {
                Color::Reset
            }))
        });
        let mut state = ListState::default().with_selected(
            items
                .iter()
                .position(|command| Some(command.id) == self.selected),
        );
        frame.render_stateful_widget(
            List::new(rows)
                .block(Block::default().borders(Borders::ALL).title("Actions"))
                .highlight_style(crate::presentation::selected()),
            areas[1],
            &mut state,
        );
        hits.list_gaps(
            Block::default().borders(Borders::ALL).inner(areas[1]),
            state.offset(),
            &items
                .iter()
                .map(|command| (2, gap, command.id))
                .collect::<Vec<_>>(),
        );
        let message = if items.is_empty() {
            "No matching commands. Backspace edits the search."
        } else {
            &self.message
        };
        frame.render_widget(Paragraph::new(format!("Type to search · ↑/↓ select · Enter run · Esc cancel\n{}\nExisting task confirmations and view/control rules apply.", ui::sanitize_history(message)))
            .wrap(Wrap { trim: false }), areas[2]);
        hits
    }
}

/// Drop/restore the palette terminal before handing the same popup to a panel.
pub fn panel(path: &Path) -> Result<Option<&'static str>, String> {
    let initial = runtime::read_run(path)?;
    let mut palette = Palette::default();
    palette.filter_changed();
    let _restore = ui::Restore;
    crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    crossterm::execute!(
        io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        EnableMouseCapture
    )
    .map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    let mut hits = Hits::new(ratatui::layout::Rect::default());
    let mut chrome = crate::presentation::Chrome::default();
    loop {
        let run = runtime::read_run(path)?;
        terminal
            .draw(|frame| {
                hits = palette.draw(frame, &run);
                chrome.draw(frame, "Commands");
            })
            .map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())? {
            continue;
        }
        let input = event::read().map_err(|e| e.to_string())?;
        let size = terminal.size().map_err(|e| e.to_string())?;
        if chrome.closing(
            &input,
            ratatui::layout::Rect::new(0, 0, size.width, size.height),
        ) {
            return Ok(None);
        }
        if let Event::Mouse(mouse) = input {
            let size = terminal.size().map_err(|e| e.to_string())?;
            let current = ratatui::layout::Rect::new(0, 0, size.width, size.height);
            if hits.frame != current {
                continue;
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some(id) = hits.get(mouse, current) {
                    palette.selected = Some(*id);
                    palette.message.clear();
                }
            } else if let Some(delta) = interaction::wheel(mouse.kind) {
                palette.move_selection(delta);
            }
            continue;
        }
        let Event::Key(key) = input else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Ok(None);
        }
        match key.code {
            KeyCode::Up => palette.move_selection(-1),
            KeyCode::Down => palette.move_selection(1),
            KeyCode::Backspace => {
                palette.query.pop();
                palette.filter_changed();
            }
            KeyCode::Char(ch)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                if palette.query.chars().count() < 256 {
                    palette.query.push(ch);
                    palette.filter_changed();
                }
            }
            KeyCode::Enter => {
                let Some(id) = palette.selected else {
                    continue;
                };
                let command = commands::get(id)?;
                let selection = commands::Selection {
                    id: id.into(),
                    target: initial.selected.clone(),
                };
                if let Err(error) = commands::validate_selection(&selection, &run) {
                    palette.message = error;
                    continue;
                }
                let payload = serde_json::to_string(&selection).map_err(|e| e.to_string())?;
                match runtime::request(path, "command", Some(&payload)) {
                    Ok(()) => {
                        return Ok(match command.dispatch {
                            commands::Dispatch::Panel(role) => Some(role),
                            _ => None,
                        });
                    }
                    Err(error) => palette.message = error,
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn search_selection_and_disabled_reasons_render_with_actual_leader() {
        let mut run = crate::navigation::fixture();
        run.leader = "C-a".into();
        let mut palette = Palette {
            query: "split".into(),
            ..Default::default()
        };
        palette.filter_changed();
        let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
        terminal
            .draw(|frame| {
                palette.draw(frame, &run);
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Ctrl+a then 2"));
        assert!(text.contains("Unavailable: Select a terminal"));
        assert!(text.contains("Esc cancel"));
        palette.query = "no-command-xyz".into();
        palette.filter_changed();
        palette.move_selection(1);
        assert!(palette.selected.is_none());
    }
}
