//! Shared panel chrome and semantic colours. No terminal or workflow operations.
use crate::interaction::Hits;
use crossterm::event::{Event, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{ListItem, Paragraph},
};

pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::Gray;
pub const ATTENTION: Color = Color::Yellow;

/// Decorative rows belong to list items for scrolling, but never receive hits.
pub fn list_gap(area: Rect) -> u16 {
    u16::from(area.width >= 40 && area.height >= 24)
}

pub fn separated(mut lines: Vec<Line<'_>>, width: u16, gap: u16) -> ListItem<'_> {
    for _ in 0..gap {
        lines.push(Line::styled(
            "─".repeat(usize::from(width)),
            Style::default().fg(Color::DarkGray).bg(Color::Reset),
        ));
    }
    ListItem::new(lines)
}

/// Disabled buttons remain visible but supply no action hit region.
pub fn button(frame: &mut Frame, area: Rect, label: &str, enabled: bool) -> Option<Rect> {
    let text = format!(" {label} ");
    let width = Line::from(text.as_str()).width() as u16;
    if area.height == 0 || area.width < width {
        return None;
    }
    let rect = Rect::new(area.x, area.y, width, 1);
    frame.render_widget(
        Paragraph::new(text).style(if enabled {
            selected()
        } else {
            Style::default().fg(Color::DarkGray)
        }),
        rect,
    );
    enabled.then_some(rect)
}

pub fn selected() -> Style {
    Style::default()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD)
}

/// Reserve one header row in supporting panels; live terminal geometry is unchanged.
pub fn body(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(u16::from(area.height > 0)),
        area.width,
        area.height.saturating_sub(1),
    )
}

#[derive(Default)]
pub struct Chrome {
    close: Option<Hits<()>>,
}

impl Chrome {
    pub fn draw(&mut self, frame: &mut Frame, title: &str) {
        let area = frame.area();
        let mut hits = Hits::new(area);
        if area.height == 0 {
            self.close = Some(hits);
            return;
        }
        let label = if area.width >= 22 {
            " Esc Close "
        } else {
            "Close"
        };
        let width = label.len() as u16;
        let close_width = if area.width >= width { width } else { 0 };
        let header = Rect::new(area.x, area.y, area.width, 1);
        frame.render_widget(
            Paragraph::new("").style(Style::default().bg(Color::Reset)),
            header,
        );
        let title_area = Rect::new(
            area.x,
            area.y,
            area.width.saturating_sub(close_width + 1),
            1,
        );
        frame.render_widget(
            Paragraph::new(Line::styled(
                format!(" {title}"),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            )),
            title_area,
        );
        if close_width > 0 {
            let close = Rect::new(area.right() - close_width, area.y, close_width, 1);
            frame.render_widget(Paragraph::new(label).style(selected()), close);
            hits.add(close, ());
        }
        self.close = Some(hits);
    }

    /// Explicit Close dismisses the panel, including searches/confirmations. It
    /// never synthesizes Enter or calls an attachment/lifecycle operation.
    pub fn closing(&self, event: &Event, current: Rect) -> bool {
        matches!(event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && self.close.as_ref().is_some_and(|hits| hits.get(*mouse, current).is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn list_spacing_is_compact_on_small_screens_and_disabled_buttons_have_no_hit() {
        assert_eq!(list_gap(Rect::new(0, 0, 80, 24)), 1);
        assert_eq!(list_gap(Rect::new(0, 0, 32, 40)), 0);
        assert_eq!(list_gap(Rect::new(0, 0, 80, 12)), 0);
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| {
                assert!(button(frame, Rect::new(0, 0, 30, 1), "Open terminal", false).is_none());
                assert!(button(frame, Rect::new(0, 1, 3, 1), "Open terminal", true).is_none());
                assert!(button(frame, Rect::new(0, 2, 30, 1), "Open terminal", true).is_some());
            })
            .unwrap();
    }

    #[test]
    fn close_is_bounded_and_refuses_stale_geometry_or_other_gestures() {
        for width in [0, 3, 5, 12, 22, 32, 100] {
            let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
            let mut chrome = Chrome::default();
            terminal
                .draw(|frame| chrome.draw(frame, "Sessions"))
                .unwrap();
            let size = Rect::new(0, 0, width, 12);
            let mut mouse = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: width.saturating_sub(1),
                row: 0,
                modifiers: KeyModifiers::NONE,
            };
            assert_eq!(chrome.closing(&Event::Mouse(mouse), size), width >= 5);
            assert!(!chrome.closing(&Event::Mouse(mouse), Rect::new(0, 0, width + 1, 12)));
            mouse.kind = MouseEventKind::Up(MouseButton::Left);
            assert!(!chrome.closing(&Event::Mouse(mouse), size));
            mouse.kind = MouseEventKind::Down(MouseButton::Left);
            mouse.row = 1;
            assert!(!chrome.closing(&Event::Mouse(mouse), size));
            assert_eq!(body(size).height, 11);
        }
        assert_eq!(body(Rect::default()), Rect::default());
    }
}
