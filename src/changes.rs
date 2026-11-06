//! Read-only Git snapshots bound to one exact terminal and repository.
use crate::{model::Target, observe, tmux};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::Read,
    os::unix::{
        ffi::OsStringExt,
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    process::Command,
};

const MAX_FILES: usize = 256;
const MAX_PREVIEW: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub root: PathBuf,
    git_dir: PathBuf,
    root_identity: (u64, u64),
    git_identity: (u64, u64),
}

#[derive(Clone, Debug)]
pub struct FileChange {
    pub status: String,
    path: OsString,
    original: Option<OsString>,
}

impl FileChange {
    pub fn label(&self) -> String {
        format!("{} {:?}", self.status, self.path)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Working,
    Staged,
    Compare {
        reference: String,
        base: String,
    },
}

impl Scope {
    pub fn label(&self) -> String {
        match self {
            Self::Working => "Working tree vs HEAD (includes untracked)".into(),
            Self::Staged => "Staged changes only".into(),
            Self::Compare { reference, base } => format!(
                "Working tree vs merge-base of {reference} ({})",
                &base[..12.min(base.len())]
            ),
        }
    }
}

#[derive(Clone)]
pub struct Snapshot {
    pub repository: Repository,
    pub scope: Scope,
    pub files: Vec<FileChange>,
    pub warning: String,
    pub head: Option<String>,
}

fn git(root: &Path, args: &[&std::ffi::OsStr]) -> Result<Vec<u8>, String> {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args([
            "--no-pager",
            "--no-optional-locks",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "color.ui=false",
        ])
        .args(args);
    // Repository selection comes from the verified cwd, never a caller's Git environment.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(key);
    }
    tmux::output_bytes(command).map_err(|e| format!("Git read failed: {e}"))
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    let args: Vec<_> = args.iter().map(std::ffi::OsStr::new).collect();
    String::from_utf8(git(root, &args)?)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .map_err(|_| "Git returned non-UTF-8 metadata".into())
}

fn repository(cwd: &Path) -> Result<Repository, String> {
    let path = |arg| -> Result<PathBuf, String> {
        let mut bytes = git(cwd, &["rev-parse".as_ref(), arg])?;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        PathBuf::from(OsString::from_vec(bytes))
            .canonicalize()
            .map_err(|e| e.to_string())
    };
    let root = path("--show-toplevel".as_ref())?;
    let git_dir = path("--absolute-git-dir".as_ref())?;
    let identity = |path: &Path| -> Result<_, String> {
        let meta = fs::metadata(path).map_err(|e| e.to_string())?;
        Ok((meta.dev(), meta.ino()))
    };
    Ok(Repository {
        root_identity: identity(&root)?,
        git_identity: identity(&git_dir)?,
        root,
        git_dir,
    })
}

pub fn exact_repository(root: &Path, expected: Option<&Repository>) -> Result<Repository, String> {
    let repo = repository(root)?;
    if repo.root != root.canonicalize().map_err(|e| e.to_string())?
        || expected.is_some_and(|old| old != &repo)
    {
        return Err(
            "Selected repository moved or was replaced; refresh and select it again".into(),
        );
    }
    Ok(repo)
}

pub fn selected_repository(
    target: &Target,
    expected: Option<&Repository>,
) -> Result<Repository, String> {
    let live = observe::targets(&target.socket)?;
    let live = live
        .iter()
        .find(|t| t.key == target.key && t.window_id == target.window_id && !t.dead)
        .ok_or("Terminal exited or was replaced; close this panel and select a live session")?;
    let repo = repository(Path::new(&live.cwd))?;
    if expected.is_some_and(|expected| expected != &repo) {
        return Err(
            "Terminal moved to another repository; close and reopen Changes explicitly".into(),
        );
    }
    Ok(repo)
}

fn status_files(bytes: &[u8]) -> Result<Vec<FileChange>, String> {
    let mut fields = bytes.split(|b| *b == 0).filter(|p| !p.is_empty());
    let mut files = Vec::new();
    while let Some(field) = fields.next() {
        if field.len() < 4 || field[2] != b' ' {
            return Err("Malformed Git status output".into());
        }
        let original = if field[..2].iter().any(|b| b"RC".contains(b)) {
            Some(OsString::from_vec(
                fields.next().ok_or("Missing rename source")?.to_vec(),
            ))
        } else {
            None
        };
        files.push(FileChange {
            status: String::from_utf8_lossy(&field[..2]).into_owned(),
            path: OsString::from_vec(field[3..].to_vec()),
            original,
        });
    }
    Ok(files)
}

fn compared_files(bytes: &[u8]) -> Result<Vec<FileChange>, String> {
    let mut fields = bytes.split(|b| *b == 0).filter(|p| !p.is_empty());
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let path = fields.next().ok_or("Malformed Git comparison output")?;
        files.push(FileChange {
            status: String::from_utf8_lossy(status).into_owned(),
            path: OsString::from_vec(path.to_vec()),
            original: None,
        });
    }
    Ok(files)
}

pub fn comparison(repo: &Repository, reference: &str) -> Result<Scope, String> {
    let reference = reference.trim();
    if reference.is_empty() || reference.len() > 256 || reference.chars().any(char::is_control) {
        return Err("Enter a local branch, tag or commit of 1–256 bytes".into());
    }
    let resolved = git_text(
        &repo.root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    let head = git_text(&repo.root, &["rev-parse", "--verify", "HEAD"])?;
    let base = git_text(&repo.root, &["merge-base", &resolved, &head])?;
    Ok(Scope::Compare {
        reference: reference.into(),
        base,
    })
}

pub fn snapshot(repo: Repository, scope: &Scope) -> Result<Snapshot, String> {
    exact_repository(&repo.root, Some(&repo))?;
    let mut files = status_files(&git(
        &repo.root,
        &[
            "status".as_ref(),
            "--porcelain=v1".as_ref(),
            "-z".as_ref(),
            "--untracked-files=all".as_ref(),
        ],
    )?)?;
    match scope {
        Scope::Working => {}
        Scope::Staged => files.retain(|f| f.status != "??" && !f.status.starts_with(' ')),
        Scope::Compare { base, .. } => {
            let untracked: Vec<_> = files.into_iter().filter(|f| f.status == "??").collect();
            files = compared_files(&git(
                &repo.root,
                &[
                    "diff".as_ref(),
                    "--name-status".as_ref(),
                    "-z".as_ref(),
                    "--no-renames".as_ref(),
                    "--no-ext-diff".as_ref(),
                    "--no-textconv".as_ref(),
                    base.as_ref(),
                    "--".as_ref(),
                ],
            )?)?;
            files.extend(untracked);
        }
    }
    let warning = if files.len() > MAX_FILES {
        format!("Showing first {MAX_FILES} files; use Git for the full list.")
    } else {
        String::new()
    };
    files.truncate(MAX_FILES);
    let head = git_text(&repo.root, &["rev-parse", "--verify", "HEAD"]).ok();
    Ok(Snapshot {
        repository: repo,
        scope: scope.clone(),
        files,
        warning,
        head,
    })
}

pub fn detail(snapshot: &Snapshot, scope: &Scope, index: usize) -> Result<String, String> {
    exact_repository(&snapshot.repository.root, Some(&snapshot.repository))?;
    if scope != &snapshot.scope {
        return Err("Git comparison changed; refresh before reading a file".into());
    }
    let Some(file) = snapshot.files.get(index) else {
        return Ok("No changes in this scope.".into());
    };
    if file.status == "??" || (*scope == Scope::Working && snapshot.head.is_none()) {
        let kind = if file.status == "??" {
            "Untracked"
        } else {
            "Working file (no commits yet)"
        };
        let path = snapshot.repository.root.join(&file.path);
        let parent = path
            .parent()
            .ok_or("No file parent")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !parent.starts_with(&snapshot.repository.root) {
            return Err("Preview path leaves the selected repository".into());
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok("File is absent from the working tree (deleted).".into());
            }
            Err(error) => return Err(error.to_string()),
        };
        if meta.file_type().is_symlink() {
            return fs::read_link(path)
                .map(|p| format!("{kind} symbolic link -> {p:?} (not followed)"))
                .map_err(|e| e.to_string());
        }
        if !meta.is_file() {
            return Ok(format!("{kind} entry is not a regular file; no preview."));
        }
        let mut bytes = Vec::new();
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .map_err(|e| e.to_string())?
            .take(MAX_PREVIEW + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.contains(&0) {
            return Ok(format!(
                "{kind} binary file ({} bytes); no text preview.",
                meta.len()
            ));
        }
        let truncated = bytes.len() as u64 > MAX_PREVIEW;
        bytes.truncate(MAX_PREVIEW as usize);
        return Ok(format!(
            "{kind} file preview\n{}{}",
            String::from_utf8_lossy(&bytes),
            if truncated {
                "\n[Preview limited to 64 KiB]"
            } else {
                ""
            }
        ));
    }
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "diff".as_ref(),
        "--no-ext-diff".as_ref(),
        "--no-textconv".as_ref(),
        "--no-color".as_ref(),
        "--submodule=short".as_ref(),
    ];
    match scope {
        Scope::Working => {
            if let Some(head) = &snapshot.head {
                args.push(head.as_ref());
            } else {
                args.push("--cached".as_ref());
            }
        }
        Scope::Staged => args.push("--cached".as_ref()),
        Scope::Compare { base, .. } => args.push(base.as_ref()),
    }
    args.push("--".as_ref());
    args.push(file.path.as_os_str());
    if let Some(original) = &file.original {
        args.push(original.as_os_str());
    }
    let text = String::from_utf8_lossy(&git(&snapshot.repository.root, &args)?).into_owned();
    Ok(if text.is_empty() {
        "No diff in this scope. Files may have changed; press r to refresh.".into()
    } else {
        text
    })
}

pub fn panel(path: &Path) -> Result<(), String> {
    use crossterm::{
        event::{
            self, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton,
            MouseEventKind,
        },
        execute,
        terminal::{EnterAlternateScreen, enable_raw_mode},
    };
    use ratatui::{
        Terminal,
        backend::CrosstermBackend,
        layout::{Constraint, Layout},
        style::{Color, Style},
        text::Line,
        widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    };
    use std::{io, time::Duration};

    let run = crate::runtime::read_run(path)?;
    let target = run
        .targets
        .iter()
        .find(|t| Some(&t.key) == run.selected.as_ref())
        .ok_or("Select a live terminal before opening Git changes")?
        .clone();
    let mut scope = Scope::Working;
    let mut captured: Option<Snapshot> = None;
    let mut message = String::new();
    let mut content = String::new();
    let mut cursor = 0usize;
    let mut vertical = 0u16;
    let mut horizontal = 0u16;
    let mut editing: Option<String> = None;
    let mut refresh = true;
    let mut load_detail = true;
    let _restore = crate::ui::Restore;
    let mut chrome = crate::presentation::Chrome::default();
    enable_raw_mode().map_err(|e| e.to_string())?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|e| e.to_string())?;
    terminal.clear().map_err(|e| e.to_string())?;
    let mut hits = crate::interaction::Hits::new(ratatui::layout::Rect::default());
    loop {
        if refresh {
            let expected = captured.as_ref().map(|s| &s.repository);
            match selected_repository(&target, expected).and_then(|repo| snapshot(repo, &scope)) {
                Ok(snapshot) => {
                    message = snapshot.warning.clone();
                    captured = Some(snapshot);
                    cursor = 0;
                    vertical = 0;
                    horizontal = 0;
                    load_detail = true;
                }
                Err(error) => {
                    message = format!("Refresh refused; previous snapshot retained: {error}")
                }
            }
            refresh = false;
        }
        if load_detail {
            if let Some(snapshot) = &captured {
                match selected_repository(&target, Some(&snapshot.repository))
                    .and_then(|_| detail(snapshot, &snapshot.scope, cursor))
                {
                    Ok(text) => content = crate::ui::sanitize_history(&text),
                    Err(error) => {
                        content.clear();
                        message = error;
                    }
                }
            }
            load_detail = false;
        }
        let size = terminal.size().map_err(|e| e.to_string())?;
        let page = size.height.saturating_sub(12).max(1);
        vertical = vertical.min(
            content
                .lines()
                .count()
                .saturating_sub(usize::from(page))
                .min(u16::MAX as usize) as u16,
        );
        terminal.draw(|frame| {
            hits = crate::interaction::Hits::new(frame.area());
            let rows = Layout::vertical([Constraint::Length(4), Constraint::Min(1), Constraint::Length(5)]).split(crate::presentation::body(frame.area()));
            let columns = Layout::horizontal([Constraint::Percentage(35), Constraint::Percentage(65)]).split(rows[1]);
            let repo = captured.as_ref().map(|s| s.repository.root.to_string_lossy().into_owned()).unwrap_or_default();
            let shown_scope = captured.as_ref().map(|s| if s.scope == Scope::Working && s.head.is_none() {
                "Working tree — no commits yet (current file previews)".into()
            } else { s.scope.label() }).unwrap_or_else(|| scope.label());
            let header = crate::ui::sanitize_history(&format!("{} · {repo}\n{shown_scope}", target.session_name));
            frame.render_widget(Paragraph::new(header).block(Block::default().borders(Borders::ALL).title("Git changes — READ ONLY")), rows[0]);
            let items = captured.as_ref().map(|s| s.files.iter().map(|file| ListItem::new(file.label())).collect::<Vec<_>>()).unwrap_or_default();
            let mut state = ListState::default();
            if !items.is_empty() { state.select(Some(cursor)); }
            frame.render_stateful_widget(List::new(items).block(Block::default().borders(Borders::ALL).title("Files · ↑/↓"))
                .highlight_style(crate::presentation::selected()), columns[0], &mut state);
            if let Some(snapshot) = &captured {
                hits.list(Block::default().borders(Borders::ALL).inner(columns[0]), state.offset(), &snapshot.files.iter().enumerate().map(|(index, _)| (1, Some(index))).collect::<Vec<_>>());
            }
            hits.add(columns[1], None);
            let lines: Vec<Line> = content.lines().map(|line| {
                let color = if line.starts_with('+') { Color::Green } else if line.starts_with('-') { Color::Red }
                    else if line.starts_with("@@") { Color::Cyan } else { Color::Reset };
                Line::styled(line, Style::default().fg(color))
            }).collect();
            frame.render_widget(Paragraph::new(lines).scroll((vertical, horizontal))
                .block(Block::default().borders(Borders::ALL).title("Diff / preview · PgUp/PgDn · h/l")), columns[1]);
            let footer = if let Some(reference) = &editing {
                format!("Compare local branch/tag/commit: {reference}\nEnter compare · Esc cancel\n{}", crate::ui::sanitize_history(&message))
            } else {
                format!("Tab working/staged · b compare base · r refresh · Esc close\n↑/↓ files · PgUp/PgDn diff · Home/End · h/l horizontal\n{}", crate::ui::sanitize_history(&message))
            };
            frame.render_widget(Paragraph::new(footer).wrap(ratatui::widgets::Wrap { trim: false }), rows[2]);
            chrome.draw(frame, "Git changes · Read only");
        }).map_err(|e| e.to_string())?;
        if !event::poll(Duration::from_millis(200)).map_err(|e| e.to_string())? {
            continue;
        }
        let input = event::read().map_err(|e| e.to_string())?;
        let now = terminal.size().map_err(|e| e.to_string())?;
        if chrome.closing(
            &input,
            ratatui::layout::Rect::new(0, 0, now.width, now.height),
        ) {
            return Ok(());
        }
        if let Event::Mouse(mouse) = input {
            let current = ratatui::layout::Rect::new(0, 0, size.width, size.height);
            let now = terminal.size().map_err(|e| e.to_string())?;
            if current.width != now.width || current.height != now.height {
                continue;
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some(Some(index)) = hits.get(mouse, current) {
                    cursor = *index;
                    vertical = 0;
                    load_detail = true;
                    editing = None;
                }
            } else if let Some(delta) = crate::interaction::wheel(mouse.kind)
                && let Some(hit) = hits.get(mouse, current)
            {
                if hit.is_none() {
                    vertical = vertical.saturating_add_signed(delta as i16);
                } else {
                    cursor = cursor.saturating_add_signed(delta).min(
                        captured
                            .as_ref()
                            .map_or(0, |s| s.files.len().saturating_sub(1)),
                    );
                    vertical = 0;
                    load_detail = true;
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
        if let Some(reference) = &mut editing {
            match key.code {
                KeyCode::Esc => editing = None,
                KeyCode::Backspace => {
                    reference.pop();
                }
                KeyCode::Enter => {
                    if let Some(snapshot) = &captured {
                        match selected_repository(&target, Some(&snapshot.repository))
                            .and_then(|repo| comparison(&repo, reference))
                        {
                            Ok(next) => {
                                scope = next;
                                refresh = true;
                                editing = None;
                            }
                            Err(error) => message = error,
                        }
                    }
                }
                KeyCode::Char(ch)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && reference.len() + ch.len_utf8() <= 256 =>
                {
                    reference.push(ch)
                }
                _ => {}
            }
            continue;
        }
        let count = captured.as_ref().map(|s| s.files.len()).unwrap_or(0);
        let before = cursor;
        match key.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                cursor = cursor.saturating_add(1).min(count.saturating_sub(1))
            }
            KeyCode::PageUp => vertical = vertical.saturating_sub(page),
            KeyCode::PageDown => vertical = vertical.saturating_add(page),
            KeyCode::Home => vertical = 0,
            KeyCode::End => vertical = u16::MAX,
            KeyCode::Char('h') | KeyCode::Left => horizontal = horizontal.saturating_sub(8),
            KeyCode::Char('l') | KeyCode::Right => {
                horizontal = horizontal.saturating_add(8).min(4096)
            }
            KeyCode::Char('r') => refresh = true,
            KeyCode::Tab => {
                scope = if scope == Scope::Working {
                    Scope::Staged
                } else {
                    Scope::Working
                };
                refresh = true;
            }
            KeyCode::Char('b') => editing = Some(String::new()),
            _ => {}
        }
        if cursor != before {
            vertical = 0;
            horizontal = 0;
            load_detail = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new(commit: bool) -> Self {
            let root = std::env::temp_dir().join(format!(
                "cockpit-changes-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            let fixture = Self(root);
            fixture.git(&["init", "-q", "-b", "main"]);
            for (key, value) in [
                ("user.name", "Cockpit Test"),
                ("user.email", "test@example.invalid"),
                ("commit.gpgsign", "false"),
                ("core.hooksPath", "/dev/null"),
            ] {
                fixture.git(&["config", key, value]);
            }
            fs::write(fixture.0.join("tracked.txt"), "original\n").unwrap();
            if commit {
                fixture.git(&["add", "tracked.txt"]);
                fixture.git(&["commit", "-qm", "baseline"]);
            }
            fixture
        }
        fn git(&self, args: &[&str]) -> String {
            git_text(&self.0, args).unwrap()
        }
        fn repo(&self) -> Repository {
            repository(&self.0).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn working_and_staged_are_distinct_and_leave_git_unchanged() {
        let f = Fixture::new(true);
        fs::write(f.0.join("tracked.txt"), "staged-marker\n").unwrap();
        f.git(&["add", "tracked.txt"]);
        fs::write(f.0.join("tracked.txt"), "working-marker\n").unwrap();
        let index = fs::read(f.0.join(".git/index")).unwrap();
        let head = f.git(&["rev-parse", "HEAD"]);
        let working = snapshot(f.repo(), &Scope::Working).unwrap();
        assert!(
            detail(&working, &Scope::Working, 0)
                .unwrap()
                .contains("+working-marker")
        );
        let staged = snapshot(f.repo(), &Scope::Staged).unwrap();
        assert!(
            detail(&staged, &Scope::Staged, 0)
                .unwrap()
                .contains("+staged-marker")
        );
        assert_eq!(fs::read(f.0.join(".git/index")).unwrap(), index);
        assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn explicit_base_uses_resolved_merge_base_and_includes_committed_work() {
        let f = Fixture::new(true);
        let initial = f.git(&["rev-parse", "HEAD"]);
        fs::write(f.0.join("tracked.txt"), "committed-marker\n").unwrap();
        f.git(&["add", "tracked.txt"]);
        f.git(&["commit", "-qm", "work"]);
        let scope = comparison(&f.repo(), &initial).unwrap();
        assert!(matches!(&scope, Scope::Compare { base, .. } if base == &initial));
        let captured = snapshot(f.repo(), &scope).unwrap();
        assert!(
            detail(&captured, &scope, 0)
                .unwrap()
                .contains("+committed-marker")
        );
        assert!(comparison(&f.repo(), "--help").is_err());
        assert!(comparison(&f.repo(), "main\nHEAD").is_err());
    }

    #[test]
    fn exact_repository_rejects_a_parent_and_a_replacement_at_the_same_path() {
        let f = Fixture::new(true);
        fs::create_dir(f.0.join("nested")).unwrap();
        assert!(exact_repository(&f.0.join("nested"), None).is_err());
        let expected = exact_repository(&f.0, None).unwrap();
        let moved = f.0.with_extension("old-review");
        fs::rename(&f.0, &moved).unwrap();
        fs::create_dir(&f.0).unwrap();
        let mut init = Command::new("git");
        init.args(["init", "-q"]).current_dir(&f.0);
        assert!(init.status().unwrap().success());
        assert!(exact_repository(&f.0, Some(&expected)).is_err());
        fs::remove_dir_all(moved).unwrap();
    }

    #[test]
    fn filenames_are_literal_and_renames_preserve_both_paths() {
        let f = Fixture::new(true);
        let name = ":(glob)odd[1].txt";
        fs::write(f.0.join(name), "old\n").unwrap();
        f.git(&["add", "--", name]);
        f.git(&["commit", "-qm", "odd name"]);
        fs::write(f.0.join(name), "literal-marker\n").unwrap();
        fs::write(f.0.join("tracked.txt"), "other-marker\n").unwrap();
        let snapshot = snapshot(f.repo(), &Scope::Working).unwrap();
        let index = snapshot.files.iter().position(|f| f.path == name).unwrap();
        let text = detail(&snapshot, &Scope::Working, index).unwrap();
        assert!(text.contains("literal-marker"));
        assert!(!text.contains("other-marker"));
        let parsed =
            status_files(b"R  renamed file\0old file\0?? unicode-\xe4\xb8\x96\xe7\x95\x8c\0")
                .unwrap();
        assert_eq!(
            parsed[0].original.as_deref(),
            Some(std::ffi::OsStr::new("old file"))
        );
        assert!(parsed[1].label().contains("世界"));
        assert!(status_files(b"R  missing source\0").is_err());
    }

    #[test]
    fn untracked_previews_are_bounded_binary_aware_and_do_not_follow_symlinks() {
        let f = Fixture::new(true);
        fs::write(f.0.join("large"), vec![b'a'; MAX_PREVIEW as usize + 500]).unwrap();
        fs::write(f.0.join("binary"), b"text\0binary").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", f.0.join("link")).unwrap();
        let snapshot = snapshot(f.repo(), &Scope::Working).unwrap();
        let preview = |name| {
            detail(
                &snapshot,
                &Scope::Working,
                snapshot.files.iter().position(|f| f.path == name).unwrap(),
            )
            .unwrap()
        };
        assert!(preview("large").contains("limited to 64 KiB"));
        assert!(preview("binary").contains("binary file"));
        let link = preview("link");
        assert!(link.contains("not followed"));
        assert!(!link.contains("root:"));
    }

    #[test]
    fn external_diff_and_textconv_are_not_executed() {
        let f = Fixture::new(true);
        let sentinel = f.0.join("SHOULD_NOT_EXIST");
        let command = format!("touch {}", sentinel.display());
        f.git(&["config", "diff.external", &command]);
        f.git(&["config", "diff.test.textconv", &command]);
        fs::write(f.0.join(".gitattributes"), "tracked.txt diff=test\n").unwrap();
        fs::write(f.0.join("tracked.txt"), "safe-marker\n").unwrap();
        let captured = snapshot(f.repo(), &Scope::Working).unwrap();
        let index = captured
            .files
            .iter()
            .position(|f| f.path == "tracked.txt")
            .unwrap();
        assert!(
            detail(&captured, &Scope::Working, index)
                .unwrap()
                .contains("safe-marker")
        );
        assert!(!sentinel.exists());
    }

    #[test]
    fn non_utf8_paths_remain_exact_and_file_limit_is_reported() {
        let f = Fixture::new(true);
        let raw = OsString::from_vec(b"raw-\xff.txt".to_vec());
        fs::write(f.0.join(&raw), "raw-path-marker\n").unwrap();
        let captured = snapshot(f.repo(), &Scope::Working).unwrap();
        let index = captured
            .files
            .iter()
            .position(|file| file.path == raw)
            .unwrap();
        assert!(
            detail(&captured, &Scope::Working, index)
                .unwrap()
                .contains("raw-path-marker")
        );
        for i in 0..MAX_FILES {
            fs::write(f.0.join(format!("extra-{i:03}")), "file\n").unwrap();
        }
        let captured = snapshot(f.repo(), &Scope::Working).unwrap();
        assert_eq!(captured.files.len(), MAX_FILES);
        assert!(!captured.warning.is_empty());
    }

    #[test]
    fn recreated_repository_at_the_same_path_has_a_different_identity() {
        let f = Fixture::new(true);
        let old = f.repo();
        fs::rename(f.0.join(".git"), f.0.join("old-git")).unwrap();
        f.git(&["init", "-q", "-b", "main"]);
        let new = f.repo();
        assert_eq!(old.root, new.root);
        assert_eq!(old.git_dir, new.git_dir);
        assert_ne!(old, new);
    }

    #[test]
    fn unborn_repository_can_show_staged_files() {
        let f = Fixture::new(false);
        f.git(&["add", "tracked.txt"]);
        let captured = snapshot(f.repo(), &Scope::Staged).unwrap();
        assert!(captured.head.is_none());
        fs::write(f.0.join("tracked.txt"), "unstaged-before-first-commit\n").unwrap();
        assert!(
            detail(&captured, &Scope::Staged, 0)
                .unwrap()
                .contains("+original")
        );
        let working = snapshot(f.repo(), &Scope::Working).unwrap();
        assert!(
            detail(&working, &Scope::Working, 0)
                .unwrap()
                .contains("unstaged-before-first-commit")
        );
    }
}
