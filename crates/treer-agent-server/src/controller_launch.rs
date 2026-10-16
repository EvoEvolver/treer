use super::*;

pub(super) fn resolve_launch(
    request: &CreateAgentRequest,
) -> Result<(String, AgentLaunch), ProtocolError> {
    if !matches!(
        request.kind.as_str(),
        "shell" | "command" | "app" | "bridge"
    ) {
        return Err(ProtocolError::new(
            "agent_kind_retired",
            "Provider-specific and auto Agent kinds are retired; select a launch profile or pass kind=command with an explicit executable and argv",
        ));
    }
    validate_publish_ports(&request.publish_ports)?;
    let (command, args) = request.args.split_first().map_or_else(
        || (interactive_shell(), vec!["-i".to_string()]),
        |(command, args)| (command.clone(), args.to_vec()),
    );
    if command.trim().is_empty() {
        return Err(ProtocolError::new(
            "invalid_request",
            "Agent executable must not be empty",
        ));
    }
    Ok((
        request.kind.clone(),
        AgentLaunch {
            command,
            args,
            initial_writes: Vec::new(),
            publish_ports: Vec::new(),
        },
    ))
}

pub(super) fn validate_publish_ports(ports: &[u16]) -> Result<Vec<u16>, ProtocolError> {
    if !ports.is_empty() {
        return Err(ProtocolError::new(
            "sandbox_ports_retired",
            "Namespace port publication is retired; bind a Controller-reachable port and register it with treer service register",
        ));
    }
    Ok(Vec::new())
}

pub(super) fn join_agent_path(treer_binary: Option<&std::path::Path>) -> String {
    let mut paths = Vec::new();
    if let Some(parent) = treer_binary.and_then(std::path::Path::parent) {
        paths.push(parent.to_path_buf());
    }
    if let Some(current_path) = std::env::var_os("PATH") {
        paths.extend(std::env::split_paths(&current_path));
    }
    let mut seen = std::collections::HashSet::new();
    paths.retain(|path| seen.insert(path.clone()));
    std::env::join_paths(paths)
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|_| std::env::var("PATH").unwrap_or_default())
}

// Resolve before crossing the Host boundary: portable-pty otherwise searches
// relative/empty PATH entries, and Windows can fall back to CreateProcessW's
// implicit search. Startup intent keeps the original command; every spawn uses
// this resolver against its final workload environment.
pub(super) fn resolve_agent_command(
    command: &str,
    environment: &BTreeMap<String, String>,
) -> Result<String, ProtocolError> {
    let requested = Path::new(command);
    if requested.is_absolute() {
        return Ok(command.to_string());
    }
    if command.is_empty()
        || command.contains(['/', '\\', ':'])
        || command == "."
        || command == ".."
        || command.chars().any(char::is_control)
        || command.ends_with(['.', ' '])
    {
        return Err(ProtocolError::new(
            "invalid_command_path",
            "use an absolute executable path or a bare executable name; relative command paths are not supported",
        ));
    }
    let path = environment.get("PATH").map(String::as_str).unwrap_or("");
    for directory in std::env::split_paths(path) {
        if !searchable_agent_path(&directory) {
            continue;
        }
        #[cfg(unix)]
        let candidates = vec![directory.join(command)];
        #[cfg(windows)]
        let candidates = match requested.extension().and_then(|value| value.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com") => {
                vec![directory.join(command)]
            }
            Some(_) => Vec::new(),
            None => vec![
                directory.join(format!("{command}.exe")),
                directory.join(format!("{command}.com")),
            ],
        };
        for candidate in candidates {
            let Ok(target) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if !searchable_agent_path(&target) {
                continue;
            }
            let Ok(metadata) = target.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            #[cfg(windows)]
            if !target
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com")
                })
            {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            // Validate the final target, but preserve the absolute PATH entry's
            // spelling: BusyBox applets and Python virtualenvs rely on the
            // symlink executable name. An absolute entry cannot invoke an OS
            // PATH/cwd fallback, even when the file is replaced after lookup.
            if let Some(candidate) = candidate.to_str() {
                return Ok(candidate.to_string());
            }
        }
    }
    Err(ProtocolError::new(
        "command_not_found",
        format!(
            "executable not found in absolute PATH directories: {}{}",
            command.chars().take(128).collect::<String>(),
            if command == "treer-agent" {
                "; install the agent launcher on this machine (treer-agent-server launcher install)"
            } else {
                ""
            }
        ),
    ))
}

fn searchable_agent_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    // Default search is drive-qualified only on Windows. Explicit absolute
    // commands retain their existing UNC/verbatim behavior.
    #[cfg(windows)]
    {
        matches!(
            path.components().next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))
        )
    }
    #[cfg(unix)]
    true
}

pub(super) fn should_replace_virtual_hosts(
    current: Option<&VirtualNetworkHostsSnapshot>,
    incoming: &VirtualNetworkHostsSnapshot,
) -> bool {
    current.is_none_or(|current| incoming.revision > current.revision)
}

#[cfg(test)]
mod command_resolution_tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("treer-command-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
        fn executable(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, b"fixture executable").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            path
        }
        fn environment(&self) -> BTreeMap<String, String> {
            BTreeMap::from([("PATH".into(), self.0.to_str().unwrap().into())])
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn bare_command_uses_absolute_path_and_never_a_system_search_fallback() {
        let fixture = Fixture::new();
        let name = if cfg!(windows) {
            "treer-agent.exe"
        } else {
            "treer-agent"
        };
        let expected = fixture.executable(name);
        assert_eq!(
            resolve_agent_command("treer-agent", &fixture.environment()).unwrap(),
            expected.to_str().unwrap()
        );
        // Even an installed executable is not an implicit search location.
        // No cwd is consulted when PATH is absent, empty, or wholly relative.
        for path in [None, Some(""), Some("."), Some(".."), Some("relative-bin")] {
            let environment = path.map_or_else(BTreeMap::new, |path| {
                BTreeMap::from([("PATH".into(), path.into())])
            });
            assert_eq!(
                resolve_agent_command("treer-agent", &environment)
                    .unwrap_err()
                    .code,
                "command_not_found"
            );
        }
    }

    #[test]
    fn empty_and_relative_path_entries_are_ignored_before_absolute_entries() {
        let fixture = Fixture::new();
        let expected = fixture.executable(if cfg!(windows) {
            "treer-agent.exe"
        } else {
            "treer-agent"
        });
        let path = std::env::join_paths([
            Path::new(""),
            Path::new("."),
            Path::new("relative"),
            &fixture.0,
        ])
        .unwrap();
        let environment = BTreeMap::from([("PATH".into(), path.to_str().unwrap().into())]);
        assert_eq!(
            resolve_agent_command("treer-agent", &environment).unwrap(),
            expected.to_str().unwrap()
        );
    }

    #[test]
    fn cwd_decoy_is_rejected_in_an_isolated_process() {
        let fixture = Fixture::new();
        fixture.executable(if cfg!(windows) {
            "treer-agent.exe"
        } else {
            "treer-agent"
        });
        std::fs::create_dir(fixture.0.join("relative")).unwrap();
        std::fs::copy(
            fixture.0.join(if cfg!(windows) {
                "treer-agent.exe"
            } else {
                "treer-agent"
            }),
            fixture.0.join("relative").join(if cfg!(windows) {
                "treer-agent.exe"
            } else {
                "treer-agent"
            }),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "controller::launch::command_resolution_tests::cwd_decoy_child",
                "--ignored",
                "--nocapture",
            ])
            .current_dir(&fixture.0)
            .env_clear()
            .envs(treer_platform::machine_exec_environment())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated cwd resolution regression failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    #[ignore = "isolated child launched by cwd_decoy_is_rejected_in_an_isolated_process"]
    fn cwd_decoy_child() {
        assert!(Path::new(if cfg!(windows) {
            "treer-agent.exe"
        } else {
            "treer-agent"
        })
        .is_file());
        for path in ["", ".", "relative"] {
            let environment = BTreeMap::from([("PATH".into(), path.into())]);
            assert_eq!(
                resolve_agent_command("treer-agent", &environment)
                    .unwrap_err()
                    .code,
                "command_not_found"
            );
        }
        assert_eq!(
            resolve_agent_command("treer-agent", &BTreeMap::new())
                .unwrap_err()
                .code,
            "command_not_found"
        );
    }

    #[test]
    fn relative_command_paths_are_rejected_and_absolute_commands_are_unchanged() {
        let fixture = Fixture::new();
        for command in [
            "./treer-agent",
            "../treer-agent",
            "sub/treer-agent",
            "sub\\treer-agent",
            "C:treer-agent",
            "treer-agent.",
            "treer-agent ",
            ".",
            "..",
        ] {
            assert_eq!(
                resolve_agent_command(command, &fixture.environment())
                    .unwrap_err()
                    .code,
                "invalid_command_path"
            );
        }
        let absolute = fixture.0.join("explicit-not-installed");
        assert_eq!(
            resolve_agent_command(absolute.to_str().unwrap(), &BTreeMap::new()).unwrap(),
            absolute.to_str().unwrap()
        );
    }

    #[test]
    fn missing_executable_error_does_not_expose_path_and_directories_are_not_commands() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.0.join(if cfg!(windows) {
            "treer-agent.exe"
        } else {
            "treer-agent"
        }))
        .unwrap();
        let error = resolve_agent_command("treer-agent", &fixture.environment()).unwrap_err();
        assert_eq!(error.code, "command_not_found");
        assert!(!error.message.contains(fixture.0.to_str().unwrap()));
        assert!(error.message.contains("treer-agent"));
        assert!(error
            .message
            .contains("treer-agent-server launcher install"));
        assert!(
            !resolve_agent_command("missing-harness", &fixture.environment())
                .unwrap_err()
                .message
                .contains("launcher install")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_requires_executable_target_and_preserves_absolute_symlink_entry() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let fixture = Fixture::new();
        let target = fixture.executable("actual");
        let entry = fixture.0.join("treer-agent");
        symlink(&target, &entry).unwrap();
        assert_eq!(
            resolve_agent_command("treer-agent", &fixture.environment()).unwrap(),
            entry.to_str().unwrap()
        );
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            resolve_agent_command("treer-agent", &fixture.environment())
                .unwrap_err()
                .code,
            "command_not_found"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_command_resolution_regression_matrix() {
        // The release job selects `windows_`; execute the shared regression
        // cases there too, including the isolated cwd-decoy child.
        bare_command_uses_absolute_path_and_never_a_system_search_fallback();
        empty_and_relative_path_entries_are_ignored_before_absolute_entries();
        cwd_decoy_is_rejected_in_an_isolated_process();
        relative_command_paths_are_rejected_and_absolute_commands_are_unchanged();
        missing_executable_error_does_not_expose_path_and_directories_are_not_commands();
    }

    #[cfg(windows)]
    #[test]
    fn windows_extensions_are_fixed_and_unc_device_drive_relative_search_is_rejected() {
        let fixture = Fixture::new();
        fixture.executable("treer-agent.cmd");
        fixture.executable("treer-agent.bat");
        let mut environment = fixture.environment();
        environment.insert("PATHEXT".into(), ".CMD;.BAT;.EXE".into());
        for command in ["treer-agent", "treer-agent.cmd", "treer-agent.bat"] {
            assert_eq!(
                resolve_agent_command(command, &environment)
                    .unwrap_err()
                    .code,
                "command_not_found"
            );
        }
        let expected = fixture.executable("treer-agent.com");
        assert_eq!(
            resolve_agent_command("treer-agent", &environment).unwrap(),
            expected.to_str().unwrap()
        );
        for path in [
            r"C:relative",
            r"\rooted",
            r"\\server\share",
            r"\\?\UNC\server\share",
            r"\\.\pipe\fixture",
        ] {
            assert!(
                !searchable_agent_path(Path::new(path)),
                "unexpected search path class"
            );
        }
        assert!(searchable_agent_path(Path::new(r"C:\absolute")));
        assert!(searchable_agent_path(Path::new(r"\\?\C:\absolute")));
    }
}
