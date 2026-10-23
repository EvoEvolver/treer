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
            treer_protocol::CAPABILITY_AGENT_PRIVATE_STATE.to_owned(),
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
    /// Select a local enrollment. Required when discovery is ambiguous.
    #[arg(long, global = true)]
    workspace: Option<String>,
    /// Select the exact local Controller URL (never an arbitrary remote endpoint).
    #[arg(long, global = true)]
    url: Option<String>,
    #[command(subcommand)]
    command: LauncherCommand,
}

#[derive(Clone, Default)]
struct ControllerSelection {
    workspace: Option<String>,
    url: Option<String>,
    server_id: Option<String>,
}

impl ControllerSelection {
    fn from_environment(
        workspace: Option<String>,
        url: Option<String>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self> {
        let bound_workspace = env("TREER_WORKSPACE_ID");
        let bound_url = env("TREER_AGENT_SERVER_URL");
        let bound_identity = env("TREER_AGENT_ID").is_some()
            || env("TREER_WORKLOAD_CREDENTIAL").is_some()
            || env("TREER_IDENTITY_MODE").as_deref() == Some("agent")
            || env("TREER_OPERATOR_CREDENTIAL").is_some_and(|v| !v.is_empty());
        if bound_identity && (bound_workspace.is_some() || bound_url.is_some()) {
            if let Some(requested) = &workspace {
                anyhow::ensure!(
                    bound_workspace.as_ref() == Some(requested),
                    "launcher_identity_binding: cannot change --workspace for this identity"
                );
            }
            if let Some(requested) = &url {
                anyhow::ensure!(
                    bound_url
                        .as_ref()
                        .is_some_and(|bound| same_url(bound, requested)),
                    "launcher_identity_binding: cannot change --url for this identity"
                );
            }
        }
        Ok(Self {
            workspace: workspace.or(bound_workspace),
            url: url.or(bound_url),
            server_id: env("TREER_SERVER_ID"),
        })
    }

    fn select(
        &self,
        configs: Vec<crate::service::ServiceConfig>,
    ) -> Result<crate::service::ServiceConfig> {
        let mut matches: Vec<_> = configs
            .into_iter()
            .filter(|config| {
                self.workspace
                    .as_ref()
                    .is_none_or(|v| v == &config.workspace)
                    && self
                        .server_id
                        .as_ref()
                        .is_none_or(|v| v == &config.server_id)
                    && self
                        .url
                        .as_ref()
                        .is_none_or(|v| same_url(v, &format!("http://{}", config.listen)))
            })
            .collect();
        if matches.len() > 1 {
            // Config fields are local but not assumed safe to print. Only bounded
            // identifiers and parsed socket addresses enter candidate diagnostics.
            matches.sort_by(|a, b| a.server_id.cmp(&b.server_id));
            let identifier = |value: &str| -> String {
                if value.len() <= 96
                    && value
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                {
                    value.to_owned()
                } else {
                    "[invalid identifier]".into()
                }
            };
            let candidates = matches
                .iter()
                .take(8)
                .map(|config| {
                    format!(
                        "{} workspace={} listen={}",
                        identifier(&config.server_id),
                        identifier(&config.workspace),
                        config
                            .listen
                            .parse::<std::net::SocketAddr>()
                            .map(|a| a.to_string())
                            .unwrap_or_else(|_| "[invalid address]".into())
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            bail!("launcher_controller_ambiguous: multiple local Controllers match; select --workspace and, if needed, --url. Candidates (up to 8): {candidates}");
        }
        matches.pop().context("launcher_controller_not_found: no matching local enrollment; check --workspace/--url or connect this machine first")
    }

    fn bind(&self, cmd: &mut Command) {
        if let Some(workspace) = &self.workspace {
            cmd.env("TREER_WORKSPACE_ID", workspace);
        }
        if let Some(url) = &self.url {
            cmd.env("TREER_AGENT_SERVER_URL", url);
        }
        if let Some(server_id) = &self.server_id {
            cmd.env("TREER_SERVER_ID", server_id);
        }
    }
}

fn same_url(left: &str, right: &str) -> bool {
    matches!((url::Url::parse(left), url::Url::parse(right)), (Ok(a), Ok(b)) if a == b)
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
    /// Refuse activation unless this binary's payload has a verified Rust default.
    /// Older Controllers reject this flag; never retry without it to bypass refusal.
    #[arg(long, value_parser = ["rust"])]
    require_runtime: Option<String>,
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
    // Rust's filesystem APIs support long Windows paths, but the ACL Win32
    // calls need the extended-length form. Check the supplied root for links
    // before canonicalizing it; keep all appended components native-separated.
    #[cfg(windows)]
    let mut parent = fs::canonicalize(root).context("cannot canonicalize launcher machine root")?;
    #[cfg(not(windows))]
    let mut parent = root.to_path_buf();
    for name in [".treer", "launchers", "agent"] {
        parent.push(name);
        private_directory(&parent).context("cannot prepare private launcher directory")?;
    }
    cleanup_stale_payloads(&parent)?;
    let stage = Stage(parent.join(format!(".controller-payload-{}", uuid::Uuid::new_v4())));
    // create_dir (not create_dir_all) prevents accepting a pre-existing staging entry.
    fs::create_dir(&stage.0)?;
    treer_platform::secure_directory(&stage.0)
        .context("cannot protect launcher extraction stage")?;
    for file in FILES {
        let mut directory = stage.0.clone();
        let components: Vec<_> = file.path.split('/').collect();
        for component in &components[..components.len() - 1] {
            directory.push(component);
            private_directory(&directory)?;
        }
        let path = payload_path(&stage.0, file.path);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut handle = options
            .open(&path)
            .with_context(|| format!("cannot create embedded launcher file {}", file.path))?;
        handle.write_all(file.bytes)?;
        handle.sync_all()?;
        drop(handle);
        treer_platform::secure_file(&path)
            .with_context(|| format!("cannot protect embedded launcher file {}", file.path))?;
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
        let path = payload_path(&stage.0, file.path);
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

fn payload_path(root: &Path, relative: &str) -> PathBuf {
    // `relative` is checked by verify_payload before extraction. Joining the
    // whole slash-delimited string preserves '/' under a verbatim Windows
    // prefix, where Win32 deliberately disables normal slash conversion.
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, component| path.join(component))
}

/// Node cannot start an entry script whose path has the `\\?\` prefix (its CommonJS
/// loader fails before any JavaScript runs), so the argument handed to it uses the
/// equivalent drive-letter form; extraction itself keeps the verbatim path.
fn node_entry_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        // Do not round-trip an OS filename through lossy UTF-8. Preserve every
        // remaining UTF-16 unit, including Unicode and trailing separators.
        if units.len() > 7
            && units.starts_with(&[92, 92, 63, 92])
            && matches!(units[4], 65..=90 | 97..=122)
            && units[5] == 58
            && units[6] == 92
        {
            return PathBuf::from(std::ffi::OsString::from_wide(&units[4..]));
        }
    }
    path
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

async fn run_json(treer: &Path, selection: &ControllerSelection, args: &[&str]) -> Result<Value> {
    run_json_with_timeout(treer, selection, args, 30).await
}

async fn run_json_with_timeout(
    treer: &Path,
    selection: &ControllerSelection,
    args: &[&str],
    seconds: u64,
) -> Result<Value> {
    let mut cmd = command(treer);
    selection.bind(&mut cmd);
    cmd.args(args);
    let output = bounded_output(cmd, None, seconds).await?;
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

// Diagnostics are untrusted text, never a channel for paths or credentials.
fn installer_message(message: &str, root: &Path, secrets: &[String]) -> String {
    let mut value = message.to_owned();
    for secret in
        std::iter::once(root.to_string_lossy().into_owned()).chain(secrets.iter().cloned())
    {
        if !secret.is_empty() {
            value = value.replace(&secret, "[redacted]");
        }
    }
    let value: String = value.chars().map(|c| {
        if c.is_control() || matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}') {
            ' '
        } else { c }
    }).collect();
    value
        .split_whitespace()
        .map(|word| {
            if word.contains(['/', '\\', ':', '=', '@']) || word.chars().count() >= 32 {
                "[redacted]"
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect()
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
    validate_controller_with_floor(value, treer_protocol::MINIMUM_LAUNCHER_SECURITY_EPOCH)
}

fn validate_required_runtime(status: &Value) -> Result<()> {
    anyhow::ensure!(
        status["default"] == "rust" && status["codex"] == "rust",
        "launcher_runtime_required: candidate must have a verified Rust default and Codex provider; no activation performed"
    );
    Ok(())
}

async fn require_rust_runtime(node: &Path, stage: &Path) -> Result<()> {
    // This module and the binding are from the verified, extracted candidate,
    // never from current.json or the previously installed runtime. Do not run
    // install.mjs until this read-only check has completed successfully.
    for relative in ["scripts/native-runtime.mjs", "native-runtime.json"] {
        anyhow::ensure!(
            payload_path(stage, relative).is_file(),
            "launcher_runtime_required: embedded candidate lacks a Rust runtime binding; no activation performed"
        );
    }
    let mut probe = command(node);
    probe.args(["--input-type=module", "--eval", r#"
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';
try {
  const root = process.argv[1];
  const { runtimeStatus } = await import(pathToFileURL(join(root, 'scripts/native-runtime.mjs')).href);
  const status = runtimeStatus(root);
  process.stdout.write(JSON.stringify({default:status.default,codex:status.codex}));
} catch { process.exitCode = 1; }
"#]);
    probe.arg(node_entry_path(stage.to_path_buf()));
    let output = bounded_output(probe, None, 30).await.context(
        "launcher_runtime_required: candidate runtime verification failed; no activation performed",
    )?;
    anyhow::ensure!(output.status.success(), "launcher_runtime_required: candidate runtime binding is unavailable, incompatible or changed; no activation performed");
    let status = serde_json::from_slice(&output.stdout).context(
        "launcher_runtime_required: invalid candidate runtime status; no activation performed",
    )?;
    validate_required_runtime(&status)
}

fn validate_controller_with_floor(value: &Value, minimum_epoch: u32) -> Result<()> {
    let reported: treer_protocol::LauncherControllerContract =
        serde_json::from_value(value.clone())
            .context("launcher_controller_unsupported: missing or invalid Controller contract")?;
    let required = controller_contract();
    if reported.protocol != required.protocol
        || reported
            .minimum_launcher_security_epoch
            .is_none_or(|epoch| epoch < minimum_epoch)
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
    let requested = ControllerSelection::from_environment(args.workspace, args.url, |key| {
        std::env::var(key).ok()
    })?;
    let config = requested.select(crate::service::launcher_configs()?)?;
    let selection = ControllerSelection {
        workspace: Some(config.workspace.clone()),
        url: Some(format!("http://{}", config.listen)),
        server_id: Some(config.server_id.clone()),
    };
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
    let controller_raw = run_json(&treer, &selection, &["interface", "contract"]).await
        .context("launcher_controller_probe_failed: selected Controller could not be queried; check connection/authentication (not a contract capability diagnosis)")?;
    if let Ok(machine) = serde_json::from_value::<treer_protocol::LauncherMachineContext>(
        controller_raw["machine"].clone(),
    ) {
        anyhow::ensure!(machine.server_id == config.server_id && machine.workspace_id == config.workspace,
            "launcher_controller_identity_mismatch: selected enrollment differs from authenticated Controller");
    }
    let context = Some(&controller_raw).and_then(|value| {
        serde_json::from_value::<treer_protocol::LauncherMachineContext>(value["machine"].clone())
            .ok()
    });
    let controller = validate_controller(&controller_raw).map(|()| controller_raw);
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
        None if operation == "status" => treer_protocol::LauncherMachineContext {
            root: config.root.display().to_string(),
            server_id: config.server_id,
            workspace_id: config.workspace,
        },
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
    let required_runtime = install.and_then(|options| options.require_runtime.as_deref());
    if required_runtime == Some("rust") {
        require_rust_runtime(&node, &stage.0).await?;
    }
    let manifest: Value = serde_json::from_str(MANIFEST_JSON)?;
    let request = json!({"schema":"treer.launcher-install/v1", "operation":operation,
        "bundle_dir":stage.0, "version":format!("sha256-{PAYLOAD_DIGEST}"), "source_commit":SOURCE_COMMIT,
        "root":root, "cwd":install.and_then(|options| options.cwd.as_deref()).unwrap_or("."),
        "agents":install.map(|options| options.agents.clone()).unwrap_or_default(), "node":node,"treer":treer,
        "npm_cli":npm_cli,
        "require_runtime":required_runtime,
        "allow_network":install.is_some_and(|options| options.allow_network),"controller":controller.unwrap_or(Value::Null),
        "payload_manifest":manifest, "rollback_version":rollback});
    let mut install_cmd = command(&node);
    selection.bind(&mut install_cmd);
    let verified_entry = payload_path(&stage.0, "scripts/install.mjs");
    let node_entry = node_entry_path(verified_entry.clone());
    #[cfg(windows)]
    if fs::canonicalize(&node_entry).context("Node installer entry is unavailable")?
        != fs::canonicalize(&verified_entry).context("verified installer entry is unavailable")?
    {
        // Dropping verbatim spelling must not select a different file (e.g.
        // machine roots with trailing dots/spaces or device-name semantics).
        bail!("Node installer entry does not resolve to the verified payload");
    }
    install_cmd.arg(node_entry).arg("--controller-request");
    let output = bounded_output(install_cmd, Some(serde_json::to_vec(&request)?), 360).await?;
    if !output.status.success() {
        let error = serde_json::from_slice::<Value>(&output.stdout).unwrap_or(Value::Null);
        match error["error"]["code"].as_str() {
            Some("launcher_incompatible") => bail!("launcher_incompatible: retained launcher and running Controller are not compatible; update Controller or choose a compatible version"),
            Some("profile_verification_failed") => bail!("profile_verification_failed: saved profile readback failed; installer attempted restoration"),
            Some("profile_rollback_incomplete") => bail!("profile_rollback_incomplete: inspect private install state; usable candidate retained; no Agent was restarted"),
            Some("install_locked") => bail!("install_locked: another installation owns the private installer lock"),
            _ => {}
        }
        let secrets = std::env::vars()
            .filter(|(name, _)| {
                let name = name.to_ascii_uppercase();
                ["TOKEN", "CREDENTIAL", "SECRET", "PASSWORD", "KEY"]
                    .iter()
                    .any(|part| name.contains(part))
            })
            .map(|(_, value)| value)
            .collect::<Vec<_>>();
        let message = installer_message(
            error["error"]["message"].as_str().unwrap_or(""),
            &root,
            &secrets,
        );
        if !message.is_empty() {
            bail!("launcher installer failed: {message}");
        }
        bail!("launcher installer failed; private state and profiles may need inspection (no Agent restart was requested)");
    }
    if output.stdout.len() > 4 * 1024 * 1024 {
        bail!("installer status exceeds limit");
    }
    let mut result: Value =
        serde_json::from_slice(&output.stdout).context("installer returned invalid status JSON")?;
    if operation == "status" {
        result["agent_startups"] =
            startup_references(&treer, &selection, &context.server_id, &root).await;
    }
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

async fn startup_references(
    treer: &Path,
    selection: &ControllerSelection,
    server_id: &str,
    root: &Path,
) -> Value {
    let Ok(agents) = run_json_with_timeout(treer, selection, &["status"], 2).await else {
        return json!({"available":false,"partial":true,"reason":"agent_list_unavailable_or_timed_out"});
    };
    let records = agents.as_array().or_else(|| agents["agents"].as_array());
    let Some(records) = records else {
        return json!({"available":false,"partial":true,"reason":"invalid_agent_list"});
    };
    let mut entries = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut partial = false;
    for agent in records
        .iter()
        .filter(|agent| agent["server_id"].as_str() == Some(server_id))
    {
        let Some(id) = agent["agent_id"].as_str() else {
            continue;
        };
        // Do not even spawn another CLI once the shared budget is exhausted.
        let saved = if tokio::time::Instant::now() >= deadline {
            Err(anyhow::anyhow!("status budget exhausted"))
        } else {
            tokio::time::timeout_at(
                deadline,
                run_json_with_timeout(
                    treer,
                    selection,
                    &["agent", "admin", "startup", "show", id],
                    1,
                ),
            )
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("status budget exhausted")))
        };
        partial |= saved.is_err();
        let version = saved
            .as_ref()
            .ok()
            .and_then(|saved| startup_version(saved, root));
        entries.push(json!({"agent_id":id,"path":agent["path"],"startup_available":saved.is_ok(),"launcher_version":version,
            "reason": if saved.is_ok() { Value::Null } else { json!("startup_unavailable_or_timed_out") }}));
    }
    json!({"available":true,"partial":partial,"agents":entries})
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

    fn selected_fixture(id: &str, workspace: &str, listen: &str) -> crate::service::ServiceConfig {
        serde_json::from_value(json!({
            "proxy":"https://proxy.invalid", "workspace":workspace, "server_id":id,
            "machine_token":"synthetic-machine-secret", "operator_credential":"synthetic-operator-secret",
            "root":"/synthetic-root", "listen":listen, "host_socket":"/synthetic-socket",
            "install_hostname":"fixture"
        })).unwrap()
    }

    #[test]
    fn controller_selection_is_explicit_bounded_and_never_discloses_credentials() {
        let configs = || {
            vec![
                selected_fixture("srv_a", "ws_a", "127.0.0.1:8790"),
                selected_fixture("srv_b", "ws_b", "127.0.0.1:8791"),
            ]
        };
        let error = ControllerSelection::default()
            .select(configs())
            .unwrap_err()
            .to_string();
        assert!(error.contains("launcher_controller_ambiguous"));
        assert!(error.contains("srv_b workspace=ws_b listen=127.0.0.1:8791"));
        assert!(!error.contains("secret"));
        assert!(!error.contains("synthetic-root"));
        for selection in [
            ControllerSelection {
                workspace: Some("ws_b".into()),
                ..Default::default()
            },
            ControllerSelection {
                url: Some("http://127.0.0.1:8791/".into()),
                ..Default::default()
            },
        ] {
            assert_eq!(selection.select(configs()).unwrap().server_id, "srv_b");
        }
        let error = ControllerSelection::default()
            .select(
                (0..100)
                    .map(|i| {
                        selected_fixture(&format!("srv_{i}"), "ws_fixture", "user:secret@invalid")
                    })
                    .collect(),
            )
            .unwrap_err()
            .to_string();
        assert!(error.len() < 2000);
        assert!(!error.contains("user:secret"));
        assert_eq!(error.matches("workspace=").count(), 8);
        assert!(ControllerSelection {
            workspace: Some("ws_missing".into()),
            ..Default::default()
        }
        .select(configs())
        .unwrap_err()
        .to_string()
        .starts_with("launcher_controller_not_found"));
    }

    #[test]
    fn launcher_selectors_preserve_managed_and_operator_identity_bindings() {
        for identity_key in ["TREER_AGENT_ID", "TREER_OPERATOR_CREDENTIAL"] {
            let env = |key: &str| match key {
                "TREER_WORKSPACE_ID" => Some("ws_a".into()),
                "TREER_AGENT_SERVER_URL" => Some("http://127.0.0.1:8790".into()),
                _ if key == identity_key => Some("synthetic-credential".into()),
                _ => None,
            };
            assert!(ControllerSelection::from_environment(Some("ws_b".into()), None, env).is_err());
            let error = ControllerSelection::from_environment(
                None,
                Some("https://user:secret@invalid".into()),
                env,
            )
            .err()
            .unwrap()
            .to_string();
            assert!(error.contains("launcher_identity_binding"));
            assert!(!error.contains("secret"));
            assert!(ControllerSelection::from_environment(
                Some("ws_a".into()),
                Some("http://127.0.0.1:8790/".into()),
                env
            )
            .is_ok());
        }
        assert!(ControllerSelection::from_environment(
            None,
            Some("http://127.0.0.1:8790".into()),
            |key| match key {
                "TREER_AGENT_ID" => Some("ag_fixture".into()),
                "TREER_WORKSPACE_ID" => Some("ws_a".into()),
                _ => None,
            }
        )
        .is_err());
        assert!(ControllerSelection::from_environment(
            None,
            Some("http://127.0.0.1:8790".into()),
            |_| None
        )
        .is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn probe_and_followup_cli_calls_use_the_same_selected_controller() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("trr-selection-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        let cli = root.join("treer");
        fs::write(&cli, "#!/bin/sh\n[ \"$TREER_WORKSPACE_ID\" = ws_b ] && [ \"$TREER_SERVER_ID\" = srv_b ] && [ \"$TREER_AGENT_SERVER_URL\" = http://127.0.0.1:8791 ] || exit 9\nprintf '%s' '{\"selected\":\"srv_b\"}'\n").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let selected = ControllerSelection {
            workspace: Some("ws_b".into()),
            url: Some("http://127.0.0.1:8791".into()),
            server_id: Some("srv_b".into()),
        };
        for args in [
            &["interface", "contract"][..],
            &["status"][..],
            &["agent", "admin", "startup", "show", "ag_fixture"][..],
        ] {
            assert_eq!(
                run_json(&cli, &selected, args).await.unwrap()["selected"],
                "srv_b"
            );
        }
        // The Node installer receives exactly the same selection, inherited by
        // its profile save/readback subprocesses; no process-global env mutation.
        let mut installer = command(&cli);
        selected.bind(&mut installer);
        assert!(bounded_output(installer, None, 2)
            .await
            .unwrap()
            .status
            .success());
        assert!(run_json(&cli, &ControllerSelection::default(), &["status"])
            .await
            .is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installer_diagnostics_preserve_prose_but_bound_and_redact_untrusted_text() {
        let root = Path::new("/private/machine root");
        let message = installer_message(
            "Launcher profile is customized; choose another name.",
            root,
            &[],
        );
        assert_eq!(
            message,
            "Launcher profile is customized; choose another name."
        );
        let message = installer_message("Bad\nprofile\r\t\u{001b}\u{202e} /private/machine root/config C:\\Users\\private\\key token=short known-secret abcdef0123456789abcdef0123456789abcdef0123", root, &["known-secret".into()]);
        assert!(!message.chars().any(char::is_control));
        for forbidden in [
            "/private",
            "machine root",
            "C:",
            "known-secret",
            "token=",
            "abcdef0123",
            "\u{202e}",
        ] {
            assert!(
                !message.contains(forbidden),
                "diagnostic disclosed a synthetic sensitive fragment"
            );
        }
        let message = installer_message(&"profile 问题 ".repeat(200), root, &[]);
        assert_eq!(message.chars().count(), 300);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn startup_report_returns_partial_when_nested_reads_do_not_complete() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("trr-status-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        let cli = root.join("treer");
        fs::write(&cli, "#!/bin/sh\nif [ \"$1\" = status ]; then\nprintf '%s\\n' '{\"agents\":[{\"server_id\":\"local\",\"agent_id\":\"one\"},{\"server_id\":\"local\",\"agent_id\":\"two\"}]}'\nelse\nexec /bin/sleep 15\nfi\n").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let report = tokio::time::timeout(
            Duration::from_secs(4),
            startup_references(&cli, &ControllerSelection::default(), "local", &root),
        )
        .await
        .unwrap();
        assert_eq!(report["available"], true);
        assert_eq!(report["partial"], true);
        assert_eq!(report["agents"].as_array().unwrap().len(), 2);
        for entry in report["agents"].as_array().unwrap() {
            assert_eq!(entry["startup_available"], false);
            assert_eq!(entry["reason"], "startup_unavailable_or_timed_out");
            assert!(entry["launcher_version"].is_null());
        }
        fs::remove_dir_all(root).unwrap();
    }
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
        assert!(validate_controller_with_floor(&contract, 2).is_err());
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
            let parsed = crate::Args::try_parse_from([
                "treer-agent-server",
                "launcher",
                operation,
                "--workspace",
                "ws_fixture",
                "--url",
                "http://127.0.0.1:8791",
            ])
            .unwrap();
            let Some(crate::Command::Launcher(args)) = parsed.command else {
                panic!("launcher expected")
            };
            assert_eq!(args.workspace.as_deref(), Some("ws_fixture"));
            assert_eq!(args.url.as_deref(), Some("http://127.0.0.1:8791"));
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
        for operation in ["install", "update"] {
            let parsed = crate::Args::try_parse_from([
                "treer-agent-server",
                "launcher",
                operation,
                "--require-runtime",
                "rust",
            ])
            .unwrap();
            let Some(crate::Command::Launcher(args)) = parsed.command else {
                panic!("launcher expected")
            };
            let options = match args.command {
                LauncherCommand::Install(options) | LauncherCommand::Update(options) => options,
                _ => panic!("install options expected"),
            };
            assert_eq!(options.require_runtime.as_deref(), Some("rust"));
        }
        for operation in ["status", "rollback"] {
            assert!(crate::Args::try_parse_from([
                "treer-agent-server",
                "launcher",
                operation,
                "--require-runtime",
                "rust",
            ])
            .is_err());
        }
        assert!(crate::Args::try_parse_from([
            "treer-agent-server",
            "launcher",
            "update",
            "--require-runtime",
            "node",
        ])
        .is_err());
    }

    #[test]
    fn required_runtime_rejects_old_unconfigured_and_incomplete_candidates() {
        for status in [
            Value::Null,
            json!({"default":"node","codex":"node"}),
            json!({"default":"unconfigured","codex":"unconfigured"}),
            json!({"default":"rust","codex":"unavailable-provider"}),
            json!({"default":"rust"}),
        ] {
            assert!(validate_required_runtime(&status)
                .unwrap_err()
                .to_string()
                .starts_with("launcher_runtime_required"));
        }
        validate_required_runtime(
            &json!({"default":"rust","codex":"rust","claude":"unavailable-provider"}),
        )
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires explicit trusted Node22+ via TREER_TEST_NODE; no installed configuration used"]
    async fn required_runtime_candidate_probe_runs_before_installer() {
        let node = node_executable(Some(
            std::env::var_os("TREER_TEST_NODE")
                .expect("TREER_TEST_NODE")
                .into(),
        ))
        .unwrap();
        let root =
            std::env::temp_dir().join(format!("trr-required-runtime-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        fs::create_dir(root.join("scripts")).unwrap();
        assert!(require_rust_runtime(&node, &root).await.is_err());
        fs::write(root.join("native-runtime.json"), "{}").unwrap();
        let module = root.join("scripts/native-runtime.mjs");
        // Synthetic producer: exercises the actual Node/module/JSON boundary.
        // Native image/hash validation itself belongs to native-runtime tests.
        for code in [
            "export function runtimeStatus() { throw new Error('fixture invalid hash'); }",
            "export function runtimeStatus() { return {default:'node',codex:'node'}; }",
            "export function runtimeStatus() { return {default:'unconfigured'}; }",
        ] {
            fs::write(&module, code).unwrap();
            assert!(require_rust_runtime(&node, &root)
                .await
                .unwrap_err()
                .to_string()
                .starts_with("launcher_runtime_required"));
        }
        fs::write(&module, "export function runtimeStatus() { return {default:'rust',codex:'rust',claude:'unavailable-provider'}; }").unwrap();
        require_rust_runtime(&node, &root).await.unwrap();
        assert!(!root.join("current.json").exists());
        fs::remove_dir_all(root).unwrap();
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
    #[cfg(windows)]
    fn node_entry_path_drops_only_the_drive_verbatim_prefix() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        assert_eq!(
            node_entry_path(PathBuf::from(r"\\?\C:\a\b.mjs")),
            PathBuf::from(r"C:\a\b.mjs")
        );
        assert_eq!(
            node_entry_path(PathBuf::from(r"C:\a\b.mjs")),
            PathBuf::from(r"C:\a\b.mjs")
        );
        assert_eq!(
            node_entry_path(PathBuf::from(r"\\?\UNC\host\share\b.mjs")),
            PathBuf::from(r"\\?\UNC\host\share\b.mjs")
        );
        for unchanged in [
            r"\\host\share\b.mjs",
            r"\\?\Volume{00000000}\b.mjs",
            r"\\.\C:\b.mjs",
            r"\\?\C:",
            r"\\?\C:\",
            r"\\?\C:/b.mjs",
        ] {
            let path = PathBuf::from(unchanged);
            assert_eq!(
                node_entry_path(path.clone()).into_os_string(),
                path.into_os_string()
            );
        }
        for (input, expected) in [
            (r"\\?\z:\space 中文\b.mjs", r"z:\space 中文\b.mjs"),
            (r"\\?\C:\a\", r"C:\a\"),
        ] {
            assert_eq!(
                node_entry_path(PathBuf::from(input)).into_os_string(),
                std::ffi::OsString::from(expected)
            );
        }
        let mut non_unicode: Vec<u16> = r"\\?\C:\a\".encode_utf16().collect();
        non_unicode.extend([0xd800, 46, 109, 106, 115]);
        let converted = node_entry_path(PathBuf::from(std::ffi::OsString::from_wide(&non_unicode)));
        assert_eq!(
            converted.as_os_str().encode_wide().collect::<Vec<_>>(),
            non_unicode[4..]
        );
    }
    #[test]
    #[cfg(windows)]
    fn windows_embedded_extraction_accepts_long_and_verbatim_roots() {
        use std::os::windows::ffi::OsStrExt;
        let base =
            std::env::temp_dir().join(format!("treer-launcher-long-{}", uuid::Uuid::new_v4()));
        // Secure the short parent first; append enough ordinary components to
        // exceed MAX_PATH, including spaces/Unicode. fs supports that path even
        // when direct ACL APIs do not accept its non-verbatim representation.
        treer_platform::secure_directory(&base).unwrap();
        let mut root = base.join("machine space 中文");
        while root.as_os_str().encode_wide().count() < 300 {
            root.push("long-component-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx");
        }
        fs::create_dir_all(&root).unwrap();
        let canonical = fs::canonicalize(&root).unwrap();
        for supplied in [&root, &canonical] {
            let stage = extract(supplied).unwrap();
            assert!(stage.0.to_string_lossy().starts_with(r"\\?\"));
            let entry = payload_path(&stage.0, "scripts/install.mjs");
            let node_entry = node_entry_path(entry.clone());
            assert!(!node_entry.to_string_lossy().starts_with(r"\\?\"));
            assert_eq!(
                fs::canonicalize(node_entry).unwrap(),
                fs::canonicalize(entry).unwrap()
            );
            for file in FILES {
                let path = payload_path(&stage.0, file.path);
                assert!(!path.as_os_str().to_string_lossy().contains('/'));
                assert_eq!(fs::read(&path).unwrap(), file.bytes);
                // Exercise the same ACL operation on every extracted asset,
                // not just std::fs (which normalizes names independently).
                treer_platform::secure_file(&path).unwrap();
            }
        }
        // Canonicalizing for Win32 spelling must not turn a supplied junction
        // into an accepted physical root. Keep the pre-canonical link check.
        let target = base.join("junction-target");
        fs::create_dir(&target).unwrap();
        let junction = base.join("junction-root");
        let result = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "native junction fixture creation failed"
        );
        assert!(
            extract(&junction).is_err(),
            "junction root must not be adopted"
        );
        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn extraction_is_private_and_rejects_managed_links() {
        let root =
            std::env::temp_dir().join(format!("treer-launcher-test-{}", uuid::Uuid::new_v4()));
        treer_platform::secure_directory(&root).unwrap();
        {
            let stage = extract(&root).unwrap();
            assert_eq!(
                fs::read(payload_path(&stage.0, "scripts/install.mjs")).unwrap(),
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
                    if FILES
                        .iter()
                        .find(|file| file.path == "scripts/install.mjs")
                        .unwrap()
                        .executable
                    {
                        0o700
                    } else {
                        0o600
                    }
                );
                assert_eq!(
                    fs::metadata(stage.0.join("scripts/install.mjs"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o077,
                    0
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
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                // CreateFile needs BACKUP_SEMANTICS for directory handles;
                // SetFileTime needs only WRITE_ATTRIBUTES, not file contents.
                options.access_mode(0x0100).custom_flags(0x0200_0000);
            }
            options
                .open(path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
            assert!(
                fs::metadata(path)
                    .unwrap()
                    .modified()
                    .unwrap()
                    .elapsed()
                    .unwrap()
                    > Duration::from_secs(86400)
            );
        }
        assert!(
            fs::metadata(&recent)
                .unwrap()
                .modified()
                .unwrap()
                .elapsed()
                .unwrap()
                < Duration::from_secs(86400)
        );
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
