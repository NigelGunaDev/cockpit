use crate::model::{ProjectSnapshot, Target};
use crate::tmux::{Tmux, output, process_start};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn own_socket(path: &str) -> Result<(), String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| format!("No tmux socket at {path}: {e}"))?;
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(format!(
            "Tmux socket must be a same-user Unix socket: {path}"
        ));
    }
    Ok(())
}

pub fn discover_project(start: &Path) -> Result<String, String> {
    let start = start.canonicalize().map_err(|e| e.to_string())?;
    if !start.is_dir() {
        return Err("Project must be an existing directory".into());
    }
    let mut command = Command::new("git");
    command
        .args([
            "-C",
            &start.to_string_lossy(),
            "rev-parse",
            "--show-toplevel",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0");
    Ok(output(command)
        .ok()
        .and_then(|root| Path::new(root.trim()).canonicalize().ok())
        .unwrap_or(start)
        .to_string_lossy()
        .into_owned())
}

pub fn project_snapshot(root: &str) -> Result<ProjectSnapshot, String> {
    let root = discover_project(Path::new(root))?;
    let mut command = Command::new("git");
    command
        .args(["-C", &root, "worktree", "list", "--porcelain"])
        .env("GIT_OPTIONAL_LOCKS", "0");
    let roots = output(command)
        .ok()
        .map(|text| {
            text.lines()
                .filter_map(|line| line.strip_prefix("worktree "))
                .take(64)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![root.clone()]);
    let workspaces = roots
        .iter()
        .map(|path| serde_json::json!({"root":path,"association":"workspace"}))
        .collect::<Vec<_>>();
    serde_json::from_value(serde_json::json!({"project_root":root,"complete":true,"workspaces":workspaces,"attention":{"project":root,"complete":true},"tasks":[]})).map_err(|e|e.to_string())
}

pub fn targets(socket: &str) -> Result<Vec<Target>, String> {
    own_socket(socket)?;
    let tmux = Tmux::new(socket);
    let server_pid = tmux.pid()?;
    let server_start = process_start(server_pid)?;
    let format = "#{session_id}\t#{session_created}\t#{session_name}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{pane_current_path}";
    let text = tmux.run(&["list-panes", "-a", "-F", format])?;
    let mut targets = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.splitn(8, '\t').collect();
        if fields.len() != 8 {
            return Err("Invalid tmux pane inventory".into());
        }
        let pane_pid: u32 = fields[5].parse().map_err(|_| "Invalid pane PID")?;
        let pane_start = process_start(pane_pid).unwrap_or_default();
        let key = format!(
            "{server_pid}:{server_start}:{}:{}:{}:{pane_pid}:{pane_start}",
            fields[0], fields[1], fields[4]
        );
        targets.push(Target {
            key,
            socket: socket.to_owned(),
            server_pid,
            server_start: server_start.clone(),
            session_id: fields[0].into(),
            session_created: fields[1].into(),
            session_name: fields[2].into(),
            window_id: fields[3].into(),
            pane_id: fields[4].into(),
            pane_pid,
            pane_start,
            cwd: fields[7].into(),
            dead: fields[6] == "1",
            project: None,
            workspace: None,
            task: None,
            claude_session_id: None,
            association: "unknown".into(),
        });
    }
    targets.sort_by(|a, b| (&a.session_name, &a.pane_id).cmp(&(&b.session_name, &b.pane_id)));
    Ok(targets)
}

pub fn associate(targets: &mut [Target], snapshots: &[ProjectSnapshot]) {
    for target in targets {
        let cwd = PathBuf::from(&target.cwd)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&target.cwd));
        let mut candidates = Vec::new();
        for snapshot in snapshots {
            for workspace in &snapshot.workspaces {
                let root = Path::new(&workspace.root);
                if cwd.starts_with(root) {
                    candidates.push((root.components().count(), snapshot, workspace));
                }
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
        if let Some((length, project, workspace)) = candidates.first() {
            if candidates.iter().filter(|c| c.0 == *length).count() > 1 {
                target.association = "ambiguous".into();
                continue;
            }
            target.project = Some(project.project_root.clone());
            target.workspace = Some(workspace.root.clone());
            target.association = workspace.association.clone();
            let task = match workspace.association.as_str() {
                "confirmed" => workspace.current_task.as_ref(),
                "inferred" if workspace.task_candidates.len() == 1 => {
                    Some(&workspace.task_candidates[0])
                }
                _ => None,
            };
            target.task = task
                .and_then(|t| t.get("name"))
                .and_then(|n| n.as_str())
                .map(str::to_owned);
            // Workspace metadata identifies a Claude run, not necessarily this
            // particular pane. Keep pane-level identity explicitly unknown.
            target.claude_session_id = None;
            if let Some(event) =
                crate::attention::observation(target, std::slice::from_ref(&project.attention))
            {
                target.claude_session_id = Some(event.session_id.clone());
            }
        }
    }
}

pub fn validate(target: &Target, control: bool) -> Result<(), String> {
    let live = targets(&target.socket)?;
    if !live.iter().any(|candidate| {
        candidate.key == target.key && candidate.window_id == target.window_id && !candidate.dead
    }) {
        return Err(
            "Selected terminal exited or was replaced; select a current session explicitly".into(),
        );
    }
    if control {
        control_policy(target)?;
    }
    Ok(())
}

pub fn control_policy(target: &Target) -> Result<(), String> {
    let value = Tmux::new(&target.socket).run(&[
        "show-options",
        "-Av",
        "-t",
        &target.session_id,
        "detach-on-destroy",
    ])?;
    if value != "on" {
        return Err(format!(
            "View only: original detach-on-destroy is {value}; control requires on. Original configuration was not changed."
        ));
    }
    Ok(())
}

pub fn capture_history(target: &Target) -> Result<String, String> {
    validate(target, false)?;
    let text = Tmux::new(&target.socket).run(&[
        "capture-pane",
        "-p",
        "-t",
        &target.tmux_target(),
        "-S",
        "-10000",
    ])?;
    Ok(sanitize(&text).chars().take(512 * 1024).collect())
}

pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn association_target(cwd: &str) -> Target {
        serde_json::from_value(serde_json::json!({
            "key":"generation", "socket":"/tmp/fixture.sock", "server_pid":1,
            "server_start":"1", "session_id":"$0", "session_created":"1",
            "session_name":"task", "window_id":"@0", "pane_id":"%0", "pane_pid":2,
            "pane_start":"2", "cwd":cwd, "dead":false, "project":null,
            "workspace":null,"task":null,"claude_session_id":null,"association":"unknown"
        }))
        .unwrap()
    }

    #[test]
    fn associations_use_longest_path_and_do_not_guess_claude_identity() {
        let snapshot: ProjectSnapshot = serde_json::from_value(serde_json::json!({
            "project_root":"/project", "complete":true, "workspaces":[
                {"root":"/project", "current_task":null, "association":"unknown"},
                {"root":"/project/.worktrees/task", "current_task":{"name":"CC-1"},
                 "claude_session_id":"not-a-pane-identity", "association":"confirmed"}
            ]
        }))
        .unwrap();
        let mut targets = vec![
            association_target("/project/.worktrees/task/src"),
            association_target("/project-sibling"),
        ];
        associate(&mut targets, &[snapshot]);
        assert_eq!(targets[0].task.as_deref(), Some("CC-1"));
        assert_eq!(targets[0].association, "confirmed");
        assert!(targets[0].claude_session_id.is_none());
        assert!(targets[1].project.is_none());
    }

    #[test]
    fn competing_workspace_associations_remain_ambiguous() {
        let snapshot = || {
            serde_json::from_value::<ProjectSnapshot>(serde_json::json!({
                "project_root":"/project", "complete":true, "workspaces":[
                    {"root":"/project", "current_task":{"name":"CC-1"}, "association":"confirmed"}
                ]
            }))
            .unwrap()
        };
        let mut targets = vec![association_target("/project/src")];
        associate(&mut targets, &[snapshot(), snapshot()]);
        assert_eq!(targets[0].association, "ambiguous");
        assert!(targets[0].task.is_none());
    }

    #[test]
    fn invalid_stored_task_is_not_presented_as_a_confirmed_task() {
        let snapshot: ProjectSnapshot = serde_json::from_value(serde_json::json!({
            "project_root":"/project", "complete":true, "workspaces":[
                {"root":"/project", "current_task":{"name":"stale-task"}, "association":"ambiguous"}
            ]
        }))
        .unwrap();
        let mut targets = vec![association_target("/project/src")];
        associate(&mut targets, &[snapshot]);
        assert_eq!(targets[0].association, "ambiguous");
        assert!(targets[0].task.is_none());
    }
    #[test]
    fn history_text_has_no_executable_controls() {
        assert_eq!(
            sanitize("hello\x1b]52;c;payload\x07\n日本語\r"),
            "hello]52;c;payload\n日本語"
        );
    }
    #[test]
    fn native_snapshot_uses_git_worktrees_and_never_runs_a_project_cli() {
        use std::{fs, os::unix::fs::PermissionsExt};
        let root = std::env::temp_dir().join(format!(
            "cp-native-project-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("sessions/bin")).unwrap();
        fs::write(
            root.join("sessions/bin/sessions"),
            "#!/bin/sh\ntouch executed-cli\nexit 99\n",
        )
        .unwrap();
        fs::set_permissions(
            root.join("sessions/bin/sessions"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let plain = project_snapshot(root.to_str().unwrap()).unwrap();
        assert!(plain.complete);
        assert!(plain.tasks.is_empty());
        assert_eq!(plain.workspaces.len(), 1);
        let run = |args: &[&str]| {
            let result = Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["commit", "-q", "--allow-empty", "-m", "fixture"]);
        let worktree = root.join("child worktree");
        run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "work",
            worktree.to_str().unwrap(),
        ]);
        let observed = project_snapshot(root.to_str().unwrap()).unwrap();
        assert!(observed.complete);
        assert_eq!(observed.workspaces.len(), 2);
        assert!(
            observed
                .workspaces
                .iter()
                .any(|w| w.root == worktree.to_string_lossy())
        );
        assert!(!root.join("executed-cli").exists());
        assert!(!root.join("sessions/sessions-state.json").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn project_discovery_accepts_plain_directories_without_an_installed_cli() {
        assert_eq!(discover_project(Path::new("/dev")).unwrap(), "/dev");
        assert!(discover_project(Path::new("/definitely-missing-cockpit-project")).is_err());
    }
}
