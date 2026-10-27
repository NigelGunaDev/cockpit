//! Evidence projection and exact terminal correlation; Node owns all receipts.
use crate::model::{
    AttentionItem, AttentionSnapshot, ExecutionObservation, ProcessOwner, Run, Target,
};
use crate::tmux::{output, process_start};
use std::path::Path;
use std::process::Command;

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

pub fn observation<'a>(
    target: &Target,
    inboxes: &'a [AttentionSnapshot],
) -> Option<&'a ExecutionObservation> {
    let mut matches = inboxes
        .iter()
        .flat_map(|inbox| &inbox.observations)
        .filter(|event| {
            target.project.as_deref() == Some(&event.project)
                && target.workspace == event.workspace_root
                && belongs_to_pane(&event.owner, target)
        });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

pub fn activity(target: &Target, inboxes: &[AttentionSnapshot]) -> String {
    match observation(target, inboxes) {
        Some(event) => {
            let label = match event.status.as_str() {
                "working" => "working observed",
                "waiting" => "waiting observed",
                "idle" => "idle observed",
                "stop_observed" => "turn end observed",
                "errored" => "error observed",
                "exited" => "session end observed",
                _ => "activity unknown",
            };
            format!(
                "{label}{} · {} · {}",
                if event.age_seconds > 300 {
                    " (stale)"
                } else {
                    ""
                },
                event.source,
                event.ts
            )
        }
        None => "activity unknown".into(),
    }
}

pub fn matching_target<'a>(item: &AttentionItem, run: &'a Run) -> Result<&'a Target, String> {
    let owner = item.owner.as_ref().ok_or(
        "No verified Claude process in this evidence; choose a terminal in the session picker",
    )?;
    let session = item
        .session_id
        .as_ref()
        .ok_or("Evidence has no Claude session identity")?;
    let mut matches = run.targets.iter().filter(|target| {
        target.project.as_deref() == Some(&item.project)
            && target.workspace == item.workspace_root
            && target.claude_session_id.as_ref() == Some(session)
            && belongs_to_pane(owner, target)
    });
    let first = matches
        .next()
        .ok_or("The original Claude session is stale/missing; no replacement will be selected")?;
    if matches.next().is_some() {
        return Err(
            "Multiple terminal targets match; select explicitly in the session picker".into(),
        );
    }
    Ok(first)
}

pub fn receipt(item: &AttentionItem, action: &str, cli_root: &str) -> Result<(), String> {
    if !["seen", "acknowledge", "resolve", "reopen"].contains(&action) {
        return Err("Unknown attention receipt".into());
    }
    let mut command = Command::new(Path::new(cli_root).join("sessions/bin/sessions"));
    command
        .args(["needs-you", action, &item.id, "--json"])
        .current_dir(cli_root)
        .env("CLAUDE_PROJECT_DIR", &item.project)
        .env_remove("CLAUDECODE");
    let text = output(command)?;
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if value.get("item_id").and_then(|id| id.as_str()) != Some(&item.id) {
        return Err("Unexpected receipt response; reload the inbox".into());
    }
    Ok(())
}

pub fn counts(inboxes: &[AttentionSnapshot]) -> (usize, usize) {
    (
        inboxes.iter().map(|inbox| inbox.counts.unresolved).sum(),
        inboxes
            .iter()
            .map(|inbox| inbox.counts.unacknowledged)
            .sum(),
    )
}

pub fn token(item: &AttentionItem) -> String {
    serde_json::json!([item.project, item.id, item.evidence_hash]).to_string()
}

pub fn read_page(
    cli_root: &str,
    project: &str,
    before: Option<&str>,
) -> Result<AttentionSnapshot, String> {
    let mut command = Command::new(Path::new(cli_root).join("sessions/bin/sessions"));
    command.args(["needs-you", "list"]);
    if let Some(before) = before {
        if before.is_empty() || before.len() > 256 || before.chars().any(char::is_control) {
            return Err("Invalid inbox page cursor".into());
        }
        command.arg(before);
    }
    command
        .arg("--json")
        .current_dir(cli_root)
        .env("CLAUDE_PROJECT_DIR", cli_root)
        .env_remove("CLAUDECODE");
    let value: serde_json::Value =
        serde_json::from_str(&output(command)?).map_err(|e| e.to_string())?;
    if value.get("version").and_then(|v| v.as_u64()) != Some(1)
        || value.get("project").and_then(|v| v.as_str()) != Some(project)
    {
        return Err(
            "Inbox page unavailable or project mismatch; refresh or update the installed CLI"
                .into(),
        );
    }
    let mut page: AttentionSnapshot = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if page.items.len() > 200
        || page
            .items
            .iter()
            .any(|item| item.project != project || item.id.is_empty())
    {
        return Err("Invalid inbox page identities".into());
    }
    page.cli_root = cli_root.into();
    page.before = before.map(str::to_owned);
    Ok(page)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Activity {
    #[default]
    All,
    Working,
    Waiting,
    Idle,
    Stopped,
    Errored,
    Exited,
    Unknown,
    Stale,
}

impl Activity {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all activity",
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
            Self::Stopped => "turn ended",
            Self::Errored => "errored",
            Self::Exited => "session ended",
            Self::Unknown => "unknown",
            Self::Stale => "stale",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Working,
            Self::Working => Self::Waiting,
            Self::Waiting => Self::Idle,
            Self::Idle => Self::Stopped,
            Self::Stopped => Self::Errored,
            Self::Errored => Self::Exited,
            Self::Exited => Self::Unknown,
            Self::Unknown => Self::Stale,
            Self::Stale => Self::All,
        }
    }
    pub fn matches(self, target: &Target, inboxes: &[AttentionSnapshot]) -> bool {
        if self == Self::All {
            return true;
        }
        let event = observation(target, inboxes);
        if self == Self::Stale {
            return event.is_some_and(|event| event.age_seconds > 300);
        }
        let observed = match event.map(|event| event.status.as_str()) {
            Some("working") => Self::Working,
            Some("waiting") => Self::Waiting,
            Some("idle") => Self::Idle,
            Some("stop_observed") => Self::Stopped,
            Some("errored") => Self::Errored,
            Some("exited") => Self::Exited,
            _ => Self::Unknown,
        };
        self == observed
    }
}

pub fn unresolved_for(target: &Target, run: &Run) -> usize {
    run.attention
        .iter()
        .flat_map(|page| &page.items)
        .filter(|item| {
            item.resolved_at.is_none()
                && matching_target(item, run).is_ok_and(|found| found.key == target.key)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dead_or_reused_process_cannot_be_correlated() {
        let target: Target = serde_json::from_value(serde_json::json!({
            "key":"fixture", "socket":"/tmp/socket", "server_pid":1,"server_start":"1",
            "session_id":"$0","session_created":"1","session_name":"task","window_id":"@0",
            "pane_id":"%0","pane_pid":std::process::id(),"pane_start":"wrong",
            "cwd":"/project","dead":false,"project":"/project","workspace":"/project",
            "task":null,"claude_session_id":null,"association":"unknown"
        }))
        .unwrap();
        let owner = ProcessOwner {
            pid: std::process::id(),
            start: process_start(std::process::id()).unwrap(),
        };
        assert!(!belongs_to_pane(&owner, &target));
        let mut live = target;
        live.pane_start = owner.start.clone();
        assert!(belongs_to_pane(&owner, &live));
        let reused = ProcessOwner {
            start: "different".into(),
            ..owner
        };
        assert!(!belongs_to_pane(&reused, &live));
    }

    #[test]
    fn observations_require_exact_process_and_unique_session_not_workspace_alone() {
        let pid = std::process::id();
        let start = process_start(pid).unwrap();
        let target: Target = serde_json::from_value(serde_json::json!({
            "key":"fixture", "socket":"/tmp/socket", "server_pid":1,"server_start":"1",
            "session_id":"$0","session_created":"1","session_name":"task","window_id":"@0",
            "pane_id":"%0","pane_pid":pid,"pane_start":start,
            "cwd":"/project","dead":false,"project":"/project","workspace":"/project",
            "task":null,"claude_session_id":"new","association":"unknown"
        }))
        .unwrap();
        let event: ExecutionObservation = serde_json::from_value(serde_json::json!({
            "project":"/project","workspace_root":"/project","session_id":"new",
            "owner":{"pid":pid,"start":start},"status":"working","source":"UserPromptSubmit",
            "ts":"2026-10-14T00:00:00Z","detail":null,"age_seconds":400
        }))
        .unwrap();
        let mut snapshot = AttentionSnapshot {
            observations: vec![event.clone()],
            ..Default::default()
        };
        assert!(activity(&target, &[snapshot.clone()]).contains("working observed (stale)"));
        assert!(Activity::Working.matches(&target, &[snapshot.clone()]));
        assert!(Activity::Stale.matches(&target, &[snapshot.clone()]));
        assert!(!Activity::Idle.matches(&target, &[snapshot.clone()]));
        let item: AttentionItem = serde_json::from_value(serde_json::json!({
            "id":"question","evidence_hash":"hash","project":"/project","ts":"now","kind":"question",
            "workspace_root":"/project","session_id":"new","owner":{"pid":pid,"start":start}
        })).unwrap();
        let mut run = crate::navigation::fixture();
        run.targets = vec![target.clone()];
        assert!(matching_target(&item, &run).is_ok());
        let mut duplicate = target.clone();
        duplicate.key = "another-target".into();
        run.targets.push(duplicate);
        assert!(
            matching_target(&item, &run)
                .unwrap_err()
                .contains("Multiple")
        );
        snapshot.observations[0].owner.start = "different".into();
        assert!(observation(&target, &[snapshot.clone()]).is_none());
        assert!(Activity::Unknown.matches(&target, &[snapshot.clone()]));
        assert!(!Activity::Working.matches(&target, &[snapshot.clone()]));
        snapshot.observations = vec![
            event.clone(),
            ExecutionObservation {
                session_id: "old".into(),
                ..event
            },
        ];
        assert!(observation(&target, &[snapshot]).is_none());
    }
}
