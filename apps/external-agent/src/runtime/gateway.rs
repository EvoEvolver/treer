//! Finite native CLI execution. Deployment selects the executable; requests supply argv only.
use super::{controller::ResultValue, Config};
use crate::{Error, Outcome};
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
const OUTPUT: usize = 2 * 1024 * 1024;
/// The configured native CLI. Identified by `treer --version`, never by file hash:
/// a Treer update replacing the binary keeps the gateway working.
pub struct CliGateway {
    executable: PathBuf,
    version: std::sync::Mutex<Result<String, String>>,
}
/// Oldest CLI release whose argv contract this gateway documents.
const MIN_VERSION: (u64, u64, u64) = (0, 1, 0);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Input {
    argv: Vec<String>,
    #[serde(default)]
    stdin: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}
fn default_timeout() -> u64 {
    30_000
}
impl Input {
    pub(super) fn validate(&mut self) -> Result<(), Error> {
        if self.argv.first().is_some_and(|s| s == "treer") {
            self.argv.remove(0);
        }
        if self.argv.is_empty()
            || self.argv.len() > 256
            || self.argv.iter().map(String::len).sum::<usize>() > 32768
            || self.argv.iter().any(|s| s.contains('\0'))
            || !(1..=60_000).contains(&self.timeout_ms)
        {
            return Err(Error::Invalid);
        }
        // Connection flags belong to the trusted deployment. Business arguments after
        // the command (including literal prompts after --) retain normal CLI semantics.
        let mut i = 0;
        while let Some(arg) = self.argv.get(i) {
            if arg == "--" || !arg.starts_with('-') {
                break;
            }
            if ["--url", "--workspace", "--endpoint"]
                .iter()
                .any(|k| arg == k || arg.starts_with(&format!("{k}=")))
            {
                return Err(Error::Invalid);
            }
            i += if arg == "--format" { 2 } else { 1 };
        }
        Ok(())
    }
    pub(super) fn aggregate_mailbox(&self) -> bool {
        let mut i = 0;
        while let Some(arg) = self.argv.get(i) {
            if arg == "--" {
                i += 1;
                break;
            }
            if !arg.starts_with('-') {
                break;
            }
            i += if arg == "--format" { 2 } else { 1 };
        }
        self.argv.get(i).is_some_and(|a| a == "message")
            && self.argv.get(i + 1).is_none_or(|a| a != "send")
    }
    pub(super) fn unsupported(&self) -> bool {
        let mut i = 0;
        while let Some(arg) = self.argv.get(i) {
            if arg == "--" {
                i += 1;
                break;
            }
            if !arg.starts_with('-') {
                break;
            }
            i += if arg == "--format" { 2 } else { 1 };
        }
        let argv = &self.argv[i.min(self.argv.len())..];
        if argv.first().is_some_and(|s| s == "message") {
            let mut args = argv.iter().take_while(|s| s.as_str() != "--");
            while let Some(arg) = args.next() {
                for flag in ["--body-file", "--external-source-file"] {
                    if arg == flag && args.next().is_none_or(|value| value != "-") {
                        return true;
                    }
                    if arg
                        .strip_prefix(flag)
                        .and_then(|s| s.strip_prefix('='))
                        .is_some_and(|value| value != "-")
                    {
                        return true;
                    }
                }
            }
        }
        matches!(
            (
                argv.first().map(String::as_str),
                argv.get(1).map(String::as_str)
            ),
            (Some("agent"), Some("attach"))
                | (Some("service"), Some("forward"))
                | (Some("network"), Some("connect"))
                | (Some("machine"), Some("put" | "get"))
        ) || argv
            .iter()
            .take_while(|a| a.as_str() != "--")
            .any(|a| a == "--follow" || a.starts_with("--follow="))
    }
}
impl CliGateway {
    pub fn new(executable: PathBuf) -> Self {
        let version = probe(&executable);
        Self {
            executable,
            version: std::sync::Mutex::new(version),
        }
    }
    /// `Ok(version)` or the reason the CLI is unusable; re-probed while unusable.
    pub(super) fn status(&self) -> Result<String, String> {
        let mut v = self
            .version
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if v.is_err() {
            *v = probe(&self.executable);
        }
        v.clone()
    }
    pub(super) fn execute(&self, c: &Config, credential: &str, input: Input) -> ResultValue {
        if let Err(reason) = self.status() {
            return ResultValue {
                status: 503,
                value: json!({"error":{"code":"cli_unavailable","message":reason}}),
                outcome: Outcome::Unavailable,
            };
        }
        match self.run(c, credential, input) {
            Ok((value, uncertain)) => ResultValue {
                status: 200,
                value,
                outcome: if uncertain {
                    Outcome::Unknown
                } else {
                    Outcome::Completed
                },
            },
            Err(_) => ResultValue {
                status: 503,
                value: json!({"error":{"code":"cli_unavailable"},"outcome_uncertain":true}),
                outcome: Outcome::Unknown,
            },
        }
    }
    fn run(
        &self,
        c: &Config,
        credential: &str,
        input: Input,
    ) -> Result<(serde_json::Value, bool), Error> {
        let mut child = Command::new(&self.executable)
            .args(&input.argv)
            .env_clear()
            .env("TREER_IDENTITY_MODE", "agent")
            .env("TREER_WORKSPACE_ID", &c.workspace)
            .env("TREER_AGENT_ID", &c.agent)
            .env("TREER_SERVER_ID", &c.machine)
            .env("TREER_AGENT_SERVER_URL", c.controller.as_str())
            .env("TREER_WORKLOAD_CREDENTIAL", credential)
            .current_dir(&c.state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|_| Error::Invalid)?;
        let pid = rustix::process::Pid::from_raw(child.id() as i32).ok_or(Error::Invalid)?;
        let result = (|| {
            let stdin = child.stdin.take().ok_or(Error::Invalid)?;
            let mut stdout = child.stdout.take().ok_or(Error::Invalid)?;
            let mut stderr = child.stderr.take().ok_or(Error::Invalid)?;
            for fd in [&stdin as &dyn std::os::fd::AsFd, &stdout, &stderr] {
                rustix::fs::fcntl_setfl(fd, rustix::fs::OFlags::NONBLOCK)
                    .map_err(|_| Error::Invalid)?;
            }
            let started = Instant::now();
            let mut sent = 0;
            let mut stdin = Some(stdin);
            let mut out = Vec::new();
            let mut err = Vec::new();
            let mut timed_out = false;
            let mut truncated = false;
            let mut status = None;
            let mut eof = [false; 2];
            loop {
                if let Some(pipe) = stdin.as_mut() {
                    match pipe.write(&input.stdin.as_bytes()[sent..]) {
                        Ok(n) => sent += n,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => stdin = None,
                        Err(_) => return Err(Error::Invalid),
                    }
                    if sent == input.stdin.len() {
                        stdin = None;
                    }
                }
                for (i, (pipe, buffer)) in [
                    (&mut stdout as &mut dyn Read, &mut out),
                    (&mut stderr as &mut dyn Read, &mut err),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut bytes = [0; 8192];
                    for _ in 0..32 {
                        match pipe.read(&mut bytes) {
                            Ok(0) => {
                                eof[i] = true;
                                break;
                            }
                            Ok(n) => {
                                buffer.extend_from_slice(&bytes[..n]);
                                if buffer.len() > OUTPUT {
                                    truncated = true;
                                    break;
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => return Err(Error::Invalid),
                        }
                    }
                }
                if out.len() + err.len() > OUTPUT {
                    truncated = true;
                }
                if status.is_none() {
                    status = child.try_wait().map_err(|_| Error::Invalid)?;
                    if status.is_some() {
                        let _ =
                            rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
                    }
                }
                if started.elapsed() >= Duration::from_millis(input.timeout_ms) {
                    timed_out = true;
                }
                if (status.is_some() && eof.iter().all(|e| *e)) || timed_out || truncated {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let uncertain = timed_out || truncated || status.is_none_or(|s| s.code().is_none());
            let code = if timed_out {
                124
            } else if truncated {
                125
            } else {
                status.and_then(|s| s.code()).unwrap_or(125)
            };
            out.truncate(OUTPUT);
            err.truncate(OUTPUT.saturating_sub(out.len()));
            Ok((
                json!({"exit_code":code,"stdout":String::from_utf8_lossy(&out),"stderr":String::from_utf8_lossy(&err),"timed_out":timed_out,"truncated":truncated,"outcome_uncertain":uncertain}),
                uncertain,
            ))
        })();
        // The dedicated group contains only this invocation, including any descendants.
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        let _ = child.wait();
        result
    }
}

/// Runs `CLI --version` with an empty environment and parses `treer X.Y.Z ...`.
fn probe(executable: &Path) -> Result<String, String> {
    if !executable.is_absolute() {
        return Err("treer_cli must be an absolute path".into());
    }
    let output = Command::new(executable)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", executable.display()))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .trim()
        .strip_prefix("treer ")
        .and_then(|v| v.split_whitespace().next())
        .filter(|_| output.status.success())
        .ok_or_else(|| {
            format!(
                "{} --version did not identify a treer CLI",
                executable.display()
            )
        })?;
    let mut parts = version.split(['.', '-', '+']).map(|p| p.parse::<u64>());
    let parsed = match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(a)), Some(Ok(b)), Some(Ok(c))) => (a, b, c),
        _ => return Err(format!("unrecognized treer CLI version {version}")),
    };
    if parsed < MIN_VERSION {
        return Err(format!(
            "treer CLI {version} is older than {}.{}.{}",
            MIN_VERSION.0, MIN_VERSION.1, MIN_VERSION.2
        ));
    }
    Ok(version.to_owned())
}
