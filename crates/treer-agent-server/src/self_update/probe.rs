//! Execute ONLY a previously authenticated image, without inherited credentials,
//! proxies or loader hooks. Version output is bounded, parsed and never logged.
//! This does not prove Host compatibility or restart any service.
use std::{
    path::{Component, Path},
    process::Stdio,
    time::Duration,
};

use anyhow::{bail, Result};
use tokio::{io::AsyncReadExt, process::Command};
use treer_platform::installation_directory::InstallationDirectory;

use super::{artifacts::Binary, hex, images::digest_file};

const OUTPUT_LIMIT: u64 = 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, PartialEq, Eq)]
pub struct ImageVersion {
    pub version: String,
    pub source_sha: String,
}

fn parse(binary: Binary, bytes: &[u8]) -> Result<ImageVersion> {
    if bytes.len() as u64 > OUTPUT_LIMIT {
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
    if !hex(authenticated_sha256, 64)
        || !executable.is_absolute()
        || !executable
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
    {
        bail!("unsafe_update_image_probe");
    }
    let parent = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unsafe_update_image_probe"))?;
    let name = executable
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("unsafe_update_image_probe"))?;
    // Command uses a filesystem path, so verify it still addresses the retained
    // physical parent. Same-account processes remain one trust domain.
    let current = InstallationDirectory::open(parent)
        .map_err(|_| anyhow::anyhow!("unsafe_update_image_probe"))?;
    if current.identity()? != directory.identity()? {
        bail!("unsafe_update_image_probe");
    }
    let file = directory
        .read_file(name)
        .map_err(|_| anyhow::anyhow!("unsafe_update_image_probe"))?;
    if digest_file(&file)? != authenticated_sha256 {
        bail!("update_image_conflict");
    }
    use std::os::unix::fs::PermissionsExt;
    if file.metadata()?.permissions().mode() & 0o100 == 0 {
        bail!("update_image_not_executable");
    }
    let mut child = Command::new(executable)
        .arg("--version")
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(parent)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| anyhow::anyhow!("update_image_probe_spawn_failed"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("update_image_probe_failed"))?;
    let result = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        stdout
            .take(OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| anyhow::anyhow!("update_image_probe_failed"))?;
        if bytes.len() as u64 > OUTPUT_LIMIT {
            bail!("update_image_probe_output_limit");
        }
        let status = child
            .wait()
            .await
            .map_err(|_| anyhow::anyhow!("update_image_probe_failed"))?;
        if !status.success() {
            bail!("update_image_probe_failed");
        }
        parse(binary, &bytes)
    })
    .await;
    let result = match result {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("update_image_probe_timeout")),
    };
    if result.is_err() {
        // Kill/reap our child only. Never kill by name, restart a service or
        // continue activation after timeout, excessive output or malformed data.
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    let version = result?;
    let reopened = directory
        .read_file(name)
        .map_err(|_| anyhow::anyhow!("update_image_conflict"))?;
    if digest_file(&reopened)? != authenticated_sha256 {
        bail!("update_image_conflict");
    }
    Ok(version)
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
