//! Agent activity from status files that a presentation client names when it
//! opens a view. Cockpit reads each file, binds it to the terminal through the
//! owner process recorded in it, and never infers activity from screen text.
//!
//! Status file: a JSON object with `status` and `owner {pid, start}`, and
//! optionally `detail` and `boot_id` (or `owner_boot`). `status` is one of
//! working|busy, waiting, idle|stop_observed, errored|failed, exited|closed; any other
//! value reads as unknown.
use crate::model::{ProcessOwner, Run, Target};
use crate::tmux::process_start;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::SystemTime;

const MAX_FILE_BYTES: u64 = 64 * 1024;
/// A live state not refreshed for this long is shown as stale.
const STALE_SECONDS: u64 = 300;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Working,
    Waiting,
    Idle,
    Errored,
    Exited,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Report {
    pub key: String,
    pub file: String,
    #[serde(default)]
    pub state: State,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub age_seconds: u64,
    /// A turn ended while the terminal was not selected. Selecting it clears this.
    #[serde(default)]
    pub unseen: bool,
}

impl Report {
    fn new(key: &str, file: &str) -> Self {
        Self {
            key: key.into(),
            file: file.into(),
            state: State::Unknown,
            detail: None,
            age_seconds: 0,
            unseen: false,
        }
    }
    pub fn done(&self) -> bool {
        self.state == State::Idle && self.unseen
    }
    pub fn stale(&self) -> bool {
        matches!(self.state, State::Working | State::Waiting) && self.age_seconds > STALE_SECONDS
    }
}

#[derive(Deserialize)]
struct StatusFile {
    status: String,
    #[serde(default)]
    detail: Option<String>,
    owner: Option<ProcessOwner>,
    #[serde(default)]
    boot_id: Option<String>,
    #[serde(default)]
    owner_boot: Option<String>,
}

pub fn belongs_to_pane(owner: &ProcessOwner, target: &Target) -> bool {
    if target.dead
        || process_start(target.pane_pid).ok().as_deref() != Some(&target.pane_start)
        || process_start(owner.pid).ok().as_deref() != Some(&owner.start)
    {
        return false;
    }
    let mut pid = owner.pid;
    for _ in 0..32 {
        if pid == target.pane_pid {
            return true;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, tail)| tail.split_whitespace().nth(1))
            .and_then(|pid| pid.parse::<u32>().ok())
            .unwrap_or(0);
        if parent <= 1 || parent == pid {
            return false;
        }
        pid = parent;
    }
    false
}

fn boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|id| id.trim().to_owned())
}

fn state_of(status: &str) -> State {
    match status {
        "working" | "busy" => State::Working,
        "waiting" => State::Waiting,
        "idle" | "stop_observed" => State::Idle,
        "errored" | "failed" => State::Errored,
        "exited" | "closed" => State::Exited,
        _ => State::Unknown,
    }
}

/// One reading of a status file. Anything unverifiable reads as unknown with a reason.
fn read(file: &str, target: &Target, boot: Option<&str>) -> (State, Option<String>, u64) {
    let unknown = |reason: &str| (State::Unknown, Some(reason.to_owned()), 0);
    let Ok(meta) = std::fs::metadata(file) else {
        return unknown("no status file");
    };
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.len() > MAX_FILE_BYTES {
        return unknown("status file is not a private regular file");
    }
    let Ok(status) = std::fs::read(file)
        .map_err(|_| ())
        .and_then(|bytes| serde_json::from_slice::<StatusFile>(&bytes).map_err(|_| ()))
    else {
        return unknown("unreadable status file");
    };
    let recorded_boot = status.boot_id.as_deref().or(status.owner_boot.as_deref());
    if recorded_boot.is_some() && boot.is_some() && recorded_boot != boot {
        return unknown("status from a previous boot");
    }
    if !status
        .owner
        .as_ref()
        .is_some_and(|owner| belongs_to_pane(owner, target))
    {
        return unknown("status owner is not in this terminal");
    }
    let age = meta
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map_or(0, |age| age.as_secs());
    let detail = status.detail.filter(|detail| !detail.is_empty());
    (state_of(&status.status), detail, age)
}

/// Registers, or replaces, the status file reported for a terminal.
pub fn register(run: &mut Run, key: &str, file: &str) -> Result<(), String> {
    if !Path::new(file).is_absolute() {
        return Err("--status-file must be an absolute path".into());
    }
    run.reports.retain(|report| report.key != key);
    run.reports.push(Report::new(key, file));
    refresh(run);
    Ok(())
}

pub fn seen(run: &mut Run, key: &str) {
    if let Some(report) = run.reports.iter_mut().find(|report| report.key == key) {
        report.unseen = false;
    }
}

/// Rereads every registered file. Reports for terminals that left the
/// original server are dropped with them.
pub fn refresh(run: &mut Run) {
    let boot = boot_id();
    let targets = &run.targets;
    run.reports
        .retain(|report| targets.iter().any(|target| target.key == report.key));
    for report in &mut run.reports {
        let Some(target) = targets.iter().find(|target| target.key == report.key) else {
            continue;
        };
        let (state, detail, age) = read(&report.file, target, boot.as_deref());
        let selected = run.selected.as_deref() == Some(report.key.as_str());
        let finished =
            state == State::Idle && matches!(report.state, State::Working | State::Waiting);
        if selected || state != State::Idle {
            report.unseen = false;
        } else if finished {
            report.unseen = true;
        }
        report.state = state;
        report.detail = detail;
        report.age_seconds = age;
    }
}

pub fn report<'a>(run: &'a Run, target: &Target) -> Option<&'a Report> {
    run.reports.iter().find(|report| report.key == target.key)
}

/// Herdr's glyph vocabulary: × needs you, ✓ unseen finish, ◐ working, ○ idle.
pub fn glyph(report: Option<&Report>) -> &'static str {
    match report {
        Some(report) if report.done() => "✓",
        Some(report) => match report.state {
            State::Waiting => "×",
            State::Errored => "!",
            State::Working => "◐",
            State::Idle => "○",
            State::Exited => "-",
            State::Unknown => "·",
        },
        None => "·",
    }
}

/// Lower ranks need the user sooner.
pub fn rank(report: Option<&Report>) -> u8 {
    match report {
        Some(report) if report.done() => 2,
        Some(report) => match report.state {
            State::Waiting => 0,
            State::Errored => 1,
            State::Working => 3,
            State::Idle => 4,
            State::Exited => 5,
            State::Unknown => 6,
        },
        None => 6,
    }
}

pub fn describe(report: Option<&Report>) -> String {
    let Some(report) = report else {
        return "· activity unknown".into();
    };
    let label = match report.state {
        _ if report.done() => "done".to_owned(),
        State::Waiting => match report.detail.as_deref() {
            Some("permission_prompt") => "waiting: permission".to_owned(),
            Some(_) => "waiting: question".to_owned(),
            None => "waiting".to_owned(),
        },
        State::Working => "working".to_owned(),
        State::Idle => "idle".to_owned(),
        State::Errored => "errored".to_owned(),
        State::Exited => "session ended".to_owned(),
        State::Unknown => match report.detail.as_deref() {
            Some(reason) => format!("activity unknown ({reason})"),
            None => "activity unknown".to_owned(),
        },
    };
    format!(
        "{} {label}{}",
        glyph(Some(report)),
        if report.stale() { " (stale)" } else { "" }
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Waiting,
    Working,
    Done,
    Idle,
    Errored,
    Exited,
    Unknown,
    Stale,
}

impl Filter {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all activity",
            Self::Waiting => "waiting",
            Self::Working => "working",
            Self::Done => "done",
            Self::Idle => "idle",
            Self::Errored => "errored",
            Self::Exited => "session ended",
            Self::Unknown => "unknown",
            Self::Stale => "stale",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Waiting,
            Self::Waiting => Self::Working,
            Self::Working => Self::Done,
            Self::Done => Self::Idle,
            Self::Idle => Self::Errored,
            Self::Errored => Self::Exited,
            Self::Exited => Self::Unknown,
            Self::Unknown => Self::Stale,
            Self::Stale => Self::All,
        }
    }
    pub fn matches(self, report: Option<&Report>) -> bool {
        let state = report.map_or(State::Unknown, |report| report.state);
        match self {
            Self::All => true,
            Self::Stale => report.is_some_and(Report::stale),
            Self::Done => report.is_some_and(Report::done),
            Self::Idle => state == State::Idle && !report.is_some_and(Report::done),
            Self::Waiting => state == State::Waiting,
            Self::Working => state == State::Working,
            Self::Errored => state == State::Errored,
            Self::Exited => state == State::Exited,
            Self::Unknown => state == State::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn live_target(key: &str) -> Target {
        let pid = std::process::id();
        serde_json::from_value(serde_json::json!({
            "key": key, "socket": "/tmp/socket", "server_pid": 1, "server_start": "1",
            "session_id": "$0", "session_created": "1", "session_name": "task", "window_id": "@0",
            "pane_id": "%0", "pane_pid": pid, "pane_start": process_start(pid).unwrap(),
            "cwd": "/project", "dead": false, "project": null, "workspace": null,
            "task": null, "claude_session_id": null, "association": "unknown"
        }))
        .unwrap()
    }

    fn write_status(path: &Path, status: &str, owner_start: &str) {
        let pid = std::process::id();
        let mut file = std::fs::File::create(path).unwrap();
        write!(
            file,
            "{}",
            serde_json::json!({"status": status, "detail": "permission_prompt",
                "owner": {"pid": pid, "start": owner_start}, "boot_id": boot_id()})
        )
        .unwrap();
    }

    #[test]
    fn dead_or_reused_process_cannot_be_bound() {
        let target = live_target("fixture");
        let owner = ProcessOwner {
            pid: std::process::id(),
            start: process_start(std::process::id()).unwrap(),
        };
        assert!(belongs_to_pane(&owner, &target));
        let mut replaced = target.clone();
        replaced.pane_start = "wrong".into();
        assert!(!belongs_to_pane(&owner, &replaced));
        let reused = ProcessOwner {
            start: "different".into(),
            ..owner
        };
        assert!(!belongs_to_pane(&reused, &target));
    }

    #[test]
    fn finished_turns_stay_unseen_until_selected_and_foreign_owners_are_unknown() {
        let directory =
            std::env::temp_dir().join(format!("cockpit-activity-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("status.json");
        let start = process_start(std::process::id()).unwrap();
        let mut run = crate::navigation::fixture();
        run.targets = vec![live_target("worker")];
        write_status(&file, "waiting", &start);
        register(&mut run, "worker", file.to_str().unwrap()).unwrap();
        let current = |run: &Run| run.reports[0].clone();
        assert_eq!(current(&run).state, State::Waiting);
        assert_eq!(describe(Some(&current(&run))), "× waiting: permission");
        assert_eq!(rank(Some(&current(&run))), 0);

        write_status(&file, "stop_observed", &start);
        refresh(&mut run);
        assert!(current(&run).done());
        assert!(Filter::Done.matches(Some(&current(&run))));
        assert_eq!(glyph(Some(&current(&run))), "✓");

        run.selected = Some("worker".into());
        refresh(&mut run);
        assert!(!current(&run).unseen);
        assert!(Filter::Idle.matches(Some(&current(&run))));

        write_status(&file, "working", "not-the-owner");
        refresh(&mut run);
        assert_eq!(current(&run).state, State::Unknown);
        assert!(describe(Some(&current(&run))).contains("owner is not in this terminal"));

        run.targets.clear();
        refresh(&mut run);
        assert!(run.reports.is_empty());
        assert!(register(&mut run, "worker", "relative.json").is_err());
        let _ = std::fs::remove_dir_all(directory);
    }
}
