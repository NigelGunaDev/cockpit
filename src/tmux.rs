use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_OUTPUT: u64 = 2 * 1024 * 1024;

pub fn output_bytes(mut command: Command) -> Result<Vec<u8>, String> {
    command.env_remove("TMUX").env_remove("TMUX_PANE");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot launch command: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream
                .take(MAX_OUTPUT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        })
    };
    let out = read(Box::new(stdout));
    let err = read(Box::new(stderr));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(None) => {
                // This group contains only the short-lived observation command,
                // never an original tmux server or Claude process.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                break Err("Command timed out after 5 seconds".to_string());
            }
            Err(e) => break Err(e.to_string()),
        }
    };
    let bytes = out
        .join()
        .map_err(|_| "Output reader failed")?
        .map_err(|e| e.to_string())?;
    let errors = err
        .join()
        .map_err(|_| "Error reader failed")?
        .map_err(|e| e.to_string())?;
    let status = status?;
    if bytes.len() as u64 > MAX_OUTPUT || errors.len() as u64 > MAX_OUTPUT {
        return Err("Command output exceeded 2 MiB".into());
    }
    if !status.success() {
        let message = String::from_utf8_lossy(&errors).trim().to_owned();
        return Err(if message.is_empty() {
            format!("Command failed ({status}) without an error message")
        } else {
            message
        });
    }
    Ok(bytes)
}

pub fn output(command: Command) -> Result<String, String> {
    let bytes = output_bytes(command)?;
    String::from_utf8(bytes)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .map_err(|_| "Command produced invalid UTF-8".into())
}

#[derive(Clone)]
pub struct Tmux {
    pub socket: String,
}

impl Tmux {
    pub fn new(socket: &str) -> Self {
        Self {
            socket: socket.to_owned(),
        }
    }
    pub fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(["-S", &self.socket]);
        command.env_remove("TMUX").env_remove("TMUX_PANE");
        command
    }
    pub fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut command = self.command();
        command.args(args);
        output(command)
    }
    pub fn pid(&self) -> Result<u32, String> {
        self.run(&["display-message", "-p", "#{pid}"])?
            .parse()
            .map_err(|_| "Invalid tmux server identity".into())
    }
}

pub fn process_start(pid: u32) -> Result<String, String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|_| format!("Process {pid} no longer exists"))?;
    // comm can contain spaces and parentheses. starttime is field 22.
    stat.rsplit_once(')')
        .and_then(|(_, tail)| tail.split_whitespace().nth(19))
        .map(str::to_owned)
        .ok_or_else(|| "Invalid process identity".into())
}

pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quote_does_not_interpret_shell_metacharacters() {
        let value = "a ' b $(touch /tmp/no) `false`\n";
        let mut command = Command::new("sh");
        command.args(["-c", &format!("printf %s {}", quote(value))]);
        assert_eq!(output(command).unwrap(), value.trim_end_matches('\n'));
    }
    #[test]
    fn process_identity_is_readable() {
        assert!(!process_start(std::process::id()).unwrap().is_empty());
    }
}
