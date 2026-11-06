//! Presentation policy only. All pane arrangement happens on the private server.
use crate::model::View;

pub const MAX_TILES: usize = 4;
pub const MIN_COLUMNS: u32 = 60;
pub const MIN_ROWS: u32 = 12;

/// Choose only among supplied visible tile rectangles, never the sidebar.
pub fn neighbor(
    panes: &[(usize, ratatui::layout::Rect)],
    active: usize,
    direction: &str,
) -> Option<usize> {
    let (_, origin) = panes.iter().find(|(index, _)| *index == active)?;
    let center = |r: &ratatui::layout::Rect| {
        (
            i32::from(r.x) * 2 + i32::from(r.width),
            i32::from(r.y) * 2 + i32::from(r.height),
        )
    };
    let (x, y) = center(origin);
    panes
        .iter()
        .filter_map(|(index, rect)| {
            if *index == active {
                return None;
            }
            let (nx, ny) = center(rect);
            let (forward, sideways) = match direction {
                "left" => (x - nx, (ny - y).abs()),
                "right" => (nx - x, (ny - y).abs()),
                "up" => (y - ny, (nx - x).abs()),
                "down" => (ny - y, (nx - x).abs()),
                _ => return None,
            };
            (forward > 0).then_some(((sideways, forward, *index), *index))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, index)| index)
}

/// A fixed sidebar + one/two/four native pane layout, not terminal emulation.
/// tmux owns rendering, borders, and terminal input after accepting this layout.
pub fn with_sidebar(
    width: u32,
    height: u32,
    sidebar: &str,
    panes: &[String],
) -> Result<String, String> {
    fn id(pane: &str) -> Result<u32, String> {
        pane.strip_prefix('%')
            .ok_or("Invalid private pane")?
            .parse()
            .map_err(|_| "Invalid private pane".into())
    }
    fn leaf(w: u32, h: u32, x: u32, y: u32, id: u32) -> String {
        format!("{w}x{h},{x},{y},{id}")
    }
    fn row(w: u32, h: u32, x: u32, y: u32, ids: &[u32]) -> String {
        if ids.len() == 1 {
            return leaf(w, h, x, y, ids[0]);
        }
        let left = (w - 1) / 2;
        format!(
            "{w}x{h},{x},{y}{{{},{}}}",
            leaf(left, h, x, y, ids[0]),
            leaf(w - left - 1, h, x + left + 1, y, ids[1])
        )
    }
    if panes.is_empty() || panes.len() > MAX_TILES || width < 40 || height < 5 {
        return Err("Unsupported private sidebar geometry".into());
    }
    let ids: Vec<_> = panes
        .iter()
        .map(|pane| id(pane))
        .collect::<Result<_, _>>()?;
    let (w, x) = (width - 33, 33);
    let terminals = if ids.len() <= 2 {
        row(w, height, x, 0, &ids)
    } else {
        let top = (height - 1) / 2;
        format!(
            "{w}x{height},{x},0[{},{}]",
            row(w, top, x, 0, &ids[..2]),
            row(w, height - top - 1, x, top + 1, &ids[2..])
        )
    };
    let body = format!(
        "{width}x{height},0,0{{{},{terminals}}}",
        leaf(32, height, 0, 0, id(sidebar)?)
    );
    let checksum = body.bytes().fold(0u16, |value, byte| {
        value.rotate_right(1).wrapping_add(u16::from(byte))
    });
    Ok(format!("{checksum:04x},{body}"))
}

pub fn visible(view: View, count: usize, active: usize) -> Vec<usize> {
    if count == 0 {
        return vec![];
    }
    let active = active.min(count - 1);
    let capacity = match view {
        View::Focus => 1,
        View::Split => 2,
        View::Grid => MAX_TILES,
    };
    let mut indices: Vec<_> = (0..count).take(capacity).collect();
    if !indices.contains(&active) {
        *indices.last_mut().unwrap() = active;
    }
    indices
}

pub fn check_size(
    view: View,
    count: usize,
    width: u32,
    height: u32,
    sidebar: bool,
) -> Result<(), String> {
    let (cols, rows) = match view {
        View::Focus => (1, 1),
        View::Split if count > 1 => (2, 1),
        View::Grid if count > 2 => (2, 2),
        View::Grid if count > 1 => (2, 1),
        _ => (1, 1),
    };
    let width = width.saturating_sub(if sidebar { 33 } else { 0 });
    // One row is reserved for the private pane title; tmux supplies split borders.
    if width.saturating_sub(cols - 1) / cols < MIN_COLUMNS
        || height.saturating_sub(rows) / rows < MIN_ROWS
    {
        return Err(format!(
            "Layout needs at least {MIN_COLUMNS}×{MIN_ROWS} per terminal. Enlarge the window or use leader + 1 (focus)."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directional_focus_uses_visible_geometry_without_wrapping() {
        use ratatui::layout::Rect;
        let panes = [
            (0, Rect::new(33, 0, 60, 15)),
            (1, Rect::new(94, 0, 60, 15)),
            (2, Rect::new(33, 16, 60, 15)),
            (3, Rect::new(94, 16, 60, 15)),
        ];
        assert_eq!(neighbor(&panes, 0, "right"), Some(1));
        assert_eq!(neighbor(&panes, 0, "down"), Some(2));
        assert_eq!(neighbor(&panes, 3, "left"), Some(2));
        assert_eq!(neighbor(&panes, 3, "up"), Some(1));
        assert_eq!(neighbor(&panes, 0, "left"), None);
        assert_eq!(neighbor(&panes[..1], 0, "right"), None);
    }
    #[test]
    fn active_tile_is_always_visible_with_stable_order() {
        assert_eq!(visible(View::Focus, 4, 3), vec![3]);
        assert_eq!(visible(View::Split, 4, 3), vec![0, 3]);
        assert_eq!(visible(View::Grid, 4, 2), vec![0, 1, 2, 3]);
        assert_eq!(visible(View::Split, 0, 0), Vec::<usize>::new());
    }
    #[test]
    fn small_tiles_and_sidebar_overflow_are_refused() {
        assert!(check_size(View::Grid, 4, 164, 46, false).is_ok());
        assert!(check_size(View::Grid, 4, 120, 46, false).is_err());
        assert!(check_size(View::Grid, 4, 140, 46, true).is_err());
        assert!(check_size(View::Split, 2, 164, 20, true).is_ok());
    }
    #[test]
    fn sidebar_layout_uses_only_validated_private_ids() {
        let value = with_sidebar(
            164,
            47,
            "%7",
            &["%1".into(), "%2".into(), "%3".into(), "%4".into()],
        )
        .unwrap();
        assert!(value.contains("32x47,0,0,7"));
        assert!(value.contains("65x23,33,0,1"));
        assert!(with_sidebar(164, 47, "%7;kill-server", &["%1".into()]).is_err());
    }
}
