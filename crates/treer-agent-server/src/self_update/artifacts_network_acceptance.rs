//! Explicit, non-activating HTTPS acceptance input. No test HTTP server, custom
//! CA, environment proxy, installed discovery, execution or service operation.
use super::*;
use serde::Deserialize;
use std::{io::Read, os::unix::fs::PermissionsExt, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    schema_version: u8,
    source_commit: String,
    platform: String,
    #[serde(deserialize_with = "parse_url")]
    artifact_base: Url,
    #[serde(deserialize_with = "parse_url")]
    enrollment_origin: Url,
    allow_artifact_origin: bool,
    sums_sha256: String,
    controller: Image,
    cli: Image,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    sha256: String,
    size: u64,
}

fn parse_url<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Url, D::Error> {
    let value = String::deserialize(deserializer)?;
    Url::parse(&value).map_err(|_| serde::de::Error::custom("invalid acceptance URL"))
}

impl Input {
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 8192 {
            bail!("network_acceptance_input_invalid");
        }
        let input: Self = serde_json::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("network_acceptance_input_invalid"))?;
        if input.schema_version != 1
            || !hex(&input.source_commit, 40)
            || !hex(&input.sums_sha256, 64)
            || !matches!(
                input.platform.as_str(),
                "linux-x86_64" | "linux-aarch64" | "darwin-x86_64" | "darwin-aarch64"
            )
        {
            bail!("network_acceptance_input_invalid");
        }
        // Unit builds normally allow loopback HTTP. This acceptance entry
        // deliberately applies the PRODUCTION URL rule before constructing
        // the unchanged production client (no alternate trust store/client).
        Distributor::validate_url(&input.artifact_base, false)?;
        Distributor::validate_url(&input.enrollment_origin, false)?;
        for image in [&input.controller, &input.cli] {
            if !hex(&image.sha256, 64) || image.size == 0 || image.size > ARTIFACT_LIMIT {
                bail!("network_acceptance_input_invalid");
            }
        }
        Ok(input)
    }
}

struct Stage(PathBuf);
impl Stage {
    fn create() -> Result<(Self, PrivateDirectory)> {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir()
            .canonicalize()?
            .join(format!("treer-network-stage-{}", uuid::Uuid::new_v4()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        let owned = Self(path);
        let directory = PrivateDirectory::open(&owned.0)?;
        Ok((owned, directory))
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        // Only this invocation's two non-executable leaves; no recursive delete
        // or caller-selected cleanup root. Unexpected entries keep the folder.
        for name in ["treer-agent-server", "treer"] {
            let _ = std::fs::remove_file(self.0.join(name));
        }
        let _ = std::fs::remove_dir(&self.0);
    }
}

#[test]
fn network_input_refuses_test_http_and_unpinned_images() {
    let valid = serde_json::json!({
        "schema_version":1,"source_commit":"a".repeat(40),"platform":"linux-x86_64",
        "artifact_base":"https://artifacts.example/build/linux-x86_64/",
        "enrollment_origin":"https://proxy.example/","allow_artifact_origin":true,
        "sums_sha256":"b".repeat(64),
        "controller":{"sha256":"c".repeat(64),"size":100},
        "cli":{"sha256":"d".repeat(64),"size":50}
    });
    assert!(Input::parse(&serde_json::to_vec(&valid).unwrap()).is_ok());
    for (field, value) in [
        ("artifact_base", serde_json::json!("http://127.0.0.1:1234/")),
        (
            "enrollment_origin",
            serde_json::json!("https://user:secret@proxy.example/"),
        ),
        ("source_commit", serde_json::json!("unbound")),
        (
            "controller",
            serde_json::json!({"sha256":"c".repeat(64),"size":0}),
        ),
        ("cli", serde_json::json!({"sha256":"bad","size":50})),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        assert!(Input::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
}

#[tokio::test]
#[ignore = "requires explicit Release-approved checksum-pinned HTTPS input; downloads only, never activation"]
async fn approved_https_distributor_stages_exact_controller_and_cli_without_activation(
) -> Result<()> {
    // JSON itself is supplied explicitly, never discovered from an enrollment
    // or user configuration file. It contains public artifact metadata only.
    let raw = std::env::var("TREER_UPDATE_NETWORK_INPUT")
        .map_err(|_| anyhow::anyhow!("network_acceptance_input_required"))?;
    let input = Input::parse(raw.as_bytes())?;
    assert!(!crate::controller_update::capability_available());
    let distributor = Distributor::new(
        input.artifact_base,
        &input.enrollment_origin,
        input.allow_artifact_origin,
    )?;
    let checksums = distributor.pinned_checksums(&input.sums_sha256).await?;
    // Independent image pins must agree BEFORE any executable body is fetched.
    for (binary, image) in [
        (Binary::Controller, &input.controller),
        (Binary::Cli, &input.cli),
    ] {
        if checksums.digest(binary) != image.sha256 {
            bail!("network_acceptance_image_pin_mismatch");
        }
    }
    let (stage, directory) = Stage::create()?;
    for (binary, image) in [
        (Binary::Controller, &input.controller),
        (Binary::Cli, &input.cli),
    ] {
        let artifact = distributor
            .stage(&directory, binary, &image.sha256, Some(image.size))
            .await?;
        assert_eq!(artifact.binary(), binary);
        assert_eq!(artifact.size(), image.size);
        assert_eq!(
            artifact.file().metadata()?.permissions().mode() & 0o777,
            0o600
        );
        // Independently hash the returned read-only handle with bounded memory.
        let mut digest = Sha256::new();
        let mut file = artifact.file();
        let mut buffer = [0u8; 32768];
        let mut total = 0;
        loop {
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
            total += n as u64;
        }
        assert_eq!(total, image.size);
        assert_eq!(format!("{:x}", digest.finalize()), image.sha256);
    }
    assert_eq!(std::fs::read_dir(&stage.0)?.count(), 2);
    assert!(!stage.0.join("treer-agent-host").exists());
    drop(directory);
    let path = stage.0.clone();
    drop(stage);
    assert!(!path.exists(), "private download cleanup must be witnessed");
    println!(
        "{}",
        serde_json::json!({
            "schema":"treer.update-network-acceptance/v1","source_commit":input.source_commit,
            "platform":input.platform,"sums_sha256":input.sums_sha256,
            "controller":{"sha256":input.controller.sha256,"size":input.controller.size},
            "cli":{"sha256":input.cli.sha256,"size":input.cli.size},
            "https_verified":true,"private_stage_verified":true,"cleanup_verified":true,
            "activation_executed":false,"native_capability_enabled":false,
            "source_version_executed":false,"proxy_db_composite":false
        })
    );
    Ok(())
}
