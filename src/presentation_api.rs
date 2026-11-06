//! Public, versioned presentation surface. These operations never start or
//! resume an agent and never manipulate the original tmux worker's lifecycle.
use crate::{observe, runtime};
use serde_json::json;
use std::path::Path;

pub fn run(args: &[String]) -> Result<(), String> {
    let operation = args
        .first()
        .map(String::as_str)
        .ok_or("Missing presentation operation")?;
    if !["inspect", "open", "focus", "close"].contains(&operation) {
        return Err("Use presentation inspect|open|focus|close --run PATH [--key KEY]".into());
    }
    let mut manifest = None;
    let mut key = None;
    let mut index = 1;
    while index < args.len() {
        let value = args
            .get(index + 1)
            .ok_or("Missing presentation option value")?;
        match args[index].as_str() {
            "--run" => manifest = Some(value),
            "--key" => key = Some(value),
            _ => return Err("Unknown presentation option".into()),
        }
        index += 2;
    }
    let path = Path::new(manifest.ok_or("Presentation requires --run PATH")?);
    if !path.is_absolute() {
        return Err("--run must be absolute".into());
    }
    if operation != "inspect" {
        runtime::request(
            path,
            &format!("presentation-{operation}"),
            Some(key.ok_or("Missing --key")?),
        )?;
    }
    let run = runtime::read_run(path)?;
    let targets = observe::targets(&run.original_socket)?;
    println!(
        "{}",
        json!({
            "version": 1,
            "instance": { "pid": run.supervisor_pid, "start": run.supervisor_start },
            "selected": run.selected,
            "targets": targets,
            "views": run.tiles.iter().map(|tile| json!({
                "key": tile.target.key, "pane": tile.pane,
                "visible": tile.visible, "disconnected": tile.disconnected,
            })).collect::<Vec<_>>(),
            "capacity": crate::layout::MAX_TILES,
            "host_raised": false,
        })
    );
    Ok(())
}
