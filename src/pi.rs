use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

pub enum PiEvent {
    Record(Value),
    Stderr(String),
    Exit,
}

pub struct PiClient {
    child: Child,
    stdin: Option<ChildStdin>,
    pub rx: Receiver<PiEvent>,
    next_id: u64,
}

impl PiClient {
    pub fn spawn(pi: &Path, cwd: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(pi)
            .args(["--mode", "rpc", "--no-session"])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = mpsc::channel();

        if let Some(stdout) = stdout {
            let tx = tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    let line = line.trim_end_matches('\r');
                    if line.trim().is_empty() {
                        continue;
                    }
                    if let Ok(value) = serde_json::from_str::<Value>(line) {
                        if tx.send(PiEvent::Record(value)).is_err() {
                            return;
                        }
                    }
                }
                let _ = tx.send(PiEvent::Exit);
            });
        }

        if let Some(stderr) = stderr {
            let tx = tx.clone();
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if !line.trim().is_empty() {
                        let _ = tx.send(PiEvent::Stderr(line));
                    }
                }
            });
        }

        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 0,
        })
    }

    pub fn send(&mut self, mut command: Value) -> std::io::Result<()> {
        let Some(stdin) = &mut self.stdin else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "pi stdin is closed",
            ));
        };
        if command.get("id").is_none() {
            self.next_id += 1;
            if let Some(object) = command.as_object_mut() {
                object.insert("id".to_string(), json!(format!("smol-{}", self.next_id)));
            }
        }
        let line = serde_json::to_string(&command)?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }

    pub fn prompt(&mut self, message: &str, busy: bool) -> std::io::Result<()> {
        let mut command = json!({ "type": "prompt", "message": message });
        if busy {
            command["streamingBehavior"] = json!("steer");
        }
        self.send(command)
    }

    pub fn abort(&mut self) -> std::io::Result<()> {
        self.send(json!({ "type": "abort" }))
    }

    pub fn get_state(&mut self) -> std::io::Result<()> {
        self.send(json!({ "type": "get_state" }))
    }

    pub fn shutdown(&mut self) {
        self.stdin.take();
        for _ in 0..20 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for PiClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn find_pi() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(if cfg!(windows) { "pi.exe" } else { "pi" });
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }

    let fallback = env::var_os("HOME")
        .map(PathBuf::from)?
        .join(".pi")
        .join("agent")
        .join("bin")
        .join(if cfg!(windows) { "pi.exe" } else { "pi" });
    is_executable(&fallback).then_some(fallback)
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
