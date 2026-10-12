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
