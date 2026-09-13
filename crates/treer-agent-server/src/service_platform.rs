use super::*;
#[cfg(target_os = "linux")]
pub(super) mod platform {
    use super::*;

    fn unit_name(server_id: &str) -> String {
        format!("treer-agent-server-{}.service", component_key(server_id))
    }

    fn unit_path(server_id: &str) -> Result<PathBuf> {
        let home = home_dir()?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        Ok(config_home.join("systemd/user").join(unit_name(server_id)))
    }

    fn wants_path(server_id: &str) -> Result<PathBuf> {
        let unit_path = unit_path(server_id)?;
        let parent = unit_path
            .parent()
            .context("systemd user unit path has no parent")?;
        Ok(parent
            .join("default.target.wants")
            .join(unit_name(server_id)))
    }

    pub fn transition(previous: Option<&ServiceConfig>, next: &ServiceConfig) -> Result<()> {
        let Some(previous) = previous else {
            return Ok(());
        };
        if previous.service_manager == next.service_manager {
            return Ok(());
        }
        match (previous.service_manager, next.service_manager) {
            (ServiceManager::SystemdUser, ServiceManager::Foreground | ServiceManager::Nohup) => {
                cleanup_systemd_registration(
                    &unit_path(&previous.server_id)?,
                    &wants_path(&previous.server_id)?,
                    &unit_name(&previous.server_id),
                    host_is_running(&previous.host_socket),
                )
            }
            (ServiceManager::Foreground, ServiceManager::SystemdUser) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to systemd; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::SystemdUser) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (ServiceManager::Foreground, ServiceManager::Nohup) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to nohup; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::Foreground) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (_, ServiceManager::Launchd) | (ServiceManager::Launchd, _) => {
                bail!("launchd service configuration cannot run on Linux")
            }
            _ => Ok(()),
        }
    }

    fn cleanup_systemd_registration(
        unit_path: &Path,
        wants_path: &Path,
        unit: &str,
        host_running: bool,
    ) -> Result<()> {
        cleanup_systemd_registration_with(
            env::var_os("TREER_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()),
            unit_path,
            wants_path,
            unit,
            host_running,
        )
    }

    pub(super) fn cleanup_systemd_registration_with(
        executable: impl AsRef<OsStr>,
        unit_path: &Path,
        wants_path: &Path,
        unit: &str,
        host_running: bool,
    ) -> Result<()> {
        let disable = run_checked(
            Command::new(executable.as_ref()).args(["--user", "disable", "--now", unit]),
            "systemctl --user disable --now",
        );
        if host_running {
            disable.context(
                "the existing systemd Host is still running, so Treer cannot safely switch supervision modes",
            )?;
        } else if let Err(error) = disable {
            eprintln!(
                "treer: warning: could not disable the inactive systemd unit ({error:#}); removing its files directly"
            );
        }
        remove_if_exists(wants_path)?;
        remove_if_exists(unit_path)?;
        if let Err(error) = run_checked(
            Command::new(executable.as_ref()).args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        ) {
            eprintln!(
                "treer: warning: systemd daemon-reload remains unavailable after removing the stale unit: {error:#}"
            );
        }
        Ok(())
    }

    pub fn register(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Nohup | ServiceManager::Foreground => return Ok(()),
            ServiceManager::SystemdUser => {}
            ServiceManager::Launchd => bail!("launchd service configuration cannot run on Linux"),
        }
        register_systemd_with(
            env::var_os("TREER_SYSTEMCTL").unwrap_or_else(|| "systemctl".into()),
            paths,
            config,
            &unit_path(&config.server_id)?,
        )
    }

    pub(super) fn register_systemd_with(
        executable: impl AsRef<OsStr>,
        paths: &ServicePaths,
        config: &ServiceConfig,
        unit_path: &Path,
    ) -> Result<()> {
        let parent = unit_path
            .parent()
            .context("systemd user unit path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        write_atomic(
            unit_path,
            systemd_unit(
                &paths.host_executable,
                &paths.host_config,
                &config.workspace,
                &config.install_hostname,
            )
            .as_bytes(),
        )?;
        run_checked(
            Command::new(executable.as_ref()).args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )?;
        let unit = unit_name(&config.server_id);
        run_checked(
            Command::new(executable.as_ref()).args(["--user", "enable", unit.as_str()]),
            "systemctl --user enable",
        )?;
        warn_if_linger_disabled();
        Ok(())
    }

    pub fn start(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return start_nohup(paths, config);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "start", unit.as_str()]),
            "systemctl --user start",
        )
    }

    pub fn stop(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, true);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "stop", unit.as_str()]),
            "systemctl --user stop",
        )
    }

    pub fn stop_remotely(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, false);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "--no-block", "stop", unit.as_str()]),
            "systemctl --user --no-block stop",
        )
    }

    pub fn restart(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            return start_nohup(paths, config);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "restart", unit.as_str()]),
            "systemctl --user restart",
        )
    }

    pub fn status(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return status_nohup(paths, config);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
        run_checked(
            systemctl_command().args(["--user", "status", "--no-pager", unit.as_str()]),
            "systemctl --user status",
        )
    }

    pub fn logs(
        paths: &ServicePaths,
        config: &ServiceConfig,
        lines: usize,
        follow: bool,
    ) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return logs_nohup(paths, config, lines, follow);
        }
        require_systemd(config)?;
        let unit = unit_name(&config.server_id);
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

    pub fn uninstall(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        let unit = unit_name(&config.server_id);
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            remove_if_exists(&wants_path(&config.server_id)?)?;
            remove_if_exists(&unit_path(&config.server_id)?)?;
            return Ok(());
        }
        if config.service_manager == ServiceManager::Foreground {
            remove_if_exists(&wants_path(&config.server_id)?)?;
            remove_if_exists(&unit_path(&config.server_id)?)?;
            return Ok(());
        }
        require_systemd(config)?;
        let _ = systemctl_command()
            .args(["--user", "disable", "--now", unit.as_str()])
            .status();
        remove_if_exists(&unit_path(&config.server_id)?)?;
        run_checked(
            systemctl_command().args(["--user", "daemon-reload"]),
            "systemctl --user daemon-reload",
        )
    }

    fn require_systemd(config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::SystemdUser => Ok(()),
            ServiceManager::Nohup => bail!("this service uses nohup mode"),
            ServiceManager::Foreground => bail!(
                "this service uses foreground mode; run `treer-agent-server service --workspace {} start` in a terminal or process supervisor",
                config.workspace
            ),
            ServiceManager::Launchd => bail!("launchd service configuration cannot run on Linux"),
        }
    }

    fn warn_if_linger_disabled() {
        let Some(user) = env::var_os("USER") else {
            eprintln!("treer: warning: USER is unset; could not check systemd linger");
            return;
        };
        let output = Command::new("loginctl")
            .args([
                "show-user",
                user.to_string_lossy().as_ref(),
                "-p",
                "Linger",
                "--value",
            ])
            .output();
        if !matches!(output, Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "yes")
        {
            eprintln!(
                "treer: warning: systemd linger is disabled; an administrator can run `loginctl enable-linger {}` to keep this service running after the last login session exits, or run the Controller in a fixed-host tmux session",
                user.to_string_lossy()
            );
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) mod platform {
    use super::*;

    fn label(server_id: &str) -> String {
        format!("dev.treer.agent-server.{}", component_key(server_id))
    }

    fn plist_path(server_id: &str) -> Result<PathBuf> {
        Ok(home_dir()?
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", label(server_id))))
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

    fn service_target(server_id: &str) -> Result<String> {
        Ok(format!("{}/{}", domain()?, label(server_id)))
    }

    pub fn transition(previous: Option<&ServiceConfig>, next: &ServiceConfig) -> Result<()> {
        let Some(previous) = previous else {
            return Ok(());
        };
        if previous.service_manager == next.service_manager {
            return Ok(());
        }
        match (previous.service_manager, next.service_manager) {
            (ServiceManager::Launchd, ServiceManager::Foreground | ServiceManager::Nohup) => {
                let target = service_target(&previous.server_id)?;
                let bootout = run_checked(
                    Command::new("launchctl").args(["bootout", target.as_str()]),
                    "launchctl bootout",
                );
                if host_is_running(&previous.host_socket) {
                    bootout.context(
                        "the existing LaunchAgent Host is still running, so Treer cannot safely switch supervision modes",
                    )?;
                }
                remove_if_exists(&plist_path(&previous.server_id)?)
            }
            (ServiceManager::Foreground, ServiceManager::Launchd) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to launchd; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::Launchd) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (ServiceManager::Foreground, ServiceManager::Nohup) => {
                if host_is_running(&previous.host_socket) {
                    bail!(
                        "cannot switch a running foreground Host to nohup; stop the foreground command with Ctrl-C, then run repair again"
                    );
                }
                Ok(())
            }
            (ServiceManager::Nohup, ServiceManager::Foreground) => {
                let paths = ServicePaths::new(&previous.server_id)?;
                stop_nohup(&paths, previous, true)
            }
            (_, ServiceManager::SystemdUser) | (ServiceManager::SystemdUser, _) => {
                bail!("systemd user service configuration cannot run on macOS")
            }
            _ => Ok(()),
        }
    }

    pub fn register(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Nohup | ServiceManager::Foreground => return Ok(()),
            ServiceManager::Launchd => {}
            ServiceManager::SystemdUser => {
                bail!("systemd user service configuration cannot run on macOS")
            }
        }
        let plist_path = plist_path(&config.server_id)?;
        let parent = plist_path
            .parent()
            .context("LaunchAgent path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let log_path = service_log_path(paths, config);
        write_atomic(
            &plist_path,
            launchd_plist(
                &paths.host_executable,
                &paths.host_config,
                &label(&config.server_id),
                &log_path,
                &log_path,
            )
            .as_bytes(),
        )?;
        Ok(())
    }

    pub fn start(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return start_nohup(paths, config);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let loaded = Command::new("launchctl")
            .args(["print", target.as_str()])
            .status()
            .context("failed to query LaunchAgent")?
            .success();
        let domain = domain()?;
        let plist = plist_path(&config.server_id)?;
        for args in launchd_start_steps(loaded, &domain, &target, &plist) {
            let description = format!(
                "launchctl {}",
                args.first().map(String::as_str).unwrap_or_default()
            );
            run_checked(Command::new("launchctl").args(&args), &description)?;
        }
        Ok(())
    }

    pub fn stop(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, true);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        run_checked(
            Command::new("launchctl").args(["bootout", target.as_str()]),
            "launchctl bootout",
        )
    }

    pub fn stop_remotely(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return stop_nohup(paths, config, false);
        }
        require_launchd(config)?;
        stop(paths, config)
    }

    pub fn restart(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            return start_nohup(paths, config);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        start(paths, config)
    }

    pub fn status(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return status_nohup(paths, config);
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        run_checked(
            Command::new("launchctl").args(["print", target.as_str()]),
            "launchctl print",
        )
    }

    pub fn logs(
        paths: &ServicePaths,
        config: &ServiceConfig,
        lines: usize,
        follow: bool,
    ) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            return logs_nohup(paths, config, lines, follow);
        }
        require_launchd(config)?;
        let log_path = service_log_path(paths, config);
        let mut command = Command::new("tail");
        command.args(["-n", &lines.to_string()]);
        if follow {
            command.arg("-f");
        }
        command.arg(log_path);
        run_checked(&mut command, "tail")
    }

    pub fn uninstall(paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Nohup {
            stop_nohup(paths, config, true)?;
            remove_if_exists(&plist_path(&config.server_id)?)?;
            return Ok(());
        }
        if config.service_manager == ServiceManager::Foreground {
            remove_if_exists(&plist_path(&config.server_id)?)?;
            return Ok(());
        }
        require_launchd(config)?;
        let target = service_target(&config.server_id)?;
        let _ = Command::new("launchctl")
            .args(["bootout", target.as_str()])
            .status();
        remove_if_exists(&plist_path(&config.server_id)?)
    }

    fn require_launchd(config: &ServiceConfig) -> Result<()> {
        match config.service_manager {
            ServiceManager::Launchd => Ok(()),
            ServiceManager::Nohup => bail!("this service uses nohup mode"),
            ServiceManager::Foreground => bail!(
                "this service uses foreground mode; run `treer-agent-server service --workspace {} start` in a terminal or process supervisor",
                config.workspace
            ),
            ServiceManager::SystemdUser => {
                bail!("systemd user service configuration cannot run on macOS")
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) mod platform {
    use super::*;

    fn unsupported() -> Result<()> {
        bail!("service management is currently supported on Linux and macOS")
    }

    pub fn transition(previous: Option<&ServiceConfig>, next: &ServiceConfig) -> Result<()> {
        if previous.is_none_or(|previous| previous.service_manager == next.service_manager)
            || next.service_manager == ServiceManager::Foreground
        {
            Ok(())
        } else {
            unsupported()
        }
    }

    pub fn register(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Foreground {
            Ok(())
        } else {
            unsupported()
        }
    }

    pub fn start(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn stop(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn stop_remotely(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn restart(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn status(_paths: &ServicePaths, _config: &ServiceConfig) -> Result<()> {
        unsupported()
    }

    pub fn logs(
        _paths: &ServicePaths,
        _config: &ServiceConfig,
        _lines: usize,
        _follow: bool,
    ) -> Result<()> {
        unsupported()
    }

    pub fn uninstall(_paths: &ServicePaths, config: &ServiceConfig) -> Result<()> {
        if config.service_manager == ServiceManager::Foreground {
            Ok(())
        } else {
            unsupported()
        }
    }
}
