//! Contextual help inside supporting panels. No terminal or workflow actions.
use crossterm::event::KeyCode;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub struct Hint {
    pub key: &'static str,
    pub label: &'static str,
    pub action: Option<KeyCode>,
}
impl Hint {
    pub fn label(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            action: None,
        }
    }
    pub fn press(key: &'static str, label: &'static str, action: KeyCode) -> Self {
        Self {
            key,
            label,
            action: Some(action),
        }
    }
}

#[derive(Clone)]
pub struct Keys {
    pub visible: bool,
    pub more: bool,
}
impl Default for Keys {
    fn default() -> Self {
        Self {
            visible: true,
            more: false,
        }
    }
}
impl Keys {
    fn full(&self, area: Rect) -> bool {
        self.visible && area.width >= 100 && area.height >= 28
    }
    pub fn height(&self, area: Rect) -> u16 {
        if self.full(area) { 10 } else { 3 }
    }
    pub fn toggle(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::F(1) => self.visible = !self.visible,
            KeyCode::F(2) => {
                self.more = !self.more;
                self.visible = true;
            }
            _ => return false,
        }
        true
    }
    pub fn draw(
        &self,
        frame: &mut Frame,
        area: Rect,
        context: &str,
        hints: &[Hint],
        status: &str,
    ) -> Vec<(Rect, KeyCode)> {
        let mut hits = Vec::new();
        if self.full(frame.area()) {
            let width = 36.min(area.width);
            let help = Rect::new(area.right() - width, area.y, width, area.height);
            let status_area = Rect::new(
                area.x,
                area.y,
                area.width.saturating_sub(width + 1),
                area.height,
            );
            frame.render_widget(
                Paragraph::new(status).wrap(Wrap { trim: false }),
                status_area,
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!("Keys: {context}"))
                .title_bottom(if self.more {
                    "F1 hide · F2 basics"
                } else {
                    "F1 hide · F2 more"
                })
                .border_style(Style::default().fg(Color::Cyan));
            let inside = block.inner(help);
            frame.render_widget(block, help);
            for (index, hint) in hints.iter().take(inside.height as usize).enumerate() {
                let row = Rect::new(inside.x, inside.y + index as u16, inside.width, 1);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(
                            format!("{:<11}", hint.key),
                            Style::default().fg(if hint.action.is_some() {
                                Color::Cyan
                            } else {
                                Color::Gray
                            }),
                        ),
                        Span::raw(hint.label),
                    ])),
                    row,
                );
                if let Some(action) = hint.action {
                    hits.push((row, action));
                }
            }
            let bottom = help.bottom().saturating_sub(1);
            hits.push((Rect::new(help.x + 1, bottom, 8, 1), KeyCode::F(1)));
            hits.push((
                Rect::new(help.x + 11, bottom, width.saturating_sub(12), 1),
                KeyCode::F(2),
            ));
        } else {
            let label = if self.visible {
                "F1 hide keys"
            } else {
                "F1 show keys"
            };
            let summary = if self.visible {
                hints
                    .iter()
                    .take(3)
                    .map(|hint| format!("{} {}", hint.key, hint.label))
                    .collect::<Vec<_>>()
                    .join(" · ")
            } else {
                "F2 more · Esc close".into()
            };
            frame.render_widget(
                Paragraph::new(format!(
                    "{label} · Esc close · F2 more · {context} · {summary}"
                )),
                Rect::new(area.x, area.y, area.width, area.height.min(1)),
            );
            frame.render_widget(
                Paragraph::new(status).wrap(Wrap { trim: false }),
                Rect::new(
                    area.x,
                    area.y + area.height.min(1),
                    area.width,
                    area.height.saturating_sub(1),
                ),
            );
            hits.push((
                Rect::new(area.x, area.y, 12.min(area.width), 1),
                KeyCode::F(1),
            ));
        }
        hits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    #[test]
    fn reserved_help_rows_are_clickable_without_covering_status_and_shrink_on_small_screens() {
        let hints = [
            Hint::label("Enter", "Confirm (keyboard)"),
            Hint::press("Esc", "Close", KeyCode::Esc),
        ];
        for (width, height) in [(140, 35), (40, 12)] {
            let keys = Keys::default();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut hits = Vec::new();
            terminal
                .draw(|frame| {
                    let area = Rect::new(
                        0,
                        height - keys.height(frame.area()),
                        width,
                        keys.height(frame.area()),
                    );
                    hits = keys.draw(frame, area, "confirmation", &hints, "STATUS PRESERVED");
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let text = (0..height)
                .map(|y| {
                    (0..width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("STATUS PRESERVED"));
            assert!(hits.iter().all(|(_, key)| *key != KeyCode::Enter));
            if width > 100 {
                let row = hits.iter().find(|(_, key)| *key == KeyCode::Esc).unwrap().0;
                assert!(row.x >= 100 && row.y >= height - 10);
            }
            assert!(
                hits.iter()
                    .all(|(rect, _)| rect.right() <= width && rect.bottom() <= height)
            );
        }
    }
}
