//! Mouse hit regions from the last rendered frame. No runtime actions here.
use crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

pub struct Hits<T> {
    pub frame: Rect,
    regions: Vec<(Rect, T)>,
}

impl<T> Hits<T> {
    pub fn new(frame: Rect) -> Self {
        Self {
            frame,
            regions: vec![],
        }
    }
    pub fn add(&mut self, area: Rect, value: T) {
        self.regions.push((area.intersection(self.frame), value));
    }
    pub fn get(&self, mouse: MouseEvent, current: Rect) -> Option<&T> {
        if self.frame != current {
            return None;
        }
        self.regions
            .iter()
            .rev()
            .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
            .map(|(_, value)| value)
    }
}

impl<T: Clone> Hits<T> {
    /// Content height and decorative gap height are distinct. ListState.offset
    /// still counts whole items; gaps scroll with them but cannot select them.
    pub fn list_gaps(&mut self, area: Rect, offset: usize, rows: &[(u16, u16, T)]) {
        let mut y = area.y;
        for (content, gap, value) in rows.iter().skip(offset) {
            let height = (*content).min(area.bottom().saturating_sub(y));
            if height == 0 {
                break;
            }
            self.add(Rect::new(area.x, y, area.width, height), value.clone());
            y = y.saturating_add(*content).saturating_add(*gap);
        }
    }
    /// ListState.offset() is an item offset, not a terminal-line offset.
    pub fn list(&mut self, area: Rect, offset: usize, rows: &[(u16, T)]) {
        let mut y = area.y;
        for (height, value) in rows.iter().skip(offset) {
            let height = (*height).min(area.bottom().saturating_sub(y));
            if height == 0 {
                break;
            }
            self.add(Rect::new(area.x, y, area.width, height), value.clone());
            y = y.saturating_add(height);
        }
    }
}

pub fn wheel(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(-3),
        MouseEventKind::ScrollDown => Some(3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton};
    #[test]
    fn decorative_gaps_never_select_items_and_offsets_count_items() {
        let area = Rect::new(0, 0, 40, 10);
        let mut hits = Hits::new(area);
        hits.list_gaps(
            area,
            1,
            &[(2, 1, "hidden"), (2, 1, "first"), (3, 1, "second")],
        );
        let click = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 4,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(hits.get(click(0), area), Some(&"first"));
        assert_eq!(hits.get(click(1), area), Some(&"first"));
        assert!(hits.get(click(2), area).is_none());
        assert_eq!(hits.get(click(3), area), Some(&"second"));
        assert_eq!(hits.get(click(5), area), Some(&"second"));
        assert!(hits.get(click(6), area).is_none());
        assert!(hits.get(click(3), Rect::new(0, 0, 41, 10)).is_none());
    }
    #[test]
    fn list_hits_use_visible_multiline_items_and_refuse_old_geometry() {
        let area = Rect::new(0, 0, 40, 12);
        let mut hits = Hits::new(area);
        hits.list(
            Rect::new(1, 2, 38, 5),
            1,
            &[(1, "header"), (3, "second"), (3, "third")],
        );
        let click = |row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(hits.get(click(2), area), Some(&"second"));
        assert_eq!(hits.get(click(6), area), Some(&"third"));
        assert!(hits.get(click(7), area).is_none());
        assert!(hits.get(click(2), Rect::new(0, 0, 60, 12)).is_none());
    }
}
