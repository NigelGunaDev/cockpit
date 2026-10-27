use std::{collections::HashSet, io, path::Path, time::Duration};

use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::{
    attention,
    interaction::{self, Hits},
    model::{AttentionItem, Run},
    observe, presentation, runtime,
    tmux::Tmux,
};

pub(crate) struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

#[derive(Default)]
struct Picker {
    query: String,
    searching: bool,
    cursor: usize,
    selected: Option<String>,
    lost_selection: bool,
    collapsed: HashSet<String>,
    group: Option<String>,
    notice: String,
    attention_first: bool,
    activity: attention::Activity,
}

impl Picker {
    fn action_key(
        &self,
        run: &Run,
        action: PickerAction,
        expected: Option<&str>,
    ) -> Result<String, String> {
        if self.group.is_some() {
            return Err(
                "Choose a terminal row first; project and task headings do not attach.".into(),
            );
        }
        let key = self
            .selected
            .as_deref()
            .ok_or("Choose a current terminal row first.")?;
        if expected.is_some_and(|expected| expected != key) {
            return Err("Selection changed. Choose the terminal again before opening it.".into());
        }
        let entry = self
            .filtered(run)
            .into_iter()
            .find(|entry| entry.target.key == key)
            .ok_or("This terminal left the current list. Select a live terminal explicitly.")?;
        if !entry.available() {
            return Err(
                "This terminal is disconnected or exited. Select a live terminal explicitly."
                    .into(),
            );
        }
        if action == PickerAction::Add
            && entry.tile.is_none()
            && run.tiles.len() >= crate::layout::MAX_TILES
        {
            return Err("Layout full (4 slots). Open terminal replaces the focused slot; adding needs a free slot.".into());
        }
        Ok(key.to_owned())
    }

    fn finish_search(&mut self) -> bool {
        let was_searching = self.searching;
        self.searching = false;
        was_searching
    }
    fn entries<'a>(&self, run: &'a Run) -> Vec<crate::navigation::Entry<'a>> {
        let mut entries = crate::navigation::entries(run, &self.query);
        entries.retain(|entry| self.activity.matches(entry.target, &run.attention));
        if self.attention_first {
            entries.sort_by_cached_key(|entry| {
                std::cmp::Reverse(attention::unresolved_for(entry.target, run))
            });
        }
        entries
    }
    fn filtered<'a>(&self, run: &'a Run) -> Vec<crate::navigation::Entry<'a>> {
        self.entries(run)
            .into_iter()
            .filter(|entry| {
                let (project, workspace) = group_ids(entry);
                self.attention_first
                    || !self.query.trim().is_empty()
                    || (!self.collapsed.contains(&project) && !self.collapsed.contains(&workspace))
            })
            .collect()
    }

    fn groups(&self, run: &Run) -> Vec<String> {
        let mut groups = vec![];
        if self.attention_first {
            return groups;
        }
        for entry in self.entries(run) {
            let (project, workspace) = group_ids(&entry);
            if !groups.contains(&project) {
                groups.push(project.clone());
            }
            if (!self.collapsed.contains(&project) || !self.query.trim().is_empty())
                && !groups.contains(&workspace)
            {
                groups.push(workspace);
            }
        }
        groups
    }

    fn cycle_group(&mut self, run: &Run, backwards: bool) {
        let groups = self.groups(run);
        if groups.is_empty() {
            return;
        }
        let next = match groups
            .iter()
            .position(|group| Some(group) == self.group.as_ref())
        {
            Some(index) if backwards => (index + groups.len() - 1) % groups.len(),
            Some(index) => (index + 1) % groups.len(),
            None => 0,
        };
        self.group = Some(groups[next].clone());
    }

    fn fold(&mut self, group: String, collapse: bool) {
        if !self.query.trim().is_empty() {
            self.notice =
                "Search keeps matching groups expanded. Clear the search to fold groups.".into();
            self.group = Some(group);
            return;
        }
        self.notice.clear();
        if collapse && self.collapsed.len() < 256 {
            self.collapsed.insert(group.clone());
        } else if !collapse {
            self.collapsed.remove(&group);
        }
        self.group = Some(group);
    }

    fn pick(&mut self, run: &Run, key: &str) {
        if let Some(index) = self
            .filtered(run)
            .iter()
            .position(|entry| entry.target.key == key)
        {
            self.cursor = index;
            self.remember(run);
        }
    }

    fn remember(&mut self, run: &Run) {
        self.selected = self
            .filtered(run)
            .get(self.cursor)
            .map(|entry| entry.target.key.clone());
        self.lost_selection = false;
        self.group = None;
        self.notice.clear();
    }

    fn reconcile(&mut self, run: &Run) {
        if let Some(key) = &self.selected {
            match self
                .filtered(run)
                .iter()
                .position(|entry| &entry.target.key == key)
            {
                Some(index) => self.cursor = index,
                None if !self
                    .entries(run)
                    .iter()
                    .any(|entry| &entry.target.key == key) =>
                {
                    self.selected = None;
                    self.group = None;
                    self.lost_selection = true;
                }
                None => {}
            }
        }
        self.cursor = self.cursor.min(self.filtered(run).len().saturating_sub(1));
    }

    fn move_cursor(&mut self, delta: isize, count: usize) {
        self.cursor = if count == 0 {
            0
        } else {
            self.cursor.saturating_add_signed(delta).min(count - 1)
        };
    }
}

#[derive(Clone)]
enum PickerHit {
    Search,
    Group(String),
    Terminal(String),
    Action(PickerAction, String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickerAction {
    Open,
    Add,
}
impl PickerAction {
    fn operation(self) -> &'static str {
        match self {
            Self::Open => "select",
            Self::Add => "add",
        }
    }
}

fn group_ids(entry: &crate::navigation::Entry<'_>) -> (String, String) {
    (
        serde_json::json!(["project", entry.project()]).to_string(),
        serde_json::json!(["workspace", entry.project(), entry.target.workspace]).to_string(),
    )
}

fn draw_picker(frame: &mut Frame, run: &Run, picker: &Picker, message: &str) -> Hits<PickerHit> {
    let mut hits = Hits::new(frame.area());
    let compact = frame.area().height < 20;
    let welcome = run.selected.is_none() && !compact;
    let areas = Layout::vertical([
        Constraint::Length(if welcome {
            4
        } else if compact {
            2
        } else {
            3
        }),
        Constraint::Min(1),
        Constraint::Length(if compact { 3 } else { 7 }),
    ])
    .split(crate::presentation::body(frame.area()));
    let title = if picker.searching {
        "Search — typing here"
    } else {
        "Sessions — / search; click selects"
    };
    let filter = format!(
        "{} · e {}",
        if picker.attention_first {
            "t attention first (loaded items)"
        } else {
            "t projects"
        },
        picker.activity.label()
    );
    let text = if welcome {
        format!(
            "{} · {filter}\nWelcome · {} then Space: commands; Enter focuses, a adds a tile",
            picker.query,
            crate::help::leader_label(&run.leader)
        )
    } else {
        format!("{} · {filter}", picker.query)
    };
    frame.render_widget(
        Paragraph::new(text).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(if picker.searching {
                    Color::Cyan
                } else {
                    Color::Gray
                })),
        ),
        areas[0],
    );
    hits.add(areas[0], PickerHit::Search);
    let entries = picker.entries(run);
    let mut items = vec![];
    let mut regions = vec![];
    let mut project = None;
    let mut workspace = None;
    let mut selected_row = None;
    let gap = presentation::list_gap(frame.area());
    let searching = !picker.query.trim().is_empty();
    for (index, entry) in entries.iter().enumerate() {
        let target = entry.target;
        let (project_key, workspace_key) = group_ids(entry);
        if !picker.attention_first && (index == 0 || project != Some(entry.project())) {
            let label = entry
                .project()
                .map(crate::navigation::name)
                .unwrap_or("Other sessions · no project");
            let folded = picker.collapsed.contains(&project_key) && !searching;
            if picker.group.as_ref() == Some(&project_key) {
                selected_row = Some(items.len());
            }
            items.push(ListItem::new(Line::styled(
                format!(
                    "{} {}",
                    if folded { "▸" } else { "▾" },
                    observe::sanitize(label)
                ),
                Style::default()
                    .fg(if picker.group.as_ref() == Some(&project_key) {
                        Color::Black
                    } else {
                        Color::LightCyan
                    })
                    .add_modifier(Modifier::BOLD),
            )));
            regions.push((1, 0, PickerHit::Group(project_key.clone())));
            project = Some(entry.project());
            workspace = None;
        }
        if !picker.attention_first && picker.collapsed.contains(&project_key) && !searching {
            continue;
        }
        if !picker.attention_first && workspace != Some(entry.workspace()) {
            let name = target
                .workspace
                .as_deref()
                .map(crate::navigation::name)
                .unwrap_or("Unassociated workspace");
            let label = if target.association == "ambiguous" {
                format!("{name} · task ambiguous")
            } else if let Some(task) = &target.task {
                if task == name {
                    task.clone()
                } else {
                    format!("{task} · {name}")
                }
            } else {
                format!("{name} · task unknown")
            };
            let folded = picker.collapsed.contains(&workspace_key) && !searching;
            if picker.group.as_ref() == Some(&workspace_key) {
                selected_row = Some(items.len());
            }
            items.push(ListItem::new(Line::styled(
                format!(
                    "  {} {}",
                    if folded { "▸" } else { "▾" },
                    observe::sanitize(&label)
                ),
                Style::default().fg(if picker.group.as_ref() == Some(&workspace_key) {
                    Color::Black
                } else {
                    Color::Gray
                }),
            )));
            regions.push((1, 0, PickerHit::Group(workspace_key.clone())));
            workspace = Some(entry.workspace());
        }
        if !picker.attention_first && picker.collapsed.contains(&workspace_key) && !searching {
            continue;
        }
        if picker.group.is_none() && picker.selected.as_deref() == Some(target.key.as_str()) {
            selected_row = Some(items.len());
        }
        let activity = if entry.disconnected {
            "Attachment disconnected".into()
        } else {
            attention::activity(target, &run.attention)
        };
        let highlighted = picker.group.is_none() && picker.selected.as_deref() == Some(&target.key);
        let secondary = Style::default().fg(if highlighted {
            Color::Black
        } else {
            presentation::MUTED
        });
        // Exact attention correlation checks process identity. Keep that work in
        // the existing attention-first view, not every ordinary sidebar redraw.
        let unresolved = if picker.attention_first {
            attention::unresolved_for(target, run)
        } else {
            0
        };
        let mut lines = vec![Line::from(vec![
            Span::styled(
                format!("  {}", observe::sanitize(&target.session_name)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", observe::sanitize(&target.pane_id)),
                secondary,
            ),
            Span::styled(
                format!(" · {}", crate::recovery::badge(run, target)),
                secondary,
            ),
        ])];
        let mode_style = if highlighted {
            secondary
        } else {
            Style::default().fg(if entry.disconnected || target.dead {
                Color::Red
            } else if run.control && run.selected.as_deref() == Some(&target.key) {
                Color::Green
            } else {
                presentation::MUTED
            })
        };
        let badges = entry.badges(run);
        if let Some((slot, mode)) = badges.rsplit_once(" · ") {
            if frame.area().width < 60 {
                lines.push(Line::styled(format!("  {slot}"), secondary));
                lines.push(Line::styled(format!("  {mode}"), mode_style));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {slot} · "), secondary),
                    Span::styled(mode.to_owned(), mode_style),
                ]));
            }
        } else {
            lines.push(Line::styled(format!("  {badges}"), mode_style));
        }
        if unresolved > 0 {
            lines.push(Line::styled(
                format!("  Attention: {unresolved} unresolved"),
                if highlighted {
                    secondary
                } else {
                    Style::default().fg(presentation::ATTENTION)
                },
            ));
        }
        if picker.attention_first {
            lines.push(Line::styled(
                format!(
                    "  {}",
                    observe::sanitize(target.project.as_deref().unwrap_or("no project"))
                ),
                secondary,
            ));
        }
        if frame.area().width >= 60 && !compact {
            lines.push(Line::styled(
                observe::sanitize(&format!("    {activity}")),
                secondary,
            ));
        }
        regions.push((
            lines.len() as u16,
            gap,
            PickerHit::Terminal(target.key.clone()),
        ));
        items.push(presentation::separated(
            lines,
            areas[1].width.saturating_sub(4),
            gap,
        ));
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(if picker.attention_first {
            "Attention-first terminals (loaded evidence)"
        } else {
            "Projects / workspaces / terminals"
        });
    let inner = block.inner(areas[1]);
    let mut state = ListState::default().with_selected(selected_row);
    frame.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(presentation::selected())
            .highlight_symbol("› ")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always),
        areas[1],
        &mut state,
    );
    hits.list_gaps(inner, state.offset(), &regions);
    let explicit_message = !message.is_empty()
        || !picker.notice.is_empty()
        || picker.lost_selection
        || entries.is_empty();
    let message = if !picker.notice.is_empty() {
        &picker.notice
    } else if !message.is_empty() {
        message
    } else if picker.lost_selection {
        "Highlighted terminal disappeared. Use arrows to choose a current terminal."
    } else if entries.is_empty() {
        "No matching sessions. Search or create a task with leader n; nothing starts automatically."
    } else {
        &run.message
    };
    let controls = if compact {
        "/ search · Esc close"
    } else if frame.area().width < 60 {
        "Open replaces · Add keeps slots\n/ search · t organize · e activity"
    } else if picker.searching {
        "Type filter · ↑/↓ select · Enter attach · Esc finish search"
    } else {
        "Click/↑↓ select · Open replaces focused slot; Add keeps other slots\n/ search · t organize · e activity · Tab groups · ←/→ fold · Esc close"
    };
    let stacked = frame.area().width < 48 && !compact;
    let action_rows = if stacked { 2 } else { 1 };
    let footer = Layout::vertical([
        Constraint::Length(action_rows),
        Constraint::Length(if compact { 1 } else { 2 }),
        Constraint::Min(0),
    ])
    .split(areas[2]);
    let labels = if compact && frame.area().width < 48 {
        ["Enter Open", "a Add"]
    } else {
        ["Enter Open terminal", "a Add to layout"]
    };
    let mut x = footer[0].x;
    for (index, action) in [PickerAction::Open, PickerAction::Add]
        .into_iter()
        .enumerate()
    {
        let target = picker.action_key(run, action, None).ok();
        let area = if stacked {
            ratatui::layout::Rect::new(footer[0].x, footer[0].y + index as u16, footer[0].width, 1)
        } else {
            ratatui::layout::Rect::new(
                x,
                footer[0].y,
                footer[0].right().saturating_sub(x),
                footer[0].height.min(1),
            )
        };
        if let Some(rect) = presentation::button(frame, area, labels[index], target.is_some())
            && let Some(key) = target
        {
            hits.add(rect, PickerHit::Action(action, key));
        }
        x = x.saturating_add(labels[index].len() as u16 + 3);
    }
    frame.render_widget(
        Paragraph::new(controls).wrap(Wrap { trim: false }),
        footer[1],
    );
    let source = picker
        .group
        .as_ref()
        .map(|key| format!("Group: {key}"))
        .unwrap_or_else(|| {
            entries
                .iter()
                .find(|entry| Some(&entry.target.key) == picker.selected.as_ref())
                .map(|entry| {
                    format!(
                        "Source: {} · {}",
                        entry.target.project.as_deref().unwrap_or("unassociated"),
                        entry.target.cwd
                    )
                })
                .unwrap_or_default()
        });
    let action_note = picker
        .action_key(run, PickerAction::Add, None)
        .err()
        .unwrap_or_else(|| {
            picker
                .selected
                .as_ref()
                .and_then(|key| entries.iter().find(|entry| &entry.target.key == key))
                .map(|entry| format!("For: {}", entry.target.session_name))
                .unwrap_or_default()
        });
    let outcome = if explicit_message {
        format!("{message}\n{action_note}\n{source}")
    } else {
        format!("{action_note}\n{message}\n{source}")
    };
    frame.render_widget(
        Paragraph::new(observe::sanitize(&outcome)).wrap(Wrap { trim: false }),
        footer[2],
    );
    hits
}

#[derive(Default)]
struct Inbox {
    selected: Option<String>,
    include_resolved: bool,
    confirming: bool,
    detail_offset: usize,
    query: String,
    searching: bool,
    project: Option<String>,
    kind: usize,
    receipt_filter: usize,
    initialized: bool,
}

impl Inbox {
    fn kind_label(&self) -> &'static str {
        [
            "all types",
            "permissions",
            "questions",
            "failures",
            "reviews",
            "other",
        ][self.kind]
    }
    fn receipt_label(&self) -> &'static str {
        if self.include_resolved {
            "all receipts"
        } else {
            ["unresolved", "unacknowledged", "unseen"][self.receipt_filter]
        }
    }
    fn kind_of(item: &AttentionItem) -> usize {
        match item.kind.as_str() {
            "permission_prompt" | "permission" => 1,
            "elicitation_dialog" | "question" => 2,
            "stop_hook_failed" | "failure" | "failed" | "error" => 3,
            "pr_open" | "review" | "ready_for_review" => 4,
            _ => 5,
        }
    }
    fn items<'a>(&self, run: &'a Run) -> Vec<&'a AttentionItem> {
        let mut items: Vec<_> = run
            .attention
            .iter()
            .flat_map(|inbox| &inbox.items)
            .filter(|item| self.include_resolved || item.resolved_at.is_none())
            .filter(|item| {
                self.project
                    .as_ref()
                    .is_none_or(|project| project == &item.project)
            })
            .filter(|item| self.kind == 0 || self.kind == Self::kind_of(item))
            .filter(|item| {
                self.include_resolved
                    || match self.receipt_filter {
                        1 => item.acknowledged_at.is_none(),
                        2 => item.seen_at.is_none(),
                        _ => true,
                    }
            })
            .filter(|item| {
                crate::navigation::match_score(
                    &self.query,
                    &[
                        &item.id,
                        &item.project,
                        &item.kind,
                        item.task.as_deref().unwrap_or(""),
                        item.workspace_root.as_deref().unwrap_or(""),
                        item.reason.as_deref().unwrap_or(""),
                        item.message.as_deref().unwrap_or(""),
                        item.session_id.as_deref().unwrap_or(""),
                    ],
                )
                .is_some()
            })
            .collect();
        items.sort_by(|a, b| {
            a.resolved_at
                .is_some()
                .cmp(&b.resolved_at.is_some())
                .then_with(|| {
                    a.acknowledged_at
                        .is_some()
                        .cmp(&b.acknowledged_at.is_some())
                })
                .then_with(|| b.ts.cmp(&a.ts))
                .then_with(|| (&a.project, &a.id).cmp(&(&b.project, &b.id)))
        });
        items
    }
    fn index(&self, run: &Run) -> usize {
        self.items(run)
            .iter()
            .position(|item| Some(attention::token(item)) == self.selected)
            .unwrap_or(0)
    }
    fn select(&mut self, run: &Run, delta: isize) {
        let items = self.items(run);
        if items.is_empty() {
            self.selected = None;
            return;
        }
        let next = self
            .index(run)
            .saturating_add_signed(delta)
            .min(items.len() - 1);
        self.selected = Some(attention::token(items[next]));
        self.detail_offset = 0;
        self.confirming = false;
    }
    fn reconcile(&mut self, run: &Run) {
        if !self.initialized {
            self.select(run, 0);
            self.initialized = true;
        } else if !self
            .items(run)
            .iter()
            .any(|item| Some(attention::token(item)) == self.selected)
        {
            self.selected = None;
            self.confirming = false;
            self.detail_offset = 0;
        }
    }
    fn next_unresolved(&mut self, run: &Run) {
        let items = self.items(run);
        let start = self
            .selected
            .as_ref()
            .and_then(|key| items.iter().position(|item| attention::token(item) == *key))
            .map(|i| i + 1)
            .unwrap_or(0);
        let selected = (0..items.len())
            .map(|i| items[(start + i) % items.len()])
            .find(|item| item.resolved_at.is_none())
            .map(attention::token);
        self.selected = selected;
        self.detail_offset = 0;
        self.confirming = false;
    }
    fn cycle_project(&mut self, run: &Run) {
        let mut projects: Vec<_> = run
            .attention
            .iter()
            .map(|page| page.project.clone())
            .collect();
        projects.sort();
        projects.dedup();
        self.project = match self
            .project
            .as_ref()
            .and_then(|key| projects.iter().position(|p| p == key))
        {
            Some(index) => projects.get(index + 1).cloned(),
            None => projects.first().cloned(),
        };
        self.selected = None;
        self.select(run, 0);
    }
}

#[derive(Clone)]
enum InboxHit {
    Search,
    Item(String),
    Details,
}

fn draw_attention(frame: &mut Frame, run: &Run, inbox: &Inbox, message: &str) -> Hits<InboxHit> {
    let mut hits = Hits::new(frame.area());
    let areas = Layout::vertical([
        Constraint::Length(4),
        Constraint::Percentage(40),
        Constraint::Min(3),
        Constraint::Length(6),
    ])
    .split(presentation::body(frame.area()));
    let items = inbox.items(run);
    let (unresolved, unacknowledged) = attention::counts(&run.attention);
    frame.render_widget(
        Paragraph::new(sanitize_history(&format!(
            "{} · {} · {} loaded matches\nProject: {} · p cycles; ] older / [ latest",
            if inbox.searching {
                format!("Search: {}", inbox.query)
            } else {
                format!("/ {} · t {}", inbox.query, inbox.kind_label())
            },
            inbox.receipt_label(),
            items.len(),
            inbox
                .project
                .as_deref()
                .unwrap_or("all projects (select one to page)")
        )))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Inbox filters"),
        ),
        areas[0],
    );
    hits.add(areas[0], InboxHit::Search);
    let list = items
        .iter()
        .map(|item| {
            ListItem::new(
                format!(
                    "[{}] {} · {} · {}",
                    if item.resolved_at.is_some() {
                        "resolved"
                    } else if item.acknowledged_at.is_some() {
                        "acknowledged; unresolved"
                    } else if item.seen_at.is_some() {
                        "seen; unresolved"
                    } else {
                        "new"
                    },
                    observe::sanitize(
                        item.task
                            .as_deref()
                            .or(item.workspace.as_deref())
                            .unwrap_or("session")
                    ),
                    observe::sanitize(&item.kind),
                    observe::sanitize(&item.ts)
                )
                .replace('\n', " "),
            )
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(
        items
            .iter()
            .position(|item| Some(attention::token(item)) == inbox.selected),
    );
    frame.render_stateful_widget(
        List::new(list)
            .block(Block::default().borders(Borders::ALL).title(format!(
                "Attention — {unresolved} unresolved / {unacknowledged} unacknowledged"
            )))
            .highlight_style(presentation::selected()),
        areas[1],
        &mut state,
    );
    hits.list(
        Block::default().borders(Borders::ALL).inner(areas[1]),
        state.offset(),
        &items
            .iter()
            .map(|item| (1, InboxHit::Item(attention::token(item))))
            .collect::<Vec<_>>(),
    );
    hits.add(areas[2], InboxHit::Details);
    let warnings = run.attention.iter().flat_map(|inbox| {
        let mut warnings = inbox.errors.iter().map(|error| format!("Warning: {}", error.get("message").and_then(|value| value.as_str()).unwrap_or("Observation incomplete"))).collect::<Vec<_>>();
        if inbox.next_before.is_some() { warnings.push(format!("{}: older items remain; p selects project, ] loads older.", inbox.project)); }
        if inbox.before.is_some() { warnings.push(format!("{}: older page; [ returns to latest. Counts cover the journal, filters cover loaded pages.", inbox.project)); }
        warnings
    }).collect::<Vec<_>>().join("\n");
    let detail = match items
        .iter()
        .find(|item| Some(attention::token(item)) == inbox.selected)
    {
        Some(item) => format!(
            "{}\nProject: {}\nTask: {} · Claude: {}\nEvidence: {} · {}\n{}\n\n{}\n\nSeen: {}\nAcknowledged: {}\nMarked resolved: {}\n{}",
            item.reason.as_deref().unwrap_or(&item.kind),
            item.project,
            item.task.as_deref().unwrap_or("unassociated"),
            item.session_id.as_deref().unwrap_or("unknown"),
            item.source.as_deref().unwrap_or("legacy needs-you record"),
            item.ts,
            item.id,
            item.message.as_deref().unwrap_or(""),
            item.seen_at.as_deref().unwrap_or("no"),
            item.acknowledged_at.as_deref().unwrap_or("no"),
            item.resolved_at.as_deref().unwrap_or("no"),
            format_args!(
                "{}\n{warnings}",
                match attention::matching_target(item, run) {
                    Ok(target) => format!(
                        "o opens {} VIEW ONLY. Seen/acknowledged/resolved are receipts, not an answer or approval.",
                        target.session_name
                    ),
                    Err(error) =>
                        format!("Terminal unavailable: {error}. Item stays in the inbox."),
                }
            )
        ),
        None => format!(
            "{}\n{warnings}\n\nStatus requires observational hooks; session presence alone means activity unknown.",
            if items.is_empty() {
                "No items in this view. Change filters, include history with h, or load another page."
            } else {
                "Choose an item with arrows, n, or a click. / searches; h includes resolved history."
            }
        ),
    };
    frame.render_widget(
        Paragraph::new(sanitize_history(&detail))
            .wrap(Wrap { trim: false })
            .scroll((inbox.detail_offset.min(u16::MAX as usize) as u16, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Evidence and receipts"),
            ),
        areas[2],
    );
    let footer = if inbox.confirming {
        "Mark this item resolved? Enter confirms; any other key cancels. This does not answer Claude or approve work."
    } else {
        "↑/↓ j/k select · n next unresolved · / search · t type · v receipts · p project · ] older · [ latest\nEnter seen · a acknowledge · r resolve · u reopen · o view terminal · h history · f refresh · PgUp/Dn · Esc close"
    };
    frame.render_widget(
        Paragraph::new(format!("{footer}\n{}", sanitize_history(message)))
            .wrap(Wrap { trim: false }),
        areas[3],
    );
    hits
}

/// History is plain panel text, never a live terminal stream. Strip control
/// sequences so captured text cannot invoke host-terminal behavior.
pub(crate) fn sanitize_history(text: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Plain,
        Escape,
        Csi,
        String,
        StringEscape,
    }
    let mut state = State::Plain;
    let mut result = String::with_capacity(text.len());
    for ch in text.chars() {
        match state {
            State::Plain => match ch {
                '\u{1b}' => state = State::Escape,
                '\n' | '\t' => result.push(ch),
                ch if !ch.is_control() => result.push(ch),
                _ => {}
            },
            State::Escape => {
                state = match ch {
                    '[' => State::Csi,
                    ']' | 'P' | '^' | '_' => State::String,
                    _ => State::Plain,
                }
            }
            State::Csi => {
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    state = State::Plain;
                }
            }
            State::String => match ch {
                '\u{7}' => state = State::Plain,
                '\u{1b}' => state = State::StringEscape,
                _ => {}
            },
            State::StringEscape => {
                state = if ch == '\\' {
                    State::Plain
                } else {
                    State::String
                }
            }
        }
    }
    result
}

fn history_capture(run: &Run) -> Result<String, String> {
    let target = run
        .targets
        .iter()
        .find(|target| Some(target.key.as_str()) == run.selected.as_deref())
        .ok_or_else(|| {
            "Selected target is disconnected; select a live session first.".to_string()
        })?;
    observe::capture_history(target).map(|text| sanitize_history(&text))
}

fn draw_history(frame: &mut Frame, text: &str, offset: usize, message: &str) {
    let areas = Layout::vertical([Constraint::Min(1), Constraint::Length(2)])
        .split(crate::presentation::body(frame.area()));
    let visible = text
        .lines()
        .skip(offset)
        .take(areas[0].height.saturating_sub(2) as usize)
        .collect::<Vec<_>>()
        .join("\n");
    frame.render_widget(
        Paragraph::new(visible).block(
            Block::default()
                .borders(Borders::ALL)
                .title("History — independent bounded snapshot"),
        ),
        areas[0],
    );
    let footer = if message.is_empty() {
        "↑/↓ PgUp/PgDn Home/End · r: recapture · c: copy visible · Esc: close"
    } else {
        message
    };
    frame.render_widget(Paragraph::new(footer), areas[1]);
}

pub fn run(role: &str, path: &Path) -> Result<(), String> {
    if role == "session-memory" {
        return crate::recovery::panel(path);
    }
    if role == "palette" {
        return match crate::palette::panel(path)? {
            Some(role) => run(role, path),
            None => Ok(()),
        };
    }
    if role == "saved-history" {
        return crate::history::panel(path);
    }
    if role == "changes" {
        return crate::changes::panel(path);
    }
    if role == "review" {
        return crate::review::panel(path, None);
    }
    if role == "queue" || role == "create" {
        return crate::queue::run(role, path);
    }
    let mut observed = runtime::read_run(path)?;
    let history = role == "history";
    let disconnected = role == "disconnected";
    let attention_panel = role == "attention";
    let help_panel = role == "help";
    let mut help = crate::help::Help::default();
    let mut chrome = crate::presentation::Chrome::default();
    let mut inbox = Inbox::default();
    // History captures stay bound to the original tile while other tiles focus.
    let history_run = observed.clone();
    let sidebar = role == "sidebar";
    let mut picker = Picker::default();
    if let Some(index) = picker
        .filtered(&observed)
        .iter()
        .position(|entry| Some(entry.target.key.as_str()) == observed.selected.as_deref())
    {
        picker.cursor = index;
    }
    picker.remember(&observed);
    let mut message = String::new();
    let mut captured = if history {
        match history_capture(&history_run) {
            Ok(text) => text,
            Err(error) => {
                message = error;
                String::new()
            }
        }
    } else {
        String::new()
    };
    let mut inbox_hits = Hits::new(ratatui::layout::Rect::default());
    let mut picker_hits = Hits::new(ratatui::layout::Rect::default());
    let mut offset = captured.lines().count().saturating_sub(20);
    let _restore = Restore;
    enable_raw_mode().map_err(|error| error.to_string())?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)
        .map_err(|error| error.to_string())?;
    if attention_panel {
        execute!(io::stdout(), EnableBracketedPaste).map_err(|e| e.to_string())?;
    }
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|error| error.to_string())?;
    terminal.clear().map_err(|error| error.to_string())?;
    loop {
        match runtime::read_run(path) {
            Ok(run) => observed = run,
            Err(error) => message = format!("Observation unavailable: {error}"),
        }
        let rows = terminal
            .size()
            .map_err(|error| error.to_string())?
            .height
            .saturating_sub(if history { 5 } else { 4 }) as usize;
        if history {
            offset = offset.min(captured.lines().count().saturating_sub(rows));
        } else if attention_panel {
            let selected = inbox.selected.clone();
            inbox.reconcile(&observed);
            if selected.is_some() && inbox.selected.is_none() {
                message = format!(
                    "{}\nSelection changed or left this view. Choose an item explicitly before another action.",
                    message.lines().next().unwrap_or("")
                );
            }
        } else {
            picker.reconcile(&observed);
        }
        terminal
            .draw(|frame| {
                if disconnected {
                    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
                    let tile = observed.tiles.iter().find(|tile| tile.pane == pane);
                    let text = tile.map(|tile| format!("{}\n\nDisconnected: {}\n\nSelect a current session with leader + s.\nThis tile never follows a reused session name.", tile.target.session_name, tile.disconnected.as_deref().unwrap_or("Attachment exited")))
                        .unwrap_or_else(|| "Disconnected attachment. Select a current session.".into());
                    frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), frame.area());
                } else if help_panel {
                    help.draw(frame, &observed.leader);
                } else if attention_panel {
                    inbox_hits = draw_attention(frame, &observed, &inbox, &message);
                } else if history {
                    draw_history(frame, &captured, offset, &message);
                } else {
                    picker_hits = draw_picker(frame, &observed, &picker, &message);
                }
                if !disconnected {
                    chrome.draw(frame, if help_panel { "Cockpit controls · Help" } else if attention_panel { "Attention" } else if history { "Terminal history · Read only" } else { "Sessions" });
                }
            })
            .map_err(|error| error.to_string())?;
        if !event::poll(Duration::from_millis(200)).map_err(|error| error.to_string())? {
            continue;
        }
        let input = event::read().map_err(|error| error.to_string())?;
        let size = terminal.size().map_err(|e| e.to_string())?;
        if !disconnected
            && chrome.closing(
                &input,
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
            )
        {
            if sidebar {
                runtime::request(path, "sidebar", None)?;
            }
            return Ok(());
        }
        if let Event::Mouse(mouse) = input {
            if attention_panel {
                let size = terminal.size().map_err(|e| e.to_string())?;
                let current = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                if inbox_hits.frame != current {
                    continue;
                }
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    inbox.confirming = false;
                    match inbox_hits.get(mouse, current) {
                        Some(InboxHit::Item(key)) => {
                            inbox.selected = Some(key.clone());
                            inbox.detail_offset = 0;
                            inbox.searching = false;
                        }
                        Some(InboxHit::Search) => inbox.searching = true,
                        _ => {}
                    }
                } else if let Some(delta) = interaction::wheel(mouse.kind) {
                    inbox.confirming = false;
                    if matches!(inbox_hits.get(mouse, current), Some(InboxHit::Details)) {
                        inbox.detail_offset = inbox.detail_offset.saturating_add_signed(delta);
                    } else {
                        inbox.select(&observed, delta);
                    }
                }
            } else if !history && !help_panel && !disconnected {
                let size = terminal.size().map_err(|e| e.to_string())?;
                let current = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                if picker_hits.frame != current {
                    continue;
                }
                if let Some(delta) = interaction::wheel(mouse.kind) {
                    picker.move_cursor(delta, picker.filtered(&observed).len());
                    picker.remember(&observed);
                } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    match picker_hits.get(mouse, current) {
                        Some(PickerHit::Search) => {
                            picker.searching = true;
                            picker.group = None;
                        }
                        Some(PickerHit::Group(key)) => {
                            picker.fold(key.clone(), !picker.collapsed.contains(key))
                        }
                        Some(PickerHit::Terminal(key)) => picker.pick(&observed, key),
                        Some(PickerHit::Action(action, key)) => {
                            match picker
                                .action_key(&observed, *action, Some(key))
                                .and_then(|key| {
                                    runtime::request(path, action.operation(), Some(&key))
                                }) {
                                Ok(()) if role == "sessions" => return Ok(()),
                                Ok(()) => message.clear(),
                                Err(error) => message = error,
                            }
                        }
                        None => {}
                    }
                }
            } else if let Some(delta) = interaction::wheel(mouse.kind) {
                if history {
                    offset = offset.saturating_add_signed(delta);
                }
                if help_panel {
                    help.key(
                        if delta < 0 {
                            KeyCode::Up
                        } else {
                            KeyCode::Down
                        },
                        rows as u16,
                    );
                }
            }
            continue;
        }
        if let Event::Paste(text) = &input {
            if attention_panel {
                inbox.confirming = false;
            }
            if attention_panel && inbox.searching {
                for ch in text.chars().filter(|ch| !ch.is_control()) {
                    if inbox.query.chars().count() >= 256 {
                        break;
                    }
                    inbox.query.push(ch);
                }
                inbox.select(&observed, 0);
            }
            continue;
        }
        let Event::Key(key) = input else {
            continue;
        };
        if disconnected {
            continue;
        }
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if attention_panel && inbox.searching {
            match key.code {
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    inbox.query.clear();
                    inbox.select(&observed, 0);
                }
                KeyCode::Esc | KeyCode::Enter => inbox.searching = false,
                KeyCode::Backspace => {
                    inbox.query.pop();
                    inbox.select(&observed, 0);
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if inbox.query.chars().count() < 256 {
                        inbox.query.push(ch);
                    }
                    inbox.select(&observed, 0);
                }
                _ => {}
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                return Ok(());
            }
            continue;
        }
        if key.code == KeyCode::Esc
            && !history
            && !attention_panel
            && !help_panel
            && picker.finish_search()
        {
            continue;
        }
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
        {
            if sidebar {
                runtime::request(path, "sidebar", None)?;
            }
            return Ok(());
        }
        if help_panel {
            help.key(
                key.code,
                rows.saturating_add(4).min(u16::MAX as usize) as u16,
            );
            continue;
        }
        if attention_panel {
            if !inbox.confirming && matches!(key.code, KeyCode::Char(']') | KeyCode::Char('[')) {
                message = match inbox.project.as_ref().and_then(|project| {
                    observed
                        .attention
                        .iter()
                        .find(|page| &page.project == project)
                }) {
                    None => {
                        "Select a project with p before paging. Cursors belong to that project."
                            .into()
                    }
                    Some(page) if key.code == KeyCode::Char(']') && page.next_before.is_none() => {
                        "No older page in this project's bounded journal window.".into()
                    }
                    Some(page) => {
                        let before = if key.code == KeyCode::Char(']') {
                            page.next_before.clone()
                        } else {
                            None
                        };
                        let payload = serde_json::json!([page.project, before]).to_string();
                        match runtime::request(path, "attention-page", Some(&payload)) {
                            Ok(()) => {
                                inbox.selected = None;
                                inbox.confirming = false;
                                "Page loaded. n selects an unresolved item; filters apply to loaded pages only.".into()
                            }
                            Err(error) => error,
                        }
                    }
                };
                continue;
            }
            let operation = if inbox.confirming {
                inbox.confirming = false;
                (key.code == KeyCode::Enter).then_some("attention-resolve")
            } else {
                match key.code {
                    KeyCode::Char('/') => {
                        inbox.searching = true;
                        None
                    }
                    KeyCode::Char('n') => {
                        inbox.next_unresolved(&observed);
                        None
                    }
                    KeyCode::Char('p') => {
                        inbox.cycle_project(&observed);
                        None
                    }
                    KeyCode::Char('t') => {
                        inbox.kind = (inbox.kind + 1) % 6;
                        inbox.select(&observed, 0);
                        None
                    }
                    KeyCode::Char('v') => {
                        inbox.include_resolved = false;
                        inbox.receipt_filter = (inbox.receipt_filter + 1) % 3;
                        inbox.select(&observed, 0);
                        None
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        inbox.select(&observed, -1);
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        inbox.select(&observed, 1);
                        None
                    }
                    KeyCode::PageUp => {
                        inbox.detail_offset = inbox.detail_offset.saturating_sub(rows.max(1));
                        None
                    }
                    KeyCode::PageDown => {
                        inbox.detail_offset = inbox.detail_offset.saturating_add(rows.max(1));
                        None
                    }
                    KeyCode::Char('h') => {
                        inbox.include_resolved = !inbox.include_resolved;
                        inbox.receipt_filter = 0;
                        inbox.select(&observed, 0);
                        None
                    }
                    KeyCode::Char('r') => {
                        inbox.confirming = inbox.selected.is_some();
                        None
                    }
                    KeyCode::Enter => Some("attention-seen"),
                    KeyCode::Char('a') => Some("attention-acknowledge"),
                    KeyCode::Char('u') => Some("attention-reopen"),
                    KeyCode::Char('o') => Some("attention-open"),
                    KeyCode::Char('f') => Some("attention-refresh"),
                    _ => None,
                }
            };
            if let Some(op) = operation {
                if op != "attention-refresh" && inbox.selected.is_none() {
                    message = "Choose a visible item with arrows, n, or a click before recording a receipt or opening its terminal.".into();
                    continue;
                }
                message = match runtime::request(path, op, inbox.selected.as_deref()) {
                    Ok(()) => {
                        if op == "attention-open" {
                            return Ok(());
                        }
                        if op == "attention-refresh" {
                            "Inbox refreshed.".into()
                        } else {
                            match op {
                                "attention-seen" => "Seen recorded; still unresolved. No answer or approval sent.",
                                "attention-acknowledge" => "Acknowledged; still unresolved. No answer or approval sent.",
                                "attention-resolve" => "Marked resolved by you; no Claude answer, permission approval or task decision sent.",
                                _ => "Reopened; unresolved again. No terminal input sent.",
                            }.into()
                        }
                    }
                    Err(error) => error,
                };
            }
            continue;
        }
        if history {
            let maximum = captured.lines().count().saturating_sub(rows);
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => offset = offset.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    offset = offset.saturating_add(1).min(maximum)
                }
                KeyCode::PageUp => offset = offset.saturating_sub(rows.max(1)),
                KeyCode::PageDown => offset = offset.saturating_add(rows.max(1)).min(maximum),
                KeyCode::Home => offset = 0,
                KeyCode::End => offset = maximum,
                KeyCode::Char('r') => match history_capture(&history_run) {
                    Ok(text) => {
                        captured = text;
                        message.clear();
                    }
                    Err(error) => message = error,
                },
                KeyCode::Char('c') => {
                    let visible = captured
                        .lines()
                        .skip(offset)
                        .take(rows)
                        .collect::<Vec<_>>()
                        .join("\n");
                    message = match Tmux::new(&observed.ui_socket).run(&["set-buffer", "-w", "--", &visible]) {
                        Ok(_) => "Copy requested through private tmux; clipboard support depends on your terminal.".to_string(),
                        Err(error) => format!("Copy failed: {error}"),
                    };
                }
                _ => {}
            }
            continue;
        }
        if !picker.searching {
            match key.code {
                KeyCode::Char('t') => {
                    picker.attention_first = !picker.attention_first;
                    picker.group = None;
                    picker.reconcile(&observed);
                    continue;
                }
                KeyCode::Char('e') => {
                    picker.activity = picker.activity.next();
                    picker.cursor = 0;
                    picker.remember(&observed);
                    continue;
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    picker.cycle_group(&observed, key.code == KeyCode::BackTab);
                    continue;
                }
                KeyCode::Left | KeyCode::Right => {
                    let group = picker.group.clone().or_else(|| {
                        picker
                            .filtered(&observed)
                            .iter()
                            .find(|entry| Some(&entry.target.key) == picker.selected.as_ref())
                            .map(|entry| group_ids(entry).1)
                    });
                    if let Some(group) = group {
                        picker.fold(group, key.code == KeyCode::Left);
                    }
                    continue;
                }
                KeyCode::Enter if picker.group.is_some() => {
                    let group = picker.group.clone().unwrap();
                    picker.fold(group.clone(), !picker.collapsed.contains(&group));
                    continue;
                }
                _ => {}
            }
        }
        if !picker.searching && picker.group.is_some() && key.code == KeyCode::Char('a') {
            message = "Choose a terminal row before adding a tile; group selection never attaches."
                .into();
            continue;
        }
        if key.code == KeyCode::Enter || (!picker.searching && key.code == KeyCode::Char('a')) {
            let action = if key.code == KeyCode::Char('a') {
                PickerAction::Add
            } else {
                PickerAction::Open
            };
            match picker
                .action_key(&observed, action, None)
                .and_then(|key| runtime::request(path, action.operation(), Some(&key)))
            {
                Ok(()) if role == "sessions" => return Ok(()),
                Ok(()) => message.clear(),
                Err(error) => message = error,
            }
        } else if picker.searching {
            match key.code {
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.cursor = 0;
                    picker.remember(&observed);
                    message.clear();
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if picker.query.chars().count() < 256 {
                        picker.query.push(ch);
                    }
                    picker.cursor = 0;
                    picker.remember(&observed);
                    message.clear();
                }
                KeyCode::Up => {
                    picker.move_cursor(-1, picker.filtered(&observed).len());
                    picker.remember(&observed);
                    message.clear();
                }
                KeyCode::Down => {
                    picker.move_cursor(1, picker.filtered(&observed).len());
                    picker.remember(&observed);
                    message.clear();
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char('/') => {
                    picker.searching = true;
                    message.clear();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    picker.move_cursor(-1, picker.filtered(&observed).len());
                    picker.remember(&observed);
                    message.clear();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    picker.move_cursor(1, picker.filtered(&observed).len());
                    picker.remember(&observed);
                    message.clear();
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Target;
    use ratatui::backend::TestBackend;

    #[test]
    fn picker_actions_refuse_headings_replaced_targets_and_exited_sessions() {
        let mut run = picker_fixture();
        let mut picker = Picker::default();
        picker.remember(&run);
        let expected = picker.selected.clone().unwrap();
        assert_eq!(
            picker
                .action_key(&run, PickerAction::Open, Some(&expected))
                .unwrap(),
            expected
        );
        picker.group = Some("project heading".into());
        assert!(
            picker
                .action_key(&run, PickerAction::Open, Some(&expected))
                .is_err()
        );
        picker.group = None;
        picker.pick(&run, "second-generation");
        assert!(
            picker
                .action_key(&run, PickerAction::Open, Some(&expected))
                .is_err()
        );
        picker.pick(&run, &expected);
        run.targets[0].dead = true;
        assert!(
            picker
                .action_key(&run, PickerAction::Open, Some(&expected))
                .is_err()
        );
        run.targets[0].dead = false;
        run.targets[0].key = "same-name-replacement".into();
        assert!(
            picker
                .action_key(&run, PickerAction::Open, Some(&expected))
                .is_err()
        );
    }

    #[test]
    fn full_layout_disables_add_but_allows_open_and_existing_slots() {
        let mut run = picker_fixture();
        let mut picker = Picker::default();
        picker.remember(&run);
        for index in 0..crate::layout::MAX_TILES {
            let mut target = run.targets[0].clone();
            target.key = format!("pinned-{index}");
            run.tiles.push(crate::model::Tile {
                target,
                pane: format!("%{index}"),
                disconnected: None,
                visible: true,
            });
        }
        assert!(picker.action_key(&run, PickerAction::Open, None).is_ok());
        assert!(
            picker
                .action_key(&run, PickerAction::Add, None)
                .unwrap_err()
                .contains("Layout full")
        );
        run.tiles[0].target = run.targets[0].clone();
        assert!(picker.action_key(&run, PickerAction::Add, None).is_ok());
    }

    #[test]
    fn folded_groups_use_full_paths_and_do_not_retarget_the_selected_task() {
        let mut run = picker_fixture();
        run.targets[0].project = Some("/a/project".into());
        run.targets[0].workspace = Some("/a/project/task".into());
        run.targets[1].project = Some("/b/project".into());
        run.targets[1].workspace = Some("/b/project/task".into());
        let mut picker = Picker::default();
        picker.remember(&run);
        let original = picker.selected.clone();
        let group = group_ids(&picker.filtered(&run)[0]).0;
        picker.fold(group.clone(), true);
        picker.reconcile(&run);
        assert_eq!(picker.selected, original);
        assert!(!picker.lost_selection);
        assert_eq!(picker.filtered(&run).len(), 1);
        assert_eq!(
            picker.filtered(&run)[0].target.project.as_deref(),
            Some("/b/project")
        );
        assert_eq!(picker.group.as_ref(), Some(&group));
        picker.fold(group, false);
        assert_eq!(picker.filtered(&run).len(), 2);
        assert_eq!(picker.selected, original);
    }

    #[test]
    fn rendered_terminal_hit_rows_match_the_visible_labels_after_list_scroll() {
        use crossterm::event::MouseEvent;
        let run = picker_fixture();
        for height in [12, 18, 30] {
            let mut picker = Picker {
                cursor: 1,
                ..Default::default()
            };
            picker.remember(&run);
            let mut terminal = Terminal::new(TestBackend::new(80, height)).unwrap();
            let mut hits = Hits::new(ratatui::layout::Rect::default());
            terminal
                .draw(|frame| {
                    hits = draw_picker(frame, &run, &picker, "error ".repeat(80).as_str());
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..height {
                let line: String = (0..80).map(|x| buffer[(x, y)].symbol()).collect();
                for target in &run.targets {
                    if line.contains(&format!("{} {}", target.session_name, target.pane_id)) {
                        let mouse = MouseEvent {
                            kind: MouseEventKind::Down(MouseButton::Left),
                            column: 4,
                            row: y,
                            modifiers: KeyModifiers::NONE,
                        };
                        assert!(
                            matches!(hits.get(mouse, hits.frame), Some(PickerHit::Terminal(key)) if key == &target.key)
                        );
                    }
                }
            }
            let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
            assert!(text.contains("Enter"));
        }
    }

    fn picker_fixture() -> Run {
        let target = |name: &str, key: &str, task: &str| Target {
            key: key.into(),
            socket: "/tmp/fixture-original".into(),
            server_pid: 1,
            server_start: "1".into(),
            session_id: "$1".into(),
            session_created: "1".into(),
            session_name: name.into(),
            window_id: "@1".into(),
            pane_id: "%1".into(),
            pane_pid: 2,
            pane_start: "2".into(),
            cwd: "/project".into(),
            dead: false,
            project: Some("/project".into()),
            workspace: Some("/project/task".into()),
            task: Some(task.into()),
            claude_session_id: None,
            association: "matched".into(),
        };
        Run {
            ui_socket: "/tmp/fixture-ui".into(),
            supervisor_pid: 1,
            supervisor_start: "1".into(),
            ui_server_pid: 3,
            ui_server_start: "3".into(),
            original_socket: "/tmp/fixture-original".into(),
            executable: "fixture".into(),
            leader: "C-]".into(),
            projects: vec!["/project".into()],
            targets: vec![
                target("alpha", "first-generation", "CC-1"),
                target("beta", "second-generation", "CC-2"),
            ],
            selected: Some("first-generation".into()),
            terminal_pane: None,
            sidebar_pane: None,
            control: true,
            tiles: vec![],
            view: crate::model::View::Focus,
            preferences: None,
            attention: vec![],
            tasks: vec![],
            task_warnings: vec![],
            recent: vec![],
            recovery: Default::default(),
            message: String::new(),
        }
    }

    #[test]
    fn filtered_selection_returns_exact_target_identity() {
        let run = picker_fixture();
        let picker = Picker {
            query: "cc-2".into(),
            ..Picker::default()
        };
        let indices = picker.filtered(&run);
        assert_eq!(indices.len(), 1);
        assert_eq!(indices[picker.cursor].target.key, "second-generation");
        let mut terminal = Terminal::new(TestBackend::new(64, 12)).unwrap();
        terminal
            .draw(|frame| {
                draw_picker(frame, &run, &picker, "");
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("beta"));
        assert!(!text.contains("alpha"));
    }

    #[test]
    fn navigation_clamps_empty_and_boundaries() {
        let mut picker = Picker::default();
        picker.move_cursor(-1, 4);
        assert_eq!(picker.cursor, 0);
        picker.move_cursor(9, 4);
        assert_eq!(picker.cursor, 3);
        picker.move_cursor(-1, 4);
        assert_eq!(picker.cursor, 2);
        picker.move_cursor(1, 0);
        assert_eq!(picker.cursor, 0);
    }

    #[test]
    fn refresh_keeps_identity_and_requires_selection_after_disappearance() {
        let mut run = picker_fixture();
        let mut picker = Picker::default();
        picker.remember(&run);
        let key = picker.selected.clone().unwrap();
        let mut added = run.targets[0].clone();
        added.key = "inserted-generation".into();
        added.project = Some("/aaa".into());
        run.targets.insert(0, added);
        picker.reconcile(&run);
        assert_eq!(picker.selected.as_deref(), Some(key.as_str()));
        assert_eq!(picker.filtered(&run)[picker.cursor].target.key, key);
        // Same labels with a new identity must not inherit the highlight.
        let old = run.targets.iter_mut().find(|t| t.key == key).unwrap();
        old.key = "replacement-generation".into();
        picker.reconcile(&run);
        assert!(picker.selected.is_none());
        assert!(picker.lost_selection);
        picker.reconcile(&run);
        assert!(picker.selected.is_none());
        picker.move_cursor(1, picker.filtered(&run).len());
        picker.remember(&run);
        assert!(picker.selected.is_some());
        assert!(!picker.lost_selection);
    }

    #[test]
    fn narrow_sidebar_shows_terminal_and_input_badges() {
        let mut run = picker_fixture();
        let target = run.targets[0].clone();
        run.tiles.push(crate::model::Tile {
            target,
            pane: "%8".into(),
            disconnected: None,
            visible: true,
        });
        let mut picker = Picker::default();
        picker.remember(&run);
        let mut terminal = Terminal::new(TestBackend::new(32, 24)).unwrap();
        terminal
            .draw(|frame| {
                draw_picker(frame, &run, &picker, "");
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("alpha"));
        assert!(text.contains("Selected · Slot 1"));
        assert!(text.contains("Input enabled"));
        assert!(text.contains("Enter Open terminal"));
        assert!(text.contains('›'));
    }

    #[test]
    fn history_end_remains_visible_below_the_shared_panel_header() {
        let content = (0..60)
            .map(|i| format!("line-{i:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        for height in [12_u16, 24, 40] {
            let mut terminal = Terminal::new(TestBackend::new(80, height)).unwrap();
            let offset = content
                .lines()
                .count()
                .saturating_sub(usize::from(height.saturating_sub(5)));
            let mut chrome = presentation::Chrome::default();
            terminal
                .draw(|frame| {
                    draw_history(frame, &content, offset, "");
                    chrome.draw(frame, "Terminal history · Read only");
                })
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("line-59"), "height {height}");
            assert!(text.contains("Esc Close"));
        }
    }

    #[test]
    fn ending_search_keeps_filtered_target_and_add_instruction_visible() {
        let run = picker_fixture();
        let mut picker = Picker {
            query: "cc-2".into(),
            searching: true,
            cursor: 0,
            ..Picker::default()
        };
        assert!(picker.finish_search());
        assert!(!picker.finish_search());
        assert_eq!(picker.query, "cc-2");
        assert_eq!(picker.filtered(&run)[0].target.key, "second-generation");
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).unwrap();
        terminal
            .draw(|frame| {
                draw_picker(frame, &run, &picker, "Selected terminal message");
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("a Add to layout"));
        assert!(text.contains("Selected terminal message"));
        assert!(text.contains("beta"));
        assert!(!text.contains("alpha"));
    }

    #[test]
    fn captured_controls_do_not_reach_the_host_terminal() {
        let text = "wide 界\n\x1b[31mred\x1b[0m\x1b]52;c;ZXhlYw==\x07\r\tend\0";
        assert_eq!(sanitize_history(text), "wide 界\nred\tend");
        assert_eq!(
            sanitize_history("hello\x1bPsecret\x1b\\world"),
            "helloworld"
        );
    }

    #[test]
    fn independent_history_draws_the_selected_offset() {
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal
            .draw(|frame| draw_history(frame, "old\nmiddle\nlatest", 1, "fixture"))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("middle"));
        assert!(text.contains("latest"));
        assert!(!text.contains("old"));
        assert!(text.contains("fixture"));
    }

    #[test]
    fn inbox_selection_survives_new_items_and_acknowledgement_is_unresolved() {
        let mut run = picker_fixture();
        let item = |id: &str, ts: &str, resolved: Option<&str>| {
            serde_json::from_value::<AttentionItem>(serde_json::json!({
                "id":id,"project":"/project","evidence_hash":"hash","ts":ts,"kind":"question",
                "task":null,"workspace":null,"workspace_root":null,"branch":null,"session_id":null,
                "owner":null,"reason":"Question","message":null,"source":"Notification",
                "seen_at":"seen","acknowledged_at":"acknowledged","resolved_at":resolved
            }))
            .unwrap()
        };
        run.attention.push(crate::model::AttentionSnapshot {
            items: vec![item("old", "2026-10-13", None)],
            ..Default::default()
        });
        let mut inbox = Inbox::default();
        inbox.select(&run, 0);
        let selected = inbox.selected.clone();
        run.attention[0].items.push(item("new", "2026-10-14", None));
        run.attention[0]
            .items
            .push(item("resolved", "2026-10-15", Some("resolved")));
        assert_eq!(inbox.items(&run).len(), 2);
        assert_eq!(inbox.index(&run), 1);
        assert_eq!(inbox.selected, selected);
        inbox.include_resolved = true;
        assert_eq!(inbox.items(&run).len(), 3);
    }

    fn attention_item(project: &str, id: &str, kind: &str) -> AttentionItem {
        serde_json::from_value(serde_json::json!({
            "id":id,"project":project,"evidence_hash":"generation-a","ts":"2026-10-16T00:00:00Z",
            "kind":kind,"task":"日本語 test","message":"inspect build output","owner":null
        }))
        .unwrap()
    }

    #[test]
    fn inbox_filters_project_type_receipts_and_unicode_without_changing_evidence() {
        let mut run = picker_fixture();
        let mut acknowledged = attention_item("/a/project", "ack", "question");
        acknowledged.acknowledged_at = Some("now".into());
        acknowledged.seen_at = Some("now".into());
        run.attention.push(crate::model::AttentionSnapshot {
            items: vec![
                acknowledged,
                attention_item("/b/project", "other", "legacy-kind"),
                attention_item("/a/project", "new", "question"),
                attention_item("/a/project", "permission", "permission_prompt"),
            ],
            ..Default::default()
        });
        let before = serde_json::to_string(&run.attention).unwrap();
        let mut inbox = Inbox {
            project: Some("/a/project".into()),
            kind: 2,
            query: "日本 build".into(),
            ..Default::default()
        };
        assert_eq!(
            inbox
                .items(&run)
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["new", "ack"]
        );
        inbox.receipt_filter = 1;
        assert_eq!(inbox.items(&run).len(), 1);
        inbox.project = Some("/b/project".into());
        inbox.kind = 5;
        assert_eq!(inbox.items(&run)[0].id, "other");
        assert_eq!(serde_json::to_string(&run.attention).unwrap(), before);
    }

    #[test]
    fn refresh_cancels_resolution_on_disappearance_or_replaced_evidence() {
        let mut run = picker_fixture();
        run.attention.push(crate::model::AttentionSnapshot {
            items: vec![
                attention_item("/project", "same-id", "question"),
                attention_item("/project", "neighbor", "question"),
            ],
            ..Default::default()
        });
        let mut inbox = Inbox::default();
        inbox.reconcile(&run);
        inbox.selected = Some(attention::token(&run.attention[0].items[0]));
        inbox.confirming = true;
        run.attention[0].items[0].evidence_hash = "generation-b".into();
        inbox.reconcile(&run);
        assert!(inbox.selected.is_none());
        assert!(!inbox.confirming);
        inbox.reconcile(&run);
        assert!(inbox.selected.is_none());
        inbox.next_unresolved(&run);
        assert!(inbox.selected.is_some());
        run.attention[0].items.clear();
        inbox.reconcile(&run);
        assert!(inbox.selected.is_none());
    }

    #[test]
    fn next_unresolved_wraps_loaded_matches_and_skips_resolved_history() {
        let mut run = picker_fixture();
        let mut done = attention_item("/project", "done", "review");
        done.resolved_at = Some("now".into());
        run.attention.push(crate::model::AttentionSnapshot {
            items: vec![
                done,
                attention_item("/project", "a", "question"),
                attention_item("/project", "b", "question"),
            ],
            ..Default::default()
        });
        let mut inbox = Inbox {
            include_resolved: true,
            ..Default::default()
        };
        inbox.reconcile(&run);
        assert_eq!(
            inbox.selected,
            Some(attention::token(&run.attention[0].items[1]))
        );
        inbox.next_unresolved(&run);
        assert_eq!(
            inbox.selected,
            Some(attention::token(&run.attention[0].items[2]))
        );
        inbox.next_unresolved(&run);
        assert_eq!(
            inbox.selected,
            Some(attention::token(&run.attention[0].items[1]))
        );
    }

    #[test]
    fn inbox_render_keeps_filters_controls_and_missing_selection_distinct() {
        let mut run = picker_fixture();
        run.attention.push(crate::model::AttentionSnapshot {
            project: "/project".into(),
            before: Some("cursor".into()),
            items: vec![attention_item("/project", "question", "question")],
            ..Default::default()
        });
        let inbox = Inbox::default();
        for (width, height) in [(80, 24), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw_attention(frame, &run, &inbox, "Receipt failed");
                })
                .unwrap();
            let text = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(text.contains("Inbox filters"));
            assert!(text.contains("next unresolved"));
            assert!(text.contains("Receipt failed"));
            assert!(!text.contains("inspect build output"));
        }
    }
}
