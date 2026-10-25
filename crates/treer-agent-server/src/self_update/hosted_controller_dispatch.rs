//! Explicit private-config entry in dedicated acceptance images only.
//! Real ProxyClient/command handling and native worker; only artifact sourcing
//! is local/pinned. This is not production capability acceptance.
use anyhow::{bail, Result};
use serde::Deserialize;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
};
use treer_platform::private_directory::PrivateDirectory;
use treer_protocol::{controller_update as wire, controller_update_runtime::DispatchBinding};

static ROOT: OnceLock<PathBuf> = OnceLock::new();
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    current: wire::Identity,
    target: wire::Target,
    sums_sha256: String,
    host_sha256: String,
}
fn read<T: serde::de::DeserializeOwned>(root: &PrivateDirectory, name: &str) -> Result<T> {
    let mut bytes = Vec::new();
    root.read_file(name)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("hosted_dispatch_input_invalid");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn input(root: &PrivateDirectory) -> Result<Input> {
    let value: Input = read(root, "acceptance-input.json")?;
    value
        .current
        .validate()
        .map_err(|_| anyhow::anyhow!("hosted_dispatch_input_invalid"))?;
    value
        .target
        .validate()
        .map_err(|_| anyhow::anyhow!("hosted_dispatch_input_invalid"))?;
    if value.target.platform != wire::Platform::LinuxX86_64
        || !super::hex(&value.sums_sha256, 64)
        || !super::hex(&value.host_sha256, 64)
    {
        bail!("hosted_dispatch_input_invalid");
    }
    Ok(value)
}
fn validate_connection(value: &serde_json::Value, connection: &serde_json::Value) -> Result<()> {
    let proxy = url::Url::parse(
        connection["proxy"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("hosted_dispatch_proxy_invalid"))?,
    )?;
    if proxy.scheme() != "http"
        || proxy.host_str() != Some("127.0.0.1")
        || proxy.port().is_none()
        || !proxy.username().is_empty()
        || proxy.password().is_some()
        || proxy.query().is_some()
        || proxy.fragment().is_some()
        || proxy.path() != "/"
        || value["proxy"] != connection["proxy"]
        || value["machine_token"] != connection["machine_token"]
        || !connection["machine_token"]
            .as_str()
            .is_some_and(|s| s.starts_with("srv_native_acceptance.") && s.len() < 512)
    {
        bail!("hosted_dispatch_proxy_invalid");
    }
    Ok(())
}
pub(crate) fn configure(config: &Path) -> Result<()> {
    // The field is an explicit fixture selector, not an environment toggle.
    // Ordinary service configs, even in feature images, do not opt in.
    let mut bytes = Vec::new();
    std::fs::File::open(config)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("hosted_dispatch_config_invalid");
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if value.get("hosted_native_dispatch").is_none() {
        return Ok(());
    }
    if value["hosted_native_dispatch"] != true {
        bail!("hosted_dispatch_config_invalid");
    }
    let path = crate::self_update::hosted_acceptance::root()?;
    if !cfg!(target_os = "linux")
        || config != path.join("xdg/treer/agent-servers/srv_native_acceptance-controller.json")
        || value["workspace"] != "ws_native_acceptance"
        || value["server_id"] != "srv_native_acceptance"
        || value["root"] != path.join("workspace").to_string_lossy().as_ref()
        || value["host_socket"] != path.join("host.sock").to_string_lossy().as_ref()
    {
        bail!("hosted_dispatch_scope_invalid");
    }
    let root = PrivateDirectory::open(&path)?;
    let connection: serde_json::Value = read(&root, "controller-proxy-ready.json")?;
    validate_connection(&value, &connection)?;
    // Re-read through the retained private component chain, not a symlink.
    let saved = root
        .open_child("xdg")?
        .open_child("treer")?
        .open_child("agent-servers")?;
    let checked: serde_json::Value = read(&saved, "srv_native_acceptance-controller.json")?;
    if checked != value {
        bail!("hosted_dispatch_config_changed");
    }
    let current = std::env::current_exe()?.canonicalize()?;
    if current != path.join("bin/treer-agent-server") {
        bail!("hosted_dispatch_image_invalid");
    }
    let info = input(&root)?;
    let digest = crate::self_update::images::digest_file(
        &root.open_child("bin")?.read_file("treer-agent-server")?,
    )?;
    if digest != info.current.controller_sha256 && digest != info.target.identity.controller_sha256
    {
        bail!("hosted_dispatch_image_invalid");
    }
    ROOT.set(path)
        .map_err(|_| anyhow::anyhow!("hosted_dispatch_already_configured"))?;
    Ok(())
}
pub(crate) fn enabled() -> bool {
    ROOT.get().is_some()
}
fn validate(binding: &DispatchBinding, info: &Input) -> Result<()> {
    if !binding.validate()
        || binding.plan_digest != binding.expected_plan_digest()?
        || binding.workspace_id != "ws_native_acceptance"
        || binding.server_id != "srv_native_acceptance"
        || binding.catalog_revision != 1
        || binding.current != info.current
        || binding.target != info.target
    {
        bail!("hosted_dispatch_binding_invalid");
    }
    Ok(())
}
pub(crate) async fn prepare(
    binding: DispatchBinding,
) -> Result<crate::self_update::web_adapter::PreparedSubmission> {
    let root = PrivateDirectory::open(
        ROOT.get()
            .ok_or_else(|| anyhow::anyhow!("hosted_dispatch_not_configured"))?,
    )?;
    validate(&binding, &input(&root)?)?;
    // Exclusive once-only actual handler admission. Never used as a transport
    // or a trigger: the receiving Controller is already executing the command.
    let mut intent = match root.create_file("controller-dispatch-binding.json") {
        Ok(file) => file,
        Err(error) => {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                if let Ok(mut repeat) = root.create_file("controller-dispatch-repeat.json") {
                    repeat.write_all(b"{\"repeated_dispatch\":true}")?;
                    repeat.sync_all()?;
                    root.sync()?;
                }
            }
            return Err(error.into());
        }
    };
    intent.write_all(&serde_json::to_vec(&binding)?)?;
    intent.sync_all()?;
    root.sync()?;
    let diagnostic = crate::self_update::hosted_acceptance::diagnostics::Diagnostic::new(root);
    let artifacts = PrivateDirectory::open(ROOT.get().unwrap())?.open_child("artifacts")?;
    let result =
        crate::self_update::web_preparation::prepare_local(binding, artifacts, &diagnostic).await;
    if let Err(error) = &result {
        diagnostic.failed(error);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn connection_is_exact_private_loopback_fixture_only() {
        let good = serde_json::json!({"proxy":"http://127.0.0.1:12345","machine_token":"srv_native_acceptance.fixture"});
        validate_connection(&good, &good).unwrap();
        for proxy in [
            "https://remote.invalid/",
            "http://localhost:12345/",
            "http://127.0.0.1:12345/other",
            "http://user@127.0.0.1:12345/",
            "http://127.0.0.1:12345/?x=1",
            "http://127.0.0.1:12345/#x",
        ] {
            let mut bad = good.clone();
            bad["proxy"] = serde_json::json!(proxy);
            assert!(validate_connection(&bad, &bad).is_err());
        }
        let mut different = good.clone();
        different["machine_token"] = serde_json::json!("srv_other.fixture");
        assert!(validate_connection(&different, &good).is_err());
    }
    #[test]
    fn ordinary_feature_process_has_no_dispatch_permission() {
        assert!(!enabled());
        assert!(!crate::controller_update::capability_available());
    }
    #[test]
    fn no_selector_or_false_selector_cannot_enable_fixture() {
        let path =
            std::env::temp_dir().join(format!("treer-dispatch-selector-{}", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{}").unwrap();
        drop(file);
        configure(&path).unwrap();
        assert!(!enabled());
        std::fs::write(&path, b"{\"hosted_native_dispatch\":false}").unwrap();
        assert!(configure(&path).is_err());
        assert!(!enabled());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn binding_cannot_choose_scope_or_artifacts() {
        let id = |c: char| wire::Identity {
            release_sha: c.to_string().repeat(40),
            artifact_source_sha: c.to_string().repeat(40),
            controller_sha256: c.to_string().repeat(64),
            cli_sha256: c.to_string().repeat(64),
        };
        let info = Input {
            current: id('a'),
            target: wire::Target {
                target_id: "new".into(),
                identity: id('b'),
                platform: wire::Platform::LinuxX86_64,
                version_label: "new".into(),
            },
            sums_sha256: "c".repeat(64),
            host_sha256: "d".repeat(64),
        };
        let mut b = DispatchBinding {
            operation_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: "ws_native_acceptance".into(),
            server_id: "srv_native_acceptance".into(),
            current: info.current.clone(),
            target: info.target.clone(),
            catalog_revision: 1,
            plan_digest: String::new(),
            created_at: chrono::Utc::now(),
        };
        b.plan_digest = b.expected_plan_digest().unwrap();
        assert!(validate(&b, &info).is_ok());
        for field in ["workspace", "server", "target", "revision", "plan"] {
            let mut bad = b.clone();
            match field {
                "workspace" => bad.workspace_id = "other".into(),
                "server" => bad.server_id = "other".into(),
                "target" => bad.target.identity = id('c'),
                "revision" => bad.catalog_revision = 2,
                _ => bad.plan_digest = "0".repeat(64),
            }
            if field != "plan" {
                bad.plan_digest = bad.expected_plan_digest().unwrap();
            }
            assert!(validate(&bad, &info).is_err(), "{field}");
        }
    }
}
