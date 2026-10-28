//! Controller self-update: download a signed release, replace the installed
//! binaries in place (keeping one `<name>.old` for manual rollback), then ask
//! the Host for a plain Controller restart. The replacement Controller's
//! reported version is the completion signal; nothing here is persisted.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use anyhow::{Context, Result};
use ed25519_dalek::VerifyingKey;
use treer_host_protocol::HostCommand;
use treer_protocol::controller_update::{valid_version, UpdateState, UpdateStatus};
use treer_protocol::ProtocolError;
use url::Url;
use uuid::Uuid;

use crate::host_client::HostClient;
use crate::release_update::{self, VerifiedRelease};

pub(crate) const DEFAULT_RELEASE_BASE_URL: &str = "https://releases.treer.ai/";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

static STATE: Mutex<(UpdateState, Option<String>)> = Mutex::new((UpdateState::Idle, None));

/// Release artifact platform for this build, if releases ship one.
pub(crate) fn release_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("macos", "x86_64") => Some("darwin-x86_64"),
        ("macos", "aarch64") => Some("darwin-aarch64"),
        ("windows", "x86_64") => Some("windows-x86_64"),
        _ => None,
    }
}

pub(crate) fn capability_available() -> bool {
    release_platform().is_some()
}

pub(crate) fn status() -> UpdateStatus {
    let state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
    UpdateStatus {
        state: state.0.clone(),
        target_version: state.1.clone(),
        controller_version: Some(treer_build_info::VERSION.to_string()),
    }
}

fn set(state: UpdateState) {
    STATE.lock().unwrap_or_else(|poison| poison.into_inner()).0 = state;
}

/// Begin updating to `version` in the background; one update at a time.
pub(crate) fn start(version: String, host: HostClient) -> Result<UpdateStatus, ProtocolError> {
    if !valid_version(&version) {
        return Err(ProtocolError::new(
            "controller_update_invalid_version",
            "version must be a release tag such as v1.2.3",
        ));
    }
    {
        let mut state = STATE.lock().unwrap_or_else(|poison| poison.into_inner());
        if state.0.active() {
            return Err(ProtocolError::new(
                "controller_update_busy",
                "a Controller update is already in progress",
            ));
        }
        *state = (UpdateState::Downloading, Some(version.clone()));
    }
    tokio::spawn(async move {
        let outcome = async {
            let platform =
                release_platform().context("no release artifacts exist for this platform")?;
            let base = release_base_url()?;
            let explicit = std::env::var_os("TREER_RELEASE_PUBLIC_KEY").map(PathBuf::from);
            let key = release_update::trusted_key(&base, explicit.as_deref())?;
            run(
                Job {
                    version,
                    base,
                    key,
                    platform,
                    binaries: InstalledBinaries::current()?,
                },
                &host,
            )
            .await
        }
        .await;
        if let Err(error) = outcome {
            tracing::warn!(error = %format!("{error:#}"), "Controller update failed");
            set(UpdateState::Failed {
                reason: format!("{error:#}"),
            });
        }
    });
    Ok(status())
}

pub(crate) struct Job {
    pub(crate) version: String,
    pub(crate) base: Url,
    pub(crate) key: VerifyingKey,
    pub(crate) platform: &'static str,
    pub(crate) binaries: InstalledBinaries,
}

/// Download → verify → replace → ask the Host to restart this Controller.
async fn run(job: Job, host: &HostClient) -> Result<()> {
    let release = release_update::download(
        &http_client()?,
        job.base,
        &job.version,
        &job.key,
        job.platform,
    )
    .await?;
    set(UpdateState::Installing);
    let binaries = job.binaries;
    tokio::task::spawn_blocking(move || binaries.install(&release)).await??;
    set(UpdateState::Restarting);
    let operation = format!("controller_update_{}", Uuid::new_v4().simple());
    host.request(HostCommand::RestartController, Some(operation))
        .await
        .context(
            "installed, but the Host restart request failed; restart the Controller to activate",
        )?;
    Ok(())
}

fn release_base_url() -> Result<Url> {
    let value = std::env::var("TREER_RELEASE_BASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASE_BASE_URL.to_string());
    Url::parse(&value).context("invalid TREER_RELEASE_BASE_URL")
}

pub(crate) fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        // Agent terminals inherit Treer's workload HTTP proxy. Updating the
        // Controller must not depend on the data plane it is replacing.
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("failed to create update client")
}

/// Installed executables updated together. The Host is replaced on disk only;
/// it runs the new image after its own next restart.
pub(crate) struct InstalledBinaries {
    pub(crate) controller: PathBuf,
    pub(crate) cli: Option<PathBuf>,
    pub(crate) host: Option<PathBuf>,
}

impl InstalledBinaries {
    pub(crate) fn current() -> Result<Self> {
        let controller = std::env::current_exe()
            .and_then(|path| path.canonicalize())
            .context("failed to resolve the installed Controller executable")?;
        let host =
            controller.with_file_name(format!("treer-agent-host{}", std::env::consts::EXE_SUFFIX));
        Ok(Self {
            cli: crate::service::installed_treer_binary(&controller),
            host: host.is_file().then_some(host),
            controller,
        })
    }

    /// Stage every changed binary first, then replace them. A failed
    /// replacement restores the binaries already replaced from `.old`.
    /// Returns the replaced destinations.
    pub(crate) fn install(&self, release: &VerifiedRelease) -> Result<Vec<PathBuf>> {
        let mut planned = Vec::new();
        if let Some(cli) = &self.cli {
            planned.push((cli, release.cli.as_slice()));
        }
        if let (Some(host), Some(bytes)) = (&self.host, &release.host) {
            planned.push((host, bytes.as_slice()));
        }
        // Last: the running image is the one most worth keeping on failure.
        planned.push((&self.controller, release.controller.as_slice()));
        let mut staged = Vec::new();
        for (destination, bytes) in planned {
            if fs::read(destination).is_ok_and(|current| current == bytes) {
                continue;
            }
            staged.push((destination, Staged::write(destination, bytes)?));
        }
        let mut replaced: Vec<PathBuf> = Vec::new();
        for (destination, staged) in staged {
            if let Err(error) = staged.replace(destination) {
                for done in replaced.iter().rev() {
                    let _ = restore_previous(done);
                }
                return Err(error);
            }
            replaced.push(destination.to_path_buf());
        }
        Ok(replaced)
    }
}

pub(crate) fn previous_path(destination: &Path) -> PathBuf {
    let mut name = destination.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    destination.with_file_name(name)
}

/// Move `<name>.old` back over `<name>` (manual rollback does the same).
pub(crate) fn restore_previous(destination: &Path) -> Result<()> {
    fs::rename(previous_path(destination), destination)
        .with_context(|| format!("failed to restore {}", destination.display()))
}

/// A fully written, synced executable next to its destination.
struct Staged(PathBuf);

impl Staged {
    fn write(destination: &Path, bytes: &[u8]) -> Result<Self> {
        use std::io::Write;
        let name = destination
            .file_name()
            .context("executable path has no file name")?
            .to_string_lossy();
        let path = destination.with_file_name(format!(".{name}.new-{}", Uuid::new_v4().simple()));
        let staged = Self(path);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o755);
        let mut file = options
            .open(&staged.0)
            .with_context(|| format!("failed to stage {}", destination.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .with_context(|| format!("failed to stage {}", destination.display()))?;
        Ok(staged)
    }

    /// Unix: keep a link to the current image as `.old`, then rename over it.
    /// Windows: a running image cannot be replaced but can be renamed, so
    /// move it to `.old` first and then move the new image into place.
    fn replace(self, destination: &Path) -> Result<()> {
        let previous = previous_path(destination);
        discard(&previous)?;
        #[cfg(unix)]
        {
            fs::hard_link(destination, &previous)
                .or_else(|_| fs::copy(destination, &previous).map(|_| ()))
                .with_context(|| format!("failed to keep {}", previous.display()))?;
            fs::rename(&self.0, destination)
                .with_context(|| format!("failed to replace {}", destination.display()))?;
        }
        #[cfg(not(unix))]
        {
            fs::rename(destination, &previous)
                .with_context(|| format!("failed to move aside {}", destination.display()))?;
            if let Err(error) = fs::rename(&self.0, destination) {
                let _ = fs::rename(&previous, destination);
                return Err(error)
                    .with_context(|| format!("failed to replace {}", destination.display()));
            }
        }
        if let Some(parent) = destination.parent() {
            if let Ok(directory) = fs::File::open(parent) {
                let _ = directory.sync_all();
            }
        }
        Ok(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Remove the older `.old`. On Windows it may still be running (a Host not
/// yet restarted); a running image can be renamed out of the way instead.
fn discard(previous: &Path) -> Result<()> {
    match fs::remove_file(previous) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => {
            let name = previous.file_name().unwrap_or_default().to_string_lossy();
            let stale =
                previous.with_file_name(format!(".{name}.stale-{}", Uuid::new_v4().simple()));
            fs::rename(previous, &stale)
                .with_context(|| format!("failed to remove {}", previous.display()))?;
            let _ = fs::remove_file(stale);
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "controller_update_tests.rs"]
mod tests;
