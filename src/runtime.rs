use crate::model::{Run, Target, Tile, View};
use crate::{commands, layout, navigation, preferences};
use crate::{
    observe,
    tmux::{Tmux, process_start, quote},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 8];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn read_run(path: &Path) -> Result<Run, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let parent = fs::symlink_metadata(path.parent().ok_or("No run directory")?)
        .map_err(|e| e.to_string())?;
    let uid = unsafe { libc::geteuid() };
    if !meta.file_type().is_file()
        || meta.uid() != uid
        || meta.mode() & 0o077 != 0
        || !parent.is_dir()
        || parent.file_type().is_symlink()
        || parent.uid() != uid
        || parent.mode() & 0o077 != 0
    {
        return Err("Cockpit manifest and directory must be private same-user resources".into());
    }
    if meta.len() > 2 * 1024 * 1024 {
        return Err("Run manifest is too large".into());
    }
    let run: Run = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if process_start(run.supervisor_pid)? != run.supervisor_start
        || run.ui_socket == run.original_socket
    {
        return Err("Stale or invalid cockpit run".into());
    }
    Ok(run)
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let temporary = path.with_extension("next");
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    fs::rename(temporary, path).map_err(|e| e.to_string())
}

#[derive(Serialize, Deserialize)]
struct Request {
    id: String,
    op: String,
    selected: Option<String>,
}

pub fn request(path: &Path, op: &str, selected: Option<&str>) -> Result<(), String> {
    read_run(path)?;
    let directory = path.parent().unwrap();
    let id = format!("{}-{}", std::process::id(), nonce()?);
    let file = directory.join(format!("request-{id}.json"));
    let payload = Request {
        id: id.clone(),
        op: op.into(),
        selected: selected.map(str::to_owned),
    };
    if serde_json::to_vec(&payload)
        .map_err(|e| e.to_string())?
        .len()
        > 4096
    {
        return Err("Cockpit request exceeds 4096 bytes; shorten the task request".into());
    }
    write_json(&file, &payload)?;
    let response = directory.join(format!("response-{id}.json"));
    let deadline =
        Instant::now() + Duration::from_secs(if op.starts_with("task-") { 90 } else { 15 });
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(&response) {
            let _ = fs::remove_file(&response);
            return serde_json::from_str::<Result<(), String>>(&text).map_err(|e| e.to_string())?;
        }
        read_run(path)?;
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("Cockpit action timed out; inspect the current selection before retrying".into())
}

/// Native bindings report refusals in tmux's message line, not its output pager.
/// Programmatic requests retain their ordinary Result and nonzero exit status.
pub fn binding_request(path: &Path, op: &str, selected: Option<&str>) -> Result<(), String> {
    if let Err(error) = request(path, op, selected) {
        let run = read_run(path)?;
        let ui = Tmux::new(&run.ui_socket);
        if ui.pid()? != run.ui_server_pid
            || process_start(run.ui_server_pid)? != run.ui_server_start
        {
            return Err("Cockpit UI is no longer available".into());
        }
        let message = observe::sanitize(&error).replace(['\n', '\t'], " ");
        ui.run(&["display-message", "-l", "-d", "5000", &message])?;
    }
    Ok(())
}

struct Resources {
    directory: PathBuf,
    socket: String,
    identity: Option<(u32, String)>,
}
impl Drop for Resources {
    fn drop(&mut self) {
        if let Some((pid, start)) = &self.identity {
            let tmux = Tmux::new(&self.socket);
            if tmux.pid().ok() == Some(*pid) && process_start(*pid).as_ref().ok() == Some(start) {
                let _ = tmux.run(&["kill-server"]);
            }
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

pub fn attach(path: &Path, key: &str) -> Result<(), String> {
    let run = read_run(path)?;
    let target = run
        .targets
        .iter()
        .find(|t| t.key == key)
        .ok_or("Target disappeared; select a current session")?;
    observe::validate(target, false)?;
    let mut command = Tmux::new(&target.socket).command();
    command.args([
        "attach-session",
        "-E",
        "-f",
        "read-only,active-pane,ignore-size",
        "-t",
        &target.tmux_target(),
    ]);
    Err(format!("Cannot attach tmux: {}", command.exec()))
}

#[derive(Clone, Debug)]
struct Client {
    tty: String,
    session: String,
    readonly: bool,
    window: String,
    pane: String,
}
impl Client {
    fn matches(&self, target: &Target) -> bool {
        self.session == target.session_id
            && self.window == target.window_id
            && self.pane == target.pane_id
    }
}
fn clients(run: &Run) -> Result<HashMap<String, Client>, String> {
    let text = Tmux::new(&run.original_socket).run(&[
        "list-clients",
        "-F",
        "#{client_name}\t#{session_id}\t#{client_readonly}\t#{window_id}\t#{pane_id}",
    ])?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            (fields.len() == 5).then(|| {
                (
                    fields[0].into(),
                    Client {
                        tty: fields[0].into(),
                        session: fields[1].into(),
                        readonly: fields[2] == "1",
                        window: fields[3].into(),
                        pane: fields[4].into(),
                    },
                )
            })
        })
        .collect())
}
fn client(run: &Run, pane: &str) -> Result<Client, String> {
    let tty =
        Tmux::new(&run.ui_socket).run(&["display-message", "-p", "-t", pane, "#{pane_tty}"])?;
    clients(run)?
        .remove(&tty)
        .ok_or_else(|| "Attachment is starting or disconnected".into())
}

fn mode(run: &Run, pane: &str, target: &Target, control: bool, sizing: bool) -> Result<(), String> {
    observe::validate(target, control)?;
    if control {
        for option in ["prefix", "prefix2"] {
            if Tmux::new(&target.socket).run(&[
                "show-options",
                "-Av",
                "-t",
                &target.session_id,
                option,
            ])? == run.leader
            {
                return Err("View only: cockpit leader conflicts with original tmux prefix; choose another --leader".into());
            }
        }
    }
    let attached = client(run, pane)?;
    if !attached.matches(target) {
        return Err("Attachment changed destination; select a current session".into());
    }
    let tmux = Tmux::new(&target.socket);
    if attached.readonly == control {
        tmux.run(&[
            "switch-client",
            "-c",
            &attached.tty,
            "-t",
            &target.tmux_target(),
            "-r",
            "-E",
        ])?;
    }
    tmux.run(&[
        "refresh-client",
        "-t",
        &attached.tty,
        "-f",
        if sizing {
            "!ignore-size"
        } else {
            "ignore-size"
        },
    ])?;
    let attached = client(run, pane)?;
    if !attached.matches(target) || attached.readonly == control {
        return Err("Input mode could not be confirmed".into());
    }
    Ok(())
}

fn role_command(run: &Run, path: &Path, role: &str, argument: &str) -> String {
    format!(
        "{} {} --run {} {}",
        quote(&run.executable),
        role,
        quote(&path.to_string_lossy()),
        argument
    )
}

fn dimensions(run: &Run) -> Result<(u32, u32), String> {
    let ui = Tmux::new(&run.ui_socket);
    let size = ui.run(&[
        "display-message",
        "-p",
        "-t",
        "cockpit",
        "#{window_width} #{window_height}",
    ])?;
    let fields: Vec<_> = size.split_whitespace().collect();
    if fields.len() != 2 {
        return Err("Cannot observe private terminal size".into());
    }
    Ok((
        fields[0].parse().map_err(|_| "Invalid width")?,
        fields[1].parse().map_err(|_| "Invalid height")?,
    ))
}

fn active_index(run: &Run) -> Result<usize, String> {
    run.tiles
        .iter()
        .position(|tile| Some(&tile.target.key) == run.selected.as_ref())
        .ok_or_else(|| "No selected terminal".into())
}

// Revoke every attachment before any rearrangement or focus handoff. Invalid
// private clients are destroyed; a labeled placeholder retains the selection.
fn quiesce(run: &mut Run) {
    run.control = false;
    for index in 0..run.tiles.len() {
        if run.tiles[index].disconnected.is_some() {
            continue;
        }
        let tile = &run.tiles[index];
        if let Err(error) = mode(run, &tile.pane, &tile.target, false, false) {
            invalidate(run, index, &error);
        }
    }
}

fn invalidate(run: &mut Run, index: usize, error: &str) {
    let tile = &run.tiles[index];
    // Kill only the disposable private attachment process. No shell or alternate
    // original task is substituted when the target dies.
    let command = format!(
        "{} __panel --run {} --kind disconnected",
        quote(&run.executable),
        quote(
            &Path::new(&run.ui_socket)
                .parent()
                .unwrap()
                .join("run.json")
                .to_string_lossy()
        )
    );
    let _ = Tmux::new(&run.ui_socket).run(&["respawn-pane", "-k", "-t", &tile.pane, &command]);
    run.tiles[index].disconnected = Some(error.into());
    if Some(&run.tiles[index].target.key) == run.selected.as_ref() {
        run.control = false;
        run.message = format!("Disconnected: {error}. Select a current session with leader + s.");
    }
}

fn sidebar(run: &mut Run, path: &Path, anchor: &str) -> Result<(), String> {
    let pane = Tmux::new(&run.ui_socket).run(&[
        "split-window",
        "-h",
        "-b",
        "-l",
        "32",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        anchor,
        &run.executable,
        "__panel",
        "--run",
        &path.to_string_lossy(),
        "--kind",
        "sidebar",
    ])?;
    run.sidebar_pane = Some(pane);
    Ok(())
}

fn reflow(run: &mut Run, path: &Path, allow_control: bool) -> Result<(), String> {
    let active = active_index(run)?;
    let ui = Tmux::new(&run.ui_socket);
    let (width, height) = dimensions(run)?;
    let mut side = run.sidebar_pane.is_some();
    let mut view = run.view;
    if let Err(error) = layout::check_size(view, run.tiles.len(), width, height, side) {
        view = View::Focus;
        if layout::check_size(view, 1, width, height, side).is_err() {
            side = false;
        }
        run.message = format!("{error} Showing focused terminal.");
    }
    let visible = layout::visible(view, run.tiles.len(), active);
    // Focus uses native zoom when no sidebar is shown. Preserve all clients and
    // panes; only hidden attachments stop contributing original terminal size.
    let composed: Vec<_> = if view == View::Focus && !side {
        (0..run.tiles.len()).collect()
    } else {
        visible.clone()
    };
    quiesce(run);
    if let Some(pane) = run.sidebar_pane.take() {
        let _ = ui.run(&["kill-pane", "-t", &pane]);
    }
    let anchor = run.tiles[composed[0]].pane.clone();
    let active_pane = run.tiles[active].pane.clone();
    // Unzoom, then park other tiles in their own private windows before joining
    // the requested set. All identifiers below belong to this private server.
    let zoomed = ui.run(&[
        "display-message",
        "-p",
        "-t",
        &anchor,
        "#{window_zoomed_flag}",
    ])?;
    if zoomed == "1" {
        ui.run(&["resize-pane", "-Z", "-t", &anchor])?;
    }
    for tile in &run.tiles {
        if tile.pane == anchor {
            continue;
        }
        let count = ui.run(&["display-message", "-p", "-t", &tile.pane, "#{window_panes}"])?;
        if count != "1" {
            ui.run(&["break-pane", "-d", "-s", &tile.pane, "-n", "parked"])?;
        }
    }
    for index in &composed {
        let pane = &run.tiles[*index].pane;
        if pane != &anchor {
            ui.run(&["join-pane", "-d", "-h", "-s", pane, "-t", &anchor])?;
        }
    }
    let window = ui.run(&["display-message", "-p", "-t", &anchor, "#{window_id}"])?;
    ui.run(&["select-window", "-t", &window])?;
    ui.run(&[
        "select-layout",
        "-t",
        &window,
        if view == View::Split {
            "even-horizontal"
        } else {
            "tiled"
        },
    ])?;
    if side {
        sidebar(run, path, &anchor)?;
        let panes: Vec<_> = composed
            .iter()
            .map(|index| run.tiles[*index].pane.clone())
            .collect();
        let native =
            layout::with_sidebar(width, height, run.sidebar_pane.as_deref().unwrap(), &panes)?;
        ui.run(&["select-layout", "-t", &window, &native])?;
    }
    if view == View::Focus && !side && run.tiles.len() > 1 {
        ui.run(&["resize-pane", "-Z", "-t", &active_pane])?;
    }
    ui.run(&["select-pane", "-Z", "-t", &active_pane])?;
    run.terminal_pane = Some(active_pane);
    for index in 0..run.tiles.len() {
        run.tiles[index].visible = visible.contains(&index);
        let tile = &run.tiles[index];
        let title = format!(
            "{} · {}{}",
            index + 1,
            observe::sanitize(&tile.target.session_name),
            if tile.disconnected.is_some() {
                " · disconnected"
            } else if index == active {
                " · focused"
            } else {
                " · view"
            }
        );
        let _ = ui.run(&["select-pane", "-t", &tile.pane, "-T", &title]);
        if tile.disconnected.is_none()
            && let Err(error) = mode(run, &tile.pane, &tile.target, false, tile.visible)
        {
            invalidate(run, index, &error);
        }
    }
    let tile = &run.tiles[active];
    if allow_control && tile.disconnected.is_none() {
        match mode(run, &tile.pane, &tile.target, true, true) {
            Ok(()) => {
                run.control = true;
            }
            Err(error) => run.message = error,
        }
    }
    Ok(())
}

fn focus_tile(run: &mut Run, path: &Path, index: usize) -> Result<(), String> {
    let previous = active_index(run)?;
    let (width, height) = dimensions(run)?;
    let effective = if layout::check_size(
        run.view,
        run.tiles.len(),
        width,
        height,
        run.sidebar_pane.is_some(),
    )
    .is_ok()
    {
        run.view
    } else {
        View::Focus
    };
    let visible = layout::visible(effective, run.tiles.len(), index);
    let old_visible: Vec<_> = run
        .tiles
        .iter()
        .enumerate()
        .filter_map(|(i, tile)| tile.visible.then_some(i))
        .collect();
    // Switching an existing tile in a grid, or a native zoomed focus view,
    // needs no pane destruction, joins, or global layout reconstruction.
    if old_visible != visible && (effective != View::Focus || run.sidebar_pane.is_some()) {
        quiesce(run);
        run.selected = Some(run.tiles[index].target.key.clone());
        return reflow(run, path, true);
    }
    let old = &run.tiles[previous];
    if old.disconnected.is_none()
        && let Err(error) = mode(
            run,
            &old.pane,
            &old.target,
            false,
            visible.contains(&previous),
        )
    {
        invalidate(run, previous, &error);
    }
    run.control = false;
    let ui = Tmux::new(&run.ui_socket);
    let pane = run.tiles[index].pane.clone();
    ui.run(&["select-window", "-t", &pane])?;
    ui.run(&["select-pane", "-Z", "-t", &pane])?;
    run.selected = Some(run.tiles[index].target.key.clone());
    run.terminal_pane = Some(pane.clone());
    for i in 0..run.tiles.len() {
        run.tiles[i].visible = visible.contains(&i);
    }
    let tile = &run.tiles[index];
    if tile.disconnected.is_none() {
        match mode(run, &pane, &tile.target, true, true) {
            Ok(()) => run.control = true,
            Err(error) => run.message = error,
        }
    }
    for i in [previous, index] {
        let tile = &run.tiles[i];
        let title = format!(
            "{} · {} · {}",
            i + 1,
            observe::sanitize(&tile.target.session_name),
            if tile.disconnected.is_some() {
                "disconnected"
            } else if i == index {
                "focused"
            } else {
                "view"
            }
        );
        ui.run(&["select-pane", "-t", &tile.pane, "-T", &title])?;
    }
    Ok(())
}

fn create_tile(
    run: &Run,
    path: &Path,
    target: Target,
    disconnected: Option<String>,
) -> Result<Tile, String> {
    let ui = Tmux::new(&run.ui_socket);
    let command = if disconnected.is_some() {
        role_command(run, path, "__panel", "--kind disconnected")
    } else {
        role_command(
            run,
            path,
            "__attach",
            &format!("--key {}", quote(&target.key)),
        )
    };
    let pane = ui.run(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "cockpit",
        "-n",
        "terminal",
        &command,
    ])?;
    if disconnected.is_none() {
        let deadline = Instant::now() + Duration::from_secs(3);
        while client(run, &pane).is_err() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Err(error) = client(run, &pane) {
            let _ = ui.run(&["kill-pane", "-t", &pane]);
            return Err(error);
        }
    }
    Ok(Tile {
        target,
        pane,
        disconnected,
        visible: false,
    })
}

fn select(
    run: &mut Run,
    path: &Path,
    key: &str,
    add: bool,
    allow_control: bool,
) -> Result<(), String> {
    select_terminal(run, path, key, add, allow_control, true)
}

fn select_terminal(
    run: &mut Run,
    path: &Path,
    key: &str,
    add: bool,
    allow_control: bool,
    close_popup: bool,
) -> Result<(), String> {
    let chosen = run
        .targets
        .iter()
        .find(|t| t.key == key)
        .cloned()
        .ok_or("Selection is stale; refresh sessions")?;
    observe::validate(&chosen, false)?;
    let original = Tmux::new(&chosen.socket);
    for option in ["prefix", "prefix2"] {
        if original.run(&["show-options", "-Av", "-t", &chosen.session_id, option])? == run.leader {
            return Err(
                "Cockpit leader conflicts with original tmux prefix; launch with another --leader"
                    .into(),
            );
        }
    }
    let existing = run.tiles.iter().position(|t| t.target.key == key);
    if existing.is_none() && add && run.tiles.len() >= layout::MAX_TILES {
        return Err(
            "Four sessions are already pinned. Enter replaces the focused tile; x removes it."
                .into(),
        );
    }
    if let Some(index) = existing {
        if close_popup {
            let _ = Tmux::new(&run.ui_socket).run(&["display-popup", "-C"]);
        }
        run.message = format!("{} · activity unknown", chosen.session_name);
        if allow_control {
            return focus_tile(run, path, index);
        }
        quiesce(run);
        run.selected = Some(key.into());
        return reflow(run, path, false);
    }
    quiesce(run);
    if existing.is_none() {
        let tile = create_tile(run, path, chosen.clone(), None)?;
        if !add && !run.tiles.is_empty() {
            let index = active_index(run)?;
            let old = std::mem::replace(&mut run.tiles[index], tile);
            let _ = Tmux::new(&run.ui_socket).run(&["kill-pane", "-t", &old.pane]);
        } else {
            run.tiles.push(tile);
        }
    }
    if close_popup {
        let _ = Tmux::new(&run.ui_socket).run(&["display-popup", "-C"]);
    }
    run.selected = Some(key.into());
    run.message = format!("{} · activity unknown", chosen.session_name);
    reflow(run, path, allow_control)
}

fn apply_recovered(run: &mut Run, path: &Path, id: &str, target: Target) -> Result<(), String> {
    observe::validate(&target, false)?;
    if run.recovery.user_navigated {
        return Ok(());
    }
    let slot = run
        .recovery
        .slots
        .iter()
        .position(|saved| saved.as_deref() == Some(id));
    if let Some(index) = slot.filter(|i| {
        *i < run.tiles.len()
            && run
                .recovery
                .slot_targets
                .get(*i)
                .is_some_and(|key| key == &run.tiles[*i].target.key)
    }) {
        if run.tiles[index].target.key == target.key {
            return Ok(());
        }
        if run.tiles[index].disconnected.is_none() {
            return Ok(());
        } // Never replace a user-selected live tile.
        let selected = run.selected.as_deref() == Some(run.tiles[index].target.key.as_str());
        let control = run.control && !selected;
        write_json(path, run)?;
        let replacement = create_tile(run, path, target.clone(), None)?;
        quiesce(run);
        let old = std::mem::replace(&mut run.tiles[index], replacement);
        if selected {
            run.selected = Some(target.key.clone());
        }
        let _ = Tmux::new(&run.ui_socket).run(&["kill-pane", "-t", &old.pane]);
        reflow(run, path, control)?;
    } else if run.tiles.is_empty() {
        write_json(path, run)?;
        select_terminal(run, path, &target.key, false, false, false)?;
    }
    if let Some(file) = run.preferences.clone() {
        preferences::save(Path::new(&file), run)?;
    }
    Ok(())
}

fn update_attention(run: &mut Run, _snapshots: &[crate::model::ProjectSnapshot]) {
    run.tasks.clear();
    run.task_warnings.clear();
    run.attention.clear();
}

fn action_inner(run: &mut Run, path: &Path, request: &Request) -> Result<(), String> {
    if [
        "select",
        "add",
        "remove",
        "last-session",
        "next",
        "previous",
    ]
    .contains(&request.op.as_str())
    {
        run.recovery.user_navigated = true;
    }
    match request.op.as_str() {
        "registry-refresh" => {
            crate::recovery::reconcile(run);
            run.recovery.error.clone().map_or(Ok(()), Err)
        }
        "registry-remember" => crate::recovery::connected(
            run,
            request.selected.as_deref().ok_or("Missing terminal")?,
            true,
        ),
        "registry-link" => crate::recovery::link(
            run,
            serde_json::from_str(
                request
                    .selected
                    .as_deref()
                    .ok_or("Missing native session link")?,
            )
            .map_err(|_| "Invalid native session link")?,
        ),
        "registry-forget" => crate::recovery::forget(
            run,
            serde_json::from_str(
                request
                    .selected
                    .as_deref()
                    .ok_or("Missing registry selection")?,
            )
            .map_err(|_| "Invalid registry selection")?,
        ),
        "command" => {
            let selection: commands::Selection =
                serde_json::from_str(request.selected.as_deref().ok_or("Missing command")?)
                    .map_err(|_| "Invalid command selection")?;
            let command = commands::validate_selection(&selection, run)?;
            if command.needs_live_target() {
                let target = run
                    .targets
                    .iter()
                    .find(|target| Some(&target.key) == run.selected.as_ref())
                    .ok_or("Selected terminal disappeared")?;
                observe::validate(target, false)?;
            }
            match command.dispatch {
                // The existing popup hands its restored terminal to the panel.
                commands::Dispatch::Panel(_) => Ok(()),
                commands::Dispatch::Operation(op) => action_inner(
                    run,
                    path,
                    &Request {
                        id: request.id.clone(),
                        op: op.into(),
                        selected: None,
                    },
                ),
                commands::Dispatch::Detach => action_inner(
                    run,
                    path,
                    &Request {
                        id: request.id.clone(),
                        op: "detach".into(),
                        selected: None,
                    },
                ),
                commands::Dispatch::LiteralLeader => {
                    Err("Use the native double-leader binding".into())
                }
            }
        }
        "last-session" => {
            let key = navigation::previous_key(run)?;
            select(run, path, &key, false, true)
        }
        "select" | "add" => select(
            run,
            path,
            request.selected.as_deref().ok_or("Missing selection")?,
            request.op == "add",
            true,
        ),
        "focus-pane" => {
            let pane = request.selected.as_deref().ok_or("Missing private pane")?;
            if let Some(index) = run.tiles.iter().position(|tile| tile.pane == pane) {
                run.message = "Focused terminal · activity unknown".into();
                focus_tile(run, path, index)
            } else if run.sidebar_pane.as_deref() == Some(pane) {
                quiesce(run);
                Tmux::new(&run.ui_socket).run(&["select-pane", "-t", pane])?;
                Ok(())
            } else {
                Err("Unknown private tile; input remains disabled".into())
            }
        }
        "focus-left" | "focus-right" | "focus-up" | "focus-down" => {
            let active = active_index(run)?;
            let pane = &run.tiles[active].pane;
            let text = Tmux::new(&run.ui_socket).run(&[
                "list-panes",
                "-t",
                pane,
                "-F",
                "#{pane_id}\t#{pane_left}\t#{pane_top}\t#{pane_width}\t#{pane_height}",
            ])?;
            let panes: Vec<_> = text
                .lines()
                .filter_map(|line| {
                    let fields: Vec<_> = line.split('\t').collect();
                    if fields.len() != 5 {
                        return None;
                    }
                    let index = run
                        .tiles
                        .iter()
                        .position(|tile| tile.pane == fields[0] && tile.visible)?;
                    Some((
                        index,
                        ratatui::layout::Rect::new(
                            fields[1].parse().ok()?,
                            fields[2].parse().ok()?,
                            fields[3].parse().ok()?,
                            fields[4].parse().ok()?,
                        ),
                    ))
                })
                .collect();
            if let Some(index) =
                layout::neighbor(&panes, active, request.op.trim_start_matches("focus-"))
            {
                run.message = "Focused terminal · activity unknown".into();
                focus_tile(run, path, index)
            } else {
                run.message =
                    "No visible terminal in that direction; choose split/grid to show neighbors"
                        .into();
                Ok(())
            }
        }
        "next" | "previous" => {
            let active = active_index(run)?;
            let index = if request.op == "next" {
                (active + 1) % run.tiles.len()
            } else {
                (active + run.tiles.len() - 1) % run.tiles.len()
            };
            run.message = "Focused terminal · activity unknown".into();
            focus_tile(run, path, index)
        }
        "focus" => reflow(run, path, true),
        "focus-layout" | "split" | "grid" => {
            let view = match request.op.as_str() {
                "split" => View::Split,
                "grid" => View::Grid,
                _ => View::Focus,
            };
            let (width, height) = dimensions(run)?;
            layout::check_size(
                view,
                run.tiles.len(),
                width,
                height,
                run.sidebar_pane.is_some(),
            )?;
            let control = run.control;
            run.view = view;
            run.message = format!("{:?} · activity unknown", view);
            reflow(run, path, control)
        }
        "remove" => {
            let index = active_index(run)?;
            quiesce(run);
            let tile = run.tiles.remove(index);
            let ui = Tmux::new(&run.ui_socket);
            let _ = ui.run(&["kill-pane", "-t", &tile.pane]);
            if run.tiles.is_empty() {
                if let Some(pane) = run.sidebar_pane.take() {
                    let _ = ui.run(&["kill-pane", "-t", &pane]);
                }
                run.selected = None;
                run.terminal_pane = None;
                ui.run(&["select-window", "-t", "cockpit:0"])?;
                run.message = "Select a session · activity unknown".into();
                Ok(())
            } else {
                run.selected = Some(run.tiles[index.min(run.tiles.len() - 1)].target.key.clone());
                reflow(run, path, true)
            }
        }
        "toggle-control" => {
            let index = active_index(run)?;
            let tile = &run.tiles[index];
            if tile.disconnected.is_some() {
                return Err("Selected tile is disconnected".into());
            }
            mode(run, &tile.pane, &tile.target, !run.control, true)?;
            run.control = !run.control;
            run.message = if run.control { "CONTROL" } else { "VIEW" }.into();
            Ok(())
        }
        "sidebar" => {
            let ui = Tmux::new(&run.ui_socket);
            let opening = run.sidebar_pane.is_none();
            if let Some(pane) = run.sidebar_pane.take() {
                ui.run(&["kill-pane", "-t", &pane])?;
            } else {
                let (width, height) = dimensions(run)?;
                layout::check_size(run.view, run.tiles.len(), width, height, true)?;
                let anchor = run
                    .terminal_pane
                    .as_deref()
                    .ok_or("Select a terminal first")?
                    .to_owned();
                let zoom = ui.run(&[
                    "display-message",
                    "-p",
                    "-t",
                    &anchor,
                    "#{window_zoomed_flag}",
                ])?;
                if zoom == "1" {
                    ui.run(&["resize-pane", "-Z", "-t", &anchor])?;
                }
                sidebar(run, path, &anchor)?;
            }
            reflow(run, path, true)?;
            if opening {
                quiesce(run);
                ui.run(&[
                    "select-pane",
                    "-t",
                    run.sidebar_pane.as_deref().ok_or("No sidebar")?,
                ])?;
            }
            Ok(())
        }
        "detach" => {
            Tmux::new(&run.ui_socket).run(&["detach-client", "-s", "cockpit"])?;
            Ok(())
        }
        _ => Err("Unsupported cockpit operation".into()),
    }
}

fn action(run: &mut Run, path: &Path, request: &Request) -> Result<(), String> {
    let before = run.selected.clone();
    let result = action_inner(run, path, request);
    let op = if request.op == "command" {
        request
            .selected
            .as_deref()
            .and_then(|text| serde_json::from_str::<commands::Selection>(text).ok())
            .and_then(|selection| commands::get(&selection.id).ok())
            .and_then(|command| match command.dispatch {
                commands::Dispatch::Operation(op) => Some(op),
                _ => None,
            })
            .unwrap_or("")
    } else {
        request.op.as_str()
    };
    if result.is_ok()
        && run.selected != before
        && matches!(
            op,
            "select"
                | "add"
                | "focus-pane"
                | "focus-left"
                | "focus-right"
                | "focus-up"
                | "focus-down"
                | "next"
                | "previous"
                | "last-session"
                | "attention-open"
                | "task-view"
        )
    {
        navigation::record_visit(run);
    }
    if result.is_err() {
        // Any partially completed operation leaves every attachment read-only.
        quiesce(run);
    }
    if result.is_ok()
        && matches!(op, "select" | "add" | "task-view" | "last-session")
        && let Some(key) = run.selected.clone()
        && let Err(error) = crate::recovery::connected(run, &key, false)
    {
        // Persistence failure must not make an otherwise valid terminal unusable.
        run.recovery.error = Some(error);
    }
    if let Some(file) = &run.preferences
        && let Err(error) = preferences::save(Path::new(file), run)
    {
        run.message = format!("Preferences not saved: {error}");
    }
    result
}

fn navigation_binding(run: &Run, path: &Path) -> Result<String, String> {
    let mut binding = String::new();
    for (id, _) in crate::status::NAVIGATION.iter().rev() {
        let command = commands::get(id)?;
        let commands::Dispatch::Panel(kind) = command.dispatch else {
            return Err("Navigation destination must be a panel".into());
        };
        let child = role_command(run, path, "__panel", &format!("--kind {}", quote(kind)));
        let open = format!("display-popup -E -w 85% -h 85% {}", quote(&child));
        let condition = quote(&format!("#{{==:#{{mouse_status_range}},cc-{id}}}"));
        binding = if binding.is_empty() {
            format!("if-shell -F {condition} {{ {open} }}")
        } else {
            format!("if-shell -F {condition} {{ {open} }} {{ {binding} }}")
        };
    }
    // Native command groups avoid exponential quote growth while preserving
    // exactly one matching branch, even if the window changes while a popup is open.
    Ok(binding)
}

fn configure(run: &Run, path: &Path) -> Result<(), String> {
    let ui = Tmux::new(&run.ui_socket);
    for args in [
        vec!["set-option", "-s", "escape-time", "10"],
        vec!["set-option", "-s", "extended-keys", "on"],
        vec!["set-option", "-g", "default-terminal", "tmux-256color"],
        vec!["set-option", "-g", "prefix", &run.leader],
        vec!["set-option", "-g", "prefix2", "None"],
        vec!["unbind-key", "-a", "-T", "prefix"],
        vec!["unbind-key", "-a", "-T", "root"],
        vec!["set-option", "-g", "mouse", "on"],
        vec!["set-option", "-g", "status-position", "top"],
        vec!["set-option", "-g", "status", "2"],
        vec!["set-option", "-g", "remain-on-exit", "on"],
        vec!["set-option", "-g", "set-clipboard", "on"],
        vec!["set-option", "-g", "pane-border-status", "top"],
        vec!["set-option", "-g", "pane-border-format", " #{pane_title} "],
        vec!["set-option", "-g", "@cockpit-focus-click", "0"],
        vec![
            "set-option",
            "-g",
            "status-format[0]",
            "Read only | attention ? | cc-cockpit",
        ],
    ] {
        ui.run(&args)?;
    }
    ui.run(&[
        "set-option",
        "-g",
        "status-format[0]",
        &crate::status::literal(&crate::status::line(run, 100)),
    ])?;
    ui.run(&[
        "set-option",
        "-g",
        "@cockpit-navigation",
        &crate::status::navigation(run, 100),
    ])?;
    ui.run(&[
        "set-option",
        "-g",
        "@cockpit-prefix-hint",
        &crate::status::prefix_hint(run, 100),
    ])?;
    ui.run(&[
        "set-option",
        "-g",
        "status-format[1]",
        crate::status::NAVIGATION_FORMAT,
    ])?;
    ui.run(&["bind-key", "Escape", "switch-client", "-T", "root"])?;
    for command in commands::COMMANDS {
        if command.dispatch == commands::Dispatch::LiteralLeader {
            ui.run(&["bind-key", &run.leader, "send-prefix"])?;
            continue;
        }
        for key in command.keys {
            match command.dispatch {
                commands::Dispatch::Panel(kind) => {
                    let child =
                        role_command(run, path, "__panel", &format!("--kind {}", quote(kind)));
                    ui.run(&[
                        "bind-key",
                        key,
                        "display-popup",
                        "-E",
                        "-w",
                        "85%",
                        "-h",
                        "85%",
                        &child,
                    ])?;
                }
                commands::Dispatch::Operation(op) => {
                    let child =
                        role_command(run, path, "__binding", &format!("--op {}", quote(op)));
                    // Serialize the native client's subsequent keys with handoff.
                    ui.run(&["bind-key", key, "run-shell", &child])?;
                }
                commands::Dispatch::Detach => {
                    ui.run(&["bind-key", key, "detach-client"])?;
                }
                commands::Dispatch::LiteralLeader => unreachable!(),
            }
        }
    }
    let click = navigation_binding(run, path)?;
    ui.run(&["bind-key", "-T", "root", "MouseDown1Status", &click])?;
    let mouse_command = role_command(
        run,
        path,
        "__binding",
        "--op focus-pane --select '#{mouse_pane}'",
    );
    for key in ["MouseDown1Pane", "DoubleClick1Pane", "TripleClick1Pane"] {
        ui.run(&[
            "bind-key",
            "-T",
            "root",
            key,
            "if-shell",
            "-F",
            // Mouse bindings resolve pane_id to the clicked pane, so comparing it
            // with mouse_pane would always succeed. pane_active tests real focus.
            "#{pane_active}",
            "set-option -g @cockpit-focus-click 0; send-keys -M",
            &format!(
                "set-option -g @cockpit-focus-click 1; run-shell {}",
                quote(&mouse_command)
            ),
        ])?;
    }
    // Consume the entire focus gesture, including a release arriving after
    // control is granted. A release alone can activate a prompt's mouse widget.
    for key in ["MouseUp1Pane", "MouseDragEnd1Pane"] {
        ui.run(&[
            "bind-key",
            "-T",
            "root",
            key,
            "if-shell",
            "-F",
            "#{@cockpit-focus-click}",
            "set-option -g @cockpit-focus-click 0",
            "send-keys -M",
        ])?;
    }
    ui.run(&[
        "bind-key",
        "-T",
        "root",
        "MouseDrag1Pane",
        "if-shell",
        "-F",
        "#{==:#{@cockpit-focus-click},0}",
        "send-keys -M",
    ])?;
    for key in ["WheelUpPane", "WheelDownPane"] {
        ui.run(&["bind-key", "-T", "root", key, "send-keys", "-M"])?;
    }
    Ok(())
}

pub fn launch(
    mut run: Run,
    directory: Option<PathBuf>,
    initial: Option<String>,
    mut snapshots: Vec<crate::model::ProjectSnapshot>,
) -> Result<(), String> {
    let directory = directory.unwrap_or(PathBuf::from(format!(
        "/tmp/cc-cockpit-{}-{}",
        std::process::id(),
        nonce()?
    )));
    if !directory.is_absolute() {
        return Err("--run-dir must be an absolute, new directory".into());
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|e| format!("Cannot create private run directory: {e}"))?;
    run.ui_socket = directory.join("ui.sock").to_string_lossy().into_owned();
    if run.ui_socket.len() >= 104 {
        let _ = fs::remove_dir(&directory);
        return Err("Private socket path is too long; use a short --run-dir under /tmp".into());
    }
    let mut resources = Resources {
        directory: directory.clone(),
        socket: run.ui_socket.clone(),
        identity: None,
    };
    let path = directory.join("run.json");
    update_attention(&mut run, &snapshots);
    crate::recovery::refresh(&mut run);
    write_json(&path, &run)?;
    let mut boot = Command::new("tmux");
    boot.args([
        "-S",
        &run.ui_socket,
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-s",
        "cockpit",
        "-x",
        "100",
        "-y",
        "30",
        &run.executable,
        "__panel",
        "--run",
        &path.to_string_lossy(),
        "--kind",
        "home",
    ]);
    crate::tmux::output(boot)?;
    let ui = Tmux::new(&run.ui_socket);
    run.ui_server_pid = ui.pid()?;
    run.ui_server_start = process_start(run.ui_server_pid)?;
    resources.identity = Some((run.ui_server_pid, run.ui_server_start.clone()));
    write_json(&path, &run)?;
    configure(&run, &path)?;
    let mut preferences_invalid = false;
    let mut first_launch = false;
    if let Some(file) = run.preferences.clone() {
        match preferences::read(Path::new(&file), &run) {
            Ok(Some(saved)) => {
                run.view = saved.view;
                run.recovery.slot_targets = saved.targets.iter().map(|t| t.key.clone()).collect();
                run.recovery.slots = saved
                    .targets
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        saved
                            .tile_registrations
                            .get(i)
                            .cloned()
                            .flatten()
                            .or_else(|| {
                                run.recovery
                                    .entries
                                    .iter()
                                    .find(|e| {
                                        e.snapshot.target.key == t.key
                                            && e.snapshot.target.socket == t.socket
                                    })
                                    .map(|e| e.id.clone())
                            })
                    })
                    .collect();
                for old in saved.targets {
                    let current = run
                        .targets
                        .iter()
                        .find(|target| target.key == old.key && target.window_id == old.window_id);
                    let (target, disconnected) = match current {
                        Some(target) if observe::validate(target, false).is_ok() => {
                            (target.clone(), None)
                        }
                        _ => (
                            old,
                            Some(
                                "Saved target is stale. Explicitly select a current session."
                                    .into(),
                            ),
                        ),
                    };
                    run.tiles
                        .push(create_tile(&run, &path, target, disconnected)?);
                }
                run.selected = saved
                    .selected
                    .filter(|key| run.tiles.iter().any(|tile| &tile.target.key == key))
                    .or_else(|| run.tiles.first().map(|tile| tile.target.key.clone()));
                if !run.tiles.is_empty() {
                    reflow(&mut run, &path, saved.control)?;
                }
            }
            Err(error) => {
                preferences_invalid = true;
                run.message =
                    format!("Preferences unavailable: {error}. Select a current session.");
            }
            Ok(None) => first_launch = true,
        }
    }
    let explicit_initial = initial.is_some();
    let initial = initial.or_else(|| {
        (!preferences_invalid && run.tiles.is_empty() && run.targets.len() == 1)
            .then(|| run.targets[0].key.clone())
    });
    if let Some(key) = initial {
        let control = explicit_initial
            || !run.targets.iter().find(|t| t.key == key).is_some_and(|t| {
                run.recovery.entries.iter().any(|e| {
                    e.enabled
                        && crate::restore::socket_matches(e, &run.original_socket)
                        && e.snapshot.target.session_name == t.session_name
                        && !e.matches(t)
                })
            });
        select(&mut run, &path, &key, false, control)?;
        write_json(&path, &run)?;
    }
    // Restored live tiles and the single-session initial attachment are real
    // connections. Merely listing/highlighting a target is not.
    let connected_keys: Vec<_> = run
        .tiles
        .iter()
        .filter(|tile| tile.disconnected.is_none())
        .map(|tile| tile.target.key.clone())
        .collect();
    for key in connected_keys {
        if let Err(error) = crate::recovery::connected(&mut run, &key, false) {
            run.recovery.error = Some(error);
        }
    }
    if first_launch && !preferences_invalid {
        let leader = crate::help::leader_label(&run.leader);
        run.message = format!("cc-cockpit · Welcome · {leader} Space commands · {leader} ? help");
    }
    navigation::record_visit(&mut run);
    write_json(&path, &run)?;
    if let Some(file) = &run.preferences
        && let Err(error) = preferences::save(Path::new(file), &run)
    {
        run.message = format!("Preferences not saved: {error}");
        write_json(&path, &run)?;
    }
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, Arc::clone(&stop)).map_err(|e| e.to_string())?;
    }
    let mut command = ui.command();
    command
        .args(["attach-session", "-t", "=cockpit"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut outer = command.spawn().map_err(|e| e.to_string())?;
    let recovery_worker = crate::restore::Worker::new();
    let mut recovery_scopes = run.projects.clone();
    recovery_scopes.extend(snapshots.iter().map(|s| s.project_root.clone()));
    for entry in run
        .recovery
        .entries
        .clone()
        .into_iter()
        .filter(|e| e.enabled)
    {
        if crate::restore::socket_matches(&entry, &run.original_socket)
            && crate::restore::scope_matches(&entry, &recovery_scopes)
            && !run.targets.iter().any(|t| entry.matches(t) && !t.dead)
            && let Err(error) = recovery_worker.schedule(&mut run, entry, None)
        {
            run.message = error;
        }
    }
    if !run.recovery.jobs.is_empty() {
        run.message = format!(
            "Reopening {} remembered conversations; new attachments start read-only",
            run.recovery.jobs.len()
        );
    }
    write_json(&path, &run)?;
    let started = Instant::now();
    let mut exit_unattached = false;
    let mut observed = Instant::now();
    let mut project_observed = Instant::now();
    snapshots.sort_by(|a, b| a.project_root.cmp(&b.project_root));
    snapshots.dedup_by(|a, b| a.project_root == b.project_root);
    let mut last_status = String::new();
    let mut last_navigation = String::new();
    let mut last_prefix_hint = String::new();
    let mut last_size = dimensions(&run).ok();
    let mut last_control = None;
    while !stop.load(Ordering::Relaxed) && outer.try_wait().map_err(|e| e.to_string())?.is_none() {
        let mut dirty = false;
        while let Ok(job) = recovery_worker.receive.try_recv() {
            run.recovery.jobs.retain(|j| j.id != job.id);
            run.recovery.jobs.push(job.clone());
            crate::recovery::refresh(&mut run);
            if let Some(target) = job.target.clone() {
                if let Ok(mut targets) = observe::targets(&run.original_socket) {
                    observe::associate(&mut targets, &snapshots);
                    run.targets = targets;
                }
                if let Err(error) = apply_recovered(&mut run, &path, &job.id, target) {
                    run.message = format!("Restoration view unavailable: {error}");
                } else {
                    run.message = job.message.clone();
                }
            } else {
                run.message = job.message.clone();
            }
            dirty = true;
        }
        if !exit_unattached
            && !ui
                .run(&["list-clients", "-F", "#{client_name}"])
                .unwrap_or_default()
                .is_empty()
        {
            ui.run(&["set-option", "-g", "exit-unattached", "on"])?;
            exit_unattached = true;
        }
        if !exit_unattached && started.elapsed() > Duration::from_secs(5) {
            let _ = outer.kill();
            break;
        }
        for entry in fs::read_dir(&directory).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("request-") || !name.ends_with(".json") {
                continue;
            }
            let meta = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            if !meta.file_type().is_file()
                || meta.len() > 4096
                || meta.uid() != unsafe { libc::geteuid() }
            {
                continue;
            }
            let request = serde_json::from_slice::<Request>(
                &fs::read(entry.path()).map_err(|e| e.to_string())?,
            );
            let _ = fs::remove_file(entry.path());
            if let Ok(request) = request {
                if !request
                    .id
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || c == '-')
                {
                    continue;
                }
                let result = if request.op == "registry-retry" || request.op == "registry-close" {
                    (|| -> Result<(), String> {
                        let value: crate::recovery::Close = serde_json::from_str(
                            request.selected.as_deref().ok_or("Missing registration")?,
                        )
                        .map_err(|e| e.to_string())?;
                        crate::recovery::refresh(&mut run);
                        let entry = run
                            .recovery
                            .entries
                            .iter()
                            .find(|e| e.id == value.id && e.revision == value.revision)
                            .cloned()
                            .ok_or("Registration changed; refresh and confirm again")?;
                        let close = if request.op == "registry-close" {
                            let target = run
                                .targets
                                .iter()
                                .find(|t| Some(&t.key) == value.key.as_ref())
                                .cloned()
                                .ok_or("Terminal changed; select it again")?;
                            if crate::recovery::logical_id(&run, &target).as_deref()
                                != Some(&entry.id)
                            {
                                return Err("Conversation association changed".into());
                            }
                            observe::validate(&target, false)?;
                            quiesce(&mut run);
                            Some(target)
                        } else {
                            None
                        };
                        recovery_worker.schedule(&mut run, entry, close)
                    })()
                } else {
                    action(&mut run, &path, &request)
                };
                if let Err(error) = &result {
                    run.message = error.clone();
                }
                write_json(&path, &run)?;
                write_json(
                    &directory.join(format!("response-{}.json", request.id)),
                    &result,
                )?;
                dirty = true;
            }
        }
        if observed.elapsed() >= Duration::from_secs(1) {
            let size = dimensions(&run).ok();
            if size != last_size && !run.tiles.is_empty() {
                let control = run.control;
                if let Err(error) = reflow(&mut run, &path, control) {
                    quiesce(&mut run);
                    run.message = error;
                }
            }
            if size.is_some() {
                last_size = size;
            }
            // One inventory/client snapshot per reconciliation tick, rather
            // than rediscovering the entire original server once per tile.
            let inventory = observe::targets(&run.original_socket);
            let attached = clients(&run);
            let pane_ttys = ui
                .run(&["list-panes", "-a", "-F", "#{pane_id}\t#{pane_tty}"])
                .map(|text| {
                    text.lines()
                        .filter_map(|line| {
                            line.split_once('\t')
                                .map(|(pane, tty)| (pane.to_owned(), tty.to_owned()))
                        })
                        .collect::<HashMap<_, _>>()
                });
            for index in 0..run.tiles.len() {
                let tile = &run.tiles[index];
                if tile.disconnected.is_some() {
                    continue;
                }
                let control = Some(&tile.target.key) == run.selected.as_ref() && run.control;
                let valid: Result<(), String> = (|| {
                    let live = inventory.as_ref().map_err(Clone::clone)?;
                    if !live.iter().any(|target| {
                        target.key == tile.target.key
                            && target.window_id == tile.target.window_id
                            && !target.dead
                    }) {
                        return Err("Selected terminal exited or was replaced".into());
                    }
                    let tty = pane_ttys
                        .as_ref()
                        .map_err(Clone::clone)?
                        .get(&tile.pane)
                        .ok_or("Private attachment disappeared")?;
                    let attached = attached
                        .as_ref()
                        .map_err(Clone::clone)?
                        .get(tty)
                        .ok_or("Attachment disconnected")?;
                    if !attached.matches(&tile.target) || attached.readonly == control {
                        return Err("Attachment destination or mode changed".into());
                    }
                    if control {
                        observe::control_policy(&tile.target)?;
                    }
                    Ok(())
                })();
                if let Err(error) = valid {
                    invalidate(&mut run, index, &error);
                }
            }
            match inventory {
                Ok(mut targets) => {
                    observe::associate(&mut targets, &snapshots);
                    run.targets = targets;
                }
                Err(error) => run.message = format!("Observation stale: {error}"),
            }
            observed = Instant::now();
            dirty = true;
        }
        if project_observed.elapsed() >= Duration::from_secs(10) {
            for project in &run.projects {
                match observe::project_snapshot(project) {
                    Ok(snapshot) => {
                        snapshots.retain(|old| old.project_root != snapshot.project_root);
                        snapshots.push(snapshot);
                    }
                    Err(error) => {
                        run.message = format!("Project observation stale: {error}");
                        for snapshot in &mut snapshots {
                            snapshot.complete = false;
                            snapshot.errors = vec![serde_json::json!({"message":run.message})];
                            snapshot.attention.complete = false;
                            snapshot.attention.errors =
                                vec![serde_json::json!({"message":run.message})];
                        }
                    }
                }
            }
            observe::associate(&mut run.targets, &snapshots);
            update_attention(&mut run, &snapshots);
            crate::recovery::reconcile(&mut run);
            // Another cockpit may have completed a recovery while this worker
            // was busy. Adopt only the registry's newly verified generation.
            for entry in run
                .recovery
                .entries
                .clone()
                .into_iter()
                .filter(|e| e.enabled)
            {
                if crate::restore::socket_matches(&entry, &run.original_socket)
                    && crate::restore::scope_matches(&entry, &recovery_scopes)
                    && let Some(target) = run
                        .targets
                        .iter()
                        .find(|t| entry.matches(t) && !t.dead)
                        .cloned()
                {
                    let _ = apply_recovered(&mut run, &path, &entry.id, target);
                }
            }
            project_observed = Instant::now();
            dirty = true;
        }
        if last_control != Some(run.control) {
            let _ = ui.run(&[
                "set-option",
                "-g",
                "@cockpit-control",
                if run.control { "1" } else { "0" },
            ]);
            last_control = Some(run.control);
        }
        if dirty || last_status.is_empty() {
            let status = crate::status::literal(&crate::status::line(
                &run,
                last_size.map(|(width, _)| width as usize).unwrap_or(80),
            ));
            let navigation = crate::status::navigation(
                &run,
                last_size.map(|(width, _)| width as usize).unwrap_or(80),
            );
            if navigation != last_navigation {
                let _ = ui.run(&["set-option", "-g", "@cockpit-navigation", &navigation]);
                last_navigation = navigation;
            }
            let prefix_hint = crate::status::prefix_hint(
                &run,
                last_size.map(|(width, _)| width as usize).unwrap_or(80),
            );
            if prefix_hint != last_prefix_hint {
                let _ = ui.run(&["set-option", "-g", "@cockpit-prefix-hint", &prefix_hint]);
                last_prefix_hint = prefix_hint;
            }
            if status != last_status {
                let _ = ui.run(&["set-option", "-g", "@cockpit-status", &status]);
                let _ = ui.run(&["set-option", "-g", "status-format[0]", &status]);
                last_status = status;
            }
        }
        if dirty {
            write_json(&path, &run)?;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = outer.kill();
    let _ = outer.wait();
    // Startup and explicit actions already save presentation choices. After an
    // outer terminal closes, a reconciliation in flight can observe the private
    // attachments disappearing and revoke control. That is teardown, not a user
    // preference: never overwrite the last explicit choice with it. Reopening
    // still validates every saved target generation and its control policy.
    drop(resources);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn fixture(directory: &Path) -> Run {
        Run {
            ui_socket: directory.join("ui.sock").to_string_lossy().into_owned(),
            original_socket: "/tmp/original-test.sock".into(),
            supervisor_pid: std::process::id(),
            supervisor_start: process_start(std::process::id()).unwrap(),
            ui_server_pid: 0,
            ui_server_start: String::new(),
            executable: "fixture".into(),
            leader: "C-]".into(),
            projects: vec![],
            targets: vec![],
            selected: None,
            terminal_pane: None,
            sidebar_pane: None,
            control: false,
            tiles: vec![],
            view: View::Focus,
            preferences: None,
            attention: vec![],
            tasks: vec![],
            task_warnings: vec![],
            recent: vec![],
            recovery: Default::default(),
            message: String::new(),
        }
    }

    #[test]
    fn native_navigation_dispatch_uses_groups_within_tmux_command_limit() {
        let directory = Path::new("/tmp/cockpit-buttons");
        let run = fixture(directory);
        let binding = navigation_binding(&run, &directory.join("run.json")).unwrap();
        assert!(binding.len() < 4096, "{} bytes", binding.len());
        assert_eq!(
            binding.matches("if-shell").count(),
            crate::status::NAVIGATION.len()
        );
        assert!(!binding.contains(" ; "));
        for (id, _) in crate::status::NAVIGATION {
            assert!(binding.contains(&format!("cc-{id}")));
        }
    }

    #[test]
    fn manifests_reject_stale_identity_and_shared_directories() {
        let directory = PathBuf::from(format!("/tmp/cc-cockpit-unit-{}", nonce().unwrap()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let _cleanup = Resources {
            directory: directory.clone(),
            socket: String::new(),
            identity: None,
        };
        let path = directory.join("run.json");
        let mut run = fixture(&directory);
        write_json(&path, &run).unwrap();
        assert!(read_run(&path).is_ok());
        run.supervisor_start.push('0');
        write_json(&path, &run).unwrap();
        assert!(read_run(&path).is_err());
        run = fixture(&directory);
        run.ui_socket = run.original_socket.clone();
        write_json(&path, &run).unwrap();
        assert!(read_run(&path).is_err());
        write_json(&path, &fixture(&directory)).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(read_run(&path).is_err());
    }

    #[test]
    fn manifests_reject_symlinks_and_non_private_files() {
        let directory = PathBuf::from(format!("/tmp/cc-cockpit-unit-{}", nonce().unwrap()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let _cleanup = Resources {
            directory: directory.clone(),
            socket: String::new(),
            identity: None,
        };
        let path = directory.join("run.json");
        write_json(&path, &fixture(&directory)).unwrap();
        let link = directory.join("linked.json");
        symlink(&path, &link).unwrap();
        assert!(read_run(&link).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_run(&path).is_err());
    }
}
