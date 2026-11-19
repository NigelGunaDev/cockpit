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
    /// Activity from status files registered by presentation clients.
    #[serde(default)]
    pub reports: Vec<crate::activity::Report>,
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
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessOwner {
    pub pid: u32,
    pub start: String,
}

#[derive(Debug, Deserialize)]
pub struct Workspace {
    pub root: String,
    pub current_task: Option<serde_json::Value>,
    #[serde(default)]
    pub task_candidates: Vec<serde_json::Value>,
    pub association: String,
}
