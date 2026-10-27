use crate::interaction::{self, Hits};
use crate::{model::Run, tmux, ui::sanitize_history};
use crossterm::{
    event::{
        self, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, List, ListState, Paragraph, Wrap},
};
use serde::Deserialize;
use std::{io, path::Path, process::Command, time::Duration};

#[derive(Clone, Debug, Deserialize)]
struct Record {
    key: String,
    source: String,
    workspace: String,
    session: String,
    task: Option<String>,
    segment: Option<usize>,
    modified: String,
    note: String,
}

#[derive(Deserialize)]
struct Catalog {
    version: usize,
    project: String,
    records: Vec<Record>,
    complete: bool,
    warnings: Vec<String>,
}

#[derive(Deserialize)]
struct Detail {
    version: usize,
    project: String,
    record: Record,
    conversation: String,
    events: String,
    complete: bool,
    warnings: Vec<String>,
}

fn read(project: &str, key: Option<&str>) -> Result<serde_json::Value, String> {
    let mut command = Command::new(Path::new(project).join("sessions/bin/sessions"));
    command
        .current_dir(project)
        .env("CLAUDE_PROJECT_DIR", project)
        .args(["logs", "history", "--json"]);
    if let Some(key) = key {
        command.args(["--record", key]);
    }
    let text = tmux::output(command)?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| format!("Invalid history JSON; update this project's installed CLI: {e}"))?;
    if let Some(error) = value.get("error") {
        return Err(format!(
            "History unavailable: {}. Update the installed CLI if history is unsupported.",
            error.as_str().unwrap_or("Invalid history response")
        ));
    }
    if value.get("version").and_then(|v| v.as_u64()) != Some(1) {
        return Err("Unsupported history response; update this project's installed CLI.".into());
    }
    Ok(value)
}

pub fn review_preview(
    cli_root: &str,
    project: &str,
    key: &str,
    events: bool,
) -> Result<String, String> {
    let detail: Detail =
        serde_json::from_value(read(cli_root, Some(key))?).map_err(|e| e.to_string())?;
    if detail.version != 1 || detail.project != project || detail.record.key != key {
        return Err("History identity changed; refresh and select again".into());
    }
    Ok(sanitize_history(&format!(
        "{} · {} · {}\n{}\n\n{}\n\n{}",
        detail.record.source,
        detail.record.session,
        detail.record.modified,
        detail.record.note,
        if events {
            detail.events
        } else {
            detail.conversation
        },
        detail.warnings.join("\n")
    )))
}

#[derive(Default)]
struct Browser {
    records: Vec<Record>,
    selected: Option<String>,
    project: String,
    filter: String,
    filtering: bool,
    finding: bool,
    query: String,
    events: bool,
    detail: Option<Detail>,
    message: String,
    offset: usize,
    horizontal: u16,
}

impl Browser {
    fn filtered(&self) -> Vec<&Record> {
        let needle = self.filter.to_lowercase();
        self.records
            .iter()
            .filter(|r| {
                format!(
                    "{} {} {} {} {}",
                    r.session,
                    r.workspace,
                    r.task.as_deref().unwrap_or(""),
                    r.source,
                    r.note
                )
                .to_lowercase()
                .contains(&needle)
            })
            .collect()
    }

    fn select(&mut self, delta: isize) {
        let rows = self.filtered();
        let old = rows
            .iter()
            .position(|r| Some(&r.key) == self.selected.as_ref());
        let index = old
            .unwrap_or(0)
            .saturating_add_signed(delta)
            .min(rows.len().saturating_sub(1));
        self.selected = rows.get(index).map(|r| r.key.clone());
        self.detail = None;
        self.offset = 0;
        self.horizontal = 0;
    }

    fn reload(&mut self, project: &str, first: bool) {
        let previous = self.selected.clone();
        self.records.clear();
        self.detail = None;
        self.selected = None;
        self.offset = 0;
        self.horizontal = 0;
        self.message.clear();
        match read(project, None)
            .and_then(|v| serde_json::from_value::<Catalog>(v).map_err(|e| e.to_string()))
        {
            Ok(catalog) if catalog.version == 1 => {
                self.project = catalog.project;
                self.records = catalog.records;
                self.message = catalog.warnings.join(" · ");
                if !catalog.complete && self.message.is_empty() {
                    self.message = "History listing is incomplete.".into();
                }
                if previous
                    .as_ref()
                    .is_some_and(|key| self.records.iter().any(|r| &r.key == key))
                {
                    self.selected = previous;
                } else if first {
                    self.select(0);
                } else if previous.is_some() {
                    self.message =
                        "Record disappeared/replaced. Select a current entry explicitly.".into();
                }
            }
            Ok(_) => self.message = "Unsupported history catalog.".into(),
            Err(error) => {
                self.project = project.into();
                self.message = error;
            }
        }
        self.message = sanitize_history(&self.message);
    }

    fn load(&mut self, project: &str) {
        self.detail = None;
        let Some(key) = &self.selected else {
            return;
        };
        match read(project, Some(key))
            .and_then(|v| serde_json::from_value::<Detail>(v).map_err(|e| e.to_string()))
        {
            Ok(mut detail)
                if detail.version == 1
                    && &detail.record.key == key
                    && detail.project == self.project =>
            {
                detail.conversation = sanitize_history(&detail.conversation);
                detail.events = sanitize_history(&detail.events);
                if !detail.warnings.is_empty() {
                    let notes = sanitize_history(&format!(
                        "\n\nRead warnings:\n{}",
                        detail.warnings.join("\n")
                    ));
                    detail.conversation.push_str(&notes);
                    detail.events.push_str(&notes);
                }
                self.message.clear();
                self.detail = Some(detail);
            }
            Ok(_) => self.message = "History identity mismatch; refresh and select again.".into(),
            Err(error) => self.message = error,
        }
        self.message = sanitize_history(&self.message);
    }

    fn text(&self) -> &str {
        self.detail
            .as_ref()
            .map(|d| {
                if self.events {
                    d.events.as_str()
                } else {
                    d.conversation.as_str()
                }
            })
            .unwrap_or(&self.message)
    }

    fn matches(&self) -> Vec<usize> {
        if self.query.is_empty() {
            return Vec::new();
        }
        let needle = self.query.to_lowercase();
        self.text()
            .lines()
            .enumerate()
            .filter_map(|(i, l)| l.to_lowercase().contains(&needle).then_some(i))
            .collect()
    }

    fn find(&mut self, backwards: bool) {
        let matches = self.matches();
        let next = if backwards {
            matches
                .iter()
                .rev()
                .find(|&&line| line < self.offset)
                .or_else(|| matches.last())
        } else {
            matches
                .iter()
                .find(|&&line| line > self.offset)
                .or_else(|| matches.first())
        };
        if let Some(&line) = next {
            self.reveal(line);
        }
    }

    fn reveal(&mut self, line: usize) {
        let column = self
            .text()
            .lines()
            .nth(line)
            .and_then(|text| {
                let lower = text.to_lowercase();
                lower
                    .find(&self.query.to_lowercase())
                    .map(|index| Line::from(&lower[..index]).width())
            })
            .unwrap_or(0);
        self.offset = line;
        self.horizontal = column.saturating_sub(8).min(u16::MAX as usize) as u16;
    }

    fn first_match(&mut self) {
        self.reveal(self.matches().first().copied().unwrap_or(0));
    }
}

#[derive(Clone)]
enum HistoryHit {
    Record(String),
    Text,
}

fn draw(frame: &mut Frame, browser: &Browser, root: &str) -> Hits<HistoryHit> {
    let mut hits = Hits::new(frame.area());
    let rows = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(1),
        Constraint::Length(6),
    ])
    .split(crate::presentation::body(frame.area()));
    let columns =
        Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).split(rows[1]);
    frame.render_widget(
        Paragraph::new(sanitize_history(&format!(
            "Project: {root}\nRecorded history; no terminal input or lifecycle actions."
        )))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Saved session history — READ ONLY"),
        ),
        rows[0],
    );
    let filtered = browser.filtered();
    let gap = crate::presentation::list_gap(frame.area());
    let items = filtered
        .iter()
        .map(|r| {
            crate::presentation::separated(
                vec![
                    Line::from(sanitize_history(&format!(
                        "{} · {}",
                        r.task.as_deref().unwrap_or("task unknown"),
                        r.source
                    ))),
                    Line::from(sanitize_history(&format!(
                        "{}{}",
                        r.session,
                        r.segment.map(|n| format!(" / s{n}")).unwrap_or_default()
                    ))),
                    Line::from(sanitize_history(&r.modified)),
                ],
                columns[0].width.saturating_sub(2),
                gap,
            )
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(
        filtered
            .iter()
            .position(|r| Some(&r.key) == browser.selected.as_ref()),
    );
    let title = if browser.filtering {
        format!("Filter: {}", browser.filter)
    } else {
        format!("Records · / filter: {}", browser.filter)
    };
    frame.render_stateful_widget(
        List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(sanitize_history(&title)),
            )
            .highlight_style(crate::presentation::selected()),
        columns[0],
        &mut state,
    );
    hits.list_gaps(
        Block::default().borders(Borders::ALL).inner(columns[0]),
        state.offset(),
        &filtered
            .iter()
            .map(|record| (3, gap, HistoryHit::Record(record.key.clone())))
            .collect::<Vec<_>>(),
    );
    hits.add(columns[1], HistoryHit::Text);
    let matches = browser.matches();
    let lines: Vec<_> = browser
        .text()
        .lines()
        .enumerate()
        .skip(browser.offset)
        .take(usize::from(columns[1].height.saturating_sub(2)))
        .map(|(i, line)| {
            Line::styled(
                line,
                if matches.binary_search(&i).is_ok() {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                },
            )
        })
        .collect();
    let title = format!(
        "{} · {} matches · line {}",
        if browser.events {
            "Tool events"
        } else {
            "Conversation"
        },
        matches.len(),
        browser.offset + 1
    );
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((0, browser.horizontal))
            .block(Block::default().borders(Borders::ALL).title(title)),
        columns[1],
    );
    let warnings = browser
        .detail
        .as_ref()
        .map(|d| {
            let partial = if d.complete { "" } else { "PARTIAL · " };
            format!("{partial}{} · {}", d.record.note, d.warnings.join(" · "))
        })
        .unwrap_or_else(|| {
            if browser.records.is_empty() {
                "No saved records available. Check the project or installed CLI.".into()
            } else {
                "Select a record with arrows.".into()
            }
        });
    let source = browser
        .detail
        .as_ref()
        .map(|d| d.record.workspace.as_str())
        .unwrap_or("");
    let text = if browser.finding {
        format!(
            "Find in displayed preview: {}\nEnter search · Esc finish",
            browser.query
        )
    } else if browser.filtering {
        format!(
            "Filter records: {}\nEnter/Esc finish filter",
            browser.filter
        )
    } else {
        "↑/↓ records · ←/→ project · / filter · f find · n/N next/previous\nTab conversation/events · PgUp/PgDn scroll · Home/End · h/l sideways · r refresh · Esc close".into()
    };
    frame.render_widget(
        Paragraph::new(sanitize_history(&format!(
            "{text}\nSource: {source}\n{warnings}\n{}",
            browser.message
        )))
        .wrap(Wrap { trim: false }),
        rows[2],
    );
    hits
}

pub fn panel(path: &Path) -> Result<(), String> {
    let run: Run = crate::runtime::read_run(path)?;
    if run.projects.is_empty() {
        return Err("No registered projects to read history from.".into());
    }
    let current = run
        .targets
        .iter()
        .find(|t| Some(&t.key) == run.selected.as_ref())
        .and_then(|t| t.project.as_ref());
    let mut project = run
        .projects
        .iter()
        .position(|root| Some(root) == current)
        .unwrap_or(0);
    let mut browser = Browser::default();
    let _restore = crate::ui::Restore;
    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    browser.reload(&run.projects[project], true);
    browser.load(&run.projects[project]);
    let mut hits = Hits::new(ratatui::layout::Rect::default());
    let mut chrome = crate::presentation::Chrome::default();
    loop {
        // Reads are snapshots. Refresh is explicit and never retargets a terminal.
        terminal
            .draw(|frame| {
                hits = draw(frame, &browser, &run.projects[project]);
                chrome.draw(frame, "Saved session history · Read only");
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
            return Ok(());
        }
        if let Event::Mouse(mouse) = input {
            let size = terminal.size().map_err(|e| e.to_string())?;
            let current = ratatui::layout::Rect::new(0, 0, size.width, size.height);
            if hits.frame != current {
                continue;
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some(HistoryHit::Record(key)) = hits.get(mouse, current) {
                    browser.selected = Some(key.clone());
                    browser.offset = 0;
                    browser.horizontal = 0;
                    browser.filtering = false;
                    browser.finding = false;
                    browser.load(&run.projects[project]);
                }
            } else if let Some(delta) = interaction::wheel(mouse.kind) {
                if matches!(hits.get(mouse, current), Some(HistoryHit::Text)) {
                    browser.offset = browser
                        .offset
                        .saturating_add_signed(delta)
                        .min(browser.text().lines().count().saturating_sub(1));
                } else if hits.get(mouse, current).is_some() {
                    browser.select(delta);
                    browser.load(&run.projects[project]);
                }
            }
            continue;
        }
        let Event::Key(key) = input else {
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(());
        }
        if browser.filtering || browser.finding {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    let filtered = browser.filtering;
                    browser.filtering = false;
                    browser.finding = false;
                    if filtered {
                        browser.load(&run.projects[project]);
                    }
                }
                KeyCode::Backspace => {
                    if browser.filtering {
                        browser.filter.pop();
                        browser.select(0);
                    } else {
                        browser.query.pop();
                        browser.first_match();
                    }
                }
                KeyCode::Char(ch)
                    if !ch.is_control()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if browser.filtering && browser.filter.len() < 256 {
                        browser.filter.push(ch);
                        browser.select(0);
                    } else if browser.finding && browser.query.len() < 256 {
                        browser.query.push(ch);
                        browser.first_match();
                    }
                }
                _ => {}
            }
            continue;
        }
        let page = usize::from(
            terminal
                .size()
                .map_err(|e| e.to_string())?
                .height
                .saturating_sub(13),
        )
        .max(1);
        let end = browser.text().lines().count().saturating_sub(page);
        match key.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => {
                browser.select(-1);
                browser.load(&run.projects[project]);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                browser.select(1);
                browser.load(&run.projects[project]);
            }
            KeyCode::Left | KeyCode::Right => {
                let next = if key.code == KeyCode::Right {
                    (project + 1) % run.projects.len()
                } else {
                    (project + run.projects.len() - 1) % run.projects.len()
                };
                if next != project {
                    project = next;
                    browser = Browser::default();
                    browser.reload(&run.projects[project], true);
                    browser.load(&run.projects[project]);
                }
            }
            KeyCode::Char('/') => browser.filtering = true,
            KeyCode::Char('f') => browser.finding = true,
            KeyCode::Char('n') => browser.find(false),
            KeyCode::Char('N') => browser.find(true),
            KeyCode::Tab => {
                browser.events = !browser.events;
                browser.offset = 0;
                browser.horizontal = 0;
            }
            KeyCode::PageUp => browser.offset = browser.offset.saturating_sub(page),
            KeyCode::PageDown => {
                browser.offset = browser
                    .offset
                    .saturating_add(page)
                    .min(browser.text().lines().count().saturating_sub(1))
            }
            KeyCode::Home => browser.offset = 0,
            KeyCode::End => browser.offset = end,
            KeyCode::Char('h') => browser.horizontal = browser.horizontal.saturating_sub(8),
            KeyCode::Char('l') => browser.horizontal = browser.horizontal.saturating_add(8),
            KeyCode::Char('r') => {
                browser.reload(&run.projects[project], false);
                browser.load(&run.projects[project]);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn fixture() -> Browser {
        let record: Record = serde_json::from_value(serde_json::json!({"key":"one","source":"native","workspace":"/project/task",
            "session":"saved-session","task":"TASK-1","segment":null,"modified":"2026-10-15","note":"Recorded"})).unwrap();
        Browser {
            records: vec![record.clone()],
            selected: Some(record.key.clone()),
            detail: Some(Detail {
                version: 1,
                project: "/project".into(),
                record,
                conversation: "old line\nUnicode 世界\nlast line 世界\n".into(),
                events: "tool event".into(),
                complete: true,
                warnings: vec![],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn filtering_and_search_keep_record_identity_and_independent_scroll() {
        let mut browser = fixture();
        browser.filter = "task-1".into();
        assert_eq!(browser.filtered()[0].key, "one");
        browser.query = "世界".into();
        browser.find(false);
        assert_eq!(browser.offset, 1);
        browser.find(false);
        assert_eq!(browser.offset, 2);
        browser.find(false);
        assert_eq!(browser.offset, 1);
        browser.find(true);
        assert_eq!(browser.offset, 2);
        browser.events = true;
        assert!(browser.matches().is_empty());
        browser.filter = "not-found".into();
        browser.select(0);
        assert!(browser.selected.is_none());
        assert!(browser.detail.is_none());
    }

    #[test]
    fn native_panel_shows_provenance_search_and_read_only_controls() {
        let mut browser = fixture();
        browser.query = "世界".into();
        browser.offset = 1;
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &browser, "/project");
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("READ ONLY"));
        assert!(text.contains("TASK-1"));
        assert!(text.contains("2 matches"));
        assert!(text.contains("Unicode 世"));
        assert!(text.contains('界'));
        assert!(!text.contains("old line"));
        assert!(text.contains("r refresh"));
    }

    #[test]
    fn search_reveals_matches_beyond_the_visible_columns_with_unicode_widths() {
        let mut browser = fixture();
        browser.detail.as_mut().unwrap().events = format!("{}needle", "界".repeat(80));
        browser.events = true;
        browser.query = "needle".into();
        browser.first_match();
        assert_eq!(browser.offset, 0);
        assert_eq!(browser.horizontal, 152);
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &browser, "/project");
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("needle"));
    }
}
