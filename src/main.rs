mod agents;
mod attention;
mod backend;
mod changes;
mod commands;
mod help;
mod interaction;
mod layout;
mod model;
mod navigation;
mod observe;
mod palette;
mod preferences;
mod presentation;
mod presentation_api;
mod recovery;
mod restore;
mod runtime;
mod session_registry;
mod status;
mod tmux;
mod ui;

use model::Run;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

fn value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn valid_leader(leader: &str) -> bool {
    // A single control key is unambiguous at both tmux layers. Do not accept a
    // tmux command/format as keyboard configuration.
    leader.len() == 3
        && leader.starts_with("C-")
        && (leader.as_bytes()[2].is_ascii_lowercase() || b"[]\\^_".contains(&leader.as_bytes()[2]))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("cc-cockpit: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "presentation") {
        return presentation_api::run(&args[1..]);
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "cc-cockpit [--project PATH] [--socket PATH | --socket-name NAME]\n  [--session exact-name-or-ID] [--leader C-]] [--list] [--run-dir NEW-PATH]\n  [--preferences ABSOLUTE-PATH] [--registry ABSOLUTE-SQLITE-PATH]\n\nPresentation API: cc-cockpit presentation inspect|open|focus|close --run /absolute/run.json [--key KEY]\n\nLaunch in a regular WSL terminal. Defaults below use Ctrl+]."
        );
        for command in commands::COMMANDS {
            println!(
                "  {}: {}",
                commands::shortcut(command, "C-]"),
                command.label
            );
        }
        println!(
            "\nPicker: Enter focus/replace, a add tile, / fuzzy search.\nOpening the cockpit starts no task. Existing tmux sessions remain owned by their original server."
        );
        return Ok(());
    }
    if let Some(role) = args.first().filter(|role| role.starts_with("__")) {
        let path = PathBuf::from(value(&args, "--run").ok_or("Internal role requires --run")?);
        return match role.as_str() {
            "__attach" => {
                runtime::attach(&path, &value(&args, "--key").ok_or("Missing target key")?)
            }
            "__panel" => ui::run(&value(&args, "--kind").ok_or("Missing panel kind")?, &path),
            "__request" => runtime::request(
                &path,
                "select",
                Some(&value(&args, "--select").ok_or("Missing selection")?),
            ),
            "__binding" => runtime::binding_request(
                &path,
                &value(&args, "--op").ok_or("Missing operation")?,
                value(&args, "--select").as_deref(),
            ),
            "__action" => runtime::request(
                &path,
                &value(&args, "--op").ok_or("Missing operation")?,
                value(&args, "--select").as_deref(),
            ),
            _ => Err("Unknown internal cockpit role".into()),
        };
    }
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--list" => index += 1,
            "--project" | "--socket" | "--socket-name" | "--session" | "--leader" | "--run-dir"
            | "--preferences" | "--registry" => {
                if args.get(index + 1).is_none_or(|arg| arg.starts_with("--")) {
                    return Err(format!("Missing value for {}", args[index]));
                }
                index += 2;
            }
            unknown => return Err(format!("Unknown option {unknown}; see --help")),
        }
    }
    let list = args.iter().any(|a| a == "--list");
    if !list {
        if std::env::var_os("TMUX").is_some() {
            return Err(
                "Open a regular WSL terminal tab outside tmux to launch the cockpit".into(),
            );
        }
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            return Err("Interactive launch requires a terminal; use --list for discovery".into());
        }
        let os = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .to_lowercase();
        if !os.contains("microsoft") {
            return Err("Interactive launch supports WSL only".into());
        }
        if !std::env::var("TERM").is_ok_and(|term| !term.is_empty() && term != "dumb") {
            return Err(
                "A supported TERM value is required; launch from a regular WSL terminal tab".into(),
            );
        }
    }
    let leader = value(&args, "--leader").unwrap_or("C-]".into());
    if !valid_leader(&leader) {
        return Err("--leader must be a single tmux control key, such as C-] or C-a".into());
    }
    if value(&args, "--socket").is_some() && value(&args, "--socket-name").is_some() {
        return Err("Choose --socket or --socket-name, not both".into());
    }
    let socket = if let Some(path) = value(&args, "--socket") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("--socket must be an absolute Unix socket path".into());
        }
        path.to_string_lossy().into_owned()
    } else {
        let name = value(&args, "--socket-name").unwrap_or("default".into());
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
        {
            return Err("--socket-name accepts letters, digits, underscores and hyphens".into());
        }
        let root = std::env::var_os("TMUX_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or(PathBuf::from("/tmp"));
        root.join(format!("tmux-{}", unsafe { libc::geteuid() }))
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    let registry_path = value(&args, "--registry").unwrap_or_else(|| {
        let state = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
            });
        state
            .join("cc-cockpit/sessions.sqlite3")
            .to_string_lossy()
            .into_owned()
    });
    if !Path::new(&registry_path).is_absolute() {
        return Err("--registry must be an absolute dedicated SQLite path".into());
    }
    let mut inventory = recovery::State {
        path: registry_path.clone(),
        ..Default::default()
    };
    if !list {
        recovery::read_inventory(&mut inventory);
    }
    let explicit_projects = args.iter().any(|a| a == "--project");
    let mut projects = Vec::new();
    for pair in args.windows(2).filter(|pair| pair[0] == "--project") {
        let root = observe::discover_project(Path::new(&pair[1]))?;
        if !projects.contains(&root) {
            projects.push(root);
        }
    }
    if !explicit_projects {
        let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
        if let Ok(root) = observe::discover_project(&cwd).or_else(|_| {
            std::env::var("COCKPIT_PROJECT_DIR")
                .map_err(|e| e.to_string())
                .and_then(|p| observe::discover_project(Path::new(&p)))
        }) {
            projects.push(root);
        }
        for e in inventory
            .entries
            .iter()
            .filter(|e| e.enabled && restore::socket_matches(e, &socket))
        {
            if let Some(repo) = &e.snapshot.repository {
                let root = e.snapshot.target.project.clone().unwrap_or_else(|| {
                    Path::new(&repo.common_directory)
                        .parent()
                        .unwrap_or(Path::new(""))
                        .to_string_lossy()
                        .into()
                });
                if !projects.contains(&root)
                    && restore::project_for(e, std::slice::from_ref(&root)).is_some()
                    && observe::discover_project(Path::new(&root)).is_ok()
                    && !projects.contains(&root)
                {
                    projects.push(root);
                }
            }
        }
    }
    if projects.is_empty() {
        return Err("No project directory found. Pass --project PATH; remembered projects must still exist.".into());
    }
    let mut targets = match observe::targets(&socket) {
        Ok(targets) => targets,
        Err(error)
            if !list
                && inventory
                    .entries
                    .iter()
                    .any(|e| e.enabled && restore::socket_matches(e, &socket))
                && recovery::missing_server(&socket, &error) =>
        {
            vec![]
        }
        Err(error) => return Err(error),
    };
    let mut errors = Vec::new();
    let mut snapshots = Vec::new();
    for project in &projects {
        match observe::project_snapshot(project) {
            Ok(snapshot) => {
                if !snapshot.complete {
                    errors.push(format!(
                        "{}: snapshot incomplete ({:?})",
                        project, snapshot.errors
                    ));
                }
                snapshots.retain(|old: &model::ProjectSnapshot| {
                    old.project_root != snapshot.project_root
                });
                snapshots.push(snapshot);
            }
            Err(error) => errors.push(format!("{project}: {error}")),
        }
    }
    observe::associate(&mut targets, &snapshots);
    if list {
        println!(
            "{}",
            serde_json::json!({"targets":targets,"projects":projects,"errors":errors})
        );
        return Ok(());
    }
    let initial = if let Some(selection) = value(&args, "--session") {
        let choices = targets
            .iter()
            .filter(|target| target.session_name == selection || target.session_id == selection)
            .collect::<Vec<_>>();
        match choices.as_slice() {
            [selected] => Some(selected.key.clone()),
            [] => return Err(format!("No current session named {selection}")),
            _ => {
                return Err(
                    "Session contains multiple panes; select its exact terminal in the picker"
                        .into(),
                );
            }
        }
    } else {
        None
    };
    projects.sort();
    let preference_path = value(&args, "--preferences").unwrap_or_else(|| {
        let root = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
            });
        // Stable filename per socket + project set. FNV-1a is an identifier,
        // not authentication; the stored socket and project set are checked too.
        let identity = format!("{}\0{}", socket, projects.join("\0"));
        let hash = identity.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
        });
        root.join("cc-cockpit")
            .join(format!("{hash:016x}.json"))
            .to_string_lossy()
            .into_owned()
    });
    if !Path::new(&preference_path).is_absolute() {
        return Err("--preferences and XDG_STATE_HOME must resolve to an absolute path".into());
    }
    let run = Run {
        recovery: inventory,
        ui_socket: String::new(),
        supervisor_pid: std::process::id(),
        supervisor_start: tmux::process_start(std::process::id())?,
        ui_server_pid: 0,
        ui_server_start: String::new(),
        original_socket: socket,
        executable: std::env::current_exe()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned(),
        leader,
        projects,
        targets,
        selected: None,
        terminal_pane: None,
        sidebar_pane: None,
        control: false,
        tiles: vec![],
        view: model::View::Focus,
        preferences: Some(preference_path),
        attention: snapshots
            .iter()
            .map(|snapshot| snapshot.attention.clone())
            .collect(),
        task_warnings: vec![],
        tasks: snapshots
            .iter()
            .flat_map(|snapshot| snapshot.tasks.clone())
            .collect(),
        message: if errors.is_empty() {
            "Select a session · activity unknown".into()
        } else {
            format!("Observation incomplete: {}", errors.join("; "))
        },
        recent: vec![],
    };
    runtime::launch(
        run,
        value(&args, "--run-dir").map(PathBuf::from),
        initial,
        snapshots,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leaders_are_keys_not_commands() {
        assert!(valid_leader("C-]"));
        assert!(valid_leader("C-a"));
        assert!(!valid_leader("C-a;kill-server"));
        assert!(!valid_leader("#{prefix}"));
    }
}
