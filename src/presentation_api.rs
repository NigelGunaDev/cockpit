//! Public presentation surface. These operations never start or resume an agent
//! and never manipulate the original tmux worker's lifecycle.
//!
//! Every reply is one JSON object on stdout with `version` and `ok`. A refusal is
//! `{"version":1,"ok":false,"error":{"code","message"}}` and exits nonzero;
//! clients switch on the code (`failure::Code`).
use crate::failure::{Code, Failure};
use crate::{observe, runtime};
use serde_json::{Value, json};
use std::path::Path;

pub const VERSION: u32 = 1;

struct Arguments<'a> {
    operation: &'a str,
    run: &'a Path,
    key: Option<&'a str>,
    status_file: Option<&'a str>,
}

fn parse(args: &[String]) -> Result<Arguments<'_>, Failure> {
    let invalid = |message: &str| Failure::new(Code::InvalidRequest, message);
    let usage =
        "Use presentation inspect|open|focus|close --run PATH [--key KEY] [--status-file PATH]";
    let operation = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| invalid(usage))?;
    if !["inspect", "open", "focus", "close"].contains(&operation) {
        return Err(invalid(usage));
    }
    let (mut run, mut key, mut status_file) = (None, None, None);
    let mut index = 1;
    while index < args.len() {
        let value = args
            .get(index + 1)
            .map(String::as_str)
            .ok_or_else(|| invalid("Missing presentation option value"))?;
        match args[index].as_str() {
            "--run" => run = Some(value),
            "--key" => key = Some(value),
            "--status-file" => status_file = Some(value),
            _ => return Err(invalid("Unknown presentation option")),
        }
        index += 2;
    }
    let run = Path::new(run.ok_or_else(|| invalid("Presentation requires --run PATH"))?);
    if !run.is_absolute() {
        return Err(invalid("--run must be absolute"));
    }
    if operation != "inspect" && key.is_none() {
        return Err(invalid("Missing --key"));
    }
    if status_file.is_some() && operation != "open" {
        return Err(invalid("--status-file applies to open only"));
    }
    if status_file.is_some_and(|file| !Path::new(file).is_absolute()) {
        return Err(invalid("--status-file must be an absolute path"));
    }
    Ok(Arguments {
        operation,
        run,
        key,
        status_file,
    })
}

fn state(arguments: &Arguments) -> Result<Value, Failure> {
    if arguments.operation != "inspect" {
        runtime::send(
            arguments.run,
            &format!("presentation-{}", arguments.operation),
            arguments.key,
            arguments.status_file,
        )?;
    }
    let run = runtime::live_run(arguments.run)?;
    let targets = observe::targets(&run.original_socket)?;
    Ok(json!({
        "version": VERSION,
        "ok": true,
        "instance": { "pid": run.supervisor_pid, "start": run.supervisor_start },
        "socket": run.original_socket,
        "selected": run.selected,
        "targets": targets,
        "views": run.tiles.iter().map(|tile| json!({
            "key": tile.target.key, "pane": tile.pane,
            "visible": tile.visible, "disconnected": tile.disconnected,
        })).collect::<Vec<_>>(),
        "activity": run.reports.iter().map(|report| json!({
            "key": report.key, "state": report.state, "detail": report.detail,
            "done": report.done(), "stale": report.stale(),
        })).collect::<Vec<_>>(),
        "capacity": crate::layout::MAX_TILES,
        "host_raised": false,
    }))
}

pub fn run(args: &[String]) -> Result<(), String> {
    match parse(args).and_then(|arguments| state(&arguments)) {
        Ok(reply) => {
            println!("{reply}");
            Ok(())
        }
        Err(failure) => {
            println!(
                "{}",
                json!({ "version": VERSION, "ok": false, "error": failure })
            );
            Err(failure.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn malformed_requests_are_invalid_before_any_host_is_contacted() {
        for text in [
            "",
            "launch --run /tmp/run.json",
            "open --run relative.json --key k",
            "open --run /tmp/run.json",
            "focus --run /tmp/run.json --key k --status-file /tmp/status.json",
            "open --run /tmp/run.json --key k --status-file relative.json",
            "inspect --run /tmp/run.json --bogus x",
        ] {
            let failure = parse(&arguments(text)).err().unwrap();
            assert_eq!(failure.code, Code::InvalidRequest, "{text}");
        }
        let open = arguments("open --run /tmp/run.json --key k --status-file /tmp/status.json");
        let parsed = parse(&open).unwrap();
        assert_eq!(parsed.status_file, Some("/tmp/status.json"));
    }

    #[test]
    fn a_missing_run_is_unavailable() {
        let failure = state(
            &parse(&arguments(
                "inspect --run /tmp/cockpit-no-such-run/run.json",
            ))
            .unwrap(),
        )
        .err()
        .unwrap();
        assert_eq!(failure.code, Code::RunUnavailable);
    }
}
