//! Local, version-bound launcher distribution. Profile transactions live in the
//! embedded launcher's shared installer, not a second Rust implementation.
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

struct EmbeddedFile {
    path: &'static str,
    bytes: &'static [u8],
    sha256: &'static str,
    executable: bool,
}
include!(concat!(env!("OUT_DIR"), "/launcher_payload.rs"));

pub(crate) fn controller_contract() -> treer_protocol::LauncherControllerContract {
    treer_protocol::LauncherControllerContract {
        machine: None,
        minimum_launcher_security_epoch: Some(treer_protocol::MINIMUM_LAUNCHER_SECURITY_EPOCH),
        git_commit: treer_build_info::GIT_COMMIT.to_owned(),
        protocol: treer_protocol::AGENT_INTERFACE_PROTOCOL_V1.to_owned(),
        capabilities: treer_protocol::CONTROLLER_INTERFACE_CAPABILITIES
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
        features: vec![
            treer_protocol::CAPABILITY_LAUNCHER_INSTALL.to_owned(),
            treer_protocol::CAPABILITY_AGENT_INTERFACE_PROVENANCE.to_owned(),
            treer_protocol::CAPABILITY_AGENT_PROMPT_DELIVERY.to_owned(),
        ],
    }
}

#[derive(Debug, Args)]
pub struct LauncherArgs {
    /// Trusted absolute Node.js 22+ executable; never selected from arbitrary PATH entries.
    #[arg(long, global = true)]
    node: Option<PathBuf>,
    /// Explicit trusted treer CLI (defaults to the Controller's installed sibling).
    #[arg(long, global = true)]
    treer: Option<PathBuf>,
    #[command(subcommand)]
    command: LauncherCommand,
}

#[derive(Debug, Subcommand)]
enum LauncherCommand {
    /// Idempotently install this binary's launcher and save profiles, without starting Agents.
    Install(InstallArgs),
    /// Atomically activate this binary's launcher; running Agents keep their physical assets.
    Update(InstallArgs),
    /// Inspect current/previous versions and visible local Agents' startup version references.
    Status,
    /// Activate a retained compatible version, without restarting any Agent.
    Rollback { version: Option<String> },
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// Profile provider (repeatable); defaults to all six bundled profiles.
    #[arg(long = "agent", value_parser = ["codex", "claude", "gemini", "grok", "cursor", "opencode"])]
    agents: Vec<String>,
    /// Machine-relative working directory for saved profiles.
    #[arg(long)]
    cwd: Option<String>,
    /// Development source-only payload: explicitly allow lockfile npm downloads.
    #[arg(long)]
    allow_network: bool,
    /// Trusted absolute npm CLI JavaScript file; only used with --allow-network.
    #[arg(long, requires = "allow_network")]
    npm_cli: Option<PathBuf>,
}

fn is_safe_relative(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['\\', ':', '\0'])
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part.chars().any(char::is_control)
        })
        && Path::new(name)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn verify_payload(files: &[EmbeddedFile], digest: &str) -> Result<()> {
    let manifest: Value = serde_json::from_str(MANIFEST_JSON)?;
    let entries = manifest["files"]
        .as_array()
        .context("invalid embedded manifest")?;
    if entries.len() != files.len() {
        bail!("embedded launcher manifest file count mismatch");
    }
    let mut previous = "";
    for file in files {
        if !is_safe_relative(file.path) || previous >= file.path {
            bail!("invalid embedded launcher file order or path");
        }
        let actual = format!("{:x}", Sha256::digest(file.bytes));
        if actual != file.sha256 {
            bail!("embedded launcher hash mismatch");
        }
        if !entries.iter().any(|entry| {
            entry["path"] == file.path
                && entry["size"].as_u64() == Some(file.bytes.len() as u64)
                && entry["sha256"] == actual
                && entry["executable"] == file.executable
        }) {
            bail!("embedded launcher manifest entry mismatch");
        }
        previous = file.path;
    }
    if format!("{:x}", Sha256::digest(MANIFEST_JSON.as_bytes())) != digest {
        bail!("embedded launcher payload digest mismatch");
    }
    Ok(())
}

fn reject_link(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if metadata.file_type().is_symlink() {
        bail!(
            "launcher path must not be a symbolic link: {}",
            path.display()
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("launcher path must not be a reparse point");
        }
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            reject_link(path, &metadata)?;
            if !metadata.is_dir() {
                bail!("launcher directory is not a directory");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                // Existing private state owned by someone else is never adopted.
                if metadata.uid() != nix::unistd::geteuid().as_raw() {
                    bail!("launcher directory is not owned by the current user");
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)?;
        }
        Err(error) => return Err(error.into()),
    }
    treer_platform::secure_directory(path)?;
    Ok(())
}

struct Stage(PathBuf);
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cleanup_stale_payloads(parent: &Path) -> Result<()> {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|name| name.strip_prefix(".controller-payload-"))
        else {
            continue;
        };
        if uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if reject_link(&entry.path(), &metadata).is_err() || !metadata.is_dir() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != nix::unistd::geteuid().as_raw() {
                continue;
            }
        }
        // Every installer child is bounded to six minutes; one day cannot be an
        // active extraction. Private parent and UUID naming bound the cleanup.
        if metadata
            .modified()?
            .elapsed()
            .is_ok_and(|age| age > Duration::from_secs(86400))
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

fn extract(root: &Path) -> Result<Stage> {
    verify_payload(FILES, PAYLOAD_DIGEST)?;
    let metadata = fs::symlink_metadata(root).context("enrolled machine root is unavailable")?;
    reject_link(root, &metadata)?;
    if !metadata.is_dir() {
        bail!("enrolled machine root must be a directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
            bail!(
                "enrolled machine root must be owned by this user and not writable by other users"
            );
        }
    }
    let mut parent = root.to_path_buf();
    for name in [".treer", "launchers", "agent"] {
        parent.push(name);
        private_directory(&parent)?;
    }
    cleanup_stale_payloads(&parent)?;
    let stage = Stage(parent.join(format!(".controller-payload-{}", uuid::Uuid::new_v4())));
    // create_dir (not create_dir_all) prevents accepting a pre-existing staging entry.
    fs::create_dir(&stage.0)?;
    treer_platform::secure_directory(&stage.0)?;
    for file in FILES {
        let mut directory = stage.0.clone();
        let components: Vec<_> = file.path.split('/').collect();
        for component in &components[..components.len() - 1] {
            directory.push(component);
            private_directory(&directory)?;
        }
        let path = stage.0.join(file.path);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut handle = options.open(&path)?;
        handle.write_all(file.bytes)?;
        handle.sync_all()?;
        drop(handle);
        treer_platform::secure_file(&path)?;
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
    }
    let manifest_path = stage.0.join("bundle-manifest.json");
    let mut manifest_handle = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_path)?;
    manifest_handle.write_all(MANIFEST_JSON.as_bytes())?;
    manifest_handle.sync_all()?;
    drop(manifest_handle);
    treer_platform::secure_file(&manifest_path)?;
    for file in FILES {
        let path = stage.0.join(file.path);
        let metadata = fs::symlink_metadata(&path)?;
        reject_link(&path, &metadata)?;
        if !metadata.is_file()
            || metadata.len() != file.bytes.len() as u64
            || format!("{:x}", Sha256::digest(fs::read(&path)?)) != file.sha256
        {
            bail!("extracted launcher verification failed before execution");
        }
    }
    Ok(stage)
}

fn trusted_file(path: &Path, executable: bool) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("executable must be an explicit absolute path");
    }
    let path = fs::canonicalize(path).context("trusted executable is missing")?;
    let metadata = fs::symlink_metadata(&path)?;
    reject_link(&path, &metadata)?;
    if !metadata.is_file() {
        bail!("trusted executable must be a regular file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = nix::unistd::geteuid().as_raw();
        for component in path.ancestors() {
            let metadata = fs::metadata(component)?;
            if ![0, uid].contains(&metadata.uid()) || metadata.mode() & 0o022 != 0 {
                bail!("trusted executable path has an untrusted owner or writable component");
            }
        }
        if executable && metadata.mode() & 0o111 == 0 {
            bail!("trusted executable is not executable");
        }
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(path)
}

fn trusted_executable(path: &Path) -> Result<PathBuf> {
    trusted_file(path, true)
}

fn node_executable(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return trusted_executable(&path);
    }
    #[cfg(unix)]
    let locations = [
        "/opt/homebrew/bin/node",
        "/usr/local/bin/node",
        "/usr/bin/node",
    ];
    #[cfg(windows)]
    let locations = [r"C:\Program Files\nodejs\node.exe"];
    for path in locations {
        if Path::new(path).exists() {
            return trusted_executable(Path::new(path));
        }
    }
    bail!("Node.js 22+ is required; install it from your trusted package manager or nodejs.org and pass --node /absolute/path/to/node")
}

fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    // Node hooks can execute code before the verified installer or version check.
    for (name, _) in std::env::vars_os() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("NODE_")
        {
            command.env_remove(name);
        }
    }
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

async fn bounded_output(mut cmd: Command, input: Option<Vec<u8>>, seconds: u64) -> Result<Output> {
    cmd.stdout(Stdio::piped());
    if input.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd
        .spawn()
        .context("cannot run trusted launcher dependency")?;
    let mut stdout = child.stdout.take().context("child stdout unavailable")?;
    let stdin = child.stdin.take();
    let result = tokio::time::timeout(Duration::from_secs(seconds), async {
        let write = async move {
            if let (Some(mut stdin), Some(input)) = (stdin, input) {
                stdin.write_all(&input).await?;
                stdin.shutdown().await?;
            }
            Ok::<_, anyhow::Error>(())
        };
        let read = async {
            let mut bytes = Vec::new();
            (&mut stdout)
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() > 4 * 1024 * 1024 {
                bail!("launcher dependency response exceeds limit");
            }
            Ok::<_, anyhow::Error>(bytes)
        };
        let (_, bytes) = tokio::try_join!(write, read)?;
        Ok::<_, anyhow::Error>(Output {
            status: child.wait().await?,
            stdout: bytes,
            stderr: Vec::new(),
        })
    })
    .await;
    match result {
        Ok(Ok(output)) => Ok(output),
        error => {
            let _ = child.kill().await;
            match error { Ok(Err(error)) => Err(error), _ => bail!("launcher dependency timed out; inspect private installer state before retrying") }
        }
    }
}

async fn run_json(treer: &Path, args: &[&str]) -> Result<Value> {
    let mut cmd = command(treer);
    cmd.args(args);
    let output = bounded_output(cmd, None, 30).await?;
    if !output.status.success() {
        bail!(
            "Treer command failed ({}); check enrollment, permissions and Controller compatibility",
            args.join(" ")
        );
    }
    if output.stdout.len() > 4 * 1024 * 1024 {
        bail!("Treer response exceeds installer limit");
    }
    serde_json::from_slice(&output.stdout).context("Treer returned invalid installer metadata")
}

fn node_major(value: &[u8]) -> Option<u32> {
    std::str::from_utf8(value)
        .ok()?
        .trim()
        .strip_prefix('v')?
        .split('.')
        .next()?
        .parse()
        .ok()
}

fn validate_controller(value: &Value) -> Result<()> {
    let reported: treer_protocol::LauncherControllerContract =
        serde_json::from_value(value.clone())
            .context("launcher_controller_unsupported: missing or invalid Controller contract")?;
    let required = controller_contract();
    if reported.protocol != required.protocol
        || reported.minimum_launcher_security_epoch.is_none()
        || required
            .features
            .iter()
            .any(|feature| !reported.features.contains(feature))
        || required
            .capabilities
            .iter()
            .any(|capability| !reported.capabilities.contains(capability))
    {
        bail!("launcher_controller_unsupported: running Controller lacks required AIS capabilities; update Controller first");
    }
    Ok(())
}

pub async fn run(args: LauncherArgs) -> Result<()> {
    verify_payload(FILES, PAYLOAD_DIGEST)?;
    let node = node_executable(args.node)?;
    let mut version_cmd = command(&node);
    version_cmd.arg("--version");
    let version_output = bounded_output(version_cmd, None, 10).await?;
    if !version_output.status.success()
        || node_major(&version_output.stdout).is_none_or(|major| major < 22)
    {
        bail!("Node.js 22+ is required; selected executable has an unsupported version");
    }
    let treer = args.treer.or_else(crate::sibling_treer_binary).context("trusted treer CLI missing; install the matching CLI or pass --treer /absolute/path/to/treer")?;
    let treer = trusted_executable(&treer)?;
    let controller_raw = run_json(&treer, &["interface", "contract"]).await;
    let context = controller_raw.as_ref().ok().and_then(|value| {
        serde_json::from_value::<treer_protocol::LauncherMachineContext>(value["machine"].clone())
            .ok()
    });
    let controller = controller_raw.and_then(|value| {
        validate_controller(&value)?;
        Ok(value)
    });
    let (operation, install, rollback) = match &args.command {
        LauncherCommand::Install(options) => ("install", Some(options), None),
        LauncherCommand::Update(options) => ("update", Some(options), None),
        LauncherCommand::Status => ("status", None, None),
        LauncherCommand::Rollback { version } => ("rollback", None, version.clone()),
    };
    if operation != "status" && controller.is_err() {
        bail!("launcher_controller_unsupported: running Controller does not advertise the launcher contract; update Controller first (no installation or restart performed)");
    }
    let context = match context {
        Some(context) => context,
        None if operation == "status" => crate::service::launcher_machine_context()?,
        None => bail!("launcher_controller_unsupported: missing authenticated machine context"),
    };
    let root = PathBuf::from(&context.root);
    if !root.is_absolute() {
        bail!("enrolled machine.root must be absolute");
    }
    if install.is_some_and(|options| !RUNTIME_READY && !options.allow_network) {
        bail!("source-only development Controller has no portable runtime; use an official release, or explicitly opt in with --allow-network and --npm-cli");
    }
    if install.is_some_and(|options| !RUNTIME_READY && options.npm_cli.is_none()) {
        bail!("source-only installation requires an explicit trusted --npm-cli /absolute/path/to/npm-cli.js");
    }
    let npm_cli = install
        .and_then(|options| options.npm_cli.as_ref())
        .map(|path| trusted_file(path, false))
        .transpose()?;
    let stage = extract(&root)?;
    let manifest: Value = serde_json::from_str(MANIFEST_JSON)?;
    let request = json!({"schema":"treer.launcher-install/v1", "operation":operation,
        "bundle_dir":stage.0, "version":format!("sha256-{PAYLOAD_DIGEST}"), "source_commit":SOURCE_COMMIT,
        "root":root, "cwd":install.and_then(|options| options.cwd.as_deref()).unwrap_or("."),
        "agents":install.map(|options| options.agents.clone()).unwrap_or_default(), "node":node,"treer":treer,
        "npm_cli":npm_cli,
        "allow_network":install.is_some_and(|options| options.allow_network),"controller":controller.unwrap_or(Value::Null),
        "payload_manifest":manifest, "rollback_version":rollback});
    let mut install_cmd = command(&node);
    install_cmd
        .arg(stage.0.join("scripts/install.mjs"))
        .arg("--controller-request");
    let output = bounded_output(install_cmd, Some(serde_json::to_vec(&request)?), 360).await?;
    if !output.status.success() {
        let code = serde_json::from_slice::<Value>(&output.stdout)
            .ok()
            .and_then(|value| value["error"]["code"].as_str().map(str::to_owned));
        match code.as_deref() {
            Some("launcher_incompatible") => bail!("launcher_incompatible: retained launcher and running Controller are not compatible; update Controller or choose a compatible version"),
            Some("profile_verification_failed") => bail!("profile_verification_failed: saved profile readback failed; installer attempted restoration"),
            Some("profile_rollback_incomplete") => bail!("profile_rollback_incomplete: inspect private install state; usable candidate retained; no Agent was restarted"),
            Some("install_locked") => bail!("install_locked: another installation owns the private installer lock"),
            _ => {}
        }
        bail!("launcher installer failed; private state and profiles may need inspection (no Agent restart was requested)");
    }
    if output.stdout.len() > 4 * 1024 * 1024 {
        bail!("installer status exceeds limit");
    }
    let mut result: Value =
        serde_json::from_slice(&output.stdout).context("installer returned invalid status JSON")?;
    if operation == "status" {
        result["agent_startups"] = startup_references(&treer, &context.server_id, &root).await;
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn startup_references(treer: &Path, server_id: &str, root: &Path) -> Value {
    let Ok(agents) = run_json(treer, &["status"]).await else {
        return json!({"available":false});
    };
    let records = agents.as_array().or_else(|| agents["agents"].as_array());
    let Some(records) = records else {
        return json!({"available":false});
    };
    let mut entries = Vec::new();
    for agent in records
        .iter()
        .filter(|agent| agent["server_id"].as_str() == Some(server_id))
    {
        let Some(id) = agent["agent_id"].as_str() else {
            continue;
        };
        let saved = run_json(treer, &["agent", "admin", "startup", "show", id]).await;
        let version = saved
            .as_ref()
            .ok()
            .and_then(|saved| startup_version(saved, root));
        entries.push(json!({"agent_id":id,"path":agent["path"],"startup_available":saved.is_ok(),"launcher_version":version}));
    }
    json!({"available":true,"agents":entries})
}

fn startup_version(saved: &Value, root: &Path) -> Option<String> {
    let spec = saved.get("startup").unwrap_or(saved);
    let paths = std::iter::once(spec["command"].as_str()).chain(
        spec["args"]
            .as_array()
            .into_iter()
            .flatten()
            .map(Value::as_str),
    );
    for candidate in paths.flatten() {
        let Ok(relative) = Path::new(candidate).strip_prefix(root.join(".treer/launchers/agent"))
        else {
            continue;
        };
        let Some(first) = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str())
        else {
            continue;
        };
        let hash = first.strip_prefix("sha256-").unwrap_or(first);
        if matches!(hash.len(), 40 | 64) && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Some(first.to_owned());
        }
        if first == "current" {
            return Some("current".into());
        }
        return Some("managed-entry".into());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn embedded_payload_is_verified_and_paths_cannot_escape() {
        verify_payload(FILES, PAYLOAD_DIGEST).unwrap();
        for name in [
            "../escape",
            "/absolute",
            "a/../b",
            "a//b",
            "a\\b",
            "C:escape",
            "a/./b",
        ] {
            assert!(!is_safe_relative(name), "{name}");
        }
        assert!(is_safe_relative("scripts/install.mjs"));
        let bad = [EmbeddedFile {
            path: "safe",
            bytes: b"corrupt",
            sha256: "wrong",
            executable: false,
        }];
        assert!(verify_payload(&bad, PAYLOAD_DIGEST).is_err());
    }
    #[test]
    fn node_version_requires_a_real_major_version() {
        assert_eq!(node_major(b"v22.1.0\n"), Some(22));
        assert_eq!(node_major(b"v20.19.0"), Some(20));
        assert_eq!(node_major(b"unexpected"), None);
        assert!(trusted_executable(Path::new("node")).is_err());
    }

    #[test]
    fn compatibility_is_explicit_and_old_controllers_fail_closed() {
        let contract = serde_json::to_value(controller_contract()).unwrap();
        validate_controller(&contract).unwrap();
        assert!(validate_controller(&json!({"protocol":"treer.agent-interface/v1"})).is_err());
        for field in ["features", "capabilities"] {
            let mut old = contract.clone();
            old[field] = json!([]);
            assert!(validate_controller(&old).is_err());
        }
        let mut legacy = contract.clone();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("minimum_launcher_security_epoch");
        assert!(validate_controller(&legacy).is_err());
        let mut unknown = contract.clone();
        unknown["protocol"] = json!("treer.agent-interface/v2");
        assert!(validate_controller(&unknown).is_err());
        let decoded: treer_protocol::LauncherControllerContract =
            serde_json::from_value(contract).unwrap();
        assert_eq!(decoded, controller_contract());
    }

    #[test]
    fn launcher_subcommands_parse_without_runtime_side_effects() {
        use clap::Parser;
        for operation in ["install", "update", "status", "rollback"] {
            let parsed =
                crate::Args::try_parse_from(["treer-agent-server", "launcher", operation]).unwrap();
            assert!(matches!(parsed.command, Some(crate::Command::Launcher(_))));
        }
        assert!(crate::Args::try_parse_from([
            "treer-agent-server",
            "launcher",
            "install",
            "--agent",
            "unknown"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from([
            "treer-agent-server",
            "launcher",
            "install",
            "--npm-cli",
            "/tmp/npm"
        ])
        .is_err());
        assert!(crate::Args::try_parse_from([
            "treer-agent-server",
            "launcher",
            "update",
            "--agent",
            "codex",
            "--agent",
            "claude",
            "--node",
            "/usr/bin/node"
        ])
        .is_ok());
    }

    #[test]
    fn startup_report_reads_node_arguments_but_never_echoes_them() {
        let root = Path::new("/machine");
        let version = format!("sha256-{}", "a".repeat(64));
        let saved = json!({"command":"/usr/bin/node","args":[format!("/machine/.treer/launchers/agent/{version}/scripts/entry.mjs"),"fixture-secret"]});
        assert_eq!(startup_version(&saved, root), Some(version));
        assert_eq!(
            startup_version(
                &json!({"command":"/bin/sh","args":["fixture-secret"]}),
                root
            ),
            None
        );
        assert_eq!(
            startup_version(
                &json!({"command":"/usr/bin/node","args":["/machine/.treer/launchers/agent/fixture-secret/run"]}),
                root
            ),
            Some("managed-entry".into())
        );
    }
    #[test]
    fn extraction_is_private_and_rejects_managed_links() {
        let root =
            std::env::temp_dir().join(format!("treer-launcher-test-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        {
            let stage = extract(&root).unwrap();
            assert_eq!(
                fs::read(stage.0.join("scripts/install.mjs")).unwrap(),
                FILES
                    .iter()
                    .find(|f| f.path == "scripts/install.mjs")
                    .unwrap()
                    .bytes
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&stage.0).unwrap().permissions().mode() & 0o777,
                    0o700
                );
                assert_eq!(
                    fs::metadata(stage.0.join("scripts/install.mjs"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
            }
        }
        fs::remove_dir_all(root.join(".treer")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, root.join(".treer")).unwrap();
            assert!(extract(&root).is_err());
            fs::remove_file(root.join(".treer")).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_cleanup_only_removes_old_uuid_payload_directories() {
        let root =
            std::env::temp_dir().join(format!("treer-launcher-cleanup-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        let stale = root.join(format!(".controller-payload-{}", uuid::Uuid::new_v4()));
        let recent = root.join(format!(".controller-payload-{}", uuid::Uuid::new_v4()));
        let version = root.join(format!("sha256-{}", "a".repeat(64)));
        let arbitrary = root.join(".controller-payload-user-data");
        for path in [&stale, &recent, &version, &arbitrary] {
            fs::create_dir(path).unwrap();
        }
        let old = std::time::SystemTime::now() - Duration::from_secs(172800);
        for path in [&stale, &version, &arbitrary] {
            fs::File::open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        cleanup_stale_payloads(&root).unwrap();
        assert!(!stale.exists());
        for path in [&recent, &version, &arbitrary] {
            assert!(path.exists());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dependency_output_is_bounded_and_hangs_time_out() {
        let mut cmd = command(Path::new("/bin/sh"));
        cmd.args(["-c", "while :; do printf '01234567890123456789012345678901234567890123456789012345678901234567890123456789'; done"]);
        let error = bounded_output(cmd, None, 10).await.unwrap_err();
        assert!(error.to_string().contains("response exceeds limit"));
        let mut cmd = command(Path::new("/bin/sh"));
        cmd.args(["-c", "exec sleep 10"]);
        let error = bounded_output(cmd, None, 1).await.unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }
}
