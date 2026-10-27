//! Durable presentation preferences. Never task state or startup authorization.
use crate::model::{Run, Target, View};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
pub struct Preferences {
    pub version: u32,
    pub socket: String,
    pub projects: Vec<String>,
    pub targets: Vec<Target>,
    pub selected: Option<String>,
    pub view: View,
    #[serde(default)]
    pub control: bool,
    /// Logical registry references for future recovery; never runtime authority.
    #[serde(default)]
    pub tile_registrations: Vec<Option<String>>,
}

fn private_file(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > 128 * 1024
    {
        return Err("Preferences must be a private, bounded, same-user regular file".into());
    }
    Ok(())
}

pub fn read(path: &Path, run: &Run) -> Result<Option<Preferences>, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
        Ok(_) => private_file(path)?,
    }
    let value: Preferences = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if value.version != 1
        || value.socket != run.original_socket
        || value.projects != run.projects
        || value.targets.len() > crate::layout::MAX_TILES
        || (!value.tile_registrations.is_empty()
            && value.tile_registrations.len() != value.targets.len())
    {
        return Err(
            "Preferences belong to another socket/project set or unsupported version".into(),
        );
    }
    let mut keys = std::collections::HashSet::new();
    if value
        .targets
        .iter()
        .any(|t| !keys.insert(&t.key) || t.socket != run.original_socket)
    {
        return Err("Duplicate or foreign targets in preferences".into());
    }
    if value
        .selected
        .as_ref()
        .is_some_and(|key| !keys.contains(key))
        || (value.selected.is_none() && !value.targets.is_empty())
    {
        return Err("Preferences have an invalid selected target; use the current picker".into());
    }
    Ok(Some(value))
}

pub fn save(path: &Path, run: &Run) -> Result<(), String> {
    if matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("sessions-state.json" | "sessions-config.json" | "run.json")
    ) {
        return Err(
            "Preferences cannot use a workflow state/config or run-manifest filename".into(),
        );
    }
    let parent = path.parent().ok_or("Preferences need a parent directory")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|e| e.to_string())?;
    let meta = fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err("Preferences directory must be private and owned by this user".into());
    }
    if fs::symlink_metadata(path).is_ok() {
        // Never overwrite a foreign/corrupt file simply because --preferences
        // names it. Only this cockpit's validated presentation schema is ours.
        read(path, run)?;
    }
    let value = Preferences {
        version: 1,
        socket: run.original_socket.clone(),
        projects: run.projects.clone(),
        targets: run.tiles.iter().map(|t| t.target.clone()).collect(),
        selected: run.selected.clone(),
        view: run.view,
        control: run.control,
        tile_registrations: run
            .tiles
            .iter()
            .map(|t| crate::recovery::logical_id(run, &t.target))
            .collect(),
    };
    let temporary = parent.join(format!(".cockpit-{}.next", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(temporary);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn roundtrip_and_scope_or_selection_corruption_fail_closed() {
        let root =
            std::env::temp_dir().join(format!("cockpit-prefs-contract-{}", std::process::id()));
        let run = Run {
            ui_socket: "/tmp/private".into(),
            supervisor_pid: 0,
            supervisor_start: "0".into(),
            ui_server_pid: 0,
            ui_server_start: "0".into(),
            original_socket: "/tmp/original".into(),
            executable: "fixture".into(),
            leader: "C-]".into(),
            projects: vec!["/project".into()],
            targets: vec![],
            selected: None,
            terminal_pane: None,
            sidebar_pane: None,
            control: false,
            message: String::new(),
            tiles: vec![],
            view: View::Grid,
            preferences: None,
            attention: vec![],
            tasks: vec![],
            task_warnings: vec![],
            recent: vec![],
            recovery: Default::default(),
        };
        let file = root.join("preferences.json");
        save(&file, &run).unwrap();
        let saved = read(&file, &run).unwrap().unwrap();
        assert_eq!(saved.view, View::Grid);
        assert!(!saved.control);
        let original: serde_json::Value =
            serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        for (field, value) in [
            ("version", serde_json::json!(2)),
            ("socket", serde_json::json!("/other")),
            ("projects", serde_json::json!(["/other"])),
            ("selected", serde_json::json!("missing-generation")),
        ] {
            let mut changed = original.clone();
            changed[field] = value;
            fs::write(&file, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(read(&file, &run).is_err(), "{field} must be rejected");
            let before = fs::read(&file).unwrap();
            assert!(save(&file, &run).is_err());
            assert_eq!(fs::read(&file).unwrap(), before);
        }
        let protected = root.join("sessions-state.json");
        assert!(save(&protected, &run).is_err());
        assert!(!protected.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn reject_shared_files_and_symlinks() {
        let root = std::env::temp_dir().join(format!("cockpit-prefs-unit-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("prefs.json");
        fs::write(&file, "{}").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(private_file(&file).is_ok());
        let link = root.join("link");
        symlink(&file, &link).unwrap();
        assert!(private_file(&link).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(private_file(&file).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
