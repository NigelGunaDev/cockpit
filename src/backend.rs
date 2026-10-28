use std::{
    io::Read,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub struct Outcome {
    pub message: String,
    pub structured: serde_json::Value,
}

pub fn execute(project: &str, args: &[String]) -> Result<Outcome, String> {
    let operation = args.get(1).ok_or("Missing native operation")?;
    if !["recover", "close-session"].contains(&operation.as_str()) {
        return Err("Unsupported native operation".into());
    }
    let request = serde_json::json!({"operation":operation,"project":project,"args":args.iter().skip(2).filter(|arg|arg.as_str()!="--json").collect::<Vec<_>>()});
    let mut command = Command::new("node");
    command
        .args([
            "-e",
            include_str!("../backend/recovery.cjs"),
            "--",
            "--cockpit-operation",
        ])
        .arg(request.to_string())
        .current_dir(project)
        .env(
            "COCKPIT_LAUNCH_PATH",
            std::env::var_os("PATH").unwrap_or_default(),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("Native recovery requires Node.js on PATH: {e}"))?;
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut data = Vec::new();
            stream
                .take(2 * 1024 * 1024 + 1)
                .read_to_end(&mut data)
                .map(|_| data)
        })
    };
    let out = read(Box::new(child.stdout.take().unwrap()));
    let err = read(Box::new(child.stderr.take().unwrap()));
    let began = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if began.elapsed() < Duration::from_secs(60) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Native operation result unknown; inspect the preserved terminal before retrying".into());
            }
        }
    };
    let out = out
        .join()
        .map_err(|_| "Native output reader failed")?
        .map_err(|e| e.to_string())?;
    let err = err
        .join()
        .map_err(|_| "Native error reader failed")?
        .map_err(|e| e.to_string())?;
    if out.len() > 2 * 1024 * 1024 || err.len() > 2 * 1024 * 1024 {
        return Err("Native operation response exceeded the output limit".into());
    }
    let structured: serde_json::Value = serde_json::from_slice(&out).map_err(|e| {
        format!(
            "Invalid native operation response: {e}; {}",
            String::from_utf8_lossy(&err)
        )
    })?;
    if !status.success() || structured["ok"] != true {
        return Err(structured["error"]
            .as_str()
            .unwrap_or("Native operation failed")
            .into());
    }
    let message = format!(
        "{}: {}",
        operation,
        structured["outcome"].as_str().unwrap_or("unknown")
    );
    Ok(Outcome {
        message,
        structured,
    })
}
