//! Refuse native Codex loads unless the adapter's writer is identified.
//! The deprecated Rust adapter embeds a different Codex core; the maintained
//! app-server adapter can be pinned to the exact native executable.
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use super::catalog::{resolve_executable, AcpAgentDef};

#[derive(Debug)]
pub struct UnsafeSessionLoad(pub String);

impl fmt::Display for UnsafeSessionLoad {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}
impl std::error::Error for UnsafeSessionLoad {}

pub fn adapter_package(command: &str) -> Option<(String, String)> {
    let executable = resolve_executable(command)?.canonicalize().ok()?;
    for parent in executable.ancestors().skip(1) {
        let file = parent.join("package.json");
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let Ok(package) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let name = package.get("name").and_then(Value::as_str)?;
        if matches!(
            name,
            "@zed-industries/codex-acp" | "@agentclientprotocol/codex-acp"
        ) {
            return Some((name.into(), package.get("version")?.as_str()?.into()));
        }
    }
    None
}

pub fn native_codex_path(def: &AcpAgentDef) -> Result<PathBuf> {
    // An override with arguments cannot be represented by CODEX_PATH. Refuse
    // rather than silently launching a different executable from PATH.
    if shell_words::split(&def.base_command)?.len() != 1 {
        return Err(anyhow!(
            "Codex --base-command must name one executable for a verified native load"
        ));
    }
    resolve_executable(&def.base_command)
        .ok_or_else(|| anyhow!("cannot resolve native Codex executable"))?
        .canonicalize()
        .context("resolve native Codex executable")
}

pub fn codex_writer_env(def: &AcpAgentDef) -> Result<Vec<(&'static str, String)>> {
    if def.id == "codex"
        && adapter_package(&def.server_command)
            .is_some_and(|(name, _)| name == "@agentclientprotocol/codex-acp")
    {
        return Ok(vec![(
            "CODEX_PATH",
            native_codex_path(def)?.to_string_lossy().into_owned(),
        )]);
    }
    Ok(Vec::new())
}

pub async fn check_codex_load(def: &AcpAgentDef, session_id: &str) -> Result<Value> {
    if def.id != "codex" {
        return Ok(json!({ "harness": def.id }));
    }
    let checked = async {
        let executable = native_codex_path(def)?;
        let output = tokio::time::timeout(Duration::from_secs(5),
            tokio::process::Command::new(&executable).arg("--version").kill_on_drop(true).output()).await??;
        if !output.status.success() { return Err(anyhow!("native codex --version failed")); }
        let text = String::from_utf8(output.stdout)?;
        let native_version = text.trim().strip_prefix("codex-cli ")
            .filter(|version| !version.is_empty()).ok_or_else(|| anyhow!("unrecognized native Codex version: {}", text.trim()))?;
        let (package, adapter_version) = adapter_package(&def.server_command)
            .ok_or_else(|| anyhow!("cannot identify adapter package/core for {}", def.server_command))?;
        let core_version = match package.as_str() {
            "@agentclientprotocol/codex-acp" => native_version,
            "@zed-industries/codex-acp" if adapter_version == "0.16.0" => "0.137.0",
            _ => return Err(anyhow!("unknown embedded Codex core in {package} {adapter_version}")),
        };
        if core_version != native_version {
            return Err(anyhow!("{package} {adapter_version} embeds Codex {core_version}, but native Codex is {native_version}; its writer can append incompatible records (missing ordinal)"));
        }
        Ok(json!({ "session_id": session_id, "adapter": package, "adapter_version": adapter_version,
            "native_codex_version": native_version, "writer_codex_version": core_version,
            "codex_path": executable }))
    }.await;
    checked.map_err(|error| UnsafeSessionLoad(format!(
        "Refusing native Codex session load {session_id}: {error}. Original rollout was not opened by the adapter. Use @agentclientprotocol/codex-acp with --base-command pointing to the native Codex executable (launcher pins CODEX_PATH); unknown/mismatched writers require an isolated copy, never the original store."
    )).into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn fixture(directory: &Path, package: &str, version: &str) -> AcpAgentDef {
        let adapter = directory.join("adapter");
        std::fs::create_dir_all(adapter.join("bin")).unwrap();
        std::fs::write(
            adapter.join("package.json"),
            json!({"name": package, "version": version}).to_string(),
        )
        .unwrap();
        let server = adapter.join("bin/codex-acp");
        std::fs::write(&server, "#!/bin/sh\ntouch adapter-started\nexit 99\n").unwrap();
        std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o700)).unwrap();
        let codex = directory.join("codex");
        std::fs::write(&codex, "#!/bin/sh\necho 'codex-cli 0.162.1'\n").unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
        AcpAgentDef {
            id: "codex".into(),
            display_name: "Codex".into(),
            description: String::new(),
            transport: "adapter".into(),
            base_command: shell_words::quote(&codex.to_string_lossy()).into_owned(),
            server_command: shell_words::quote(&server.to_string_lossy()).into_owned(),
            install_command: None,
            model_list_command: None,
        }
    }

    #[tokio::test]
    async fn old_embedded_writer_is_rejected_before_adapter_start() {
        let directory = tempfile::tempdir().unwrap();
        let def = fixture(directory.path(), "@zed-industries/codex-acp", "0.16.0");
        let error = check_codex_load(&def, "native").await.unwrap_err();
        assert!(error.is::<UnsafeSessionLoad>());
        assert!(error.to_string().contains("0.137.0"));
        assert!(error.to_string().contains("0.162.1"));
        let error = crate::serve(crate::AisConfig {
            agent_id: "test-guard".into(),
            cwd: directory.path().to_path_buf(),
            state_dir: directory.path().join("state"),
            port: 0,
            ui_dist: None,
            builtin_ui: false,
            harness: crate::HarnessSpec::Configured {
                name: "codex".into(),
                base_command: def.base_command,
                server_command: def.server_command,
            },
            bind_session_id: Some("native".into()),
            startup_timeout_ms: 8000,
            tower: None,
        })
        .await
        .err()
        .expect("unsafe load must fail startup, never start a fresh session");
        assert!(error.is::<UnsafeSessionLoad>());
        assert!(!directory.path().join("adapter-started").exists());
    }

    #[tokio::test]
    async fn maintained_adapter_is_pinned_to_the_verified_native_writer() {
        let directory = tempfile::tempdir().unwrap();
        let def = fixture(directory.path(), "@agentclientprotocol/codex-acp", "2.2.2");
        let info = check_codex_load(&def, "native").await.unwrap();
        assert_eq!(info["writer_codex_version"], "0.162.1");
        assert_eq!(codex_writer_env(&def).unwrap()[0].0, "CODEX_PATH");
        assert_eq!(
            PathBuf::from(&codex_writer_env(&def).unwrap()[0].1),
            native_codex_path(&def).unwrap()
        );
    }

    #[tokio::test]
    async fn unidentified_writer_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        let mut def = fixture(directory.path(), "unknown", "1");
        assert!(check_codex_load(&def, "native")
            .await
            .unwrap_err()
            .is::<UnsafeSessionLoad>());
        def.id = "claude".into();
        assert!(check_codex_load(&def, "native").await.is_ok());
    }
}
