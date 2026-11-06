//! Native identity evidence only. This module never starts an agent or reads credentials.
use crate::model::Target;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Claude,
    Codex,
    Pi,
    #[default]
    Unknown,
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Pi => "Pi",
            Self::Unknown => "Unknown",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::Unknown => "unknown",
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Claude => Self::Codex,
            Self::Codex => Self::Pi,
            _ => Self::Claude,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Identity {
    pub kind: Kind,
    pub native_session_id: String,
    pub state_root: String,
    pub native_file: String,
    pub native_cwd: String,
}

fn clean_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !matches!(id, "." | "..")
        && !id.trim().is_empty()
        && !id.chars().any(char::is_control)
}

fn native_rows(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("Native session unavailable: {e}"))?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.uid() != unsafe { libc::geteuid() }
    {
        return Err("Native session must be a same-user regular file, not a symlink".into());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| e.to_string())?;
    let opened = file.metadata().map_err(|e| e.to_string())?;
    if (meta.dev(), meta.ino()) != (opened.dev(), opened.ino()) || !opened.is_file() {
        return Err("Native session changed while opening".into());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(256 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .take(1024)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// Explicit native-file linking still validates type, ID, storage shape and cwd.
/// Exported conversation text cannot stand in for native resumable storage.
pub fn from_file(kind: Kind, path: &Path, workspace: &Path) -> Result<Identity, String> {
    if !path.is_absolute() || kind == Kind::Unknown {
        return Err("Choose an agent and an absolute native session file".into());
    }
    let rows = native_rows(path)?;
    let path = path.canonicalize().map_err(|e| e.to_string())?;
    let workspace = workspace.canonicalize().map_err(|e| e.to_string())?;
    let (id, cwd) = match kind {
        Kind::Codex => rows
            .iter()
            .find(|r| r["type"] == "session_meta")
            .and_then(|r| Some((r["payload"]["id"].as_str()?, r["payload"]["cwd"].as_str()?))),
        Kind::Pi => rows
            .first()
            .filter(|r| r["type"] == "session")
            .and_then(|r| Some((r["id"].as_str()?, r["cwd"].as_str()?))),
        Kind::Claude => rows
            .iter()
            .find_map(|r| Some((r["sessionId"].as_str()?, r["cwd"].as_str()?))),
        Kind::Unknown => None,
    }
    .ok_or("No native agent session header with identity and workspace was found")?;
    if !Path::new(cwd).is_absolute() {
        return Err("Native workspace must be absolute".into());
    }
    let native_cwd = Path::new(cwd)
        .canonicalize()
        .map_err(|_| "Native session workspace is unavailable")?;
    if !clean_id(id) || native_cwd != workspace {
        return Err("Native conversation belongs to another workspace or has an invalid ID".into());
    }
    let parent = path
        .parent()
        .ok_or("Native session has no storage directory")?;
    let state = match kind {
        Kind::Claude => {
            if path.file_stem().and_then(|s| s.to_str()) != Some(id)
                || parent
                    .parent()
                    .and_then(|p| p.file_name())
                    .and_then(|s| s.to_str())
                    != Some("projects")
            {
                return Err(
                    "Choose a native Claude projects/<project>/<session-ID>.jsonl file".into(),
                );
            }
            parent.parent().and_then(|p| p.parent()).unwrap()
        }
        Kind::Codex => {
            if !path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(&format!("{id}.jsonl"))
            {
                return Err("Codex transcript filename does not match its native ID".into());
            }
            parent
                .ancestors()
                .find(|p| p.file_name().is_some_and(|n| n == "sessions"))
                .and_then(|p| p.parent())
                .ok_or("Choose a native Codex sessions/.../rollout-<ID>.jsonl file")?
        }
        Kind::Pi => parent,
        Kind::Unknown => unreachable!(),
    };
    Ok(Identity {
        kind,
        native_session_id: id.into(),
        state_root: state.to_string_lossy().into(),
        native_file: path.to_string_lossy().into(),
        native_cwd: native_cwd.to_string_lossy().into(),
    })
}

fn argv_kind(argv: &[String]) -> Kind {
    let Some(first) = argv.first() else {
        return Kind::Unknown;
    };
    match Path::new(first)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
    {
        "claude" => Kind::Claude,
        "codex" => Kind::Codex,
        "pi" => Kind::Pi,
        "node" | "bun" => {
            let script = argv.get(1).map(String::as_str).unwrap_or("");
            if script.contains("/@anthropic-ai/claude-code/") {
                Kind::Claude
            } else if script.contains("/@openai/codex/") {
                Kind::Codex
            } else if script.contains("/pi-coding-agent/") {
                Kind::Pi
            } else {
                Kind::Unknown
            }
        }
        _ => Kind::Unknown,
    }
}

fn process(target: &Target) -> Option<(Kind, u32, Vec<String>)> {
    let mut queue = vec![target.pane_pid];
    let mut visited = 0;
    let mut candidates = Vec::new();
    while let Some(pid) = queue.pop() {
        visited += 1;
        if visited > 128 {
            return None;
        }
        let bytes = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let argv: Vec<_> = bytes
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        let kind = argv_kind(&argv);
        if kind != Kind::Unknown {
            candidates.push((kind, pid, argv));
            continue; // Wrappers may exec/spawn the same agent: count only the outer CLI.
        }
        if let Ok(children) = fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            queue.extend(
                children
                    .split_whitespace()
                    .filter_map(|p| p.parse::<u32>().ok()),
            );
        }
    }
    if candidates.len() == 1 {
        candidates.pop()
    } else {
        None
    }
}

pub fn kind(target: &Target) -> Kind {
    process(target)
        .map(|p| p.0)
        .unwrap_or(if target.claude_session_id.is_some() {
            Kind::Claude
        } else {
            Kind::Unknown
        })
}

fn variable(pid: u32, name: &str) -> Option<PathBuf> {
    let bytes = fs::read(format!("/proc/{pid}/environ")).ok()?;
    bytes.split(|b| *b == 0).find_map(|b| {
        let value = std::str::from_utf8(b)
            .ok()?
            .strip_prefix(&format!("{name}="))?;
        Path::new(value).is_absolute().then(|| PathBuf::from(value))
    })
}

fn exact_arg(argv: &[String], flag: &str) -> Option<String> {
    argv.windows(2)
        .find(|p| p[0] == flag && !p[1].starts_with('-'))
        .map(|p| p[1].clone())
}

/// Only an exact resume argument or existing verified Claude observation can
/// identify a live conversation. Never pick history by cwd or modification time.
pub fn identify(target: &Target) -> Result<Identity, String> {
    let detected = process(target);
    let kind = detected
        .as_ref()
        .map(|p| p.0)
        .unwrap_or(if target.claude_session_id.is_some() {
            Kind::Claude
        } else {
            Kind::Unknown
        });
    let argv = detected.as_ref().map(|p| p.2.as_slice()).unwrap_or(&[]);
    let pid = detected.as_ref().map(|p| p.1).unwrap_or(target.pane_pid);
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unavailable")?);
    let workspace = Path::new(&target.cwd);
    if kind == Kind::Pi {
        let file = exact_arg(argv, "--session")
            .filter(|s| Path::new(s).is_absolute())
            .ok_or("Pi identity unavailable; link its exact native session file")?;
        return from_file(kind, Path::new(&file), workspace);
    }
    let id = match kind {
        Kind::Claude => target
            .claude_session_id
            .clone()
            .or_else(|| exact_arg(argv, "--resume"))
            .or_else(|| exact_arg(argv, "-r")),
        Kind::Codex => exact_arg(argv, "resume"),
        _ => None,
    }
    .filter(|id| clean_id(id))
    .ok_or("Conversation not verified; link the agent's native session file")?;
    let root = match kind {
        Kind::Claude => variable(pid, "CLAUDE_CONFIG_DIR")
            .unwrap_or(home.join(".claude"))
            .join("projects"),
        Kind::Codex => variable(pid, "CODEX_HOME")
            .unwrap_or(home.join(".codex"))
            .join("sessions"),
        _ => return Err("Agent is unknown; live terminal use is still available".into()),
    };
    let mut directories = vec![(root, 0)];
    let mut found = Vec::new();
    let mut visited = 0;
    while let Some((dir, depth)) = directories.pop() {
        let entries = fs::read_dir(dir).map_err(|e| format!("Native history unavailable: {e}"))?;
        for entry in entries {
            visited += 1;
            if visited > 8192 {
                return Err(
                    "Native history lookup bounded; link the exact native file instead".into(),
                );
            }
            let entry = entry.map_err(|e| e.to_string())?;
            let ty = entry.file_type().map_err(|e| e.to_string())?;
            if ty.is_dir() && depth < 4 {
                directories.push((entry.path(), depth + 1));
            }
            if ty.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .ends_with(&format!("{id}.jsonl"))
            {
                let identity = from_file(kind, &entry.path(), workspace)?;
                if identity.native_session_id == id {
                    found.push(identity);
                }
            }
        }
    }
    if found.len() != 1 {
        return Err("Native conversation is missing or ambiguous; link its exact file".into());
    }
    Ok(found.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kinds_do_not_classify_shell_text_or_unrelated_scripts() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            argv_kind(&args(&["bash", "-c", "codex resume id"])),
            Kind::Unknown
        );
        assert_eq!(
            argv_kind(&args(&["node", "/tmp/pi-script.js"])),
            Kind::Unknown
        );
        assert_eq!(argv_kind(&args(&["codex", "resume", "id"])), Kind::Codex);
        assert_eq!(
            argv_kind(&args(&[
                "node",
                "/x/node_modules/@earendil-works/pi-coding-agent/dist/cli.js"
            ])),
            Kind::Pi
        );
    }
    #[test]
    fn native_files_require_exact_identity_workspace_and_existing_non_symlink_file() {
        let root = std::env::temp_dir().join(format!("cockpit-native-{}", std::process::id()));
        fs::create_dir_all(root.join("sessions/2026/10/19")).unwrap();
        let path = root.join("sessions/2026/10/19/rollout-id.jsonl");
        fs::write(
            &path,
            serde_json::json!({"type":"session_meta","payload":{"id":"id","cwd":root}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            from_file(Kind::Codex, &path, &root)
                .unwrap()
                .native_session_id,
            "id"
        );
        assert!(from_file(Kind::Claude, &path, &root).is_err());
        assert!(from_file(Kind::Codex, &path, Path::new("/tmp")).is_err());
        let missing = root.join("missing.jsonl");
        assert!(from_file(Kind::Pi, &missing, &root).is_err());
        assert!(!missing.exists());
        let pi = root.join("pi.jsonl");
        fs::write(
            &pi,
            serde_json::json!({"type":"session","id":"custom-pi-id","cwd":root}).to_string(),
        )
        .unwrap();
        assert_eq!(
            from_file(Kind::Pi, &pi, &root).unwrap().native_session_id,
            "custom-pi-id"
        );
        let link = root.join("link.jsonl");
        std::os::unix::fs::symlink(&pi, &link).unwrap();
        assert!(from_file(Kind::Pi, &link, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
