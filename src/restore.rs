//! One transient worker per cockpit; bundled native operations own process recovery.
use crate::{
    backend,
    model::{Run, Target},
    session_registry::{self, Entry, Registry},
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::mpsc::{self, Receiver, Sender},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub status: String,
    pub message: String,
    pub target: Option<Target>,
    pub loaded: bool,
}
#[derive(Clone)]
struct Work {
    path: String,
    entry: Entry,
    projects: Vec<String>,
    socket: String,
    close: Option<Target>,
}
pub struct Worker {
    send: Sender<Work>,
    pub receive: Receiver<Job>,
}
pub fn socket_matches(entry: &Entry, socket: &str) -> bool {
    entry.snapshot.target.socket == socket
        || entry
            .snapshot
            .socket_name
            .as_ref()
            .is_some_and(|name| session_registry::socket_name(socket).as_ref() == Some(name))
}
pub fn scope_matches(entry: &Entry, projects: &[String]) -> bool {
    entry
        .snapshot
        .target
        .project
        .as_ref()
        .is_some_and(|root| projects.contains(root))
        || entry.snapshot.repository.as_ref().is_some_and(|repo| {
            projects.iter().any(|p| {
                p == &repo.root
                    || Path::new(&repo.common_directory)
                        .parent()
                        .is_some_and(|root| Path::new(p) == root)
            })
        })
}
pub fn project_for(entry: &Entry, projects: &[String]) -> Option<String> {
    let repo = entry.snapshot.repository.as_ref()?;
    projects
        .iter()
        .find(|root| {
            session_registry::repository(root).is_some_and(|r| {
                r.common_directory == repo.common_directory
                    && r.common_device == repo.common_device
                    && r.common_inode == repo.common_inode
            })
        })
        .cloned()
}
pub fn arguments(
    entry: &Entry,
    socket: &str,
    close: Option<&Target>,
) -> Result<Vec<String>, String> {
    let repo = entry
        .snapshot
        .repository
        .as_ref()
        .ok_or("No registered Git repository identity; link from a project first")?;
    let current = session_registry::repository(&entry.snapshot.identity.native_cwd)
        .ok_or("Saved workspace is unavailable")?;
    if current.root != repo.root
        || current.common_directory != repo.common_directory
        || current.common_device != repo.common_device
        || current.common_inode != repo.common_inode
        || current.branch != repo.branch
        || repo.branch.is_none()
    {
        return Err("Repository/worktree identity or branch changed; nothing started".into());
    }
    let id = &entry.snapshot.identity;
    let mut args = vec![
        "workspace".into(),
        if close.is_some() {
            "close-session"
        } else {
            "recover"
        }
        .into(),
    ];
    for (key, value) in [
        ("--root", repo.root.clone()),
        ("--cwd", id.native_cwd.clone()),
        ("--agent", id.kind.key().into()),
        ("--session-id", id.native_session_id.clone()),
        ("--state-root", id.state_root.clone()),
        ("--session-file", id.native_file.clone()),
        (
            "--tmux-name",
            close.unwrap_or(&entry.snapshot.target).session_name.clone(),
        ),
        ("--socket", socket.into()),
        ("--common-dir", repo.common_directory.clone()),
        ("--common-device", repo.common_device.to_string()),
        ("--common-inode", repo.common_inode.to_string()),
        ("--branch", repo.branch.clone().unwrap()),
    ] {
        args.push(key.into());
        args.push(value);
    }
    if let Some(target) = close {
        args.push("--expected-key".into());
        args.push(target.key.clone());
    }
    args.push("--json".into());
    Ok(args)
}
fn perform(work: Work) -> Job {
    let id = work.entry.id.clone();
    let mut job = Job {
        id: id.clone(),
        status: "failed".into(),
        message: String::new(),
        target: None,
        loaded: false,
    };
    let mut forgotten = false;
    let result = (|| -> Result<(), String> {
        let _lock = session_registry::operation_lock(Path::new(&work.path), &id)?;
        let current = Registry::open(Path::new(&work.path))?
            .list()?
            .into_iter()
            .find(|e| e.id == id)
            .ok_or("Registration disappeared")?;
        if current.revision != work.entry.revision || (!current.enabled && work.close.is_none()) {
            return Err("Saved intent changed; refresh before retrying".into());
        }
        let project = project_for(&current, &work.projects)
            .ok_or("No matching saved Git project is available")?;
        let args = arguments(&current, &work.socket, work.close.as_ref())?;
        if work.close.is_some() {
            Registry::open(Path::new(&work.path))?.forget(&id, current.revision)?;
            forgotten = true;
        }
        // No registry transaction is held while the native helper creates or closes a process.
        let result = backend::execute(&project, &args)?;
        let value = result.structured;
        job.status = value["outcome"]
            .as_str()
            .ok_or("Missing recovery outcome")?
            .into();
        job.message = format!(
            "{}{}",
            result.message,
            value["warnings"]
                .as_array()
                .filter(|v| !v.is_empty())
                .map(|v| format!(
                    " · {}",
                    v.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                ))
                .unwrap_or_default()
        );
        job.loaded = value["readiness"] == "native-file-open";
        job.target = value
            .get("target")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value(v.clone()).map_err(|e| e.to_string()))
            .transpose()?;
        if let Some(target) = &job.target {
            if target.socket != work.socket
                || Path::new(&target.cwd).canonicalize().ok()
                    != Path::new(&current.snapshot.identity.native_cwd)
                        .canonicalize()
                        .ok()
            {
                return Err("Native helper returned a foreign terminal".into());
            }
            crate::observe::validate(target, false)?;
            let _ = arguments(&current, &work.socket, None)?;
            // Requested identity is useful for view-only navigation even before
            // native loading/trust completes. It is not a confirmed rebinding.
            if job.loaded {
                let mut db = Registry::open(Path::new(&work.path))?;
                let fresh = db
                    .list()?
                    .into_iter()
                    .find(|e| e.id == id)
                    .ok_or("Registration disappeared")?;
                if !fresh.enabled || fresh.revision != current.revision {
                    return Err("Saved intent changed during recovery".into());
                }
                db.remember(current.snapshot.identity.clone(), target, false)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        job.status = "failed".into();
        job.message = if forgotten {
            format!("Forgotten; close result unconfirmed: {e}")
        } else {
            e
        };
        job.target = None;
    }
    job
}
impl Worker {
    pub fn new() -> Self {
        let (send, requests) = mpsc::channel::<Work>();
        let (results, receive) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(work) = requests.recv() {
                if results.send(perform(work)).is_err() {
                    break;
                }
            }
        });
        Self { send, receive }
    }
    pub fn schedule(
        &self,
        run: &mut Run,
        entry: Entry,
        close: Option<Target>,
    ) -> Result<(), String> {
        if !socket_matches(&entry, &run.original_socket) {
            return Err("Registration belongs to another socket".into());
        }
        if run
            .recovery
            .jobs
            .iter()
            .any(|j| j.id == entry.id && j.status == "queued")
        {
            return Err("Operation already queued".into());
        }
        let job = Job {
            id: entry.id.clone(),
            status: "queued".into(),
            message: if close.is_some() {
                "Closing terminal"
            } else {
                "Restoring remembered conversation"
            }
            .into(),
            target: None,
            loaded: false,
        };
        self.send
            .send(Work {
                path: run.recovery.path.clone(),
                entry,
                projects: run.projects.clone(),
                socket: run.original_socket.clone(),
                close,
            })
            .map_err(|_| "Recovery worker unavailable")?;
        run.recovery.jobs.retain(|j| j.id != job.id);
        run.recovery.jobs.push(job);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Identity, Kind};
    fn entry(root: &str) -> Entry {
        Entry{id:"1234-abcd".into(),enabled:true,revision:1,updated:0,snapshot:session_registry::Snapshot{
            socket_name:None,boot_id:"old-boot".into(),repository:session_registry::repository(root),
            identity:Identity{kind:Kind::Pi,native_session_id:"custom-id".into(),state_root:root.into(),native_file:format!("{root}/session.jsonl"),native_cwd:root.into()},
            target:serde_json::from_value(serde_json::json!({"key":"old-generation","socket":"/tmp/original.sock","server_pid":1,"server_start":"1","session_id":"$0","session_created":"1","session_name":"task-a","window_id":"@0","pane_id":"%0","pane_pid":2,"pane_start":"2","cwd":root,"dead":false,"project":root,"workspace":root,"task":null,"claude_session_id":null,"association":"unknown"})).unwrap()
        }}
    }
    #[test]
    fn recovery_args_preserve_exact_identity_and_refuse_repository_changes() {
        let dir = std::env::temp_dir().join(format!("cockpit-recover-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q", "-b", "main"])
                .arg(&dir)
                .status()
                .unwrap()
                .success()
        );
        let mut record = entry(dir.to_str().unwrap());
        let args = arguments(&record, "/tmp/custom.sock", None).unwrap();
        assert!(args.windows(2).any(|p| p == ["--session-id", "custom-id"]));
        assert!(!args.iter().any(|a| a == "new" || a == "--last"));
        assert!(project_for(&record, &[dir.to_string_lossy().into()]).is_some());
        assert!(socket_matches(&record, "/tmp/original.sock"));
        assert!(!socket_matches(&record, "/tmp/other.sock"));
        record.snapshot.repository.as_mut().unwrap().branch = Some("other".into());
        assert!(arguments(&record, "/tmp/custom.sock", None).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn per_session_locks_serialize_only_the_same_registration_and_release_on_drop() {
        let dir =
            std::env::temp_dir().join(format!("cockpit-operation-lock-{}", std::process::id()));
        let db = dir.join("sessions.sqlite3");
        drop(Registry::open(&db).unwrap());
        let one = session_registry::operation_lock(&db, "aaaa-bbbb").unwrap();
        assert!(session_registry::operation_lock(&db, "aaaa-bbbb").is_err());
        assert!(session_registry::operation_lock(&db, "cccc-dddd").is_ok());
        assert!(session_registry::operation_lock(&db, "../evil").is_err());
        drop(one);
        assert!(session_registry::operation_lock(&db, "aaaa-bbbb").is_ok());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
