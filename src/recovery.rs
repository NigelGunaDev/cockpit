//! Native session inventory and controls. Bundled native operations perform process restoration.
use crate::{
    agents::{self, Identity, Kind},
    model::{Run, Target},
    observe,
    session_registry::{Entry, Registry},
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub jobs: Vec<crate::restore::Job>,
    #[serde(default)]
    pub slots: Vec<Option<String>>,
    #[serde(default)]
    pub slot_targets: Vec<String>,
    #[serde(default)]
    pub user_navigated: bool,
    pub path: String,
    pub entries: Vec<Entry>,
    pub opened: Vec<String>,
    pub pending: Vec<Pending>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pending {
    pub key: String,
    pub kind: Kind,
    pub reason: String,
}
#[derive(Serialize, Deserialize)]
pub struct Link {
    pub key: String,
    pub identity: Identity,
}
#[derive(Serialize, Deserialize)]
pub struct Forget {
    pub id: String,
    pub revision: i64,
}
#[derive(Serialize, Deserialize)]
pub struct Close {
    pub id: String,
    pub revision: i64,
    pub key: Option<String>,
}

pub fn read_inventory(state: &mut State) {
    match Registry::open(Path::new(&state.path)).and_then(|db| db.list()) {
        Ok(entries) => {
            state.entries = entries;
            state.error = None;
        }
        Err(e) => state.error = Some(e),
    }
}
pub fn missing_server(socket: &str, error: &str) -> bool {
    match std::fs::symlink_metadata(socket) {
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        Ok(_) => {
            observe::own_socket(socket).is_ok()
                && (error.contains("no server running")
                    || error.contains("Connection refused")
                    || error.contains("failed to connect"))
        }
    }
}
pub fn logical_id(run: &Run, target: &Target) -> Option<String> {
    entry(run, target)
        .map(|e| e.id.clone())
        .or_else(|| {
            run.recovery
                .jobs
                .iter()
                .find(|j| {
                    j.target
                        .as_ref()
                        .is_some_and(|t| t.key == target.key && t.socket == target.socket)
                })
                .map(|j| j.id.clone())
        })
        .or_else(|| {
            run.recovery
                .slot_targets
                .iter()
                .position(|key| key == &target.key)
                .and_then(|i| run.recovery.slots.get(i).cloned().flatten())
        })
}

pub fn refresh(run: &mut Run) {
    if run.recovery.path.is_empty() {
        return;
    }
    match Registry::open(Path::new(&run.recovery.path)).and_then(|db| db.list()) {
        Ok(entries) => {
            run.recovery.entries = entries;
            run.recovery.error = None;
        }
        Err(error) => run.recovery.error = Some(error),
    }
}
pub fn entry<'a>(run: &'a Run, target: &Target) -> Option<&'a Entry> {
    run.recovery.entries.iter().find(|e| e.matches(target))
}
pub fn badge(run: &Run, target: &Target) -> String {
    if run.recovery.error.is_some() {
        return "registry unavailable".into();
    }
    if let Some(job) = run.recovery.jobs.iter().find(|j| {
        !j.loaded
            && j.target
                .as_ref()
                .is_some_and(|t| t.key == target.key && t.socket == target.socket)
    }) {
        return format!("restore {} · load unconfirmed", job.status);
    }
    if let Some(e) = entry(run, target) {
        return format!(
            "{} · {}",
            e.snapshot.identity.kind.label(),
            if e.enabled { "remembered" } else { "forgotten" }
        );
    }
    match run.recovery.pending.iter().find(|p| p.key == target.key) {
        Some(p) => format!("{} · not verified", p.kind.label()),
        None => "not registered".into(),
    }
}
fn target(run: &Run, key: &str) -> Result<Target, String> {
    let value = run
        .targets
        .iter()
        .find(|t| t.key == key)
        .cloned()
        .ok_or("Terminal changed; select it again")?;
    observe::validate(&value, false)?;
    Ok(value)
}

/// Called only for an actual connection or explicit Remember, not discovery.
pub fn connected(run: &mut Run, key: &str, explicitly_enable: bool) -> Result<(), String> {
    if run.recovery.path.is_empty() {
        return Ok(());
    }
    let chosen = target(run, key)?;
    if !explicitly_enable
        && logical_id(run, &chosen).is_some_and(|id| {
            run.recovery
                .entries
                .iter()
                .any(|e| e.id == id && !e.matches(&chosen))
        })
    {
        return Ok(());
    }
    if !explicitly_enable
        && run
            .recovery
            .jobs
            .iter()
            .any(|j| !j.loaded && j.target.as_ref().is_some_and(|t| t.key == key))
    {
        return Ok(());
    }
    if explicitly_enable && let Some(old) = entry(run, &chosen) {
        let identity = old.snapshot.identity.clone();
        let current_kind = agents::kind(&chosen);
        if current_kind != Kind::Unknown && current_kind != identity.kind {
            return Err(
                "The terminal is running a different agent; review its native link again".into(),
            );
        }
        if let Ok(current) = agents::identify(&chosen)
            && current != identity
        {
            return Err("The active conversation changed; link its native file again".into());
        }
        let verified = agents::from_file(
            identity.kind,
            Path::new(&identity.native_file),
            Path::new(&chosen.cwd),
        )?;
        if verified != identity {
            return Err("Saved conversation changed; link its native file again".into());
        }
        save(run, &chosen, identity, true)?;
        if !run.recovery.opened.iter().any(|opened| opened == key) {
            run.recovery.opened.push(key.into());
        }
        return Ok(());
    }
    if !explicitly_enable && entry(run, &chosen).is_some_and(|e| !e.enabled) {
        return Ok(()); // Forget remains in force until explicit Remember/link.
    }
    if !run.recovery.opened.iter().any(|k| k == key) {
        if run.recovery.opened.len() >= 256 {
            return Err("Open-session inventory is full".into());
        }
        run.recovery.opened.push(key.into());
    }
    if !explicitly_enable && run.recovery.pending.iter().any(|p| p.key == key) {
        return Ok(()); // Slow tick/explicit Remember retries unavailable identity, not every switch.
    }
    if !explicitly_enable
        && let Some(e) = entry(run, &chosen)
        && (e.snapshot.identity.kind != Kind::Claude
            || chosen
                .claude_session_id
                .as_deref()
                .is_none_or(|id| id == e.snapshot.identity.native_session_id))
    {
        return Ok(());
    }
    match agents::identify(&chosen) {
        Ok(identity) => save(run, &chosen, identity, explicitly_enable),
        Err(reason) => {
            run.recovery.pending.retain(|p| p.key != key);
            run.recovery.pending.push(Pending {
                key: key.into(),
                kind: agents::kind(&chosen),
                reason: reason.clone(),
            });
            if explicitly_enable {
                Err(reason)
            } else {
                Ok(())
            }
        }
    }
}
fn save(run: &mut Run, chosen: &Target, identity: Identity, enable: bool) -> Result<(), String> {
    // Recheck after potentially slow native-file validation, before registration.
    observe::validate(chosen, false)?;
    let old_id = run
        .recovery
        .entries
        .iter()
        .find(|e| e.snapshot.identity == identity)
        .map(|e| e.id.clone());
    let _lock = old_id
        .map(|id| crate::session_registry::operation_lock(Path::new(&run.recovery.path), &id))
        .transpose()?;
    let result = Registry::open(Path::new(&run.recovery.path))
        .and_then(|mut db| db.remember(identity, chosen, enable));
    match result {
        Ok(_) => {
            run.recovery.pending.retain(|p| p.key != chosen.key);
            refresh(run);
            Ok(())
        }
        Err(error) => {
            run.recovery.error = Some(error.clone());
            Err(error)
        }
    }
}
pub fn link(run: &mut Run, request: Link) -> Result<(), String> {
    let chosen = target(run, &request.key)?;
    let kind = agents::kind(&chosen);
    if kind != Kind::Unknown && kind != request.identity.kind {
        return Err("The selected terminal is running a different agent".into());
    }
    let verified = agents::from_file(
        request.identity.kind,
        Path::new(&request.identity.native_file),
        Path::new(&chosen.cwd),
    )?;
    if verified != request.identity {
        return Err("Native session changed; review the link again".into());
    }
    save(run, &chosen, verified, true)?;
    if !run.recovery.opened.contains(&chosen.key) {
        run.recovery.opened.push(chosen.key);
    }
    Ok(())
}
pub fn forget(run: &mut Run, request: Forget) -> Result<(), String> {
    let old = run
        .recovery
        .entries
        .iter()
        .find(|e| e.id == request.id && e.revision == request.revision)
        .ok_or("Registry selection changed; refresh before forgetting")?;
    if old.snapshot.target.socket != run.original_socket {
        return Err("Session belongs to another tmux socket".into());
    }
    let key = old.snapshot.target.key.clone();
    let _lock = crate::session_registry::operation_lock(Path::new(&run.recovery.path), &old.id)?;
    Registry::open(Path::new(&run.recovery.path))?.forget(&request.id, request.revision)?;
    run.recovery.opened.retain(|opened| opened != &key);
    refresh(run);
    Ok(())
}

/// Retry only connections awaiting identity; no native-store scans on paint or
/// every tmux poll. Known entries are revisited only after observed ID changes.
pub fn reconcile(run: &mut Run) {
    refresh(run);
    if run.recovery.error.is_some() {
        return;
    }
    for key in run.recovery.opened.clone() {
        let Some(t) = run.targets.iter().find(|t| t.key == key) else {
            continue;
        };
        if let Some(e) = entry(run, t)
            && (e.snapshot.identity.kind != Kind::Claude
                || t.claude_session_id
                    .as_deref()
                    .is_none_or(|id| id == e.snapshot.identity.native_session_id))
        {
            continue;
        }
        run.recovery.pending.retain(|p| p.key != key);
        let _ = connected(run, &key, false);
    }
}

// The native panel reads run snapshots; filesystem/SQLite work happens in the
// supervisor only on explicit actions and the existing slow reconciliation tick.
pub fn panel(path: &Path) -> Result<(), String> {
    use crossterm::{
        event::{
            self, EnableBracketedPaste, EnableMouseCapture, Event, KeyCode, KeyEventKind,
            KeyModifiers, MouseButton, MouseEventKind,
        },
        terminal::{EnterAlternateScreen, enable_raw_mode},
    };
    use ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::{Constraint, Layout},
        text::Line,
        widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    };
    use std::{io, time::Duration};
    #[derive(Clone)]
    struct Item {
        key: String,
        label: String,
        detail: String,
        live: Option<Target>,
        saved: Option<Entry>,
    }
    fn items(run: &Run) -> Vec<Item> {
        let mut out = Vec::new();
        for t in &run.targets {
            let saved = entry(run, t).cloned().or_else(|| {
                logical_id(run, t)
                    .and_then(|id| run.recovery.entries.iter().find(|e| e.id == id).cloned())
            });
            let detail = saved.as_ref().map(|e| format!("Agent: {}\nNative ID: {}\nFile: {}\nWorkspace: {}\nRestore intent: {}\nLive terminal; Forget does not stop it.",e.snapshot.identity.kind.label(),e.snapshot.identity.native_session_id,e.snapshot.identity.native_file,t.workspace.as_deref().unwrap_or(&t.cwd),if e.enabled { "remembered" } else { "disabled" }))
                .unwrap_or_else(|| format!("Workspace: {}\n{}\n\nUse r to verify native identity, or l to link its existing native session file. Highlighting does not register a session.",t.cwd,run.recovery.pending.iter().find(|p|p.key==t.key).map(|p|p.reason.as_str()).unwrap_or("Not registered.")));
            out.push(Item {
                key: format!("live:{}", t.key),
                label: format!("{} · {}", t.session_name, badge(run, t)),
                detail,
                live: Some(t.clone()),
                saved,
            });
        }
        for e in &run.recovery.entries {
            if e.snapshot.target.socket != run.original_socket
                || run
                    .targets
                    .iter()
                    .any(|t| logical_id(run, t).as_deref() == Some(&e.id))
            {
                continue;
            }
            out.push(Item { key:format!("saved:{}",e.id), label:format!("{} · {} · {} · offline",e.snapshot.target.session_name,e.snapshot.identity.kind.label(),if e.enabled { "remembered" } else { "forgotten" }), detail:format!("Native ID: {}\nFile: {}\nWorkspace: {}\nNo live matching terminal. Saved intent is retained; missing does not mean closed.",e.snapshot.identity.native_session_id,e.snapshot.identity.native_file,e.snapshot.target.workspace.as_deref().unwrap_or(&e.snapshot.target.cwd)), live:None, saved:Some(e.clone()) });
        }
        out
    }
    struct Form {
        key: String,
        target_label: String,
        kind: Kind,
        file: String,
        verified: Option<Identity>,
    }
    let _restore = crate::ui::Restore;
    enable_raw_mode().map_err(|e| e.to_string())?;
    crossterm::execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    let mut selected: Option<String> = None;
    let mut message = String::new();
    let mut form: Option<Form> = None;
    let mut closing: Option<(Close, String, String)> = None;
    let mut chrome = crate::presentation::Chrome::default();
    let mut hits = crate::interaction::Hits::new(ratatui::layout::Rect::default());
    loop {
        let run = crate::runtime::read_run(path)?;
        let rows = items(&run);
        if selected.is_none() {
            selected = rows.first().map(|i| i.key.clone());
        }
        let index = rows.iter().position(|i| Some(&i.key) == selected.as_ref());
        terminal.draw(|frame| {
            let areas = Layout::vertical([Constraint::Length(4),Constraint::Min(1),Constraint::Length(5)]).split(crate::presentation::body(frame.area()));
            frame.render_widget(Paragraph::new("Remembered sessions\nRestoration submits no prompt; trust remains yours.").block(Block::default().borders(Borders::ALL)),areas[0]);
            hits = crate::interaction::Hits::new(frame.area());
            if let Some((_, terminal_name, native_id)) = &closing {
                let text = format!("Close this agent terminal?\n\nTerminal: {terminal_name}\nNative conversation: {native_id}\n\nThis forgets its restore intent, then stops only the exact foreground agent session.\nFiles, worktrees, branches, native history and task state remain.\n\nEnter confirms · Esc cancels");
                frame.render_widget(Paragraph::new(crate::ui::sanitize_history(&text)).wrap(Wrap{trim:false}).block(Block::default().borders(Borders::ALL).title("Close terminal")),areas[1]);
            } else if let Some(f) = &form {
                let text = if let Some(id)=&f.verified { format!("Confirm linking this conversation to the selected terminal\n\nTerminal: {}\nAgent: {}\nNative ID: {}\nFile: {}\n\nEnter confirms · Esc cancels",f.target_label,id.kind.label(),id.native_session_id,id.native_file) }
                    else { format!("Link an existing native conversation\n\nTerminal: {}\nAgent: {} (Tab changes)\nAbsolute session file: {}\n\nPaste the native JSONL file path. Enter reviews; Esc cancels.\nNo agent is started and no conversation is created.",f.target_label,f.kind.label(),f.file) };
                frame.render_widget(Paragraph::new(crate::ui::sanitize_history(&text)).wrap(Wrap{trim:false}).block(Block::default().borders(Borders::ALL).title("Native session link")),areas[1]);
            } else {
                let split=Layout::horizontal([Constraint::Percentage(43),Constraint::Min(1)]).split(areas[1]);
                let mut state=ListState::default().with_selected(index);
                frame.render_stateful_widget(List::new(rows.iter().map(|i|ListItem::new(Line::from(observe::sanitize(&i.label))))).block(Block::default().borders(Borders::ALL).title("Live and saved sessions")).highlight_style(crate::presentation::selected()),split[0],&mut state);
                hits.list(Block::default().borders(Borders::ALL).inner(split[0]),state.offset(),&rows.iter().map(|i|(1,i.key.clone())).collect::<Vec<_>>());
                let mut detail = index.map(|i|rows[i].detail.clone()).unwrap_or("Choose a session. No saved selection is redirected after a refresh.".into());
                if let Some(job)=index.and_then(|i|rows[i].saved.as_ref()).and_then(|e|run.recovery.jobs.iter().find(|j|j.id==e.id)) {detail.push_str(&format!("\n\nRestoration: {}\n{}",job.status,job.message));}
                frame.render_widget(Paragraph::new(crate::ui::sanitize_history(&detail)).wrap(Wrap{trim:false}).block(Block::default().borders(Borders::ALL).title("Session details")),split[1]);
            }
            frame.render_widget(Paragraph::new(format!("↑/↓ select · r remember · l link · f forget · t retry restore · c close terminal · u refresh · Esc close\n{}\n{}",run.recovery.error.as_deref().unwrap_or("Remove tile and Detach keep saved intent. Forget leaves the agent running."),crate::ui::sanitize_history(&message))).wrap(Wrap{trim:false}),areas[2]);
            chrome.draw(frame,"Session memory");
        }).map_err(|e|e.to_string())?;
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
        if let Event::Mouse(mouse) = &input {
            if form.is_none()
                && closing.is_none()
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some(key) = hits.get(
                    *mouse,
                    ratatui::layout::Rect::new(0, 0, size.width, size.height),
                )
            {
                selected = Some(key.clone());
            }
            continue;
        }
        if let Event::Paste(text) = &input {
            if let Some(f) = &mut form
                && f.verified.is_none()
                && f.file.len() + text.len() <= 2048
                && !text.chars().any(char::is_control)
            {
                f.file.push_str(text);
            }
            continue;
        }
        let Event::Key(key) = input else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(());
        }
        if let Some((request, terminal_name, native_id)) = closing.take() {
            match key.code {
                KeyCode::Enter => {
                    message = crate::runtime::request(
                        path,
                        "registry-close",
                        Some(&serde_json::to_string(&request).map_err(|e| e.to_string())?),
                    )
                    .err()
                    .unwrap_or("Close queued. Saved intent is disabled before termination.".into());
                }
                KeyCode::Esc => {}
                _ => closing = Some((request, terminal_name, native_id)),
            }
            continue;
        }
        if form.is_none() && key.code == KeyCode::Esc {
            return Ok(());
        }
        if let Some(f) = &mut form {
            match key.code {
                KeyCode::Esc => {
                    form = None;
                    message.clear();
                }
                KeyCode::Tab if f.verified.is_none() => f.kind = f.kind.next(),
                KeyCode::Backspace if f.verified.is_none() => {
                    f.file.pop();
                }
                KeyCode::Char(c)
                    if f.verified.is_none()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && f.file.len() < 2048 =>
                {
                    f.file.push(c)
                }
                KeyCode::Enter => {
                    if let Some(identity) = f.verified.clone() {
                        let payload = serde_json::to_string(&Link {
                            key: f.key.clone(),
                            identity,
                        })
                        .map_err(|e| e.to_string())?;
                        match crate::runtime::request(path, "registry-link", Some(&payload)) {
                            Ok(()) => {
                                form = None;
                                message="Native conversation linked and remembered. No process started.".into();
                            }
                            Err(e) => {
                                f.verified = None;
                                message = e;
                            }
                        }
                    } else if let Some(t) = run.targets.iter().find(|t| t.key == f.key) {
                        match agents::from_file(f.kind, Path::new(&f.file), Path::new(&t.cwd)) {
                            Ok(identity) => {
                                f.verified = Some(identity);
                                message.clear();
                            }
                            Err(e) => message = e,
                        }
                    } else {
                        form = None;
                        message = "Terminal changed; choose it again.".into();
                    }
                }
                _ => {}
            }
            continue;
        }
        let item = index.map(|i| rows[i].clone());
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                if !rows.is_empty() {
                    let i = index.unwrap_or(0);
                    let i = if key.code == KeyCode::Up {
                        i.saturating_sub(1)
                    } else {
                        (i + 1).min(rows.len() - 1)
                    };
                    selected = Some(rows[i].key.clone());
                }
            }
            KeyCode::Char('t') | KeyCode::Char('c') => {
                if let Some(item) = item
                    && let Some(saved) = item.saved
                {
                    let terminal_name = item
                        .live
                        .as_ref()
                        .map(|t| t.session_name.clone())
                        .unwrap_or_default();
                    let native_id = saved.snapshot.identity.native_session_id.clone();
                    let request = Close {
                        id: saved.id,
                        revision: saved.revision,
                        key: item.live.map(|t| t.key),
                    };
                    if key.code == KeyCode::Char('c') {
                        if request.key.is_some() {
                            closing = Some((request, terminal_name, native_id));
                        } else {
                            message = "No live terminal to close; use Forget.".into();
                        }
                    } else {
                        message = crate::runtime::request(
                            path,
                            "registry-retry",
                            Some(&serde_json::to_string(&request).map_err(|e| e.to_string())?),
                        )
                        .err()
                        .unwrap_or("Restoration queued.".into());
                    }
                }
            }
            KeyCode::Char('u') => {
                message = crate::runtime::request(path, "registry-refresh", None)
                    .err()
                    .unwrap_or("Registry refreshed.".into());
            }
            KeyCode::Char('r') => {
                if let Some(t) = item.and_then(|i| i.live) {
                    message = crate::runtime::request(path, "registry-remember", Some(&t.key))
                        .err()
                        .unwrap_or("Conversation remembered. No process started.".into());
                } else {
                    message = "Choose a live terminal to verify its native conversation.".into();
                }
            }
            KeyCode::Char('l') => {
                if let Some(t) = item.and_then(|i| i.live) {
                    let kind = entry(&run, &t)
                        .map(|e| e.snapshot.identity.kind)
                        .or_else(|| {
                            run.recovery
                                .pending
                                .iter()
                                .find(|p| p.key == t.key)
                                .map(|p| p.kind)
                        })
                        .unwrap_or(Kind::Claude);
                    form = Some(Form {
                        target_label: format!("{} {}", t.session_name, t.pane_id),
                        key: t.key,
                        kind: if kind == Kind::Unknown {
                            Kind::Claude
                        } else {
                            kind
                        },
                        file: String::new(),
                        verified: None,
                    });
                    message.clear();
                }
            }
            KeyCode::Char('f') => {
                if let Some(e) = item.and_then(|i| i.saved) {
                    let payload = serde_json::to_string(&Forget {
                        id: e.id,
                        revision: e.revision,
                    })
                    .map_err(|e| e.to_string())?;
                    message = crate::runtime::request(path, "registry-forget", Some(&payload))
                        .err()
                        .unwrap_or(
                            "Forgotten for restoration; existing agent is still running.".into(),
                        );
                } else {
                    message = "This terminal has no saved registration to forget.".into();
                }
            }
            _ => {}
        }
    }
}
