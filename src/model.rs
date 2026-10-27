use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    pub key: String,
    pub socket: String,
    pub server_pid: u32,
    pub server_start: String,
    pub session_id: String,
    pub session_created: String,
    pub session_name: String,
    pub window_id: String,
    pub pane_id: String,
    pub pane_pid: u32,
    pub pane_start: String,
    pub cwd: String,
    pub dead: bool,
    pub project: Option<String>,
    pub workspace: Option<String>,
    pub task: Option<String>,
    pub claude_session_id: Option<String>,
    pub association: String,
}

impl Target {
    pub fn tmux_target(&self) -> String {
        format!("{}:{}.{}", self.session_id, self.window_id, self.pane_id)
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum View {
    #[default]
    Focus,
    Split,
    Grid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tile {
    pub target: Target,
    pub pane: String,
    pub disconnected: Option<String>,
    pub visible: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub recovery: crate::recovery::State,
    pub ui_socket: String,
    pub supervisor_pid: u32,
    pub supervisor_start: String,
    pub ui_server_pid: u32,
    pub ui_server_start: String,
    pub original_socket: String,
    pub executable: String,
    pub leader: String,
    pub projects: Vec<String>,
    pub targets: Vec<Target>,
    pub selected: Option<String>,
    pub terminal_pane: Option<String>,
    pub sidebar_pane: Option<String>,
    pub control: bool,
    pub message: String,
    pub tiles: Vec<Tile>,
    pub view: View,
    pub preferences: Option<String>,
    #[serde(default)]
    pub attention: Vec<AttentionSnapshot>,
    #[serde(default)]
    pub tasks: Vec<TaskEntry>,
    #[serde(default)]
    pub task_warnings: Vec<String>,
    /// Explicit visits in this run, newest first. Never lifecycle state.
    #[serde(default)]
    pub recent: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ProjectSnapshot {
    pub project_root: String,
    pub complete: bool,
    #[serde(default)]
    pub errors: Vec<serde_json::Value>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub attention: AttentionSnapshot,
    #[serde(default)]
    pub tasks: Vec<TaskEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TaskEntry {
    #[serde(default)]
    pub cli_root: String,
    pub token: String,
    pub project: String,
    pub name: String,
    pub file: String,
    pub branch: String,
    pub status: String,
    pub source_root: String,
    pub workspace: Option<String>,
    pub workspace_name: Option<String>,
    pub association: String,
    pub current: bool,
    pub state_health: String,
    pub protocol: Option<String>,
    #[serde(default)]
    pub lifecycle: Option<serde_json::Value>,
    pub supervision: Option<Supervision>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Supervision {
    pub token: String,
    pub can_approve: bool,
    pub can_retry: bool,
    pub stop: Option<serde_json::Value>,
    pub plan_refused: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AttentionSnapshot {
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub cli_root: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub errors: Vec<serde_json::Value>,
    #[serde(default)]
    pub counts: AttentionCounts,
    #[serde(default)]
    pub items: Vec<AttentionItem>,
    #[serde(default)]
    pub observations: Vec<ExecutionObservation>,
    pub next_before: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AttentionCounts {
    pub unresolved: usize,
    pub unseen: usize,
    pub unacknowledged: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessOwner {
    pub pid: u32,
    pub start: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttentionItem {
    pub id: String,
    pub project: String,
    pub evidence_hash: String,
    pub ts: String,
    pub kind: String,
    pub task: Option<String>,
    pub workspace: Option<String>,
    pub workspace_root: Option<String>,
    pub branch: Option<String>,
    pub session_id: Option<String>,
    pub owner: Option<ProcessOwner>,
    pub reason: Option<String>,
    pub message: Option<String>,
    pub source: Option<String>,
    pub seen_at: Option<String>,
    pub acknowledged_at: Option<String>,
    pub resolved_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionObservation {
    pub project: String,
    pub workspace_root: Option<String>,
    pub session_id: String,
    pub owner: ProcessOwner,
    pub status: String,
    pub source: String,
    pub detail: Option<String>,
    pub ts: String,
    #[serde(default)]
    pub age_seconds: u64,
}

#[derive(Debug, Deserialize)]
pub struct Workspace {
    pub root: String,
    pub current_task: Option<serde_json::Value>,
    #[serde(default)]
    pub task_candidates: Vec<serde_json::Value>,
    pub association: String,
}
