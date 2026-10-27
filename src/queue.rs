//! Task presentation only. Commands and freshness checks belong to actions/CLI.
use crate::{
    actions::CreateRequest,
    interaction::{self, Hits},
    key_hints::{Hint, Keys},
    model::{Run, TaskEntry},
    observe, runtime,
};
use crossterm::{
    event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, List, ListState, Paragraph, Wrap},
};
use std::{io, path::Path, time::Duration};

struct Restore;
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
struct Queue {
    selected: Option<String>,
    all: bool,
    query: String,
    searching: bool,
    confirming: Option<(String, String)>,
    outcome: bool,
    detail_offset: u16,
}
fn identity(task: &TaskEntry) -> String {
    serde_json::to_string(&(&task.project, &task.file, &task.branch)).unwrap()
}

fn lifecycle_detail(task: &TaskEntry) -> String {
    let text = |pointer: &str| {
        task.lifecycle
            .as_ref()
            .and_then(|value| value.pointer(pointer))
            .and_then(|value| value.as_str())
    };
    let state = if let Some(protocol) = task.protocol.as_deref() {
        format!("Open stage: {protocol}")
    } else if text("/stop/kind").is_some() {
        "Stage chain stopped; inspect the terminal".into()
    } else if let Some(finished) = text("/finished/protocol") {
        format!("Finished stage: {finished}; awaiting the existing Stop hook")
    } else {
        "No open stage observed".into()
    };
    format!(
        "{state}\nRecorded stop: {}\nRetry: {}\nDue stage: {}\n{}",
        text("/stop/kind").unwrap_or("none observed"),
        if text("/resume/kind").is_some() {
            "recorded; inspect terminal before retrying"
        } else {
            "none observed"
        },
        text("/due/stage").unwrap_or("none or unavailable"),
        text("/due/reason").unwrap_or("No readable due-stage evidence")
    )
}
impl Queue {
    fn items<'a>(&self, run: &'a Run) -> Vec<&'a TaskEntry> {
        let query = self.query.to_lowercase();
        run.tasks
            .iter()
            .filter(|task| {
                (self.all || task.status == "pending")
                    && format!(
                        "{} {} {} {}",
                        task.name, task.file, task.project, task.branch
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }
    fn select(&mut self, run: &Run, delta: isize) {
        let items = self.items(run);
        let index = items
            .iter()
            .position(|task| Some(identity(task)) == self.selected)
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(items.len().saturating_sub(1));
        self.selected = items.get(index).map(|task| identity(task));
        self.detail_offset = 0;
    }
    fn current<'a>(&self, run: &'a Run) -> Option<&'a TaskEntry> {
        self.items(run)
            .into_iter()
            .find(|task| Some(identity(task)) == self.selected)
    }
}

struct Creation {
    project: usize,
    field: usize,
    name: String,
    request: String,
    confirming: bool,
}

impl Default for Creation {
    fn default() -> Self {
        Self {
            project: 0,
            field: 2,
            name: String::new(),
            request: String::new(),
            confirming: false,
        }
    }
}
#[derive(Clone)]
enum QueueHit {
    Field(usize),
    Task(String),
    Details,
    Shortcut(KeyCode),
}

fn hints(
    queue: &Queue,
    creation: &Creation,
    creating: bool,
    run: &Run,
    keys: &Keys,
) -> (&'static str, Vec<Hint>) {
    use KeyCode::*;
    if (creating && creation.confirming) || queue.confirming.is_some() {
        return (
            "confirmation",
            vec![
                Hint::label("Enter", "Confirm (keyboard)"),
                Hint::label("Other key", "Cancel confirmation"),
                Hint::label("Esc", "Close this screen"),
            ],
        );
    }
    if creating {
        let mut rows = vec![
            Hint::label(
                if creation.field == 0 {
                    "Project"
                } else {
                    "Type"
                },
                if creation.field == 0 {
                    if run.projects.len() < 2 {
                        "Only one registered"
                    } else {
                        "Use arrow keys"
                    }
                } else {
                    "Edit this field"
                },
            ),
            Hint::press("Tab", "Next field", Tab),
            Hint::press("Shift+Tab", "Previous field", BackTab),
        ];
        if creation.field == 0 && run.projects.len() > 1 {
            rows.push(Hint::label("← / →", "Choose project"));
        }
        if creation.request.trim().is_empty() {
            rows.push(Hint::label("Enter", "Review request"));
        } else {
            rows.push(Hint::press("Enter", "Review request", Enter));
        }
        rows.push(Hint::press("Esc", "Close this screen", Esc));
        return (
            match creation.field {
                0 => "project",
                1 => "task name",
                _ => "request",
            },
            rows,
        );
    }
    if queue.searching {
        return (
            "search",
            vec![
                Hint::label("Type", "Edit task search"),
                Hint::press("Enter", "Apply search", Enter),
                Hint::press("Esc", "Close task queue", Esc),
            ],
        );
    }
    let selected = queue.current(run).is_some();
    let action = |key, label, code| {
        if selected {
            Hint::press(key, label, code)
        } else {
            Hint::label(key, label)
        }
    };
    let rows = if keys.more {
        vec![
            Hint::label("PgUp/PgDn", "Scroll task details"),
            Hint::press("o", "Show / hide outcome", Char('o')),
            action("a", "Batch-plan approval…", Char('a')),
            action("r", "Retry stopped step…", Char('r')),
            Hint::press("Esc", "Return to terminal", Esc),
        ]
    } else {
        vec![
            Hint::label("↑ / ↓", "Select task"),
            Hint::press("/", "Search tasks", Char('/')),
            Hint::press("t", "Pending / all tasks", Char('t')),
            action("Enter", "View terminal", Enter),
            action("Shift+R", "Review task", Char('R')),
            Hint::press("n", "Create task…", Char('n')),
            Hint::press("f", "Refresh task list", Char('f')),
            Hint::press("Esc", "Return to terminal", Esc),
        ]
    };
    ("task queue", rows)
}

impl Creation {
    fn draw(&self, frame: &mut Frame, run: &Run, area: Rect) -> Hits<QueueHit> {
        let mut hits = Hits::new(frame.area());
        let compact = area.height < 10;
        let areas = if compact {
            Layout::vertical([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(area)
        } else {
            Layout::vertical([
                Constraint::Length(4),
                Constraint::Length(3),
                Constraint::Min(3),
            ])
            .split(area)
        };
        let project = run
            .projects
            .get(self.project)
            .map(String::as_str)
            .unwrap_or("No registered project");
        let count = format!(
            "{} of {}",
            if run.projects.is_empty() {
                0
            } else {
                self.project + 1
            },
            run.projects.len()
        );
        let hint = if run.projects.len() <= 1 {
            "Only one registered project; cycling cannot change it."
        } else {
            "Left/Right chooses a registered project while this field is focused."
        };
        for (index, (label, value)) in [
            ("Project", format!("{project}\n{hint}")),
            ("Name (optional)", self.name.clone()),
            ("Request", self.request.clone()),
        ]
        .into_iter()
        .enumerate()
        {
            let focused = self.field == index;
            let title = format!(
                "{} {label}{}{}",
                if focused { "▶" } else { " " },
                if index == 0 {
                    format!(" · {count}")
                } else {
                    String::new()
                },
                if focused && index != 0 {
                    " · typing here"
                } else {
                    ""
                }
            );
            let text = if compact {
                format!("{title}: {}", value.lines().next().unwrap_or(""))
            } else {
                value
            };
            let mut paragraph = Paragraph::new(observe::sanitize(&text)).wrap(Wrap { trim: false });
            if !compact {
                paragraph = paragraph.block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(title)
                        .border_style(Style::default().fg(if focused {
                            Color::Cyan
                        } else {
                            Color::Gray
                        })),
                );
            }
            if focused {
                paragraph = paragraph.style(Style::default().bg(Color::DarkGray));
            }
            frame.render_widget(paragraph, areas[index]);
            hits.add(areas[index], QueueHit::Field(index));
        }
        hits
    }
}

fn insert_text(destination: &mut String, text: &str, limit: usize) {
    for c in text.chars() {
        let c = if c == '\n' || c == '\r' || c == '\t' {
            ' '
        } else {
            c
        };
        if !c.is_control() && destination.len() + c.len_utf8() <= limit {
            destination.push(c);
        }
    }
}

pub fn run(role: &str, path: &Path) -> Result<(), String> {
    let _restore = Restore;
    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )
    .map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    let mut observed = runtime::read_run(path)?;
    let mut queue = Queue::default();
    let mut creation = Creation::default();
    let mut creating = role == "create";
    let mut message = String::new();
    let mut hits = Hits::new(Rect::default());
    let mut keys = Keys::default();
    let mut chrome = crate::presentation::Chrome::default();
    loop {
        match runtime::read_run(path) {
            Ok(run) => observed = run,
            Err(e) => message = e,
        }
        if !queue.items(&observed).is_empty()
            && !queue
                .items(&observed)
                .iter()
                .any(|task| Some(identity(task)) == queue.selected)
        {
            queue.select(&observed, 0);
        }
        terminal.draw(|frame| {
            let areas = Layout::vertical([Constraint::Length(3), Constraint::Min(1), Constraint::Length(keys.height(frame.area()))]).split(crate::presentation::body(frame.area()));
            let title = if creating && creation.confirming { "Confirm creation — Enter launches /task-creation; any other key returns to editing" }
                else if creating { "Create task — existing /task-creation launcher" } else { "Task queue — artifact status; no automatic dispatch" };
            frame.render_widget(Paragraph::new(title).block(Block::default().borders(Borders::ALL)), areas[0]);
            hits = Hits::new(frame.area());
            if creating {
                hits = creation.draw(frame, &observed, areas[1]);
            } else {
                let split = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(areas[1]);
                let items = queue.items(&observed);
                let gap = crate::presentation::list_gap(frame.area());
                let rows: Vec<_> = items.iter().map(|task| crate::presentation::separated(vec![Line::from(format!("{} [{}]", observe::sanitize(&task.name), observe::sanitize(&task.status))),
                    Line::from(format!("  {} · {}", observe::sanitize(&task.branch), task.association))], split[0].width.saturating_sub(2), gap)).collect();
                let mut state = ListState::default().with_selected(items.iter().position(|task| Some(identity(task)) == queue.selected));
                frame.render_stateful_widget(List::new(rows).highlight_style(crate::presentation::selected())
                    .block(Block::default().borders(Borders::ALL).title(format!("{} · / {} · click selects", if queue.all { "All tasks" } else { "Pending" }, observe::sanitize(&queue.query)))), split[0], &mut state);
                hits.list_gaps(Block::default().borders(Borders::ALL).inner(split[0]), state.offset(), &items.iter().map(|task| (2, gap, QueueHit::Task(identity(task)))).collect::<Vec<_>>());
                hits.add(split[1], QueueHit::Details);
                let detail = queue.current(&observed).map(|task| format!("{}\n\nProject: {}\nFile: sessions/tasks/{}\nBranch: {}\nArtifact status: {}\nOwner: {}\nState: {} · protocol: {}\nCurrent task: {}\n\nBatch refusal: {}\nRecorded stop: {}\n\nLifecycle evidence: {}\n\nView terminal is VIEW ONLY; no lifecycle command is sent.",
                    task.name, task.project, task.file, task.branch, task.status, task.workspace.as_deref().unwrap_or("No unique worktree"), task.state_health,
                    task.protocol.as_deref().unwrap_or("none"), task.current,
                    task.supervision.as_ref().and_then(|s| s.plan_refused.as_ref()).map(|s| s.to_string()).unwrap_or_else(|| "none".into()),
                    task.supervision.as_ref().and_then(|s| s.stop.as_ref()).map(|s| s.to_string()).unwrap_or_else(|| "none".into()), lifecycle_detail(task)))
                    .unwrap_or_else(|| "No matching tasks. Pending artifacts appear here; this is not a scheduler. Use t to show all tasks or n to create one. Missing data may require updating the installed CLI.".into());
                let detail = if queue.outcome { format!("Last outcome\n\n{}\n\n{}\n\n{}", observed.message, message, observed.task_warnings.join("\n")) } else { detail };
                frame.render_widget(Paragraph::new(observe::sanitize(&detail)).wrap(Wrap { trim: false }).scroll((queue.detail_offset, 0))
                    .block(Block::default().borders(Borders::ALL).title(if queue.outcome { "Last outcome · o details · PgUp/PgDn" } else { "Task details · o outcome · PgUp/PgDn" })), split[1]);
            }
            let confirmation = if creating && creation.confirming { "Enter creates the task; any other key returns to editing. ".into() }
                else if let Some((op, _)) = &queue.confirming { format!("Confirm {} for the displayed task with Enter; any other key cancels. Claude permissions and Git merge are separate. ", if op == "task-approve" { "refused batch-plan approval" } else { "one stopped-step retry" }) } else { String::new() };
            let note = if creating { "Creates a branch/worktree and opens Claude for task creation; confirm the spec in Claude, then the existing workflow chains stages. " } else { "" };
            let status = format!("{confirmation}{note}{}\nLast outcome: {}\n{}", observe::sanitize(&message), observe::sanitize(&observed.message), observe::sanitize(&observed.task_warnings.join("; ")));
            let (context,rows) = hints(&queue,&creation,creating,&observed,&keys);
            for (area,key) in keys.draw(frame,areas[2],context,&rows,&status) { hits.add(area,QueueHit::Shortcut(key)); }
            chrome.draw(frame, if creating { "Create task" } else { "Tasks" });
        }).map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())? {
            continue;
        }
        let event = event::read().map_err(|e| e.to_string())?;
        let size = terminal.size().map_err(|e| e.to_string())?;
        if chrome.closing(&event, Rect::new(0, 0, size.width, size.height)) {
            return Ok(());
        }
        let event = if !creation.confirming
            && queue.confirming.is_none()
            && let Event::Mouse(mouse) = &event
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(QueueHit::Shortcut(code)) = hits.get(*mouse, {
                let size = terminal.size().map_err(|e| e.to_string())?;
                Rect::new(0, 0, size.width, size.height)
            }) {
            Event::Key(KeyEvent::new(*code, KeyModifiers::NONE))
        } else {
            event
        };
        if let Event::Mouse(mouse) = event {
            let size = terminal.size().map_err(|e| e.to_string())?;
            let current = Rect::new(0, 0, size.width, size.height);
            if hits.frame != current {
                continue;
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                creation.confirming = false;
                queue.confirming = None;
                match hits.get(mouse, current) {
                    Some(QueueHit::Field(field)) if creating => {
                        creation.field = *field;
                        message.clear();
                    }
                    Some(QueueHit::Task(key)) if !creating => {
                        queue.selected = Some(key.clone());
                        queue.detail_offset = 0;
                    }
                    _ => {}
                }
            } else if let Some(delta) = interaction::wheel(mouse.kind) {
                queue.confirming = None;
                creation.confirming = false;
                if !creating {
                    if matches!(hits.get(mouse, current), Some(QueueHit::Details)) {
                        queue.detail_offset =
                            queue.detail_offset.saturating_add_signed(delta as i16);
                    } else {
                        queue.select(&observed, delta);
                    }
                }
            }
            continue;
        }
        if let Event::Paste(text) = &event {
            if creating && !creation.confirming {
                match creation.field {
                    1 => insert_text(&mut creation.name, text, 128),
                    2 => insert_text(&mut creation.request, text, 2048),
                    _ => {}
                }
            } else if queue.searching {
                insert_text(&mut queue.query, text, 256);
            }
            continue;
        }
        let Event::Key(key) = event else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if keys.toggle(key.code) {
            creation.confirming = false;
            queue.confirming = None;
            continue;
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Ok(());
        }
        if creating {
            if creation.confirming {
                creation.confirming = false;
                if key.code == KeyCode::Enter
                    && let Some(cli_root) = observed.projects.get(creation.project)
                {
                    let payload = serde_json::to_string(&CreateRequest {
                        cli_root: cli_root.clone(),
                        name: creation.name.clone(),
                        request: creation.request.clone(),
                    })
                    .map_err(|e| e.to_string())?;
                    message = match runtime::request(path, "task-create", Some(&payload)) {
                        Ok(()) => format!(
                            "Creation result below; press {}, release, then s to inspect its terminal. No automatic retry.",
                            crate::help::leader_label(&observed.leader)
                        ),
                        Err(e) => e,
                    };
                    creating = false;
                    queue.outcome = true;
                    queue.detail_offset = 0;
                }
                continue;
            }
            match key.code {
                KeyCode::Tab => creation.field = (creation.field + 1) % 3,
                KeyCode::BackTab => creation.field = (creation.field + 2) % 3,
                KeyCode::Left if creation.field == 0 => {
                    creation.project = creation.project.saturating_sub(1)
                }
                KeyCode::Right if creation.field == 0 => {
                    creation.project =
                        (creation.project + 1).min(observed.projects.len().saturating_sub(1))
                }
                KeyCode::Backspace => {
                    if creation.field == 1 {
                        creation.name.pop();
                    } else if creation.field == 2 {
                        creation.request.pop();
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    match creation.field {
                        1 => insert_text(&mut creation.name, &c.to_string(), 128),
                        2 => insert_text(&mut creation.request, &c.to_string(), 2048),
                        _ => message = "Project is a selector. Press Tab for Name, or Tab again for Request; use Left/Right to choose a project.".into(),
                    }
                }
                KeyCode::Enter => {
                    if creation.request.trim().is_empty() {
                        message = "Enter a task request first.".into();
                    } else {
                        creation.confirming = true;
                        message.clear();
                    }
                }
                _ => {}
            }
            continue;
        }
        if let Some((op, token)) = queue.confirming.take() {
            if key.code == KeyCode::Enter {
                message = match runtime::request(path, &op, Some(&token)) {
                    Ok(()) => "Decision result below. No automatic retry.".into(),
                    Err(e) => e,
                };
                queue.outcome = true;
                queue.detail_offset = 0;
            }
            continue;
        }
        if queue.searching {
            match key.code {
                KeyCode::Enter => queue.searching = false,
                KeyCode::Backspace => {
                    queue.query.pop();
                }
                KeyCode::Char(c) => insert_text(&mut queue.query, &c.to_string(), 256),
                _ => {}
            }
            queue.select(&observed, 0);
            continue;
        }
        match key.code {
            KeyCode::PageUp => queue.detail_offset = queue.detail_offset.saturating_sub(8),
            KeyCode::PageDown => queue.detail_offset = queue.detail_offset.saturating_add(8),
            KeyCode::Char('o') => {
                queue.outcome = !queue.outcome;
                queue.detail_offset = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => queue.select(&observed, -1),
            KeyCode::Down | KeyCode::Char('j') => queue.select(&observed, 1),
            KeyCode::Char('/') => queue.searching = true,
            KeyCode::Char('t') => {
                queue.all = !queue.all;
                queue.select(&observed, 0);
            }
            KeyCode::Char('n') => {
                creation = Creation::default();
                creating = true;
                message.clear();
            }
            KeyCode::Char('f') => {
                message = match runtime::request(path, "task-refresh", None) {
                    Ok(()) => "Read-only task refresh.".into(),
                    Err(e) => e,
                }
            }
            KeyCode::Char('a') | KeyCode::Char('r') => {
                if let Some(task) = queue.current(&observed) {
                    let approve = key.code == KeyCode::Char('a');
                    match crate::actions::decision_args(task, approve) {
                        Ok(_) => {
                            queue.outcome = false;
                            queue.detail_offset = 0;
                            queue.confirming = Some((
                                if approve {
                                    "task-approve"
                                } else {
                                    "task-retry"
                                }
                                .into(),
                                task.token.clone(),
                            ))
                        }
                        Err(e) => message = e,
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(task) = queue.current(&observed) {
                    message = match runtime::request(path, "task-view", Some(&task.token)) {
                        Ok(()) => return Ok(()),
                        Err(e) => e,
                    };
                }
            }
            KeyCode::Char('R') => {
                if let Some(task) = queue.current(&observed).cloned() {
                    drop(terminal);
                    drop(_restore);
                    return crate::review::panel(path, Some(task));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmation_hints_never_offer_clickable_submission_or_mutate_form_text() {
        let run = crate::navigation::fixture();
        let mut creation = Creation {
            request: "Retained request".into(),
            confirming: true,
            ..Default::default()
        };
        let mut queue = Queue::default();
        let keys = Keys::default();
        let (context, rows) = hints(&queue, &creation, true, &run, &keys);
        assert_eq!(context, "confirmation");
        assert!(rows.iter().all(|row| row.action.is_none()));
        assert_eq!(creation.request, "Retained request");
        creation.confirming = false;
        queue.confirming = Some(("task-approve".into(), "exact-token".into()));
        assert!(
            hints(&queue, &creation, false, &run, &keys)
                .1
                .iter()
                .all(|row| row.action.is_none())
        );
        queue.confirming = None;
        assert!(
            hints(&queue, &creation, true, &run, &keys)
                .1
                .iter()
                .any(|row| row.action == Some(KeyCode::Enter))
        );
    }
    #[test]
    fn creation_renders_project_count_and_actual_field_hits_on_small_and_large_screens() {
        use crossterm::event::MouseEvent;
        use ratatui::backend::TestBackend;
        let mut run = crate::navigation::fixture();
        run.projects = vec!["/example/project".into()];
        for (width, height) in [(80, 20), (36, 7)] {
            let creation = Creation::default();
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut hits = Hits::new(Rect::default());
            terminal
                .draw(|frame| {
                    hits = creation.draw(frame, &run, frame.area());
                })
                .unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(text.contains("1 of 1"));
            assert!(text.contains("Request"));
            let mouse = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: height - 1,
                modifiers: KeyModifiers::NONE,
            };
            assert!(matches!(
                hits.get(mouse, hits.frame),
                Some(QueueHit::Field(2))
            ));
            assert!(!creation.confirming);
        }
    }
    #[test]
    fn new_creation_form_starts_in_an_empty_editable_request() {
        let creation = Creation::default();
        assert_eq!(creation.field, 2);
        assert!(creation.request.is_empty());
        assert!(creation.name.is_empty());
        assert!(!creation.confirming);
    }
    #[test]
    fn pasted_requests_cannot_insert_terminal_controls_and_stay_bounded() {
        let mut text = String::new();
        insert_text(&mut text, "a\x1b\x07\n日本語", 8);
        assert_eq!(text, "a 日本");
    }
}
