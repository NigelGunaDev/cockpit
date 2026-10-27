//! Narrow adapter to the existing CLI. No protocol/state writes or terminal injection.
use crate::model::{Run, Target, TaskEntry};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateRequest {
    pub cli_root: String,
    pub name: String,
    pub request: String,
}

pub fn selected_task<'a>(run: &'a Run, token: &str) -> Result<&'a TaskEntry, String> {
    let mut matches = run.tasks.iter().filter(|task| task.token == token);
    let task = matches
        .next()
        .ok_or("Task evidence changed; reload the queue and confirm again")?;
    if matches.next().is_some() || task.association == "ambiguous" {
        return Err("Task ownership is ambiguous; use the existing workflow to resolve it".into());
    }
    Ok(task)
}

pub fn task_target<'a>(run: &'a Run, task: &TaskEntry) -> Result<&'a Target, String> {
    if task.association != "owned" || task.state_health != "readable" {
        return Err("Viewing a task needs a unique healthy workspace".into());
    }
    // Viewing grants no input or lifecycle authority.
    let mut matches = run.targets.iter().filter(|target| {
        !target.dead
            && target.project.as_deref() == Some(task.project.as_str())
            && target.workspace == task.workspace
            && target.claude_session_id.is_some()
    });
    let target = matches.next().ok_or(
        "No verified live Claude session for this workspace; select its terminal manually",
    )?;
    if matches.next().is_some() {
        return Err(
            "Several Claude terminals belong to this workspace; select one manually".into(),
        );
    }
    Ok(target)
}

pub fn decision_args(task: &TaskEntry, approve: bool) -> Result<Vec<String>, String> {
    if !task.current || task.association != "owned" || task.state_health != "readable" {
        return Err("A confirmed current task is required for this decision".into());
    }
    let evidence = task
        .supervision
        .as_ref()
        .ok_or("Task has no validated decision evidence")?;
    if !(if approve {
        evidence.can_approve
    } else {
        evidence.can_retry
    }) {
        return Err(if approve {
            "No refused batch plan is eligible for approval"
        } else {
            "No stopped step is eligible for one retry"
        }
        .into());
    }
    Ok(vec![
        "tasks".into(),
        if approve { "approve" } else { "resume" }.into(),
        task.workspace_name.clone().ok_or("No workspace name")?,
        "--expect".into(),
        evidence.token.clone(),
        "--json".into(),
    ])
}

pub fn create_args(request: &CreateRequest) -> Result<Vec<String>, String> {
    if request.request.trim().is_empty()
        || request.request.len() > 2048
        || request.name.len() > 128
        || request.request.contains('\0')
        || request.name.contains('\0')
        || request.request.trim().starts_with("--")
        || request.name.trim().starts_with("--")
    {
        return Err(
            "Enter a request of 1–2048 bytes and an optional name of at most 128 bytes".into(),
        );
    }
    let mut args = vec!["task".into(), "new".into()];
    if !request.name.trim().is_empty() {
        args.push(request.name.trim().into());
    }
    args.push(request.request.trim().into());
    args.push("--json".into());
    Ok(args)
}

pub fn check_launcher_socket(run: &Run) -> Result<(), String> {
    let mut command = Command::new("tmux");
    command.args(["display-message", "-p", "#{socket_path}"]);
    let default = crate::tmux::output(command).map_err(|_| "Creation uses the existing launcher's default tmux server. Launch on that server or create with sessions task new in your terminal.")?;
    if default != run.original_socket {
        return Err("Creation refused: the cockpit uses a custom socket, but the existing launcher uses the default server. Create from your terminal; no socket configuration was changed.".into());
    }
    Ok(())
}

pub fn decode_result(text: &str, operation: &str, success: bool) -> Result<String, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| "CLI returned an unstructured result; inspect the workspace before retrying and update its installation")?;
    if !success
        || value.get("ok").and_then(|v| v.as_bool()) != Some(true)
        || value.get("error").is_some_and(|v| !v.is_null())
    {
        let message = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("CLI refused the operation");
        let recovery = value
            .get("workspace")
            .filter(|v| !v.is_null())
            .map(|v| format!(" · Preserved workspace: {v}; inspect it before retrying"))
            .unwrap_or_default();
        return Err(format!("{message}{recovery}"));
    }
    if value.get("operation").and_then(|v| v.as_str()) != Some(operation) {
        return Err(
            "CLI operation contract mismatch; inspect current state before retrying".into(),
        );
    }
    if operation == "CreateTask" {
        return Ok(format!(
            "Creation workflow launched in {}. Select its terminal and confirm the spec in Claude; the existing workflow then chains stages.{}",
            value
                .pointer("/workspace/path")
                .and_then(|v| v.as_str())
                .unwrap_or("workspace"),
            value
                .get("warnings")
                .filter(|v| v.as_array().is_some_and(|a| !a.is_empty()))
                .map(|v| format!(" Warnings: {v}"))
                .unwrap_or_default()
        ));
    }
    if operation == "RestoreSession" || operation == "CloseSession" {
        let outcome = value
            .get("outcome")
            .and_then(|v| v.as_str())
            .ok_or("Missing recovery outcome")?;
        let accepted = if operation == "RestoreSession" {
            &["started", "existing", "uncertain"][..]
        } else {
            &["closed", "already-ended"][..]
        };
        if !accepted.contains(&outcome) {
            return Err("Unexpected recovery outcome".into());
        }
        let message = match outcome {
            "started" => "Conversation reopened",
            "existing" => "Connected to the existing terminal",
            "uncertain" => "Startup needs inspection",
            "closed" => "Terminal closed",
            _ => "Terminal already ended",
        };
        if operation == "CloseSession" {
            return Ok(message.into());
        }
        return Ok(format!(
            "{message} · {}",
            if value.get("readiness").and_then(|v| v.as_str()) == Some("native-file-open") {
                "native session file opened"
            } else {
                "conversation loading is unconfirmed"
            }
        ));
    }
    if value
        .pointer("/decision/recorded")
        .and_then(|v| v.as_bool())
        != Some(true)
    {
        return Err(
            "CLI did not confirm a recorded decision; inspect state before retrying".into(),
        );
    }
    let result = if operation == "ApproveRefusedBatchPlan" {
        "Batch-plan approval recorded"
    } else {
        "One stopped-step retry recorded (no process restart)"
    };
    if value.pointer("/wake/woken").and_then(|v| v.as_bool()) == Some(true) {
        Ok(format!(
            "{result}; inbox message sent. This does not confirm execution."
        ))
    } else {
        Ok(format!(
            "{result}; session not woken ({}). Attach and continue; do not repeat the decision.",
            value
                .pointer("/wake/error")
                .and_then(|v| v.as_str())
                .unwrap_or("no confirmation")
        ))
    }
}

pub struct Outcome {
    pub message: String,
    pub workspace: Option<String>,
    pub structured: serde_json::Value,
}

pub fn execute(
    cli_root: &str,
    project: &str,
    args: &[String],
    operation: &str,
) -> Result<Outcome, String> {
    let mut command = Command::new(Path::new(cli_root).join("sessions/bin/sessions"));
    command
        .args(args)
        .current_dir(project)
        .env("CLAUDE_PROJECT_DIR", project)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot launch existing CLI: {e}"))?;
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        })
    };
    let out = read(Box::new(child.stdout.take().unwrap()));
    let err = read(Box::new(child.stderr.take().unwrap()));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < Duration::from_secs(60) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                // Kill only this CLI client, never its process group: the launcher
                // may already have started persistent work. Never retry automatically.
                let _ = child.kill();
                let _ = child.wait();
                return Err("CLI result unknown after timeout/wait failure. Inspect task state and preserved workspaces before retrying.".into());
            }
        }
    };
    let bytes = out
        .join()
        .map_err(|_| "CLI output reader failed")?
        .map_err(|e| e.to_string())?;
    let errors = err
        .join()
        .map_err(|_| "CLI error reader failed")?
        .map_err(|e| e.to_string())?;
    if bytes.len() > 2 * 1024 * 1024 || errors.len() > 2 * 1024 * 1024 {
        return Err(
            "CLI output exceeded limit; operation outcome unknown, inspect before retrying".into(),
        );
    }
    let text = String::from_utf8(bytes).map_err(|_| "CLI returned invalid UTF-8")?;
    let bounded = |text: String| {
        if text.chars().count() <= 4096 {
            text
        } else {
            format!(
                "{} … Result display limited; inspect the workspace/CLI before retrying.",
                text.chars().take(4096).collect::<String>()
            )
        }
    };
    let workspace = if operation == "CreateTask" {
        serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/workspace/path")
                    .and_then(|p| p.as_str())
                    .map(str::to_owned)
            })
    } else {
        None
    };
    decode_result(&text, operation, status.success())
        .map(bounded)
        .map(|message| Outcome {
            message,
            workspace,
            structured: serde_json::from_str(&text).unwrap_or_default(),
        })
        .map_err(|e| {
            if text.trim().is_empty() {
                bounded(format!("{e}: {}", String::from_utf8_lossy(&errors).trim()))
            } else {
                bounded(e)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run() -> Run {
        serde_json::from_value(serde_json::json!({
            "ui_socket":"/tmp/private.sock", "supervisor_pid":1,"supervisor_start":"1",
            "ui_server_pid":2,"ui_server_start":"2","original_socket":"/tmp/original.sock",
            "executable":"cockpit","leader":"C-]","projects":["/project"],"targets":[],
            "selected":null,"terminal_pane":null,"sidebar_pane":null,"control":false,
            "message":"","tiles":[],"view":"focus","preferences":null
        }))
        .unwrap()
    }
    fn task() -> TaskEntry {
        TaskEntry {
            token: "fresh".into(),
            project: "/project".into(),
            name: "task".into(),
            file: "task/README.md".into(),
            branch: "feature/task".into(),
            status: "pending".into(),
            workspace: Some("/project/.worktrees/task".into()),
            association: "owned".into(),
            state_health: "readable".into(),
            ..Default::default()
        }
    }
    fn target() -> Target {
        serde_json::from_value(serde_json::json!({
            "key":"generation","socket":"/tmp/original.sock","server_pid":1,"server_start":"1",
            "session_id":"$1","session_created":"1","session_name":"task","window_id":"@1",
            "pane_id":"%1","pane_pid":2,"pane_start":"2","cwd":"/project/.worktrees/task",
            "dead":false,"project":"/project","workspace":"/project/.worktrees/task",
            "task":null,"claude_session_id":null,"association":"unknown"
        }))
        .unwrap()
    }
    #[test]
    fn stale_tokens_and_ambiguous_ownership_cannot_receive_decisions() {
        let mut run = run();
        run.tasks.push(task());
        assert!(selected_task(&run, "old").is_err());
        run.tasks[0].association = "ambiguous".into();
        assert!(selected_task(&run, "fresh").is_err());
    }
    #[test]
    fn task_navigation_requires_one_verified_claude_target() {
        let mut run = run();
        run.targets.push(target());
        let mut task = task();
        assert!(task_target(&run, &task).is_err());
        run.targets[0].claude_session_id = Some("verified".into());
        assert_eq!(task_target(&run, &task).unwrap().key, "generation");
        run.targets.push(run.targets[0].clone());
        assert!(task_target(&run, &task).is_err());
        run.targets.pop();
        for (protocol, status) in [
            ("task-creation", "pending"),
            ("task-startup", "pending"),
            ("task-execution", "in-progress"),
            ("task-completion", "review"),
        ] {
            task.status = status.into();
            task.protocol = Some(protocol.into());
            let before = serde_json::to_value(&task).unwrap();
            assert_eq!(task_target(&run, &task).unwrap().key, "generation");
            assert_eq!(serde_json::to_value(&task).unwrap(), before);
        }
        assert!(
            !run.control,
            "Target lookup grants no input or startup authority"
        );
    }
    #[test]
    fn task_navigation_refuses_unusable_or_mismatched_targets() {
        let mut run = run();
        let mut target = target();
        target.claude_session_id = Some("verified".into());
        run.targets.push(target);
        let mut task = task();
        task.protocol = Some("task-creation".into());
        task.state_health = "corrupt".into();
        assert!(task_target(&run, &task).is_err());
        task.state_health = "readable".into();
        task.state_health = "missing".into();
        assert!(task_target(&run, &task).is_err());
        task.state_health = "readable".into();
        task.status = "pending".into();
        task.association = "ambiguous".into();
        assert!(task_target(&run, &task).is_err());
        task.association = "owned".into();
        run.targets[0].dead = true;
        assert!(task_target(&run, &task).is_err());
        run.targets[0].dead = false;
        run.targets[0].workspace = Some("/project/.worktrees/other".into());
        assert!(task_target(&run, &task).is_err());
        run.targets[0].workspace = task.workspace.clone();
        run.targets[0].project = Some("/other-project".into());
        assert!(task_target(&run, &task).is_err());
    }
    #[test]
    fn errors_with_zero_exit_status_are_refused() {
        assert!(decode_result(r#"{"error":"denied"}"#, "CreateTask", true).is_err());
        assert!(
            decode_result(
                r#"{"ok":true,"operation":"wrong","error":null}"#,
                "CreateTask",
                true
            )
            .is_err()
        );
    }
    #[test]
    fn failed_wake_is_a_recorded_decision_not_a_retry() {
        let text = r#"{"ok":true,"operation":"RetryStoppedStep","decision":{"recorded":true},"wake":{"woken":false,"error":"no socket"},"error":null}"#;
        let result = decode_result(text, "RetryStoppedStep", true).unwrap();
        assert!(result.contains("do not repeat"));
        assert!(result.contains("no process restart"));
    }
    #[test]
    fn creation_keeps_request_as_one_literal_argument() {
        let request = CreateRequest {
            cli_root: "/project".into(),
            name: "CC-1".into(),
            request: "$(false) `false` --expect hi".into(),
        };
        assert_eq!(create_args(&request).unwrap()[3], request.request);
        assert!(!create_args(&request).unwrap().iter().any(|a| a == "start"));
    }
}
