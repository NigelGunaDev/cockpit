//! Durable open-session intent. Never task state, process startup or terminal input.
use crate::{agents::Identity, model::Target};
use rusqlite::{Connection, OpenFlags, params};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::os::{
    fd::AsRawFd,
    unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub socket_name: Option<String>,
    pub identity: Identity,
    pub target: Target,
    pub boot_id: String,
    pub repository: Option<Repository>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repository {
    pub root: String,
    pub common_directory: String,
    pub common_device: u64,
    pub common_inode: u64,
    pub branch: Option<String>,
}
pub fn repository(cwd: &str) -> Option<Repository> {
    let mut command = std::process::Command::new("git");
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
    command.args([
        "-C",
        cwd,
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--git-common-dir",
    ]);
    let text = crate::tmux::output(command).ok()?;
    let (root, common) = text.split_once('\n')?;
    let common = Path::new(common).canonicalize().ok()?;
    let meta = fs::metadata(&common).ok()?;
    let mut command = std::process::Command::new("git");
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
    command.args(["-C", cwd, "symbolic-ref", "--short", "HEAD"]);
    Some(Repository {
        root: root.into(),
        common_directory: common.to_string_lossy().into(),
        common_device: meta.dev(),
        common_inode: meta.ino(),
        branch: crate::tmux::output(command).ok(),
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub snapshot: Snapshot,
    pub enabled: bool,
    pub revision: i64,
    pub updated: i64,
}
impl Entry {
    pub fn matches(&self, target: &Target) -> bool {
        self.snapshot.boot_id == boot_id()
            && !self.snapshot.boot_id.is_empty()
            && self.snapshot.target.key == target.key
            && self.snapshot.target.socket == target.socket
            && self.snapshot.target.window_id == target.window_id
    }
}
fn boot_id() -> &'static str {
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .unwrap_or_default()
            .trim()
            .to_owned()
    })
}

fn error(e: impl std::fmt::Display) -> String {
    format!("Session registry: {e}")
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn private(path: &Path, directory: bool) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(error)?;
    if meta.file_type().is_symlink()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || (if directory {
            !meta.is_dir()
        } else {
            !meta.is_file() || meta.nlink() != 1
        })
    {
        return Err(error(
            "storage must be private, same-user and not symlinked or hard-linked",
        ));
    }
    Ok(())
}

pub struct Registry {
    db: Connection,
    _lock: File,
}
impl Registry {
    pub fn open(path: &Path) -> Result<Self, String> {
        if !path.is_absolute()
            || matches!(
                path.file_name().and_then(|n| n.to_str()),
                None | Some("sessions-state.json" | "sessions-config.json" | "run.json")
            )
        {
            return Err(error("choose an absolute dedicated SQLite path"));
        }
        let parent = path.parent().ok_or_else(|| error("no storage directory"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(error)?;
        private(parent, true)?;
        let lockpath = path.with_extension("sqlite3.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&lockpath)
            .map_err(error)?;
        private(&lockpath, false)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(error("busy; retry after the other cockpit update"));
        }
        let fresh = match fs::symlink_metadata(path) {
            Ok(meta) => {
                private(path, false)?;
                if meta.len() > 8 * 1024 * 1024 {
                    return Err(error(
                        "database exceeds the 8 MiB safety bound; left intact",
                    ));
                }
                false
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(path)
                    .map_err(error)?;
                true
            }
            Err(e) => return Err(error(e)),
        };
        for ext in ["-journal", "-wal", "-shm"] {
            let auxiliary = std::path::PathBuf::from(format!("{}{ext}", path.display()));
            if fs::symlink_metadata(&auxiliary).is_ok() {
                private(&auxiliary, false)?;
            }
        }
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(error)?;
        db.busy_timeout(Duration::from_millis(500)).map_err(error)?;
        db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL;")
            .map_err(error)?;
        if fresh {
            db.execute_batch(
                "BEGIN IMMEDIATE;
                CREATE TABLE sessions (
                    id TEXT PRIMARY KEY, kind TEXT NOT NULL, store TEXT NOT NULL,
                    native_id TEXT NOT NULL, data TEXT NOT NULL,
                    enabled INTEGER NOT NULL CHECK(enabled IN (0,1)),
                    revision INTEGER NOT NULL, updated INTEGER NOT NULL,
                    UNIQUE(kind,store,native_id));
                PRAGMA user_version=1; COMMIT;",
            )
            .map_err(error)?;
            File::open(parent)
                .and_then(|f| f.sync_all())
                .map_err(error)?;
        } else {
            let version: i64 = db
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .map_err(error)?;
            if version != 1 {
                return Err(error("unrecognized database; left intact"));
            }
        }
        let value = Self { db, _lock: lock };
        value.list()?; // Validate before any mutation of an existing registry.
        Ok(value)
    }

    pub fn list(&self) -> Result<Vec<Entry>, String> {
        let mut stmt = self.db.prepare("SELECT id,data,enabled,revision,updated,kind,store,native_id FROM sessions ORDER BY updated DESC,id LIMIT 257").map_err(error)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })
            .map_err(error)?;
        let mut out = Vec::new();
        for row in rows {
            let (id, data, enabled, revision, updated, kind, store, native_id) =
                row.map_err(error)?;
            if data.len() > 16 * 1024 {
                return Err(error("record exceeds its size limit"));
            }
            let snapshot: Snapshot = serde_json::from_str(&data).map_err(error)?;
            if snapshot.identity.kind.key() != kind
                || snapshot.identity.state_root != store
                || snapshot.identity.native_session_id != native_id
                || revision < 1
            {
                return Err(error("record identity is inconsistent; left intact"));
            }
            out.push(Entry {
                id,
                snapshot,
                enabled,
                revision,
                updated,
            });
        }
        if out.len() > 256 {
            return Err(error("more than 256 records; registry left intact"));
        }
        Ok(out)
    }

    /// Ordinary reconnects update evidence, but never silently undo Forget.
    pub fn remember(
        &mut self,
        identity: Identity,
        target: &Target,
        explicitly_enable: bool,
    ) -> Result<Entry, String> {
        let before = self.list()?;
        let old = before.iter().find(|e| {
            e.snapshot.identity.kind == identity.kind
                && e.snapshot.identity.state_root == identity.state_root
                && e.snapshot.identity.native_session_id == identity.native_session_id
        });
        if old.is_none() && before.len() >= 256 {
            return Err(error("registry is full (256 sessions)"));
        }
        let snapshot = Snapshot {
            socket_name: socket_name(&target.socket),
            repository: repository(&identity.native_cwd),
            identity: identity.clone(),
            target: target.clone(),
            boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .map_err(error)?
                .trim()
                .into(),
        };
        let data = serde_json::to_string(&snapshot).map_err(error)?;
        if data.len() > 16 * 1024 {
            return Err(error("session metadata exceeds limit"));
        }
        if let Some(old) = old
            && serde_json::to_string(&old.snapshot).map_err(error)? == data
            && (!explicitly_enable || old.enabled)
        {
            return Ok(old.clone());
        }
        let id = old.map(|e| e.id.clone()).unwrap_or(
            fs::read_to_string("/proc/sys/kernel/random/uuid")
                .map_err(error)?
                .trim()
                .into(),
        );
        let tx = self.db.transaction().map_err(error)?;
        // A verified native conversation switch in the same original runtime
        // retires the prior conversation, not its task or its history.
        for previous in &before {
            if previous.matches(target)
                && previous.snapshot.boot_id == snapshot.boot_id
                && previous.id != id
                && previous.enabled
            {
                tx.execute(
                    "UPDATE sessions SET enabled=0,revision=revision+1,updated=? WHERE id=?",
                    params![now(), previous.id],
                )
                .map_err(error)?;
            }
        }
        tx.execute(
            "INSERT INTO sessions(id,kind,store,native_id,data,enabled,revision,updated)
            VALUES(?1,?2,?3,?4,?5,1,1,?6)
            ON CONFLICT(kind,store,native_id) DO UPDATE SET data=excluded.data,
            enabled=CASE WHEN ?7 THEN 1 ELSE sessions.enabled END,
            revision=sessions.revision+1,updated=excluded.updated",
            params![
                id,
                identity.kind.key(),
                identity.state_root,
                identity.native_session_id,
                data,
                now(),
                explicitly_enable
            ],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        self.list()?
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| error("registered record disappeared"))
    }

    /// No live target is required: forgetting an offline session is intentional.
    pub fn forget(&mut self, id: &str, revision: i64) -> Result<(), String> {
        let changed = self.db.execute("UPDATE sessions SET enabled=0,revision=revision+1,updated=? WHERE id=? AND revision=?",
            params![now(), id, revision]).map_err(error)?;
        if changed != 1 {
            return Err(error("selection changed; refresh before forgetting"));
        }
        Ok(())
    }
}

pub fn socket_name(socket: &str) -> Option<String> {
    let base = std::env::var_os("TMUX_TMPDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or("/tmp".into());
    let path = Path::new(socket);
    (path.parent()
        == Some(
            base.join(format!("tmux-{}", unsafe { libc::geteuid() }))
                .as_path(),
        ))
    .then(|| path.file_name().map(|n| n.to_string_lossy().into_owned()))
    .flatten()
}

/// Held across a launch or close; short registry transactions take the other lock.
pub fn operation_lock(path: &Path, id: &str) -> Result<File, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(error("invalid registration identity"));
    }
    let parent = path.parent().ok_or_else(|| error("no storage directory"))?;
    private(parent, true)?;
    let file = parent.join(format!("session-{id}.lock"));
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&file)
        .map_err(error)?;
    private(&file, false)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(error(
            "session recovery/close is in progress; refresh before retrying",
        ));
    }
    Ok(lock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::Kind;
    fn fixture() -> (std::path::PathBuf, Identity, Target) {
        let root = std::env::temp_dir().join(format!(
            "cockpit-registry-{}",
            fs::read_to_string("/proc/sys/kernel/random/uuid")
                .unwrap()
                .trim()
        ));
        let identity = Identity {
            kind: Kind::Codex,
            native_session_id: "same-id".into(),
            state_root: "/store".into(),
            native_file: "/store/session.jsonl".into(),
            native_cwd: "/store".into(),
        };
        let target =
            crate::navigation::target("live-a", Some("/project"), Some("/project/workspace"));
        (root, identity, target)
    }
    #[test]
    fn reopen_forget_stale_selection_and_reconnect_preserve_intent() {
        let (root, identity, target) = fixture();
        let path = root.join("sessions.sqlite3");
        let mut db = Registry::open(&path).unwrap();
        let entry = db.remember(identity.clone(), &target, false).unwrap();
        assert!(entry.enabled);
        let mut previous_boot = entry.clone();
        previous_boot.snapshot.boot_id = "previous-boot".into();
        assert!(!previous_boot.matches(&target));
        db.forget(&entry.id, entry.revision).unwrap();
        assert!(db.forget(&entry.id, entry.revision).is_err());
        assert!(
            !db.remember(identity.clone(), &target, false)
                .unwrap()
                .enabled
        );
        drop(db);
        let mut db = Registry::open(&path).unwrap();
        assert!(!db.list().unwrap()[0].enabled);
        assert!(db.remember(identity, &target, true).unwrap().enabled);
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn agents_stores_concurrent_writers_and_rollback_do_not_lose_intent() {
        let (root, identity, target) = fixture();
        let path = root.join("sessions.sqlite3");
        let mut db = Registry::open(&path).unwrap();
        db.remember(identity.clone(), &target, false).unwrap();
        assert!(Registry::open(&path).is_err());
        let mut other = identity.clone();
        other.kind = Kind::Pi;
        let mut another_target = target.clone();
        another_target.key = "other".into();
        db.remember(other, &another_target, false).unwrap();
        let mut other = identity;
        other.state_root = "/another-store".into();
        another_target.key = "third".into();
        db.remember(other, &another_target, false).unwrap();
        db.db
            .execute_batch("BEGIN; UPDATE sessions SET enabled=0; ROLLBACK;")
            .unwrap();
        assert_eq!(db.list().unwrap().iter().filter(|e| e.enabled).count(), 3);
        drop(db);
        assert_eq!(Registry::open(&path).unwrap().list().unwrap().len(), 3);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn corrupt_foreign_and_symlinked_storage_is_preserved() {
        let (root, _, _) = fixture();
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let path = root.join("sessions.sqlite3");
        fs::write(&path, b"not sqlite").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Registry::open(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not sqlite");
        let link = root.join("alias.sqlite3");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(Registry::open(&link).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Registry::open(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn independent_writers_keep_both_sessions_and_a_conversation_switch_retires_only_its_runtime() {
        let (root, identity, target) = fixture();
        let path = root.join("sessions.sqlite3");
        drop(Registry::open(&path).unwrap());
        let workers: Vec<_> = (0..2)
            .map(|i| {
                let path = path.clone();
                let mut identity = identity.clone();
                let mut target = target.clone();
                identity.native_session_id = format!("conversation-{i}");
                target.key = format!("runtime-{i}");
                std::thread::spawn(move || {
                    for _ in 0..100 {
                        match Registry::open(&path) {
                            Ok(mut db) => return db.remember(identity, &target, false).unwrap(),
                            Err(e) if e.contains("busy") => {
                                std::thread::sleep(Duration::from_millis(5))
                            }
                            Err(e) => panic!("{e}"),
                        }
                    }
                    panic!("registry lock did not become available")
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let mut db = Registry::open(&path).unwrap();
        assert_eq!(db.list().unwrap().iter().filter(|e| e.enabled).count(), 2);
        let mut changed = identity;
        changed.native_session_id = "new-conversation".into();
        let mut old_runtime = target;
        old_runtime.key = "runtime-0".into();
        db.remember(changed, &old_runtime, false).unwrap();
        let all = db.list().unwrap();
        assert_eq!(all.iter().filter(|e| e.enabled).count(), 2);
        assert!(
            !all.iter()
                .find(|e| e.snapshot.identity.native_session_id == "conversation-0")
                .unwrap()
                .enabled
        );
        assert!(
            all.iter()
                .find(|e| e.snapshot.identity.native_session_id == "conversation-1")
                .unwrap()
                .enabled
        );
        drop(db);
        fs::remove_dir_all(root).unwrap();
    }
}
