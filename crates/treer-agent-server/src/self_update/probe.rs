//! Execute ONLY a previously authenticated image, without inherited credentials,
//! proxies or loader hooks. Version output is bounded, parsed and never logged.
//! This does not prove Host compatibility or restart any service.
use std::{path::Path, process::Command, time::Duration};

use anyhow::{bail, Result};
use treer_platform::installation_directory::InstallationDirectory;

use super::{artifacts::Binary, hex, verified_image::VerifiedImage};

const OUTPUT_LIMIT: usize = 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

fn check_deadline(deadline: tokio::time::Instant) -> Result<()> {
    if tokio::time::Instant::now() >= deadline {
        bail!("update_image_probe_timeout");
    }
    Ok(())
}

// Synchronous filesystem work cannot be interrupted safely. Account for its
// elapsed time on BOTH sides; a slow disk cannot grant a late probe result or
// a fresh child timeout after authentication consumed the original budget.
pub(super) fn within_budget<T>(
    deadline: tokio::time::Instant,
    work: impl FnOnce() -> Result<T>,
) -> Result<T> {
    check_deadline(deadline)?;
    let result = work();
    check_deadline(deadline)?;
    result
}

#[derive(Clone, PartialEq, Eq)]
pub struct ImageVersion {
    pub version: String,
    pub source_sha: String,
}

fn parse(binary: Binary, bytes: &[u8]) -> Result<ImageVersion> {
    if bytes.len() > OUTPUT_LIMIT {
        bail!("invalid_update_image_version");
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("invalid_update_image_version"))?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.chars().any(char::is_control) {
        bail!("invalid_update_image_version");
    }
    let Some(text) = text
        .strip_prefix(binary.name())
        .and_then(|s| s.strip_prefix(' '))
    else {
        bail!("invalid_update_image_version");
    };
    let Some((version, commit)) = text.split_once(" (") else {
        bail!("invalid_update_image_version");
    };
    let Some(source_sha) = commit.strip_suffix(')') else {
        bail!("invalid_update_image_version");
    };
    if version.is_empty()
        || version.len() > 64
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+'))
        || !version.as_bytes()[0].is_ascii_digit()
        || !hex(source_sha, 40)
    {
        bail!("invalid_update_image_version");
    }
    Ok(ImageVersion {
        version: version.into(),
        source_sha: source_sha.into(),
    })
}

/// Digest must come from authenticated metadata/a trusted explicit checksum pin,
/// never from the image being executed. Hold installation/enrollment locks.
pub async fn version(
    directory: &InstallationDirectory,
    executable: &Path,
    binary: Binary,
    authenticated_sha256: &str,
) -> Result<ImageVersion> {
    run(directory, executable, binary, authenticated_sha256, TIMEOUT).await
}

async fn run(
    directory: &InstallationDirectory,
    executable: &Path,
    binary: Binary,
    authenticated_sha256: &str,
    timeout: Duration,
) -> Result<ImageVersion> {
    let deadline = tokio::time::Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| anyhow::anyhow!("update_image_probe_timeout"))?;
    version_until(
        directory,
        executable,
        binary,
        authenticated_sha256,
        deadline,
    )
    .await
}

/// Internal multi-image preflight shares its original absolute budget with
/// every hash and child, rather than resetting the budget between probes.
pub(super) async fn version_until(
    directory: &InstallationDirectory,
    executable: &Path,
    binary: Binary,
    authenticated_sha256: &str,
    deadline: tokio::time::Instant,
) -> Result<ImageVersion> {
    let image = within_budget(deadline, || {
        VerifiedImage::open(directory, executable, authenticated_sha256)
    })?;
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unsafe_update_image_probe"))?;
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(parent);
    // Keep authentication and path/retained-inode verification directly next
    // to spawn. No ambient identity, loader hook, proxy or PATH survives.
    within_budget(deadline, || image.verify())?;
    let output =
        treer_platform::bounded_child::output_quiescent_until(&mut command, deadline, OUTPUT_LIMIT)
            .await
            .map_err(|error| {
                anyhow::anyhow!(match error.kind() {
                    std::io::ErrorKind::TimedOut => "update_image_probe_timeout",
                    std::io::ErrorKind::FileTooLarge => "update_image_probe_output_limit",
                    _ => "update_image_probe_failed",
                })
            })?;
    // Successful output includes the strict group witness, not only leader
    // exit. Recheck the same physical image BEFORE accepting even valid version
    // text; same-byte replacement with a different inode also fails.
    within_budget(deadline, || image.verify())?;
    if !output.status().success() {
        bail!("update_image_probe_failed");
    }
    within_budget(deadline, || parse(binary, output.stdout()))
}

/// Both installed images must identify the same native build. That build SHA
/// can differ from the release wrapper SHA when native artifacts were reused.
pub fn consistent(controller: &ImageVersion, cli: &ImageVersion) -> Result<()> {
    if controller != cli {
        bail!("update_image_versions_disagree");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        io::Write,
        os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
        path::PathBuf,
    };
    use uuid::Uuid;

    struct Fixture {
        path: PathBuf,
        directory: InstallationDirectory,
    }

    #[test]
    fn synchronous_authentication_cannot_extend_the_probe_deadline_or_grant_a_late_result() {
        let error = within_budget::<()>(tokio::time::Instant::now(), || {
            panic!("expired budget must reject before authentication")
        })
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "update_image_probe_timeout");
        let deadline = tokio::time::Instant::now() + Duration::from_millis(5);
        let error = within_budget(deadline, || {
            // Deterministic stand-in for synchronous hashing blocked on disk.
            // It cannot be preempted, but its completed result must be rejected.
            std::thread::sleep(Duration::from_millis(20));
            Ok(())
        })
        .err()
        .unwrap();
        assert_eq!(error.to_string(), "update_image_probe_timeout");
    }

    #[tokio::test]
    async fn exhausted_total_budget_never_executes_the_authenticated_image() {
        let f = Fixture::new();
        let (path, digest) = f.executable("#!/bin/sh\nprintf executed > sentinel\n");
        let result = run(
            &f.directory,
            &path,
            Binary::Controller,
            &digest,
            Duration::ZERO,
        )
        .await;
        assert_eq!(
            result.err().unwrap().to_string(),
            "update_image_probe_timeout"
        );
        assert!(!f.path.join("sentinel").exists());
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("treer-update-probe-{}", Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            let directory = InstallationDirectory::open(&path).unwrap();
            Self { path, directory }
        }
        fn executable(&self, script: &str) -> (PathBuf, String) {
            let mut file = self.directory.create_file("verified-stage").unwrap();
            file.write_all(script.as_bytes()).unwrap();
            file.sync_all().unwrap();
            file.set_permissions(std::fs::Permissions::from_mode(0o700))
                .unwrap();
            file.sync_all().unwrap();
            drop(file);
            (
                self.path.join("verified-stage"),
                format!("{:x}", Sha256::digest(script.as_bytes())),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }

    #[test]
    fn version_parser_requires_exact_binary_label_full_source_sha_and_one_line() {
        let sha = "a".repeat(40);
        let valid = format!("treer-agent-server 0.1.14 ({sha})\n");
        let version = parse(Binary::Controller, valid.as_bytes()).unwrap();
        assert_eq!(version.source_sha, sha);
        assert!(parse(Binary::Cli, valid.as_bytes()).is_err());
        for invalid in [
            format!("treer-agent-server 0.1.14 ({})", "a".repeat(7)),
            format!("treer-agent-server 0.1.14 ({})", "A".repeat(40)),
            format!("{valid}\n"),
            format!("warning\n{valid}"),
            "treer-agent-server 0.1.14 (unknown)".to_owned(),
            format!("treer-agent-server x;command ({sha})"),
        ] {
            assert!(parse(Binary::Controller, invalid.as_bytes()).is_err());
        }
        assert!(consistent(
            &version,
            &ImageVersion {
                version: version.version.clone(),
                source_sha: "b".repeat(40)
            }
        )
        .is_err());
        assert!(consistent(&version, &version).is_ok());
    }

    #[tokio::test]
    async fn verified_probe_has_no_identity_proxy_or_loader_environment_and_does_not_use_path() {
        let f = Fixture::new();
        let sha = "a".repeat(40);
        let script = format!("#!/bin/sh\n[ \"$#\" = 1 ] && [ \"$1\" = --version ] || exit 1\n[ \"${{TREER_WORKLOAD_CREDENTIAL+x}}${{TREER_OPERATOR_CREDENTIAL+x}}${{HTTPS_PROXY+x}}${{LD_PRELOAD+x}}${{DYLD_INSERT_LIBRARIES+x}}\" = '' ] || exit 1\nprintf '%s\\n' 'treer-agent-server 0.1.14 ({sha})'\n");
        let (path, digest) = f.executable(&script);
        let result = version(&f.directory, &path, Binary::Controller, &digest)
            .await
            .unwrap();
        assert_eq!(result.source_sha, sha);
    }

    #[tokio::test]
    async fn wrong_digest_or_alias_never_executes_even_a_valid_version_script() {
        let f = Fixture::new();
        let (path, digest) =
            f.executable("#!/bin/sh\nprintf executed > sentinel\nprintf invalid\n");
        assert!(
            version(&f.directory, &path, Binary::Controller, &"f".repeat(64))
                .await
                .is_err()
        );
        assert!(!f.path.join("sentinel").exists());
        symlink("verified-stage", f.path.join("alias")).unwrap();
        assert!(version(
            &f.directory,
            &f.path.join("alias"),
            Binary::Controller,
            &digest
        )
        .await
        .is_err());
        assert!(!f.path.join("sentinel").exists());
        let other = Fixture::new();
        assert!(
            version(&other.directory, &path, Binary::Controller, &digest)
                .await
                .is_err()
        );
        assert!(!f.path.join("sentinel").exists());
    }

    #[test]
    fn retained_authentication_rejects_same_bytes_replaced_inode_and_permission_changes() {
        let f = Fixture::new();
        let script = "#!/bin/sh\nprintf executed > sentinel\n";
        let (path, digest) = f.executable(script);
        let image = VerifiedImage::open(&f.directory, &path, &digest).unwrap();
        image.verify().unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            image.verify().err().unwrap().to_string(),
            "update_image_conflict"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        image.verify().unwrap();
        std::fs::rename(&path, f.path.join("original")).unwrap();
        f.executable(script);
        assert_eq!(
            image.verify().err().unwrap().to_string(),
            "update_image_conflict"
        );
        assert!(!f.path.join("sentinel").exists());
    }

    #[tokio::test]
    async fn valid_version_cannot_hide_replacement_of_the_authenticated_inode() {
        let f = Fixture::new();
        let sha = "a".repeat(40);
        // Isolated fixture mimics a same-byte replacement during execution;
        // no race with a real updater or installed executable is induced.
        let script = format!("#!/bin/sh\n/bin/cp \"$0\" replacement && /bin/chmod 700 replacement && /bin/mv replacement \"$0\" || exit 9\nprintf '%s\\n' 'treer-agent-server 0.1.14 ({sha})'\n");
        let (path, digest) = f.executable(&script);
        assert_eq!(
            version(&f.directory, &path, Binary::Controller, &digest)
                .await
                .err()
                .unwrap()
                .to_string(),
            "update_image_conflict"
        );
    }

    #[tokio::test]
    async fn probe_completion_timeout_and_cancellation_prevent_descendant_late_writes() {
        for outcome in ["success", "timeout", "cancel"] {
            let f = Fixture::new();
            let sha = "a".repeat(40);
            let finish = if outcome == "success" {
                format!("printf '%s\\n' 'treer-agent-server 0.1.14 ({sha})'; exit 0")
            } else {
                "while :; do :; done".into()
            };
            let script = format!("#!/bin/sh\n/bin/sh -c '/bin/sleep 0.4; printf leaked > late' &\nprintf ready > ready\n{finish}\n");
            let (path, digest) = f.executable(&script);
            if outcome == "cancel" {
                let directory = InstallationDirectory::open(&f.path).unwrap();
                let task = tokio::spawn(async move {
                    run(&directory, &path, Binary::Controller, &digest, TIMEOUT).await
                });
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !f.path.join("ready").exists() {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                task.abort();
                assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            } else {
                let timeout = if outcome == "timeout" {
                    Duration::from_millis(150)
                } else {
                    TIMEOUT
                };
                let result = run(&f.directory, &path, Binary::Controller, &digest, timeout).await;
                if outcome == "timeout" {
                    assert_eq!(
                        result.err().unwrap().to_string(),
                        "update_image_probe_timeout"
                    );
                } else {
                    assert_eq!(result.unwrap().source_sha, sha);
                }
            }
            tokio::time::sleep(Duration::from_millis(550)).await;
            assert!(!f.path.join("late").exists());
        }
    }

    #[tokio::test]
    async fn excess_output_nonzero_and_timeout_are_bounded_fixed_diagnostics() {
        for (script, expected) in [
            (
                "#!/bin/sh\ni=0; while [ $i -lt 1500 ]; do printf x; i=$((i+1)); done\n",
                "update_image_probe_output_limit",
            ),
            (
                "#!/bin/sh\nprintf private-child-diagnostic >&2; exit 7\n",
                "update_image_probe_failed",
            ),
            (
                "#!/bin/sh\nwhile :; do :; done\n",
                "update_image_probe_timeout",
            ),
        ] {
            let f = Fixture::new();
            let (path, digest) = f.executable(script);
            let deadline = if expected == "update_image_probe_timeout" {
                Duration::from_millis(100)
            } else {
                TIMEOUT
            };
            let result = run(&f.directory, &path, Binary::Controller, &digest, deadline).await;
            assert_eq!(result.err().unwrap().to_string(), expected);
        }
    }
}
