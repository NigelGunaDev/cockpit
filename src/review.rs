use crate::{
    changes, history,
    interaction::{self, Hits},
    key_hints::{Hint, Keys},
    model::{Run, TaskEntry},
    runtime, tmux,
    ui::sanitize_history,
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
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};
use serde::Deserialize;
use std::{
    io,
    path::Path,
    process::Command,
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize)]
struct Record {
    key: String,
    project: String,
    workspace: String,
    artifact: String,
    file: String,
    name: String,
    branch: String,
    status: String,
    archived: bool,
    owner: bool,
    association: String,
    hash: String,
    modified: String,
}

#[derive(Deserialize)]
struct Catalog {
    version: usize,
    project: String,
    records: Vec<Record>,
    complete: bool,
    warnings: Vec<String>,
}

#[derive(Clone, Deserialize)]
struct Evidence {
    path: String,
    hash: String,
    modified: String,
    text: String,
}

#[derive(Clone, Deserialize)]
struct HistoryRecord {
    key: String,
    source: String,
    session: String,
    modified: String,
}

#[derive(Clone, Deserialize)]
struct Detail {
    version: usize,
    project: String,
    record: Record,
    artifact: Evidence,
    verification: String,
    evidence: Vec<Evidence>,
    base_reference: Option<String>,
    history_records: Vec<HistoryRecord>,
    evidence_key: String,
    complete: bool,
    warnings: Vec<String>,
}

fn read<T: for<'a> Deserialize<'a>>(cli_root: &str, key: Option<&str>) -> Result<T, String> {
    let mut command = Command::new(Path::new(cli_root).join("sessions/bin/sessions"));
    command
        .current_dir(cli_root)
        .env("CLAUDE_PROJECT_DIR", cli_root)
        .args(["tasks", "review", "--json"]);
    if let Some(key) = key {
        command.arg(key);
    }
    let output = tmux::output(command).map_err(|error| {
        format!("Review CLI unavailable: {error}. Check or update the project's installed CLI.")
    })?;
    let value: serde_json::Value = serde_json::from_str(&output).map_err(|e| {
        format!("Invalid review response: {e}. Check or update the project's installed CLI.")
    })?;
    if let Some(error) = value.get("error") {
        return Err(format!(
            "Review unavailable: {}. Check or update the project's installed CLI.",
            error.as_str().unwrap_or("invalid response")
        ));
    }
    serde_json::from_value(value).map_err(|e| {
        format!("Unsupported review response: {e}. Check or update the project's installed CLI.")
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Focus {
    #[default]
    Tasks,
    Entries,
    Content,
}

#[derive(Clone)]
enum Hit {
    Task(String),
    Tab(usize),
    Entry(usize),
    Content,
    Shortcut(KeyCode),
}

#[derive(Clone, Default)]
struct Browser {
    keys: Keys,
    project_index: usize,
    project: String,
    records: Vec<Record>,
    selected: Option<String>,
    detail: Option<Detail>,
    tab: usize,
    focus: Focus,
    query: String,
    searching: bool,
    content_search: bool,
    find: String,
    offset: u16,
    horizontal: u16,
    snapshot: Option<changes::Snapshot>,
    entry: usize,
    preview: String,
    events: bool,
    stale: String,
    message: String,
    loading: Option<String>,
    detail_cache: Vec<Detail>,
}

impl Browser {
    fn rows(&self) -> Vec<&Record> {
        let query = self.query.to_lowercase();
        self.records
            .iter()
            .filter(|row| {
                let haystack = format!(
                    "{} {} {} {}",
                    row.name, row.branch, row.status, row.artifact
                )
                .to_lowercase();
                query.split_whitespace().all(|word| haystack.contains(word))
            })
            .collect()
    }
    fn reset_detail(&mut self) {
        self.detail = None;
        self.snapshot = None;
        self.preview.clear();
        self.stale.clear();
        self.entry = 0;
        self.offset = 0;
        self.horizontal = 0;
    }
    fn reload(&mut self, cli_root: &str, initial: bool, preferred: Option<&TaskEntry>) {
        self.detail_cache.clear();
        let selected = self.selected.clone();
        self.records.clear();
        self.selected = None;
        self.reset_detail();
        self.message.clear();
        match read::<Catalog>(cli_root, None) {
            Ok(catalog) if catalog.version == 1 => {
                self.project = catalog.project;
                self.records = catalog.records;
                self.message = format!(
                    "{}{}",
                    if catalog.complete {
                        "Snapshot; r refresh"
                    } else {
                        "Partial snapshot; r refresh"
                    },
                    if catalog.warnings.is_empty() {
                        String::new()
                    } else {
                        format!(" · {}", catalog.warnings.join("; "))
                    }
                );
                self.selected = selected
                    .clone()
                    .filter(|key| self.records.iter().any(|record| &record.key == key));
                if initial {
                    self.selected = preferred.and_then(|task| {
                        self.records
                            .iter()
                            .find(|record| {
                                record.project == task.project
                                    && record.name == task.name
                                    && record.branch == task.branch
                                    && record.file.trim_start_matches("done/") == task.file
                                    && task
                                        .workspace
                                        .as_ref()
                                        .is_none_or(|root| root == &record.workspace)
                            })
                            .map(|record| record.key.clone())
                    });
                    if preferred.is_none() {
                        self.selected = self.rows().first().map(|record| record.key.clone());
                    }
                } else if self.selected.is_none() && selected.is_some() {
                    self.message =
                        "Task evidence changed or disappeared; select the task explicitly".into();
                }
            }
            Ok(_) => self.message = "Unsupported review catalog; update the installed CLI".into(),
            Err(error) => self.message = error,
        }
    }
    fn load_detail(&mut self, cli_root: &str) {
        self.reset_detail();
        let Some(key) = self.selected.clone() else {
            return;
        };
        let cached = self
            .detail_cache
            .iter()
            .find(|detail| detail.record.key == key)
            .cloned();
        match cached
            .map(Ok)
            .unwrap_or_else(|| read::<Detail>(cli_root, Some(&key)))
        {
            Ok(detail)
                if detail.version == 1
                    && detail.project == self.project
                    && detail.record.key == key
                    && detail.record.hash == detail.artifact.hash =>
            {
                self.detail_cache.retain(|old| old.record.key != key);
                self.detail_cache.push(detail.clone());
                if self.detail_cache.len() > 4 {
                    self.detail_cache.remove(0);
                }
                self.detail = Some(detail)
            }
            Ok(_) => self.message = "Review identity changed; refresh and select explicitly".into(),
            Err(error) => self.message = error,
        }
    }
    fn select(&mut self, delta: isize) -> bool {
        let rows = self.rows();
        let old = rows
            .iter()
            .position(|record| Some(&record.key) == self.selected.as_ref());
        let index = old
            .map(|index| index.saturating_add_signed(delta))
            .unwrap_or(0)
            .min(rows.len().saturating_sub(1));
        let selected = rows.get(index).map(|record| record.key.clone());
        if self.selected == selected {
            return false;
        }
        self.selected = selected;
        self.reset_detail();
        true
    }
    fn set_tab(&mut self, tab: usize) {
        self.tab = tab;
        self.entry = 0;
        self.preview.clear();
        self.offset = 0;
        self.horizontal = 0;
        if self.focus == Focus::Entries && tab != 1 && tab != 3 {
            self.focus = Focus::Content;
        }
    }
    fn cycle_focus(&mut self, reverse: bool) {
        let areas: &[Focus] = if self.tab == 1 || self.tab == 3 {
            &[Focus::Tasks, Focus::Entries, Focus::Content]
        } else {
            &[Focus::Tasks, Focus::Content]
        };
        let index = areas
            .iter()
            .position(|focus| *focus == self.focus)
            .unwrap_or(0);
        self.focus = areas[(index + if reverse { areas.len() - 1 } else { 1 }) % areas.len()];
    }
    fn load_changes(&mut self) {
        self.snapshot = None;
        self.preview.clear();
        self.entry = 0;
        self.stale.clear();
        let result = (|| {
            let detail = self
                .detail
                .as_ref()
                .ok_or("Select retained task evidence first")?;
            if detail.record.association == "ambiguous" {
                return Err("Task ownership is ambiguous; inspect its artifact and select the repository yourself".into());
            }
            let base = detail
                .base_reference
                .as_deref()
                .ok_or("Task has no base reference; a comparison cannot be guessed")?;
            let repo = changes::task_repository(Path::new(&detail.record.workspace), None)?;
            let scope = changes::task_scope(&repo, &detail.record.branch, base)?;
            changes::snapshot(repo, &scope)
        })();
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.entry_preview();
            }
            Err(error) => self.preview = error,
        }
    }
    fn entries(&self) -> Vec<String> {
        if self.tab == 1 {
            return self
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.files.iter().map(|file| file.label()).collect())
                .unwrap_or_default();
        }
        if self.tab == 3 {
            return self
                .detail
                .as_ref()
                .map(|detail| {
                    detail
                        .history_records
                        .iter()
                        .map(|record| {
                            format!(
                                "{} · {} · {}",
                                record.source, record.session, record.modified
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        vec![]
    }
    fn move_entry(&mut self, delta: isize) {
        self.entry = self
            .entry
            .saturating_add_signed(delta)
            .min(self.entries().len().saturating_sub(1));
        self.preview.clear();
        self.offset = 0;
    }
    fn entry_preview(&mut self) {
        if let Some(snapshot) = &self.snapshot {
            if matches!(
                snapshot.scope,
                changes::Scope::Working | changes::Scope::Staged
            ) && self.detail.as_ref().is_none_or(|detail| {
                !changes::on_task_branch(&snapshot.repository, &detail.record.branch)
            }) {
                self.preview = "Task branch changed; refresh and select explicitly".into();
                return;
            }
            self.preview = changes::detail(snapshot, &snapshot.scope, self.entry)
                .unwrap_or_else(|error| error);
        }
    }
    fn open_history(&mut self, cli_root: &str) {
        let Some(record) = self
            .detail
            .as_ref()
            .and_then(|detail| detail.history_records.get(self.entry))
        else {
            return;
        };
        self.preview = history::review_preview(cli_root, &self.project, &record.key, self.events)
            .unwrap_or_else(|error| error);
        self.offset = 0;
    }
    fn cycle_scope(&mut self) {
        let Some(detail) = &self.detail else {
            return;
        };
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if !detail.record.owner
            || !changes::on_task_branch(&snapshot.repository, &detail.record.branch)
        {
            self.message =
                "Working/staged views require this exact task's checked-out branch".into();
            return;
        }
        let scope = match &snapshot.scope {
            changes::Scope::Working => Ok(changes::Scope::Staged),
            changes::Scope::Staged => changes::task_scope(
                &snapshot.repository,
                &detail.record.branch,
                detail.base_reference.as_deref().unwrap_or(""),
            ),
            _ => Ok(changes::Scope::Working),
        };
        let result = scope.and_then(|scope| changes::snapshot(snapshot.repository.clone(), &scope));
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.entry = 0;
                self.offset = 0;
                self.entry_preview();
            }
            Err(error) => self.message = error,
        }
    }
    fn check_revision(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(detail) = &self.detail else {
            return;
        };
        let result =
            changes::task_repository(&snapshot.repository.root, Some(&snapshot.repository))
                .and_then(|repo| match &snapshot.scope {
                    changes::Scope::Commits { .. } => changes::task_scope(
                        &repo,
                        &detail.record.branch,
                        detail.base_reference.as_deref().unwrap_or(""),
                    )
                    .map(|scope| scope == snapshot.scope),
                    _ => Ok(changes::on_task_branch(&repo, &detail.record.branch)
                        && changes::current_head(&repo).ok() == snapshot.head),
                });
        self.stale = match result {
            Ok(true) => String::new(),
            _ => "STALE · repository or branch refs changed; pinned evidence retained, r refresh"
                .into(),
        };
    }
    fn content(&self) -> String {
        let Some(detail) = &self.detail else {
            return "Select a task. Retained archives remain available without a live Claude session.".into();
        };
        match self.tab {
            0 => format!(
                "Source: {}\nModified: {}\nArtifact SHA256: {}\n\n{}",
                detail.artifact.path,
                detail.artifact.modified,
                detail.artifact.hash,
                artifact_body(&detail.artifact.text)
            ),
            1 => format!(
                "{}\n{}\n{}\n{}\n\n{}",
                self.snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.scope.label())
                    .unwrap_or_else(|| "Comparison unavailable".into()),
                "Commit comparison resolves current local refs, not a recorded historical review baseline. Working/staged previews are live filesystem reads.",
                self.snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.warning.as_str())
                    .unwrap_or_default(),
                self.stale,
                self.preview
            ),
            2 => format!(
                "RECORDED EVIDENCE · no checks executed by this view\nEvidence snapshot: {}\n\n{}\n{}",
                detail.evidence_key,
                detail.verification,
                detail
                    .evidence
                    .iter()
                    .map(|file| format!(
                        "\nSource: {}\nModified: {} · SHA256: {}\n{}",
                        file.path, file.modified, file.hash, file.text
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            _ => {
                if self.preview.is_empty() {
                    "Recorded task-name/workspace associations, not proof of a Git revision. Select a record and press Enter. Other or ambiguous records remain in Saved history.".into()
                } else {
                    self.preview.clone()
                }
            }
        }
    }
    fn find_next(&mut self, reverse: bool) {
        if self.find.is_empty() {
            return;
        }
        let query = self.find.to_lowercase();
        let content = sanitize_history(&self.content());
        let lines: Vec<_> = content
            .lines()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&query))
            .map(|(index, _)| index.min(u16::MAX as usize) as u16)
            .collect();
        let target = if reverse {
            lines
                .iter()
                .rev()
                .find(|line| **line < self.offset)
                .or_else(|| lines.last())
        } else {
            lines
                .iter()
                .find(|line| **line > self.offset)
                .or_else(|| lines.first())
        };
        if let Some(line) = target {
            self.offset = *line;
        }
    }
}

#[derive(Clone, Copy)]
enum ReadKind {
    Catalog(bool),
    Detail,
    Changes,
    File,
    History,
    Scope,
    Revision,
}

struct Work {
    generation: u64,
    kind: ReadKind,
    browser: Browser,
    root: String,
    preferred: Option<TaskEntry>,
}

// One disposable reader at a time; rapid navigation replaces the queued read.
// Only matching generations may publish. No reader can send terminal input.
#[derive(Default)]
struct Reads {
    generation: u64,
    desired: Option<ReadKind>,
    active: Option<Receiver<Work>>,
    queued: Option<Work>,
}

impl Reads {
    fn start(&mut self, mut work: Work) {
        let (send, receive) = mpsc::channel();
        self.active = Some(receive);
        std::thread::spawn(move || {
            match work.kind {
                ReadKind::Catalog(initial) => {
                    work.browser
                        .reload(&work.root, initial, work.preferred.as_ref())
                }
                ReadKind::Detail => work.browser.load_detail(&work.root),
                ReadKind::Changes => work.browser.load_changes(),
                ReadKind::File => work.browser.entry_preview(),
                ReadKind::History => work.browser.open_history(&work.root),
                ReadKind::Scope => work.browser.cycle_scope(),
                ReadKind::Revision => work.browser.check_revision(),
            }
            let _ = send.send(work);
        });
    }
    fn cancel(&mut self, browser: &mut Browser) {
        self.generation += 1;
        self.desired = None;
        self.queued = None;
        browser.loading = None;
    }
    fn submit(
        &mut self,
        browser: &mut Browser,
        kind: ReadKind,
        root: &str,
        preferred: Option<TaskEntry>,
    ) {
        self.generation += 1;
        self.desired = Some(kind);
        if !matches!(kind, ReadKind::Revision) {
            browser.loading = Some("Loading evidence… navigation and Esc remain available".into());
        }
        let work = Work {
            generation: self.generation,
            kind,
            browser: browser.clone(),
            root: root.into(),
            preferred,
        };
        if self.active.is_some() {
            self.queued = Some(work);
        } else {
            self.start(work);
        }
    }
    fn poll(&mut self, browser: &mut Browser, root: &str) -> bool {
        let result = match self.active.as_ref().map(|receive| receive.try_recv()) {
            Some(Ok(work)) => Some(work),
            Some(Err(TryRecvError::Disconnected)) => {
                self.active = None;
                if let Some(work) = self.queued.take() {
                    self.start(work);
                } else {
                    browser.loading = None;
                    browser.message = "Evidence reader stopped; r retries".into();
                }
                return true;
            }
            _ => return false,
        };
        self.active = None;
        let mut dirty = false;
        if let Some(work) = result
            && work.generation == self.generation
        {
            self.desired = None;
            let loaded = work.browser;
            browser.loading = None;
            match work.kind {
                ReadKind::Revision => browser.stale = loaded.stale,
                ReadKind::File | ReadKind::History => browser.preview = loaded.preview,
                _ => {
                    if matches!(work.kind, ReadKind::Catalog(_)) {
                        browser.records = loaded.records;
                        browser.project = loaded.project;
                        browser.selected = loaded.selected;
                    }
                    browser.detail = loaded.detail;
                    browser.detail_cache = loaded.detail_cache;
                    browser.snapshot = loaded.snapshot;
                    browser.preview = loaded.preview;
                    browser.stale = loaded.stale;
                    browser.message = loaded.message;
                    if matches!(work.kind, ReadKind::Changes | ReadKind::Scope) {
                        browser.entry = 0;
                        browser.offset = 0;
                    }
                }
            }
            if browser
                .selected
                .as_ref()
                .is_some_and(|key| !browser.rows().iter().any(|row| &row.key == key))
            {
                browser.selected = None;
                browser.reset_detail();
                if matches!(work.kind, ReadKind::Catalog(true)) && work.preferred.is_none() {
                    browser.selected = browser.rows().first().map(|row| row.key.clone());
                }
            }
            if matches!(work.kind, ReadKind::Catalog(_)) && browser.selected.is_some() {
                self.submit(browser, ReadKind::Detail, root, None);
            } else if matches!(work.kind, ReadKind::Detail)
                && browser.tab == 1
                && browser.detail.is_some()
            {
                self.submit(browser, ReadKind::Changes, root, None);
            }
            dirty = true;
        }
        if self.active.is_none()
            && let Some(work) = self.queued.take()
        {
            self.start(work);
        }
        dirty
    }
    fn tab(&mut self, browser: &mut Browser, root: &str) {
        if matches!(self.desired, Some(ReadKind::Catalog(_))) {
            return;
        }
        self.cancel(browser);
        if browser.detail.is_none() && browser.selected.is_some() {
            self.submit(browser, ReadKind::Detail, root, None);
        } else if browser.tab == 1 && browser.detail.is_some() {
            self.submit(
                browser,
                if browser.snapshot.is_some() {
                    ReadKind::File
                } else {
                    ReadKind::Changes
                },
                root,
                None,
            );
        }
    }
    fn entry(&mut self, browser: &mut Browser, root: &str) {
        self.cancel(browser);
        if browser.tab == 1 && browser.snapshot.is_some() {
            self.submit(browser, ReadKind::File, root, None);
        }
    }
}

fn artifact_body(text: &str) -> String {
    if text.lines().next().is_some_and(|line| line.trim() == "---")
        && let Some((end, _)) = text
            .lines()
            .enumerate()
            .find(|(index, line)| *index > 0 && line.trim() == "---")
    {
        return text.lines().skip(end + 1).collect::<Vec<_>>().join("\n");
    }
    text.into()
}

fn hints(browser: &Browser, project_count: usize) -> (&'static str, Vec<Hint>) {
    use KeyCode::*;
    if browser.searching {
        return (
            "search",
            vec![
                Hint::label("Type", "Edit search"),
                Hint::press("Enter", "Apply search", Enter),
                Hint::press("Esc", "Finish search", Esc),
            ],
        );
    }
    let context = match browser.focus {
        Focus::Tasks => "task list",
        Focus::Entries if browser.tab == 3 => "history",
        Focus::Entries => "files",
        Focus::Content => "evidence",
    };
    let mut rows = if browser.keys.more {
        vec![
            Hint::label("Home / End", "Top / bottom"),
            Hint::label("h / l", "Scroll sideways"),
            Hint::label("n / N", "Next / previous match"),
            Hint::press("Shift+Tab", "Previous section", BackTab),
        ]
    } else {
        let mut rows = vec![Hint::label(
            "↑ / ↓",
            if browser.focus == Focus::Content {
                "Scroll evidence"
            } else if browser.focus == Focus::Tasks {
                "Select task"
            } else {
                "Select entry"
            },
        )];
        if browser.focus == Focus::Content {
            rows.push(Hint::label("PgUp/PgDn", "Scroll a page"));
        }
        if browser.focus == Focus::Entries && browser.tab == 3 && !browser.entries().is_empty() {
            rows.push(Hint::press("Enter", "Open conversation", Enter));
        }
        rows.push(Hint::press(
            "/",
            if browser.focus == Focus::Tasks {
                "Search tasks"
            } else {
                "Find in evidence"
            },
            Char('/'),
        ));
        rows.push(Hint::press("Tab", "Next section", Tab));
        rows.push(Hint::label("1–4", "Review tabs"));
        rows
    };
    if browser.keys.more
        && browser.tab == 1
        && browser.snapshot.is_some()
        && browser
            .detail
            .as_ref()
            .is_some_and(|detail| detail.record.owner)
    {
        rows.push(Hint::press("w", "Change diff scope", Char('w')));
    }
    if browser.keys.more && browser.tab == 3 && !browser.entries().is_empty() {
        rows.push(Hint::press("e", "Conversation / events", Char('e')));
    }
    if browser.focus == Focus::Tasks && project_count > 1 {
        rows.push(Hint::label("← / →", "Change project"));
    }
    rows.push(Hint::press("r", "Refresh evidence", Char('r')));
    rows.push(Hint::press("Esc", "Return to terminal", Esc));
    (context, rows)
}

fn draw(frame: &mut Frame, browser: &Browser, projects: &[String]) -> Hits<Hit> {
    let mut hits = Hits::new(frame.area());
    let compact = frame.area().height < 18;
    let areas = Layout::vertical([
        Constraint::Length(if compact { 2 } else { 4 }),
        Constraint::Length(if compact { 1 } else { 3 }),
        Constraint::Min(1),
        Constraint::Length(browser.keys.height(frame.area())),
    ])
    .split(crate::presentation::body(frame.area()));
    let selected = browser
        .detail
        .as_ref()
        .map(|detail| {
            format!(
                "{} [{}]{} · {} · {} · {}",
                detail.record.name,
                detail.record.status,
                if detail.record.archived {
                    " archived"
                } else {
                    ""
                },
                detail.record.branch,
                detail.record.association,
                detail.record.modified
            )
        })
        .unwrap_or_else(|| "No task selected".into());
    frame.render_widget(
        Paragraph::new(sanitize_history(&format!(
            "{} · Project {}/{} {}\n{selected}",
            "Task review — READ ONLY",
            browser.project_index + 1,
            projects.len(),
            browser.project
        )))
        .block(Block::default().borders(if compact {
            Borders::NONE
        } else {
            Borders::ALL
        })),
        areas[0],
    );
    let tabs = Layout::horizontal([Constraint::Percentage(25); 4]).split(areas[1]);
    for (index, title) in ["1 Overview", "2 Changes", "3 Verification", "4 History"]
        .iter()
        .enumerate()
    {
        frame.render_widget(
            Paragraph::new(*title)
                .block(Block::default().borders(if compact { Borders::NONE } else { Borders::ALL }))
                .style(if browser.tab == index {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                }),
            tabs[index],
        );
        hits.add(tabs[index], Hit::Tab(index));
    }
    let columns = Layout::horizontal([Constraint::Percentage(28), Constraint::Percentage(72)])
        .split(areas[2]);
    let rows = browser.rows();
    let gap = crate::presentation::list_gap(frame.area());
    let items: Vec<_> = rows
        .iter()
        .map(|record| {
            crate::presentation::separated(
                vec![
                    Line::from(sanitize_history(&record.name)),
                    Line::from(sanitize_history(&format!(
                        "{} · {}",
                        record.status,
                        if record.archived {
                            "archive"
                        } else {
                            "active artifact"
                        }
                    ))),
                ],
                columns[0].width.saturating_sub(2),
                gap,
            )
        })
        .collect();
    let mut state = ListState::default().with_selected(
        rows.iter()
            .position(|row| Some(&row.key) == browser.selected.as_ref()),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Tasks · / {}", sanitize_history(&browser.query)))
        .border_style(Style::default().fg(if browser.focus == Focus::Tasks {
            Color::Cyan
        } else {
            Color::Gray
        }));
    let inside = block.inner(columns[0]);
    frame.render_stateful_widget(
        List::new(items)
            .highlight_style(crate::presentation::selected())
            .block(block),
        columns[0],
        &mut state,
    );
    hits.list_gaps(
        inside,
        state.offset(),
        &rows
            .iter()
            .map(|record| (2, gap, Hit::Task(record.key.clone())))
            .collect::<Vec<_>>(),
    );
    let entries = browser.entries();
    let content_area = if browser.tab == 1 || browser.tab == 3 {
        let parts = Layout::vertical([
            Constraint::Length((entries.len() as u16 + 2).clamp(3, 7)),
            Constraint::Min(1),
        ])
        .split(columns[1]);
        let block = Block::default()
            .borders(Borders::ALL)
            .title(if browser.tab == 1 {
                "Changed files · w scope"
            } else {
                "History · Enter open · e events"
            })
            .border_style(Style::default().fg(if browser.focus == Focus::Entries {
                Color::Cyan
            } else {
                Color::Gray
            }));
        let inside = block.inner(parts[0]);
        let mut state = ListState::default().with_selected(if entries.is_empty() {
            None
        } else {
            Some(browser.entry)
        });
        frame.render_stateful_widget(
            List::new(
                entries
                    .iter()
                    .map(|entry| ListItem::new(sanitize_history(entry))),
            )
            .highlight_style(crate::presentation::selected())
            .block(block),
            parts[0],
            &mut state,
        );
        hits.list(
            inside,
            state.offset(),
            &(0..entries.len())
                .map(|index| (1, Hit::Entry(index)))
                .collect::<Vec<_>>(),
        );
        parts[1]
    } else {
        columns[1]
    };
    frame.render_widget(
        Paragraph::new(sanitize_history(
            &browser
                .content()
                .lines()
                .skip(browser.offset as usize)
                .take(content_area.height.saturating_sub(2) as usize)
                .collect::<Vec<_>>()
                .join("\n"),
        ))
        .scroll((0, browser.horizontal))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(if browser.focus == Focus::Content {
                    "Evidence · focused · / find"
                } else {
                    "Evidence"
                })
                .border_style(Style::default().fg(if browser.focus == Focus::Content {
                    Color::Cyan
                } else {
                    Color::Gray
                })),
        ),
        content_area,
    );
    hits.add(content_area, Hit::Content);
    let message = if browser.searching {
        format!(
            "{}: {} · Enter/Esc finish",
            if browser.content_search {
                "Find evidence"
            } else {
                "Filter tasks"
            },
            if browser.content_search {
                &browser.find
            } else {
                &browser.query
            }
        )
    } else {
        format!(
            "{}\n{}",
            browser.loading.as_deref().unwrap_or(&browser.message),
            browser
                .detail
                .as_ref()
                .map(|detail| format!(
                    "{}{}",
                    if detail.complete {
                        ""
                    } else {
                        "Partial evidence · "
                    },
                    detail.warnings.join("; ")
                ))
                .unwrap_or_default()
        )
    };
    let (context, rows) = hints(browser, projects.len());
    for (area, key) in
        browser
            .keys
            .draw(frame, areas[3], context, &rows, &sanitize_history(&message))
    {
        hits.add(area, Hit::Shortcut(key));
    }
    hits
}

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

pub fn panel(path: &Path, preferred: Option<TaskEntry>) -> Result<(), String> {
    let run: Run = runtime::read_run(path)?;
    if run.projects.is_empty() {
        return Err("No registered project to review".into());
    }
    let _restore = Restore;
    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    let mut browser = Browser {
        project_index: preferred
            .as_ref()
            .and_then(|task| run.projects.iter().position(|root| root == &task.cli_root))
            .unwrap_or(0),
        ..Default::default()
    };
    let mut reads = Reads::default();
    reads.submit(
        &mut browser,
        ReadKind::Catalog(true),
        &run.projects[preferred
            .as_ref()
            .and_then(|task| run.projects.iter().position(|root| root == &task.cli_root))
            .unwrap_or(0)],
        preferred,
    );
    let mut checked_at = Instant::now();
    let mut dirty = true;
    let mut hits = Hits::new(Rect::default());
    let mut chrome = crate::presentation::Chrome::default();
    loop {
        let cli_root = &run.projects[browser.project_index];
        dirty |= reads.poll(&mut browser, cli_root);
        if browser.tab == 1
            && reads.active.is_none()
            && checked_at.elapsed() > Duration::from_secs(2)
        {
            reads.submit(&mut browser, ReadKind::Revision, cli_root, None);
            checked_at = Instant::now();
        }
        if dirty {
            terminal
                .draw(|frame| {
                    hits = draw(frame, &browser, &run.projects);
                    chrome.draw(frame, "Task review · Read only");
                })
                .map_err(|e| e.to_string())?;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(25)).map_err(|e| e.to_string())? {
            continue;
        }
        dirty = true;
        let cli_root = &run.projects[browser.project_index];
        let event = event::read().map_err(|e| e.to_string())?;
        let size = terminal.size().map_err(|e| e.to_string())?;
        if chrome.closing(&event, Rect::new(0, 0, size.width, size.height)) {
            return Ok(());
        }
        let event = if let Event::Mouse(mouse) = &event
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(Hit::Shortcut(code)) = hits.get(*mouse, {
                let size = terminal.size().map_err(|e| e.to_string())?;
                Rect::new(0, 0, size.width, size.height)
            }) {
            Event::Key(KeyEvent::new(*code, KeyModifiers::NONE))
        } else {
            event
        };
        match event {
            Event::Paste(text) if browser.searching => {
                let query = if browser.content_search {
                    &mut browser.find
                } else {
                    &mut browser.query
                };
                for c in text.chars().filter(|c| !c.is_control()) {
                    if query.len() + c.len_utf8() > 256 {
                        break;
                    }
                    query.push(c);
                }
            }
            Event::Mouse(mouse) => {
                let size = terminal.size().map_err(|e| e.to_string())?;
                let Some(hit) = hits
                    .get(mouse, Rect::new(0, 0, size.width, size.height))
                    .cloned()
                else {
                    continue;
                };
                if let Some(delta) = interaction::wheel(mouse.kind) {
                    match hit {
                        Hit::Task(_) => {
                            if browser.select(delta) {
                                reads.submit(&mut browser, ReadKind::Detail, cli_root, None);
                            }
                        }
                        Hit::Entry(_) => {
                            browser.move_entry(delta);
                            reads.entry(&mut browser, cli_root);
                        }
                        Hit::Shortcut(_) => {}
                        _ => browser.offset = browser.offset.saturating_add_signed(delta as i16),
                    }
                } else if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    match hit {
                        Hit::Task(key) => {
                            browser.focus = Focus::Tasks;
                            if browser.selected.as_ref() != Some(&key) {
                                browser.selected = Some(key);
                                browser.reset_detail();
                                reads.submit(&mut browser, ReadKind::Detail, cli_root, None);
                            }
                        }
                        Hit::Tab(tab) => {
                            browser.set_tab(tab);
                            reads.tab(&mut browser, cli_root);
                        }
                        Hit::Entry(index) => {
                            browser.focus = Focus::Entries;
                            browser.entry = index;
                            browser.preview.clear();
                            reads.entry(&mut browser, cli_root);
                        }
                        Hit::Content => browser.focus = Focus::Content,
                        Hit::Shortcut(_) => {}
                    }
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if browser.keys.toggle(key.code) {
                    continue;
                }
                if browser.searching {
                    let query = if browser.content_search {
                        &mut browser.find
                    } else {
                        &mut browser.query
                    };
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter => {
                            browser.searching = false;
                            if browser.content_search {
                                browser.offset = 0;
                                browser.find_next(false);
                            } else if key.code == KeyCode::Enter
                                && (browser.select(0)
                                    || (browser.detail.is_none() && browser.selected.is_some()))
                            {
                                reads.submit(&mut browser, ReadKind::Detail, cli_root, None);
                            }
                        }
                        KeyCode::Backspace => {
                            query.pop();
                        }
                        KeyCode::Char(c)
                            if !key.modifiers.contains(KeyModifiers::CONTROL)
                                && !c.is_control()
                                && query.len() < 256 =>
                        {
                            query.push(c)
                        }
                        _ => {}
                    }
                    if !browser.content_search
                        && browser.selected.as_ref().is_some_and(|key| {
                            !browser.rows().iter().any(|record| &record.key == key)
                        })
                    {
                        browser.selected = None;
                        browser.reset_detail();
                        reads.cancel(&mut browser);
                    }
                    continue;
                }
                match key.code {
                    KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(());
                    }
                    KeyCode::Char('1'..='4') => {
                        if let KeyCode::Char(c) = key.code {
                            browser.set_tab(c as usize - '1' as usize);
                            reads.tab(&mut browser, cli_root);
                        }
                    }
                    KeyCode::Tab | KeyCode::BackTab => {
                        browser.cycle_focus(key.code == KeyCode::BackTab)
                    }
                    KeyCode::Char('/') => {
                        browser.searching = true;
                        browser.content_search = browser.focus != Focus::Tasks;
                    }
                    KeyCode::Char('n') => browser.find_next(false),
                    KeyCode::Char('N') => browser.find_next(true),
                    KeyCode::Char('r') => {
                        browser.reset_detail();
                        browser.detail_cache.clear();
                        reads.submit(&mut browser, ReadKind::Catalog(false), cli_root, None);
                    }
                    KeyCode::Char('w') if browser.tab == 1 && browser.snapshot.is_some() => {
                        reads.submit(&mut browser, ReadKind::Scope, cli_root, None)
                    }
                    KeyCode::Char('e') if browser.tab == 3 && !browser.entries().is_empty() => {
                        browser.events = !browser.events;
                        reads.submit(&mut browser, ReadKind::History, cli_root, None);
                    }
                    KeyCode::Enter
                        if browser.tab == 3
                            && browser.focus == Focus::Entries
                            && !browser.entries().is_empty() =>
                    {
                        reads.submit(&mut browser, ReadKind::History, cli_root, None);
                        browser.focus = Focus::Content;
                    }
                    KeyCode::Enter if browser.focus == Focus::Tasks => {
                        if browser.select(0)
                            || (browser.detail.is_none() && browser.selected.is_some())
                        {
                            reads.submit(&mut browser, ReadKind::Detail, cli_root, None);
                        }
                    }
                    KeyCode::Up | KeyCode::Down | KeyCode::Char('j' | 'k') => {
                        let delta = if matches!(key.code, KeyCode::Up | KeyCode::Char('k')) {
                            -1
                        } else {
                            1
                        };
                        match browser.focus {
                            Focus::Tasks => {
                                if browser.select(delta) {
                                    reads.submit(&mut browser, ReadKind::Detail, cli_root, None);
                                }
                            }
                            Focus::Entries => {
                                browser.move_entry(delta);
                                reads.entry(&mut browser, cli_root);
                            }
                            Focus::Content => {
                                browser.offset = browser.offset.saturating_add_signed(delta as i16)
                            }
                        }
                    }
                    KeyCode::Left | KeyCode::Right if browser.focus == Focus::Tasks => {
                        if run.projects.len() < 2 {
                            continue;
                        }
                        let delta = if key.code == KeyCode::Left {
                            run.projects.len() - 1
                        } else {
                            1
                        };
                        browser.project_index =
                            (browser.project_index + delta) % run.projects.len();
                        browser.selected = None;
                        browser.project.clear();
                        browser.records.clear();
                        browser.reset_detail();
                        let next_root = &run.projects[browser.project_index];
                        reads.submit(&mut browser, ReadKind::Catalog(true), next_root, None);
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        browser.horizontal = browser.horizontal.saturating_sub(4)
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        browser.horizontal = browser.horizontal.saturating_add(4)
                    }
                    KeyCode::PageUp => browser.offset = browser.offset.saturating_sub(12),
                    KeyCode::PageDown => browser.offset = browser.offset.saturating_add(12),
                    KeyCode::Home => browser.offset = 0,
                    KeyCode::End => {
                        browser.offset = browser
                            .content()
                            .lines()
                            .count()
                            .saturating_sub(6)
                            .min(u16::MAX as usize) as u16
                    }
                    _ => {}
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
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn row(key: &str) -> Record {
        serde_json::from_value(serde_json::json!({"key":key,"project":"/project","workspace":"/project",
            "artifact":"/project/sessions/tasks/done/task/README.md","file":"done/task/README.md","name":format!("task-{key}"),
            "branch":"feature/task","status":"completed","archived":true,"owner":false,"association":"retained",
            "hash":"artifact-hash","modified":"recorded-time"})).unwrap()
    }
    fn detail(key: &str) -> Detail {
        serde_json::from_value(serde_json::json!({"version":1,"project":"/project",
            "record":row_json(key),
            "artifact":{"path":"task/README.md","hash":"artifact-hash","modified":"recorded-time","text":"Source artifact body\n## Review\nRetained finding"},
            "verification":"Recorded check passed","evidence":[],"base_reference":null,"history_records":[],
            "evidence_key":"evidence-generation","complete":true,"warnings":[]})).unwrap()
    }
    fn row_json(key: &str) -> serde_json::Value {
        serde_json::json!({"key":key,"project":"/project","workspace":"/project","artifact":"task/README.md",
            "file":"done/task/README.md","name":format!("task-{key}"),"branch":"feature/task","status":"completed",
            "archived":true,"owner":false,"association":"retained","hash":"artifact-hash","modified":"recorded-time"})
    }

    #[test]
    fn contextual_keys_follow_focus_and_hide_impossible_project_switches() {
        let mut browser = Browser::default();
        let (context, rows) = hints(&browser, 1);
        assert_eq!(context, "task list");
        assert!(rows.iter().any(|row| row.label == "Select task"));
        assert!(!rows.iter().any(|row| row.label == "Change project"));
        assert!(
            hints(&browser, 2)
                .1
                .iter()
                .any(|row| row.label == "Change project")
        );
        browser.focus = Focus::Content;
        let (context, rows) = hints(&browser, 2);
        assert_eq!(context, "evidence");
        assert!(rows.iter().any(|row| row.label == "Scroll evidence"));
        assert!(!rows.iter().any(|row| row.label == "Change project"));
        browser.searching = true;
        browser.query = "retained query".into();
        browser.keys.toggle(KeyCode::F(1));
        assert_eq!(hints(&browser, 1).0, "search");
        assert_eq!(browser.query, "retained query");
    }

    #[test]
    fn slow_reads_leave_navigation_available_and_cannot_publish_an_old_selection() {
        let root = std::env::temp_dir().join(format!(
            "cockpit-review-async-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("sessions/bin")).unwrap();
        let cli = root.join("sessions/bin/sessions");
        fs::write(&cli, format!("#!/bin/sh\ntouch {}\nwhile [ ! -f {} ]; do sleep 0.01; done\ncat \"{}\"/\"$4\".json\n", tmux::quote(root.join("started").to_str().unwrap()), tmux::quote(root.join("release").to_str().unwrap()), root.display())).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        for key in ["old", "new"] {
            let value = serde_json::json!({"version":1,"project":"/project","record":row_json(key),"artifact":{"path":"task/README.md","hash":"artifact-hash","modified":"recorded-time","text":key},"verification":"","evidence":[],"base_reference":null,"history_records":[],"evidence_key":"evidence","complete":true,"warnings":[]});
            fs::write(root.join(format!("{key}.json")), value.to_string()).unwrap();
        }
        let mut browser = Browser {
            project: "/project".into(),
            records: vec![row("old"), row("new")],
            selected: Some("old".into()),
            ..Default::default()
        };
        let mut reads = Reads::default();
        reads.submit(&mut browser, ReadKind::Detail, root.to_str().unwrap(), None);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !root.join("started").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(root.join("started").exists());
        let start = Instant::now();
        assert!(browser.select(1));
        browser.cycle_focus(false);
        reads.submit(&mut browser, ReadKind::Detail, root.to_str().unwrap(), None);
        assert!(start.elapsed() < Duration::from_millis(100));
        assert_eq!(browser.focus, Focus::Content);
        assert!(browser.detail.is_none());
        fs::write(root.join("release"), "ready").unwrap();
        while browser.detail.is_none() && Instant::now() < deadline {
            reads.poll(&mut browser, root.to_str().unwrap());
            assert!(
                browser
                    .detail
                    .as_ref()
                    .is_none_or(|value| value.record.key == "new")
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(browser.detail.as_ref().unwrap().record.key, "new");
        // Revisiting exact evidence is local until explicit refresh clears the cache.
        fs::write(&cli, "#!/bin/sh\nexit 1\n").unwrap();
        browser.load_detail(root.to_str().unwrap());
        assert!(browser.detail.is_some());
        browser.reload(root.to_str().unwrap(), false, None);
        assert!(browser.detail_cache.is_empty());
        assert!(browser.detail.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn moving_history_entries_discards_a_preview_that_finishes_late() {
        let mut browser = Browser {
            project: "/project".into(),
            records: vec![row("task")],
            selected: Some("task".into()),
            detail: Some(detail("task")),
            tab: 3,
            ..Default::default()
        };
        let mut old = browser.clone();
        old.preview = "OLD HISTORY PREVIEW".into();
        let (send, receive) = mpsc::channel();
        let mut reads = Reads {
            generation: 1,
            desired: Some(ReadKind::History),
            active: Some(receive),
            ..Default::default()
        };
        browser.entry = 1;
        reads.entry(&mut browser, "/project");
        assert!(
            send.send(Work {
                generation: 1,
                kind: ReadKind::History,
                browser: old,
                root: "/project".into(),
                preferred: None
            })
            .is_ok()
        );
        reads.poll(&mut browser, "/project");
        assert!(browser.preview.is_empty());
        assert_eq!(browser.entry, 1);
    }

    #[test]
    fn unsupported_installed_cli_explains_how_to_recover() {
        let root = std::env::temp_dir().join(format!(
            "cockpit-review-cli-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("sessions/bin")).unwrap();
        let cli = root.join("sessions/bin/sessions");
        fs::write(
            &cli,
            "#!/bin/sh\nprintf '%s\\n' '{\"error\":\"Unknown task command: review\"}'\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let error = match read::<Catalog>(root.to_str().unwrap(), None) {
            Err(error) => error,
            Ok(_) => panic!("unsupported CLI unexpectedly succeeded"),
        };
        assert!(error.contains("Review CLI unavailable"));
        assert!(error.contains("Check or update the project's installed CLI"));
        for response in ["{}", "invalid-json", r#"{"error":"Unsupported review"}"#] {
            fs::write(
                &cli,
                format!("#!/bin/sh\nprintf '%s\\n' {}\n", tmux::quote(response)),
            )
            .unwrap();
            let error = match read::<Catalog>(root.to_str().unwrap(), None) {
                Err(error) => error,
                Ok(_) => panic!("invalid CLI response unexpectedly succeeded"),
            };
            assert!(error.contains("Check or update the project's installed CLI"));
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn review_changes_displays_file_list_truncation_warning() {
        let root = std::env::temp_dir().join(format!(
            "cockpit-review-limit-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(
            Command::new("git")
                .current_dir(&root)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        for index in 0..257 {
            fs::write(root.join(format!("file-{index:03}.txt")), "untracked\n").unwrap();
        }
        let repository = changes::task_repository(&root, None).unwrap();
        let snapshot = changes::snapshot(repository, &changes::Scope::Working).unwrap();
        assert_eq!(snapshot.files.len(), 256);
        assert!(!snapshot.warning.is_empty());
        let warning = snapshot.warning.clone();
        let browser = Browser {
            tab: 1,
            detail: Some(detail("task")),
            snapshot: Some(snapshot),
            ..Default::default()
        };
        assert!(browser.content().contains(&warning));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refresh_cancels_selection_and_previews_when_the_artifact_is_replaced() {
        let root = std::env::temp_dir().join(format!(
            "cockpit-review-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("sessions/bin")).unwrap();
        let cli = root.join("sessions/bin/sessions");
        let script = format!(
            "#!/bin/sh\nif [ \"$#\" -gt 3 ]; then cat {}; else cat {}; fi\n",
            tmux::quote(root.join("detail.json").to_str().unwrap()),
            tmux::quote(root.join("catalog.json").to_str().unwrap())
        );
        fs::write(&cli, script).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("catalog.json"), serde_json::json!({"version":1,"project":"/project","records":[row_json("old")],"complete":true,"warnings":[]}).to_string()).unwrap();
        fs::write(
            root.join("detail.json"),
            serde_json::json!({"error":"old evidence no longer available"}).to_string(),
        )
        .unwrap();
        let mut browser = Browser {
            project: "/project".into(),
            selected: Some("old".into()),
            detail: Some(detail("old")),
            preview: "old preview".into(),
            ..Default::default()
        };
        fs::write(root.join("catalog.json"), serde_json::json!({"version":1,"project":"/project","records":[row_json("replacement")],"complete":true,"warnings":[]}).to_string()).unwrap();
        browser.reload(root.to_str().unwrap(), false, None);
        assert!(browser.selected.is_none());
        assert!(browser.detail.is_none());
        assert!(browser.preview.is_empty());
        assert!(
            browser.message.contains("select the task explicitly"),
            "{}",
            browser.message
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archived_review_without_terminal_renders_provenance_and_exact_mouse_rows() {
        for (width, height) in [(140, 35), (40, 12)] {
            let mut browser = Browser {
                project: "/project".into(),
                records: (0..25).map(|i| row(&format!("{i:02}"))).collect(),
                selected: Some("24".into()),
                detail: Some(detail("24")),
                ..Default::default()
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut hits = Hits::new(Rect::default());
            terminal
                .draw(|frame| hits = draw(frame, &browser, &["/project".into()]))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let lines: Vec<String> = (0..height)
                .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
                .collect();
            let (y, line) = lines
                .iter()
                .enumerate()
                .find(|(_, line)| line.contains("task-24"))
                .unwrap();
            // The header may contain the name too; the last occurrence is the visible task row.
            let (y, line) = lines
                .iter()
                .enumerate()
                .rfind(|(_, line)| line.contains("task-24"))
                .unwrap_or((y, line));
            let x = line.find("task-24").unwrap() as u16;
            let mouse = crossterm::event::MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y as u16,
                modifiers: KeyModifiers::NONE,
            };
            assert!(
                matches!(hits.get(mouse,Rect::new(0,0,width,height)),Some(Hit::Task(key)) if key=="24")
            );
            assert!(
                hits.get(mouse, Rect::new(0, 0, width + 1, height))
                    .is_none()
            );
            browser.tab = 2;
            assert!(browser.content().contains("RECORDED EVIDENCE"));
            browser.tab = 1;
            browser.load_changes();
            assert!(browser.preview.contains("no base reference"));
        }
    }

    #[test]
    fn content_search_wraps_and_task_filter_keeps_archived_identity() {
        let mut browser = Browser {
            records: vec![row("one"), row("two")],
            selected: Some("two".into()),
            detail: Some(detail("two")),
            find: "Retained finding".into(),
            ..Default::default()
        };
        browser.query = "completed task-two".into();
        assert_eq!(browser.rows().len(), 1);
        browser.query = "task-two".into();
        assert_eq!(browser.rows()[0].key, "two");
        browser.find_next(false);
        assert!(browser.offset > 0);
        let offset = browser.offset;
        browser.find_next(false);
        assert_eq!(browser.offset, offset);
        assert_eq!(browser.selected.as_deref(), Some("two"));
    }

    #[test]
    fn focus_cycles_in_both_directions_and_never_stays_in_a_hidden_entry_list() {
        let mut browser = Browser {
            tab: 3,
            ..Default::default()
        };
        browser.cycle_focus(false);
        assert_eq!(browser.focus, Focus::Entries);
        browser.cycle_focus(true);
        assert_eq!(browser.focus, Focus::Tasks);
        browser.cycle_focus(true);
        assert_eq!(browser.focus, Focus::Content);
        browser.focus = Focus::Entries;
        browser.set_tab(0);
        assert_eq!(browser.focus, Focus::Content);
        browser.cycle_focus(true);
        assert_eq!(browser.focus, Focus::Tasks);
    }
}
