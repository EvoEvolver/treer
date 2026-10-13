//! Shared service/workload inheritance boundary. Explicit launch environment is
//! applied *after* this filter; never log the removed values.
use std::{collections::BTreeMap, ffi::OsString};

pub fn inherited_variable_allowed(name: &str, value: &str) -> bool {
    // Windows environment names are case insensitive. Matching uniformly also
    // avoids forwarding alternate spellings through shells on Unix.
    let name = name.to_ascii_uppercase();
    if name.starts_with("AGENT_UI_") || name.starts_with("CODEX_UI_") {
        return false;
    }
    if matches!(
        name.as_str(),
        "TREER_AGENT_ID"
            | "TREER_WORKLOAD_CREDENTIAL"
            | "TREER_AGENT_SERVER_URL"
            | "TREER_WORKSPACE_ID"
            | "TREER_SERVER_ID"
            | "TREER_IDENTITY_MODE"
            | "TREER_HTTP_PROXY"
            | "TREER_BIN"
            | "TREER_MACHINE_TOKEN"
            | "TREER_OPERATOR_CREDENTIAL"
            | "TREER_ENROLLMENT_KEY"
    ) {
        return false;
    }
    if matches!(name.as_str(), "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY") {
        let value = value.trim();
        let parsed = if value.contains("://") {
            url::Url::parse(value)
        } else {
            url::Url::parse(&format!("http://{value}"))
        };
        if let Ok(url) = parsed {
            if let Some(host) = url.host_str() {
                let host = host.trim_matches(['[', ']']).trim_end_matches('.');
                return !host.eq_ignore_ascii_case("localhost")
                    && !host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback());
            }
        }
    }
    true
}

pub fn inherited_environment() -> BTreeMap<OsString, OsString> {
    std::env::vars_os()
        .filter(|(key, value)| {
            inherited_variable_allowed(&key.to_string_lossy(), &value.to_string_lossy())
        })
        .collect()
}

/// Machine exec is not a workload identity. Pass only the OS/session baseline,
/// never the Controller's service configuration, credentials or loader hooks.
/// Names are compared without case so mixed-case Windows variables survive.
/// Adding variables expands a security boundary and requires cross-review.
pub fn machine_exec_variable_allowed(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("LC_")
        || matches!(
            name.as_str(),
            "PATH"
                | "HOME"
                | "USER"
                | "LOGNAME"
                | "SHELL"
                | "LANG"
                | "TMPDIR"
                | "TEMP"
                | "TMP"
                | "TERM"
                | "XDG_CONFIG_HOME"
                | "XDG_CACHE_HOME"
                | "XDG_DATA_HOME"
                | "XDG_STATE_HOME"
                | "XDG_RUNTIME_DIR"
                | "SYSTEMROOT"
                | "WINDIR"
                | "SYSTEMDRIVE"
                | "COMSPEC"
                | "PATHEXT"
                | "USERPROFILE"
                | "HOMEDRIVE"
                | "HOMEPATH"
                | "USERNAME"
                | "USERDOMAIN"
                | "USERDOMAIN_ROAMINGPROFILE"
                | "APPDATA"
                | "LOCALAPPDATA"
                | "ALLUSERSPROFILE"
                | "PROGRAMDATA"
                | "PROGRAMFILES"
                | "PROGRAMFILES(X86)"
                | "PROGRAMW6432"
                | "COMMONPROGRAMFILES"
                | "COMMONPROGRAMFILES(X86)"
                | "COMMONPROGRAMW6432"
                | "COMPUTERNAME"
                | "OS"
                | "PROCESSOR_ARCHITECTURE"
                | "PROCESSOR_ARCHITEW6432"
                | "NUMBER_OF_PROCESSORS"
                | "PROCESSOR_LEVEL"
                | "PROCESSOR_REVISION"
        )
}

pub fn machine_exec_environment() -> BTreeMap<OsString, OsString> {
    let environment = std::env::vars_os()
        .filter(|(key, _)| machine_exec_variable_allowed(&key.to_string_lossy()))
        .collect();
    #[cfg(windows)]
    return windows_system_exec_environment(
        environment,
        super::windows::system_windows_directory(),
    );
    #[cfg(not(windows))]
    environment
}

/// No environment-block reading: fixed OS installation paths only. Caller
/// module paths are not an opt-in; arbitrary modules need a separate design.
#[cfg(any(windows, test))]
fn windows_system_exec_environment(
    mut environment: BTreeMap<OsString, OsString>,
    system: std::io::Result<std::path::PathBuf>,
) -> BTreeMap<OsString, OsString> {
    environment.retain(|name, _| !name.to_string_lossy().eq_ignore_ascii_case("PSMODULEPATH"));
    let system = match system {
        Ok(system) => system,
        Err(error) => {
            tracing::warn!(
                os_error = error.raw_os_error(),
                "Windows installation directory unavailable; module path omitted"
            );
            return environment;
        }
    };
    environment.retain(|name, _| {
        !matches!(
            name.to_string_lossy().to_ascii_uppercase().as_str(),
            "SYSTEMROOT" | "WINDIR"
        )
    });
    environment.insert("SystemRoot".into(), system.clone().into_os_string());
    environment.insert("windir".into(), system.clone().into_os_string());
    let modules = system
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("Modules");
    if system_module_directory_is_valid(&modules) {
        environment.insert("PSModulePath".into(), modules.into_os_string());
        tracing::debug!(
            source = "system_default",
            segments = 1,
            "Configured Windows PowerShell module search"
        );
    } else {
        tracing::warn!(
            source = "system_default",
            segments = 0,
            "Windows system module directory unavailable"
        );
    }
    environment
}

#[cfg(any(windows, test))]
fn system_module_directory_is_valid(path: &std::path::Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let mut prefix = std::path::PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return false;
        }
        prefix.push(component.as_os_str());
        #[cfg(windows)]
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&prefix) else {
            return false;
        };
        if metadata.file_type().is_symlink() {
            return false;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
    }
    path.is_dir()
}

/// Call only from synchronous binary entry points, before creating threads or
/// the Tokio runtime. Child launchers should use env_clear + inherited_environment.
pub fn clean_process_environment_before_threads() -> std::io::Result<()> {
    let clean = inherited_environment();
    if clean.len() == std::env::vars_os().count() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        // exec preserves the supervisor's PID and also removes identity from
        // the OS-visible initial environment (remove_var alone does not).
        use std::os::unix::process::CommandExt;
        Err(std::process::Command::new(std::env::current_exe()?)
            .args(std::env::args_os().skip(1))
            .env_clear()
            .envs(clean)
            .exec())
    }
    #[cfg(not(unix))]
    {
        for (key, _) in std::env::vars_os() {
            if !clean.contains_key(&key) {
                std::env::remove_var(key);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_system_modules_ignore_caller_paths_and_systemroot_overrides() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("trr-system-modules-{}", std::process::id()));
        let modules = root
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("Modules");
        std::fs::create_dir_all(&modules).unwrap();
        let source = BTreeMap::from([
            ("PSModulePath".into(), "synthetic-user-module-path".into()),
            (
                "psmodulepath".into(),
                "synthetic-workspace-module-path".into(),
            ),
            ("SYSTEMROOT".into(), "synthetic-caller-systemroot".into()),
        ]);
        let environment = windows_system_exec_environment(source, Ok(root.clone()));
        assert_eq!(
            environment[&OsString::from("PSModulePath")],
            modules.into_os_string()
        );
        assert_eq!(
            environment[&OsString::from("SystemRoot")],
            root.clone().into_os_string()
        );
        assert!(!environment.contains_key(&OsString::from("psmodulepath")));
        assert!(
            !machine_exec_variable_allowed("PSModulePath"),
            "caller module paths must remain outside inheritance allowlist"
        );
        let failed = windows_system_exec_environment(
            BTreeMap::from([("PSModulePath".into(), "synthetic-caller-modules".into())]),
            Err(std::io::Error::from_raw_os_error(5)),
        );
        assert!(!failed.contains_key(&OsString::from("PSModulePath")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn system_module_validation_rejects_symlink_indirection() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("trr-module-symlink-{}", std::process::id()));
        std::fs::create_dir_all(root.join("user-writable")).unwrap();
        std::os::unix::fs::symlink(root.join("user-writable"), root.join("alias")).unwrap();
        assert!(!system_module_directory_is_valid(&root.join("alias")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn machine_exec_allowlist_keeps_windows_baseline_and_drops_all_treer_identity() {
        for name in [
            "SystemRoot",
            "windir",
            "USERPROFILE",
            "HOMEDRIVE",
            "HOMEPATH",
            "USERNAME",
            "APPDATA",
            "LOCALAPPDATA",
            "ProgramData",
            "ProgramFiles",
            "ProgramFiles(x86)",
            "COMPUTERNAME",
            "ComSpec",
            "PATHEXT",
            "PATH",
            "HOME",
            "LC_MESSAGES",
        ] {
            assert!(
                machine_exec_variable_allowed(name),
                "missing OS baseline {name}"
            );
        }
        for name in [
            "TREER_MACHINE_TOKEN",
            "treer_operator_credential",
            "TREER_ENROLLMENT_KEY",
            "TREER_WORKLOAD_CREDENTIAL",
            "TREER_SERVER_ID",
            "TREER_WORKSPACE_ID",
            "TREER_PROXY_URL",
            "TREER_BIN",
            "AGENT_UI_SESSION_ID",
            "CODEX_UI_THREAD_ID",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NODE_OPTIONS",
            "NODE_PATH",
            "LD_PRELOAD",
            "DYLD_INSERT_LIBRARIES",
            "AWS_SECRET_ACCESS_KEY",
            "SSH_AUTH_SOCK",
            "PSModulePath",
        ] {
            assert!(
                !machine_exec_variable_allowed(name),
                "unexpected exec variable {name}"
            );
        }
    }

    #[test]
    fn strips_identity_ui_and_loopback_proxies_but_preserves_service_configuration() {
        for name in [
            "TREER_AGENT_ID",
            "TREER_WORKLOAD_CREDENTIAL",
            "TREER_AGENT_SERVER_URL",
            "TREER_WORKSPACE_ID",
            "TREER_SERVER_ID",
            "TREER_IDENTITY_MODE",
            "TREER_HTTP_PROXY",
            "TREER_BIN",
            "TREER_MACHINE_TOKEN",
            "TREER_OPERATOR_CREDENTIAL",
            "TREER_ENROLLMENT_KEY",
            "AGENT_UI_INSTANCE_ID",
            "AGENT_UI_SESSION_ID",
            "AGENT_UI_HARNESS",
            "CODEX_UI_THREAD_ID",
        ] {
            assert!(!inherited_variable_allowed(name, "fixture"), "{name}");
        }
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            for value in [
                "http://127.0.0.1:59060",
                "http://127.2.3.4:1",
                "localhost:80",
                "http://LOCALHOST.:80",
                "socks5://[::1]:80",
                "http://user:password@localhost:80",
            ] {
                assert!(!inherited_variable_allowed(name, value), "{name}: {value}");
            }
            assert!(inherited_variable_allowed(
                name,
                "https://proxy.example:443"
            ));
        }
        for name in [
            "HOME",
            "PATH",
            "USER",
            "LANG",
            "LC_MESSAGES",
            "TMPDIR",
            "SystemRoot",
            "TREER_STATE_DIR",
            "TREER_RUNTIME_DIR",
            "TREER_PROXY_URL",
            "TREER_WORKSPACE_ROOT",
            "TREER_AGENT_SERVER_LISTEN",
            "TREER_RELEASE_PUBLIC_KEY",
            "NO_PROXY",
        ] {
            assert!(inherited_variable_allowed(name, "fixture"), "{name}");
        }
    }
}
