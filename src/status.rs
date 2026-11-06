//! A bounded native status line: input mode/attention before optional context.
use crate::{attention, commands, help, model::Run, observe};
use ratatui::text::Line;

fn width(text: &str) -> usize {
    Line::from(text).width()
}

fn clip(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        return text.into();
    }
    if columns == 0 {
        return String::new();
    }
    let mut result = String::new();
    for ch in text.chars() {
        result.push(ch);
        if width(&result) + 1 > columns {
            result.pop();
            break;
        }
    }
    result.push('…');
    result
}

/// Only these fixed, read-only destinations can be clicked in native status.
pub const NAVIGATION: &[(&str, &str)] = &[
    ("sessions", "Sessions"),
    ("palette", "Commands"),
    ("help", "Help"),
];

// tmux evaluates this per client when its key table changes, without a helper
// process. The option values contain already-escaped text and fixed styles.
pub const NAVIGATION_FORMAT: &str =
    "#{?client_prefix,#{E:@cockpit-prefix-hint},#{E:@cockpit-navigation}}";

pub fn prefix_hint(run: &Run, columns: usize) -> String {
    let sessions = commands::get("sessions").unwrap().keys[0];
    let palette = commands::get("palette").unwrap().keys[0];
    let help = commands::get("help").unwrap().keys[0];
    let full = format!(
        "Cockpit shortcuts ({}) · {sessions} Sessions · {palette} Commands · {help} Help · Esc Cancel",
        help::leader_label(&run.leader)
    );
    let compact = format!("Shortcuts · {sessions} Sessions · {help} Help · Esc Cancel");
    let text = [&full, &compact, "Shortcuts · Esc Cancel", "Esc Cancel"]
        .into_iter()
        .find(|text| width(text) <= columns)
        .unwrap_or("Esc Cancel");
    format!(
        "#[fg=black,bg=yellow,bold]{}#[default]",
        literal(&clip(text, columns))
    )
}

/// Native tmux mouse ranges; all dynamic text is escaped independently.
pub fn navigation(run: &Run, columns: usize) -> String {
    let full = NAVIGATION
        .iter()
        .map(|(_, label)| label.len() + 3)
        .sum::<usize>();
    let items = if columns >= full {
        NAVIGATION.to_vec()
    } else if columns >= 29 {
        vec![NAVIGATION[0], NAVIGATION[1], NAVIGATION[2]]
    } else {
        vec![NAVIGATION[0], NAVIGATION[2]]
    };
    let mut text = String::new();
    let mut used = 0;
    for (id, label) in items {
        let button = format!(" {label} ");
        if used + button.len() > columns {
            continue;
        }
        // IDs are fixed constants, never task names, paths or command input.
        text.push_str(&format!(
            "#[fg=black,bg=cyan,bold,range=user|cc-{id}]{button}#[default,norange]"
        ));
        used += button.len();
        if used < columns {
            text.push(' ');
            used += 1;
        }
    }
    let hint = format!(
        "{} · click to open",
        commands::shortcut(commands::get("help").unwrap(), &run.leader)
    );
    if columns.saturating_sub(used) >= width(&hint) {
        text.push_str(&literal(&hint));
    } else {
        let hint = format!("{} ?", help::leader_label(&run.leader));
        if columns.saturating_sub(used) >= width(&hint) {
            text.push_str(&literal(&hint));
        }
    }
    text
}

pub fn line(run: &Run, columns: usize) -> String {
    let disconnected = run
        .tiles
        .iter()
        .find(|tile| Some(&tile.target.key) == run.selected.as_ref())
        .is_some_and(|tile| tile.disconnected.is_some());
    let mode = if run.control && !disconnected {
        "Input enabled"
    } else {
        "Read only"
    };
    let essential = mode.to_owned();
    if width(&essential) >= columns {
        return clip(&essential, columns);
    }
    let help = format!(" | {} ?", run.leader);
    let help = if width(&essential) + width(&help) <= columns {
        help
    } else {
        String::new()
    };
    let budget = columns.saturating_sub(width(&essential) + width(&help));
    let target = run
        .targets
        .iter()
        .find(|t| Some(&t.key) == run.selected.as_ref())
        .or_else(|| {
            run.tiles
                .iter()
                .find(|t| Some(&t.target.key) == run.selected.as_ref())
                .map(|t| &t.target)
        });
    let context = if let Some(target) = target {
        let activity = if disconnected {
            "disconnected".into()
        } else {
            attention::activity(target, &run.attention)
                .split(" · ")
                .next()
                .unwrap_or("activity unknown")
                .to_owned()
        };
        let routine = format!("{} · activity unknown", target.session_name);
        if run.message.is_empty() || run.message == routine {
            format!("cc-cockpit · {} · {activity}", target.session_name)
        } else {
            format!("{} · {}", target.session_name, run.message)
        }
    } else {
        format!("cc-cockpit · {}", run.message)
    };
    let context = observe::sanitize(&context).replace(['\n', '\t'], " ");
    if budget >= 6 {
        format!("{essential} | {}{help}", clip(&context, budget - 3))
    } else {
        format!("{essential}{help}")
    }
}

/// Escape native tmux format/style introducers; labels are text, not programs.
pub fn literal(text: &str) -> String {
    text.replace('#', "##")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prefix_hint_uses_actual_leader_and_fits_small_windows() {
        let mut run = crate::navigation::fixture();
        run.leader = "C-a".into();
        assert!(prefix_hint(&run, 100).contains("Ctrl+a"));
        assert!(prefix_hint(&run, 100).contains("Space Commands"));
        for columns in [0, 5, 12, 24, 40, 80, 120] {
            let text = prefix_hint(&run, columns);
            let visible = text
                .strip_prefix("#[fg=black,bg=yellow,bold]")
                .unwrap()
                .strip_suffix("#[default]")
                .unwrap();
            assert!(width(visible) <= columns);
            if columns >= 12 {
                assert!(visible.contains("Esc Cancel"));
            }
        }
        assert!(NAVIGATION_FORMAT.contains("client_prefix"));
    }
    #[test]
    fn navigation_ranges_are_fixed_bounded_and_keep_sessions_and_help_on_small_screens() {
        let mut run = crate::navigation::fixture();
        run.leader = "C-a".into();
        run.message = "#{run-shell evil}".into();
        for columns in [0, 12, 24, 29, 40, 57, 58, 60, 120] {
            let text = navigation(&run, columns);
            let mut visible = text.clone();
            while let Some(start) = visible.find("#[") {
                let end = visible[start..].find(']').unwrap() + start;
                visible.replace_range(start..=end, "");
            }
            assert!(width(&visible) <= columns, "{columns}: {visible}");
            assert!(!text.contains("evil"));
            if columns >= 24 {
                for id in ["sessions", "help"] {
                    assert!(text.contains(&format!("range=user|cc-{id}")));
                }
            }
            if columns >= 29 {
                assert!(text.contains("range=user|cc-palette"));
            }
            if columns >= 58 {
                for (id, _) in NAVIGATION {
                    assert!(format!("cc-{id}").len() <= 15);
                    assert!(text.contains(&format!("range=user|cc-{id}")));
                    assert!(matches!(
                        commands::get(id).unwrap().dispatch,
                        commands::Dispatch::Panel(_)
                    ));
                }
            }
        }
        assert!(navigation(&run, 120).contains("Ctrl+a then ?"));
    }
    #[test]
    fn input_mode_fits_before_long_unicode_context() {
        let mut run = crate::navigation::fixture();
        run.control = true;
        run.message = "長いメッセージ ".repeat(100);
        for columns in [24, 32, 40, 60, 80, 120, 240] {
            let text = line(&run, columns);
            assert!(width(&text) <= columns, "{columns}: {text}");
            assert!(text.starts_with("Input enabled"));
        }
        run.control = false;
        assert!(line(&run, 80).starts_with("Read only"));
        assert_eq!(literal("#[fg=red]#{command}"), "##[fg=red]##{command}");
    }

    #[test]
    fn actual_leader_and_read_only_mode_are_visible() {
        let mut run = crate::navigation::fixture();
        run.leader = "C-a".into();
        assert!(line(&run, 80).starts_with("Read only"));
        assert!(line(&run, 80).contains("C-a ?"));
        assert!(line(&run, 40).contains("C-a ?"));
    }

    #[test]
    fn disconnected_pin_reports_view_and_messages_stay_on_one_line() {
        let mut run = crate::navigation::fixture();
        let target = crate::navigation::target("old", None, None);
        run.selected = Some(target.key.clone());
        run.tiles.push(crate::model::Tile {
            target,
            pane: "%8".into(),
            disconnected: Some("exited".into()),
            visible: true,
        });
        run.control = true;
        run.message = "line one\nline two\ttab\u{1b}".into();
        let text = line(&run, 120);
        assert!(text.starts_with("Read only"));
        assert!(!text.chars().any(char::is_control));
    }
}
