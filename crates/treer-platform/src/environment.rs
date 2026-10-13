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
                | "PSMODULEPATH"
        )
}

pub fn machine_exec_environment() -> BTreeMap<OsString, OsString> {
    let inherited = std::env::vars_os()
        .filter(|(key, _)| machine_exec_variable_allowed(&key.to_string_lossy()))
        .collect();
    #[cfg(windows)]
    {
        let system = super::windows::system_windows_directory();
        let environment =
            windows_exec_environment(inherited, super::windows::current_user_environment());
        return add_windows_system_modules(environment, system);
    }
    #[cfg(not(windows))]
    inherited
}

#[cfg(any(windows, test))]
fn windows_exec_environment(
    inherited: BTreeMap<OsString, OsString>,
    os_environment: std::io::Result<BTreeMap<OsString, OsString>>,
) -> BTreeMap<OsString, OsString> {
    let normalize = |values: BTreeMap<OsString, OsString>| {
        values
            .into_iter()
            .filter_map(|(key, value)| {
                let name = key.to_string_lossy().to_ascii_uppercase();
                machine_exec_variable_allowed(&name).then(|| (OsString::from(name), value))
            })
            .collect::<BTreeMap<_, _>>()
    };
    let mut inherited = normalize(inherited);
    // Module search paths supplied by the invoking Agent/Controller are never
    // configuration for privileged exec. Only an OS block candidate survives.
    inherited.remove(&OsString::from("PSMODULEPATH"));
    let os_environment = match os_environment {
        Ok(environment) => environment,
        Err(error) => {
            // No credentials and no empty environment on API failure. Still remove
            // empty/relative PATH entries before falling back to the legacy source.
            let path = merge_exec_path(inherited.get(&OsString::from("PATH")), None);
            if path.is_empty() {
                inherited.remove(&OsString::from("PATH"));
            } else {
                inherited.insert("PATH".into(), path);
            }
            tracing::warn!(
                os_error = error.raw_os_error(),
                "Windows OS environment unavailable; using filtered fallback"
            );
            return inherited;
        }
    };
    let os_environment = normalize(os_environment);
    let path = merge_exec_path(
        inherited.get(&OsString::from("PATH")),
        os_environment.get(&OsString::from("PATH")),
    );
    // Profile identity must belong to the current process token. An unloaded
    // service profile must not acquire the invoking Agent's profile by fallback.
    for name in [
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
        "USERNAME",
        "USERDOMAIN",
        "USERDOMAIN_ROAMINGPROFILE",
    ] {
        inherited.remove(&OsString::from(name));
    }
    let temporary: Vec<_> = ["TEMP", "TMP"]
        .into_iter()
        .filter_map(|name| {
            inherited
                .get(&OsString::from(name))
                .map(|value| (OsString::from(name), value.clone()))
        })
        .collect();
    inherited.extend(os_environment);
    inherited.extend(temporary);
    if let Some(profile) = inherited.get(&OsString::from("USERPROFILE")).cloned() {
        inherited.insert("HOME".into(), profile);
    }
    if path.is_empty() {
        inherited.remove(&OsString::from("PATH"));
    } else {
        inherited.insert("PATH".into(), path);
    }
    inherited
}

#[cfg(any(windows, test))]
fn add_windows_system_modules(
    mut environment: BTreeMap<OsString, OsString>,
    system: std::io::Result<std::path::PathBuf>,
) -> BTreeMap<OsString, OsString> {
    let candidate = environment.remove(&OsString::from("PSMODULEPATH"));
    let system = match system {
        Ok(system) => system,
        Err(error) => {
            tracing::warn!(
                os_error = error.raw_os_error(),
                "Windows installation directory unavailable; module search path omitted"
            );
            return environment;
        }
    };
    let modules = system
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("Modules");
    // No user Documents/temporary/workspace path, UNC, relative path, or
    // reparse indirection can enter PowerShell's default module search roots.
    let trusted = canonical_module_directory(&modules);
    let selected = candidate.as_ref().filter(|value| value.len() <= 32_767).and_then(|value| {
        std::env::split_paths(value).take(16).find(|path| {
            #[cfg(windows)]
            if !matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_))) { return false; }
            path.is_absolute()
                && trusted.as_ref().is_some_and(|trusted| {
                    canonical_module_directory(path).as_ref() == Some(trusted)
                })
        }).map(|_| modules.clone())
    });
    if trusted.is_some() {
        environment.insert(
            "PSMODULEPATH".into(),
            selected.unwrap_or(modules).into_os_string(),
        );
    } else {
        tracing::warn!("Windows system module directory unavailable; module search path omitted");
    }
    environment.insert("SYSTEMROOT".into(), system.clone().into_os_string());
    environment.insert("WINDIR".into(), system.clone().into_os_string());
    // An OS-resolved fallback ensures Windows utilities remain discoverable
    // even when the filtered caller and registry block have no useful PATH.
    if environment
        .get(&OsString::from("PATH"))
        .is_none_or(|value| value.is_empty())
    {
        environment.insert("PATH".into(), system.join("System32").into_os_string());
    }
    environment
}

#[cfg(any(windows, test))]
fn canonical_module_directory(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut prefix = std::path::PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return None;
        }
        prefix.push(component.as_os_str());
        #[cfg(windows)]
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&prefix).ok()?;
        if metadata.file_type().is_symlink() {
            return None;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return None;
            }
        }
    }
    path.is_dir()
        .then(|| std::fs::canonicalize(path).ok())
        .flatten()
}

#[cfg(any(windows, test))]
fn merge_exec_path(caller: Option<&OsString>, os: Option<&OsString>) -> OsString {
    let mut seen = std::collections::BTreeSet::new();
    let paths = [caller, os]
        .into_iter()
        .flatten()
        .flat_map(std::env::split_paths)
        .filter(|path| path.is_absolute())
        .filter(|path| std::env::join_paths([path]).is_ok())
        .filter(|path| {
            seen.insert(
                path.to_string_lossy()
                    .replace('\\', "/")
                    .to_ascii_lowercase(),
            )
        });
    std::env::join_paths(paths).unwrap_or_default()
}

#[cfg(any(windows, test))]
pub(crate) fn parse_windows_environment_block(
    block: &[u16],
) -> std::io::Result<BTreeMap<OsString, OsString>> {
    let invalid = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid OS environment block",
        )
    };
    let mut environment = BTreeMap::new();
    let mut offset = 0;
    while offset < block.len() {
        let end = block[offset..]
            .iter()
            .position(|value| *value == 0)
            .map(|length| offset + length)
            .ok_or_else(invalid)?;
        if end == offset {
            if offset == 0 && block.get(1) != Some(&0) {
                return Err(invalid());
            }
            return Ok(environment);
        }
        let entry = String::from_utf16(&block[offset..end]).map_err(|_| invalid())?;
        // Win32 drive current-directory entries (=C:=...) aren't variables.
        if !entry.starts_with('=') {
            let (name, value) = entry.split_once('=').ok_or_else(invalid)?;
            let name = name.to_ascii_uppercase();
            if machine_exec_variable_allowed(&name) {
                environment.insert(name.into(), value.into());
            }
        }
        offset = end + 1;
    }
    Err(invalid())
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
    fn os_environment_block_is_filtered_case_insensitively_and_requires_termination() {
        let block: Vec<u16> = "SystemRoot=synthetic-system\0USERPROFILE=synthetic-profile\0userprofile=synthetic-final-profile\0TREER_X=synthetic-secret\0TREER_WORKLOAD_CREDENTIAL=synthetic-secret\0NODE_OPTIONS=synthetic-hook\0HTTPS_PROXY=synthetic-proxy\0AGENT_UI_INSTANCE_ID=synthetic-id\0=C:=synthetic-drive\0\0".encode_utf16().collect();
        let parsed = parse_windows_environment_block(&block).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed.get(&OsString::from("USERPROFILE")),
            Some(&OsString::from("synthetic-final-profile"))
        );
        assert!(parse_windows_environment_block(&[0]).is_err());
        assert!(
            parse_windows_environment_block(&"PATH=x\0".encode_utf16().collect::<Vec<_>>())
                .is_err()
        );
        assert!(parse_windows_environment_block(&[0, 0]).unwrap().is_empty());
    }

    #[test]
    fn windows_exec_profile_comes_from_token_and_fallback_remains_filtered() {
        let inherited = BTreeMap::from([
            ("userprofile".into(), "synthetic-caller-profile".into()),
            ("APPDATA".into(), "synthetic-caller-appdata".into()),
            ("TREER_X".into(), "synthetic-secret".into()),
            ("NODE_OPTIONS".into(), "synthetic-hook".into()),
            (
                "PATH".into(),
                std::env::join_paths([
                    std::path::Path::new("relative"),
                    std::env::temp_dir().as_path(),
                ])
                .unwrap(),
            ),
        ]);
        let os = BTreeMap::from([
            ("USERPROFILE".into(), "synthetic-service-profile".into()),
            ("TREER_X".into(), "synthetic-registry-secret".into()),
        ]);
        let clean = windows_exec_environment(inherited.clone(), Ok(os));
        assert_eq!(
            clean.get(&OsString::from("USERPROFILE")),
            Some(&OsString::from("synthetic-service-profile"))
        );
        assert_eq!(
            clean.get(&OsString::from("HOME")),
            Some(&OsString::from("synthetic-service-profile"))
        );
        assert!(
            !clean.contains_key(&OsString::from("APPDATA")),
            "an unloaded profile must not borrow caller appdata"
        );
        assert!(!clean.contains_key(&OsString::from("TREER_X")));
        assert!(!clean.contains_key(&OsString::from("NODE_OPTIONS")));
        let fallback = windows_exec_environment(
            inherited,
            Err(std::io::Error::other("synthetic API failure")),
        );
        assert_eq!(
            fallback.get(&OsString::from("USERPROFILE")),
            Some(&OsString::from("synthetic-caller-profile"))
        );
        assert!(!fallback.contains_key(&OsString::from("TREER_X")));
        assert!(!fallback.contains_key(&OsString::from("NODE_OPTIONS")));
        assert!(std::env::split_paths(&fallback[&OsString::from("PATH")])
            .all(|path| path.is_absolute()));
    }

    #[test]
    fn windows_modules_drop_caller_and_user_paths_and_use_only_os_system_directory() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("trr-module-test-{}", std::process::id()));
        let system = root.join("synthetic-system");
        let modules = system.join("System32/WindowsPowerShell/v1.0/Modules");
        std::fs::create_dir_all(&modules).unwrap();
        let inherited = BTreeMap::from([
            (
                "PSModulePath".into(),
                root.join("synthetic-user-writable-modules")
                    .into_os_string(),
            ),
            (
                "SystemRoot".into(),
                root.join("synthetic-false-systemroot").into_os_string(),
            ),
        ]);
        let baseline = windows_exec_environment(inherited, Ok(BTreeMap::new()));
        assert!(!baseline.contains_key(&OsString::from("PSMODULEPATH")));
        let clean = add_windows_system_modules(baseline, Ok(system.clone()));
        assert_eq!(
            clean[&OsString::from("PSMODULEPATH")],
            modules.clone().into_os_string()
        );
        assert_eq!(
            clean[&OsString::from("PATH")],
            system.join("System32").into_os_string()
        );
        assert_eq!(
            clean[&OsString::from("SYSTEMROOT")],
            system.clone().into_os_string()
        );
        let candidate = std::env::join_paths([root.join("user-modules"), modules.clone()]).unwrap();
        let os = BTreeMap::from([("PSModulePath".into(), candidate)]);
        let clean = add_windows_system_modules(
            windows_exec_environment(BTreeMap::new(), Ok(os)),
            Ok(system.clone()),
        );
        assert_eq!(
            clean[&OsString::from("PSMODULEPATH")],
            modules.clone().into_os_string()
        );
        let fallback = add_windows_system_modules(
            windows_exec_environment(
                BTreeMap::from([("PSModulePath".into(), "synthetic-caller-modules".into())]),
                Err(std::io::Error::from_raw_os_error(5)),
            ),
            Ok(system),
        );
        assert_eq!(
            fallback[&OsString::from("PSMODULEPATH")],
            modules.into_os_string()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn windows_module_validation_rejects_reparse_style_indirection() {
        let root = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("trr-module-link-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("user-writable")).unwrap();
        std::os::unix::fs::symlink(root.join("user-writable"), root.join("alias")).unwrap();
        assert!(canonical_module_directory(&root.join("alias")).is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exec_path_merge_keeps_absolute_tools_deduplicates_and_drops_cwd_search() {
        let root = std::env::temp_dir();
        let tools = root.join("synthetic-tools");
        let system = root.join("synthetic-system");
        let caller = std::env::join_paths([
            tools.as_path(),
            std::path::Path::new(""),
            std::path::Path::new("."),
            std::path::Path::new("relative"),
        ])
        .unwrap();
        let os = std::env::join_paths([tools.as_path(), system.as_path()]).unwrap();
        assert_eq!(
            std::env::split_paths(&merge_exec_path(Some(&caller), Some(&os))).collect::<Vec<_>>(),
            vec![tools, system]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_exec_uses_service_account_profile_not_polluted_parent_profile() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "environment::tests::windows_os_environment_profile_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("USERPROFILE", "synthetic-caller-profile")
            .env("APPDATA", "synthetic-caller-profile")
            .env("LOCALAPPDATA", "synthetic-caller-profile")
            .env("TREER_X", "synthetic-not-a-real-credential")
            .env("NODE_OPTIONS", "synthetic-not-a-real-hook")
            .env("PSModulePath", "synthetic-caller-module-path")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "OS environment fixture failed; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "isolated subprocess launched by the polluted-profile regression"]
    fn windows_os_environment_profile_fixture() {
        let os = super::super::windows::current_user_environment().unwrap();
        let environment = machine_exec_environment();
        for name in ["USERPROFILE", "APPDATA", "LOCALAPPDATA"] {
            let key = OsString::from(name);
            assert!(
                environment.get(&key) == os.get(&key),
                "profile source mismatch for {name}"
            );
            assert!(
                environment.get(&key) != Some(&OsString::from("synthetic-caller-profile")),
                "caller profile retained for {name}"
            );
        }
        assert!(environment
            .keys()
            .all(|key| machine_exec_variable_allowed(&key.to_string_lossy())));
        let modules = environment
            .get(&OsString::from("PSMODULEPATH"))
            .expect("missing system PowerShell modules");
        assert!(modules != &OsString::from("synthetic-caller-module-path"));
        let expected = super::super::windows::system_windows_directory()
            .unwrap()
            .join("System32/WindowsPowerShell/v1.0/Modules");
        assert!(
            canonical_module_directory(std::path::Path::new(modules))
                == canonical_module_directory(&expected),
            "unexpected module search root"
        );
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
            "PSModulePath",
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
