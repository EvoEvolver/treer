//! `treer-launch`, the Agent launcher, ships in the signed release next to
//! `treer`, the Host and the Controller. Machine installs and Controller
//! updates place it next to `treer`; this command repairs or updates only it.
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde_json::json;
use url::Url;

use crate::controller_update::{self, LAUNCHER};

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
    // Accepted and ignored: invocations saved by the pre-Rust installer.
    #[arg(long, global = true, hide = true)]
    node: Option<String>,
    #[arg(long, global = true, hide = true)]
    treer: Option<String>,
    #[arg(long, global = true, hide = true)]
    workspace: Option<String>,
    #[arg(long, global = true, hide = true)]
    url: Option<String>,
    #[command(subcommand)]
    command: LauncherCommand,
}

#[derive(Debug, Subcommand)]
enum LauncherCommand {
    /// Install or update treer-launch (and the deprecated treer-agent alias) from
    /// the signed release; idempotent. Running Agents are not restarted.
    #[command(visible_alias = "update")]
    Install(Box<InstallArgs>),
    /// Show where treer-launch is installed and which version it reports.
    Status {
        /// Directory holding treer-launch (default: next to the installed treer).
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// Directory to install into (default: next to the installed treer CLI).
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Install this local treer-launch build instead of downloading one.
    #[arg(long, conflicts_with_all = ["version", "channel"])]
    from: Option<PathBuf>,
    /// Exact release tag (default: this Controller's own version).
    #[arg(long)]
    version: Option<String>,
    /// Follow a signed release channel instead of this Controller's version.
    #[arg(long, conflicts_with = "version")]
    channel: Option<String>,
    /// Signed release distributor URL.
    #[arg(
        long,
        env = "TREER_RELEASE_BASE_URL",
        default_value = controller_update::DEFAULT_RELEASE_BASE_URL
    )]
    release_base_url: Url,
    /// Override the release trust with a local SPKI Ed25519 PEM key.
    #[arg(long, env = "TREER_RELEASE_PUBLIC_KEY")]
    public_key: Option<PathBuf>,
    // Accepted and ignored: profiles are workspace defaults, not installer state.
    #[arg(long, hide = true)]
    require_runtime: Option<String>,
    #[arg(long = "agent", hide = true)]
    agents: Vec<String>,
    #[arg(long, hide = true)]
    cwd: Option<String>,
    #[arg(long, hide = true)]
    allow_network: bool,
    #[arg(long, hide = true)]
    npm_cli: Option<PathBuf>,
}

fn launcher_in(dir: Option<PathBuf>) -> Result<PathBuf> {
    match dir {
        Some(dir) => Ok(dir.join(format!("{LAUNCHER}{}", std::env::consts::EXE_SUFFIX))),
        None => {
            let controller = std::env::current_exe()
                .and_then(|path| path.canonicalize())
                .context("failed to resolve this Controller executable")?;
            Ok(controller_update::launcher_path(&controller))
        }
    }
}

/// `treer-launch --version`, if the file runs.
fn reported_version(launcher: &Path) -> Option<String> {
    let output = std::process::Command::new(launcher)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (output.status.success() && text.starts_with("treer-launch ")).then(|| text.to_owned())
}

pub async fn run(args: LauncherArgs) -> Result<()> {
    match args.command {
        LauncherCommand::Status { dir } => {
            let launcher = launcher_in(dir)?;
            let alias = launcher.with_file_name(format!(
                "{}{}",
                controller_update::LEGACY_LAUNCHER,
                std::env::consts::EXE_SUFFIX
            ));
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "path": launcher,
                    "installed": launcher.is_file(),
                    "version": reported_version(&launcher),
                    "alias": alias.exists().then_some(alias),
                    "controller_version": treer_build_info::DISPLAY,
                }))?
            );
            Ok(())
        }
        LauncherCommand::Install(options) => install(*options).await,
    }
}

async fn install(options: InstallArgs) -> Result<()> {
    let launcher = launcher_in(options.dir)?;
    let directory = launcher
        .parent()
        .context("launcher path has no directory")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let (bytes, expected) = match &options.from {
        Some(path) => (
            std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?,
            None,
        ),
        None => {
            let key = crate::release_update::trusted_key(
                &options.release_base_url,
                options.public_key.as_deref(),
            )?;
            let client = controller_update::http_client()?;
            let version = match (&options.version, &options.channel) {
                (Some(version), _) => version.clone(),
                (None, Some(channel)) => {
                    crate::release_update::channel_version(
                        &client,
                        options.release_base_url.clone(),
                        channel,
                        &key,
                    )
                    .await?
                }
                (None, None) => format!("v{}", treer_build_info::VERSION),
            };
            let platform = controller_update::release_platform()
                .context("no release artifacts exist for this platform")?;
            eprintln!(
                "treer: downloading signed {version} {LAUNCHER} from {}",
                options.release_base_url
            );
            let bytes = crate::release_update::download_launcher(
                &client,
                options.release_base_url.clone(),
                &version,
                &key,
                platform,
            )
            .await?;
            (bytes, Some(version))
        }
    };
    let changed = controller_update::replace_executable(&launcher, &bytes)?;
    let Some(reported) = reported_version(&launcher) else {
        if changed {
            let _ = controller_update::restore_previous(&launcher);
        }
        bail!(
            "{} did not run; the previous launcher was restored",
            launcher.display()
        );
    };
    if let Some(version) = &expected {
        let number = version.trim_start_matches('v');
        if reported.split_whitespace().nth(1) != Some(number) {
            if changed {
                let _ = controller_update::restore_previous(&launcher);
            }
            bail!("installed launcher reports {reported:?}, expected {version}; the previous launcher was restored");
        }
    }
    let alias = controller_update::link_legacy_launcher(&launcher)?;
    if !alias {
        eprintln!("treer: warning: an unrelated treer-agent script exists next to {}; saved profiles that call treer-agent will not use the launcher", launcher.display());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "path": launcher,
            "version": reported,
            "changed": changed,
            "legacy_alias": alias,
            "running_agents": "unchanged",
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[cfg(unix)]
    struct TempDir(PathBuf);
    #[cfg(unix)]
    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    #[cfg(unix)]
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[cfg(unix)]
    fn tempdir() -> TempDir {
        let path = std::env::temp_dir().join(format!(
            "treer-launcher-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir(&path).unwrap();
        TempDir(path.canonicalize().unwrap())
    }

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        launcher: LauncherArgs,
    }

    #[test]
    fn saved_pre_rust_invocations_still_parse() {
        for argv in [
            vec![
                "x",
                "--node",
                "/usr/bin/node",
                "--treer",
                "/bin/treer",
                "install",
                "--agent",
                "codex",
                "--cwd",
                ".",
            ],
            vec![
                "x",
                "--node",
                "/usr/bin/node",
                "update",
                "--require-runtime",
                "rust",
            ],
            vec![
                "x",
                "install",
                "--from",
                "/tmp/treer-launch",
                "--dir",
                "/tmp/bin",
            ],
            vec!["x", "update", "--version", "v1.2.3"],
            vec!["x", "status"],
        ] {
            assert!(Cli::try_parse_from(argv.clone()).is_ok(), "{argv:?}");
        }
        assert!(Cli::try_parse_from(["x", "install", "--from", "/a", "--version", "v1"]).is_err());
        assert!(Cli::try_parse_from(["x", "rollback"]).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn local_install_replaces_launcher_and_old_node_shim_alias() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempdir();
        let dir = root.path().join("bin");
        std::fs::create_dir(&dir).unwrap();
        // A pre-Rust Node shim and its receipt.
        std::fs::write(
            dir.join("treer-agent"),
            "#!/bin/sh\n# treer-agent-shim/v1\nexec node run.mjs \"$@\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("treer-agent.json"), "{}").unwrap();
        let build = root.path().join("build-launch");
        std::fs::write(&build, "#!/bin/sh\necho 'treer-launch 9.9.9 (abc)'\n").unwrap();
        std::fs::set_permissions(&build, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cli = Cli::try_parse_from([
            "x",
            "install",
            "--from",
            build.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ])
        .unwrap();
        run(cli.launcher).await.unwrap();
        assert_eq!(
            std::fs::read(dir.join("treer-launch")).unwrap(),
            std::fs::read(&build).unwrap()
        );
        assert_eq!(
            std::fs::read_link(dir.join("treer-agent")).unwrap(),
            Path::new("treer-launch")
        );
        assert!(!dir.join("treer-agent.json").exists());
        // Idempotent.
        let cli = Cli::try_parse_from([
            "x",
            "install",
            "--from",
            build.to_str().unwrap(),
            "--dir",
            dir.to_str().unwrap(),
        ])
        .unwrap();
        run(cli.launcher).await.unwrap();
        // A user's own script is kept.
        std::fs::remove_file(dir.join("treer-agent")).unwrap();
        std::fs::write(dir.join("treer-agent"), "#!/bin/sh\necho mine\n").unwrap();
        assert!(!controller_update::link_legacy_launcher(&dir.join("treer-launch")).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.join("treer-agent")).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_launcher_that_does_not_run_is_rolled_back() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempdir();
        let good = root.path().join("good");
        std::fs::write(&good, "#!/bin/sh\necho 'treer-launch 1.0.0 (abc)'\n").unwrap();
        let bad = root.path().join("bad");
        std::fs::write(&bad, "#!/bin/sh\nexit 3\n").unwrap();
        for path in [&good, &bad] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let dir = root.path().to_str().unwrap().to_owned();
        for (file, ok) in [(&good, true), (&bad, false)] {
            let cli = Cli::try_parse_from([
                "x",
                "install",
                "--from",
                file.to_str().unwrap(),
                "--dir",
                &dir,
            ])
            .unwrap();
            assert_eq!(run(cli.launcher).await.is_ok(), ok);
        }
        assert_eq!(
            std::fs::read(root.path().join("treer-launch")).unwrap(),
            std::fs::read(&good).unwrap()
        );
    }
}
