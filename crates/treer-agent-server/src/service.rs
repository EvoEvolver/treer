use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub proxy: String,
    pub workspace: String,
    pub root: PathBuf,
    pub listen: String,
}

impl ServiceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read service config {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid service config {}", path.display()))
    }

    fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("service config path has no parent directory")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(self).context("failed to encode service config")?;
        write_atomic(path, &bytes)
    }
}

pub fn install(config: ServiceConfig) -> Result<()> {
    validate_workspace(&config.workspace)?;
    let paths = ServicePaths::new(&config.workspace)?;
    fs::create_dir_all(&paths.state_dir)
        .with_context(|| format!("failed to create {}", paths.state_dir.display()))?;
    config.save(&paths.config)?;
    platform::install(&paths, &config.workspace)?;
    println!("treer: agent server service installed and started");
    println!(
        "treer: status: \"{}\" service --workspace {} status",
        paths.executable.display(),
        config.workspace,
    );
    Ok(())
}

pub fn start(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::start(&ServicePaths::new(workspace)?, workspace)
}

pub fn stop(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::stop(&ServicePaths::new(workspace)?, workspace)
}

pub fn restart(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::restart(&ServicePaths::new(workspace)?, workspace)
}

pub fn status(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    platform::status(&ServicePaths::new(workspace)?, workspace)
}

pub fn logs(workspace: &str, lines: usize, follow: bool) -> Result<()> {
    validate_workspace(workspace)?;
    platform::logs(&ServicePaths::new(workspace)?, workspace, lines, follow)
}

pub fn uninstall(workspace: &str) -> Result<()> {
    validate_workspace(workspace)?;
    let paths = ServicePaths::new(workspace)?;
    platform::uninstall(&paths, workspace)?;
    remove_if_exists(&paths.config)?;
    println!("treer: agent server service uninstalled");
    Ok(())
}

#[derive(Debug)]
struct ServicePaths {
    executable: PathBuf,
    config: PathBuf,
    state_dir: PathBuf,
}

impl ServicePaths {
    fn new(workspace: &str) -> Result<Self> {
        let home = home_dir()?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let state_dir = env::var_os("TREER_STATE_DIR")
            .map(PathBuf::from)
            .or_else(|| env::var_os("XDG_STATE_HOME").map(|path| PathBuf::from(path).join("treer")))
            .unwrap_or_else(|| home.join(".local/state/treer"));
        let key = workspace_key(workspace);
        let executable = env::current_exe()
            .context("failed to find the treer-agent-server executable")?
            .canonicalize()
            .context("failed to resolve the treer-agent-server executable")?;
        Ok(Self {
            executable,
            config: config_home
                .join("treer/agent-servers")
                .join(format!("{key}.json")),
            state_dir,
        })
    }
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is required to manage the agent-server service")
}

fn validate_workspace(workspace: &str) -> Result<()> {
    if workspace.trim().is_empty() {
        bail!("workspace must not be empty");
    }
    Ok(())
}

fn workspace_key(workspace: &str) -> String {
    workspace
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("failed to replace {}", path.display()))
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

fn run_checked(command: &mut Command, description: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to run {description}"))?;
    require_success(status, description)
}

fn require_success(status: ExitStatus, description: &str) -> Result<()> {
    if status.success() {
        Ok(())
    } else {
        bail!("{description} exited with {status}")
    }
}

#[cfg(any(target_os = "linux", test))]
fn systemd_unit(executable: &Path, config: &Path, workspace: &str) -> String {
    format!(
        "[Unit]\nDescription=Treer agent server ({workspace})\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart={} run --config {}\nRestart=always\nRestartSec=2\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(executable),
        systemd_quote(config)
    )
}

#[cfg(any(target_os = "linux", test))]
fn systemd_quote(path: &Path) -> String {
    let escaped = path
        .to_string_lossy()
        .replace('%', "%%")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(any(target_os = "macos", test))]
fn launchd_plist(
    executable: &Path,
    config: &Path,
    label: &str,
    stdout_path: &Path,
    stderr_path: &Path,
) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{executable}</string>
    <string>run</string>
    <string>--config</string>
    <string>{config}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
  <key>StandardOutPath</key>
  <string>{stdout_path}</string>
  <key>StandardErrorPath</key>
  <string>{stderr_path}</string>
</dict>
</plist>
"#,
        label = xml_escape(label),
        executable = xml_escape(&executable.to_string_lossy()),
        config = xml_escape(&config.to_string_lossy()),
        stdout_path = xml_escape(&stdout_path.to_string_lossy()),
        stderr_path = xml_escape(&stderr_path.to_string_lossy()),
    )
}

#[cfg(any(target_os = "macos", test))]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    fn unit_name(workspace: &str) -> String {
        format!("treer-agent-server-{}.service", workspace_key(workspace))
    }

    fn unit_path(workspace: &str) -> Result<PathBuf> {
        let home = home_dir()?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(config_home.join("systemd/user").join(unit_name(workspace)))
    }

    pub fn install(paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit_path = unit_path(workspace)?;
        let parent = unit_path
            .parent()
            .context("systemd user unit path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        write_atomic(
            &unit_path,
            systemd_unit(&paths.executable, &paths.config, workspace).as_bytes(),
        )?;
        run_checked(
            Command::new("systemctl").args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )?;
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "enable", unit.as_str()]),
            "systemctl --user enable",
        )?;
        run_checked(
            Command::new("systemctl").args(["--user", "restart", unit.as_str()]),
            "systemctl --user restart",
        )?;
        enable_linger();
        Ok(())
    }

    pub fn start(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "start", unit.as_str()]),
            "systemctl --user start",
        )
    }

    pub fn stop(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "stop", unit.as_str()]),
            "systemctl --user stop",
        )
    }

    pub fn restart(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "restart", unit.as_str()]),
            "systemctl --user restart",
        )
    }

    pub fn status(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        run_checked(
            Command::new("systemctl").args(["--user", "status", "--no-pager", unit.as_str()]),
            "systemctl --user status",
        )
    }

    pub fn logs(_paths: &ServicePaths, workspace: &str, lines: usize, follow: bool) -> Result<()> {
        let unit = unit_name(workspace);
        let mut command = Command::new("journalctl");
        command.args([
            "--user",
            "-u",
            unit.as_str(),
            "--no-pager",
            "-n",
            &lines.to_string(),
        ]);
        if follow {
            command.arg("-f");
        }
        run_checked(&mut command, "journalctl")
    }

    pub fn uninstall(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let unit = unit_name(workspace);
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", unit.as_str()])
            .status();
        remove_if_exists(&unit_path(workspace)?)?;
        run_checked(
            Command::new("systemctl").args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )
    }

    fn enable_linger() {
        let Some(user) = env::var_os("USER") else {
            eprintln!("treer: warning: USER is unset; could not enable systemd linger");
            return;
        };
        match Command::new("loginctl")
            .arg("--no-ask-password")
            .arg("enable-linger")
            .arg(&user)
            .status()
        {
            Ok(status) if status.success() => {}
            _ => eprintln!(
                "treer: warning: could not enable linger; run `loginctl enable-linger {}` to keep the service running without a login session",
                user.to_string_lossy()
            ),
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    fn label(workspace: &str) -> String {
        format!("dev.treer.agent-server.{}", workspace_key(workspace))
    }

    fn plist_path(workspace: &str) -> Result<PathBuf> {
        Ok(home_dir()?
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(workspace))))
    }

    fn domain() -> Result<String> {
        let output = Command::new("id")
            .arg("-u")
            .output()
            .context("failed to determine the current user id")?;
        require_success(output.status, "id -u")?;
        let uid = String::from_utf8(output.stdout)
            .context("id -u returned non-UTF-8 output")?
            .trim()
            .to_owned();
        Ok(format!("gui/{uid}"))
    }

    fn service_target(workspace: &str) -> Result<String> {
        Ok(format!("{}/{}", domain()?, label(workspace)))
    }

    pub fn install(paths: &ServicePaths, workspace: &str) -> Result<()> {
        let plist_path = plist_path(workspace)?;
        let parent = plist_path
            .parent()
            .context("LaunchAgent path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let log_path = paths
            .state_dir
            .join(format!("agent-server-{}.log", workspace_key(workspace)));
        write_atomic(
            &plist_path,
            launchd_plist(
                &paths.executable,
                &paths.config,
                &label(workspace),
                &log_path,
                &log_path,
            )
            .as_bytes(),
        )?;
        let domain = domain()?;
        let target = service_target(workspace)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        run_checked(
            Command::new("launchctl").args([
                "bootstrap",
                domain.as_str(),
                plist_path.to_string_lossy().as_ref(),
            ]),
            "launchctl bootstrap",
        )
    }

    pub fn start(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        let loaded = Command::new("launchctl")
            .args(["print", target.as_str()])
            .status()
            .context("failed to query LaunchAgent")?
            .success();
        if loaded {
            run_checked(
                Command::new("launchctl").args(["kickstart", "-k", target.as_str()]),
                "launchctl kickstart",
            )
        } else {
            let domain = domain()?;
            let plist = plist_path(workspace)?;
            run_checked(
                Command::new("launchctl").args([
                    "bootstrap",
                    domain.as_str(),
                    plist.to_string_lossy().as_ref(),
                ]),
                "launchctl bootstrap",
            )
        }
    }

    pub fn stop(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        run_checked(
            Command::new("launchctl").args(["bootout", target.as_str()]),
            "launchctl bootout",
        )
    }

    pub fn restart(paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        start(paths, workspace)
    }

    pub fn status(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        run_checked(
            Command::new("launchctl").args(["print", target.as_str()]),
            "launchctl print",
        )
    }

    pub fn logs(paths: &ServicePaths, workspace: &str, lines: usize, follow: bool) -> Result<()> {
        let log_path = paths
            .state_dir
            .join(format!("agent-server-{}.log", workspace_key(workspace)));
        let mut command = Command::new("tail");
        command.args(["-n", &lines.to_string()]);
        if follow {
            command.arg("-f");
        }
        command.arg(log_path);
        run_checked(&mut command, "tail")
    }

    pub fn uninstall(_paths: &ServicePaths, workspace: &str) -> Result<()> {
        let target = service_target(workspace)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        remove_if_exists(&plist_path(workspace)?)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use super::*;

    fn unsupported() -> Result<()> {
        bail!("service management is currently supported on Linux and macOS")
    }

    pub fn install(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn start(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn stop(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn restart(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn status(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }

    pub fn logs(
        _paths: &ServicePaths,
        _workspace: &str,
        _lines: usize,
        _follow: bool,
    ) -> Result<()> {
        unsupported()
    }

    pub fn uninstall(_paths: &ServicePaths, _workspace: &str) -> Result<()> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_keys_are_safe_for_service_names() {
        assert_eq!(workspace_key("team one/alpha"), "team_one_alpha");
        assert_eq!(workspace_key("default"), "default");
    }

    #[test]
    fn systemd_unit_quotes_paths_and_restarts() {
        let unit = systemd_unit(
            Path::new("/home/test user/bin/treer-agent-server"),
            Path::new("/home/test%user/config.json"),
            "team one",
        );
        assert!(unit.contains("Restart=always"));
        assert!(unit.contains("\"/home/test user/bin/treer-agent-server\" run"));
        assert!(unit.contains("test%%user"));
    }

    #[test]
    fn launchd_plist_escapes_values_and_keeps_process_alive() {
        let plist = launchd_plist(
            Path::new("/Users/a&b/treer-agent-server"),
            Path::new("/Users/a&b/config.json"),
            "dev.treer.test",
            Path::new("/tmp/out.log"),
            Path::new("/tmp/error.log"),
        );
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("/Users/a&amp;b/treer-agent-server"));
        assert!(plist.contains("<string>run</string>"));
    }
}
