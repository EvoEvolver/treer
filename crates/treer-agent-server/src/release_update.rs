//! Consumer of the signed wire format published by scripts/release-r2.mjs.
use std::{io::Read, path::Path};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{
    pkcs8::{DecodePublicKey, EncodePublicKey},
    Signature, VerifyingKey,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

const MAX_METADATA_BYTES: usize = 1024 * 1024;
const MAX_BINARY_BYTES: usize = 128 * 1024 * 1024;
pub(super) const OFFICIAL_PUBLIC_KEY_PEM: &str =
    include_str!("../../../keys/official-release-ed25519.pub.pem");

#[derive(Deserialize)]
struct DetachedSignature {
    schema_version: u32,
    algorithm: String,
    key_id: String,
    signature: String,
}

#[derive(Deserialize)]
struct Channel {
    schema_version: u32,
    channel: String,
    version: String,
    manifest_path: String,
    manifest_sha256: String,
}

#[derive(Deserialize)]
struct Manifest {
    schema_version: u32,
    version: String,
    signing_key_id: String,
    artifacts: Vec<Artifact>,
}

#[derive(Clone, Deserialize)]
pub(super) struct Artifact {
    binary: String,
    platform: String,
    pub(super) path: String,
    pub(super) sha256: String,
    pub(super) size: usize,
}

/// Produced only after both signatures, channel/manifest digest and exact
/// platform/binary/path bindings have been checked. Consumers may then stream
/// the objects into private files instead of buffering executable bytes.
pub(super) struct AuthenticatedRelease {
    #[cfg(unix)]
    pub(super) version: String,
    pub(super) controller: Artifact,
    pub(super) cli: Artifact,
    #[cfg(any(windows, test))]
    pub(super) manifest_sha256: String,
}

/// Constructed only after authenticating metadata and matching the binary digest.
pub(super) struct VerifiedBinary(Vec<u8>);

impl VerifiedBinary {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.0
    }
}

pub(super) fn load_public_key(path: &Path) -> Result<VerifyingKey> {
    let mut pem = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(16 * 1024 + 1).read_to_string(&mut pem))
        .with_context(|| format!(
        "cannot read trusted release public key {}; provision the publisher's SPKI PEM public key locally or use --public-key (TREER_RELEASE_PUBLIC_KEY)", path.display()
    ))?;
    if pem.len() > 16 * 1024 {
        bail!("release public key exceeds 16 KiB");
    }
    VerifyingKey::from_public_key_pem(&pem)
        .context("release public key must be an Ed25519 SPKI PEM key")
}

/// Select trust before acquiring update locks or staging executable bytes.
/// Existing local configuration is authoritative, including read/parse errors;
/// a missing implicit key alone may use the official distribution's pinned key.
pub(super) fn resolve_public_key(
    base: &Url,
    explicit: Option<&Path>,
    local: &Path,
    official_pem: Option<&str>,
) -> Result<VerifyingKey> {
    let base = normalize_base_url(base.clone())?;
    if let Some(path) = explicit {
        return load_public_key(path);
    }
    match std::fs::symlink_metadata(local) {
        Ok(_) => return load_public_key(local),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "cannot inspect trusted release public key {}",
                    local.display()
                )
            });
        }
    }
    if base.as_str() == "https://releases.treer.ai/" {
        if let Some(pem) = official_pem {
            return VerifyingKey::from_public_key_pem(pem)
                .context("bundled official release public key is invalid");
        }
        bail!("this Controller distribution has no bundled official release key; install an official signed-update-enabled Controller package, or supply a separately trusted publisher key with --public-key PATH (TREER_RELEASE_PUBLIC_KEY); no files were changed");
    }
    bail!("custom release distributors require a separately trusted publisher key: treer-agent-server update --release-base-url {base} --public-key PATH (or TREER_RELEASE_PUBLIC_KEY); no files were changed");
}

fn key_id(key: &VerifyingKey) -> Result<String> {
    // Node's publisher hashes the full SPKI DER, not the raw 32-byte key.
    Ok(digest(key.to_public_key_der()?.as_bytes())[..16].to_owned())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn verify_signed(bytes: &[u8], signature: &[u8], key: &VerifyingKey) -> Result<()> {
    let detached: DetachedSignature =
        serde_json::from_slice(signature).context("invalid release signature document")?;
    if detached.schema_version != 1
        || detached.algorithm != "ed25519"
        || detached.key_id != key_id(key)?
    {
        bail!("release signature format or signing key does not match the trusted key");
    }
    let raw = STANDARD
        .decode(&detached.signature)
        .context("invalid base64 release signature")?;
    let signature = Signature::from_slice(&raw).context("invalid Ed25519 signature length")?;
    key.verify_strict(bytes, &signature)
        .context("release signature verification failed")
}

fn validate_channel_name(channel: &str) -> Result<()> {
    if !channel.starts_with(|c: char| c.is_ascii_lowercase())
        || !channel
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    {
        bail!("invalid release channel; expected lowercase letters, digits and hyphens");
    }
    Ok(())
}

fn validate_channel(channel: &Channel, expected: &str) -> Result<()> {
    let version = &channel.version;
    if channel.schema_version != 1
        || channel.channel != expected
        || !version.starts_with('v')
        || !version.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
        || !version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-')
        || channel.manifest_path != format!("releases/{version}/manifest.json")
    {
        bail!("signed release channel has an invalid version, path or channel binding");
    }
    Ok(())
}

fn select_artifact<'a>(
    manifest: &'a Manifest,
    channel: &Channel,
    key: &VerifyingKey,
    platform: &str,
    binary: &str,
) -> Result<&'a Artifact> {
    if manifest.schema_version != 2
        || manifest.version != channel.version
        || manifest.signing_key_id != key_id(key)?
    {
        bail!("signed release manifest does not match the channel or trusted key");
    }
    let mut matches = manifest
        .artifacts
        .iter()
        .filter(|artifact| artifact.platform == platform && artifact.binary == binary);
    let artifact = matches
        .next()
        .with_context(|| format!("signed manifest does not contain {platform}/{binary}"))?;
    if matches.next().is_some()
        || artifact.path != format!("releases/{}/{platform}/{binary}", channel.version)
        || artifact.size == 0
        || artifact.size > MAX_BINARY_BYTES
        || artifact.sha256.len() != 64
        || !artifact
            .sha256
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        bail!("signed manifest has an invalid or duplicate {platform}/{binary} artifact");
    }
    Ok(artifact)
}

pub(super) fn normalize_base_url(mut base: Url) -> Result<Url> {
    let loopback = match base.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if base.scheme() != "https" && !(base.scheme() == "http" && loopback) {
        bail!("release distributor must use HTTPS (HTTP is allowed only for loopback tests)");
    }
    if !base.username().is_empty() || base.password().is_some() {
        bail!("release URL must not contain credentials");
    }
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.set_query(None);
    base.set_fragment(None);
    Ok(base)
}

async fn fetch(client: &reqwest::Client, url: Url, limit: usize) -> Result<Vec<u8>> {
    let mut response = client
        .get(url.clone())
        .header("cache-control", "no-cache")
        .send()
        .await
        .with_context(|| format!("failed to fetch signed release object {url}"))?;
    if !response.status().is_success() {
        bail!("signed release object {url} returned {}; unsigned Proxy artifacts cannot be used for updates", response.status());
    }
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        bail!("release object {url} exceeds its size limit");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            bail!("release object {url} exceeds its size limit");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        bail!("release object {url} is empty");
    }
    Ok(bytes)
}

async fn fetch_signed(
    client: &reqwest::Client,
    base: &Url,
    path: &str,
    key: &VerifyingKey,
) -> Result<Vec<u8>> {
    let (bytes, signature) = tokio::try_join!(
        fetch(client, base.join(path)?, MAX_METADATA_BYTES),
        fetch(
            client,
            base.join(&format!("{path}.sig"))?,
            MAX_METADATA_BYTES
        ),
    )?;
    verify_signed(&bytes, &signature, key)?;
    Ok(bytes)
}

async fn fetch_binary(
    client: &reqwest::Client,
    base: &Url,
    artifact: &Artifact,
) -> Result<VerifiedBinary> {
    let bytes = fetch(client, base.join(&artifact.path)?, artifact.size).await?;
    if bytes.len() != artifact.size || digest(&bytes) != artifact.sha256 {
        bail!("{} failed signed size/SHA256 verification", artifact.binary);
    }
    Ok(VerifiedBinary(bytes))
}

pub(super) async fn authenticate(
    client: &reqwest::Client,
    base: Url,
    channel_name: &str,
    key: &VerifyingKey,
    platform: &str,
) -> Result<AuthenticatedRelease> {
    let base = normalize_base_url(base)?;
    validate_channel_name(channel_name)?;
    let channel_bytes =
        fetch_signed(client, &base, &format!("channels/{channel_name}.json"), key).await?;
    let channel: Channel =
        serde_json::from_slice(&channel_bytes).context("invalid signed release channel")?;
    validate_channel(&channel, channel_name)?;
    let manifest_bytes = fetch_signed(client, &base, &channel.manifest_path, key).await?;
    authenticated_manifest(&channel, &manifest_bytes, key, platform)
}

fn authenticated_manifest(
    channel: &Channel,
    manifest_bytes: &[u8],
    key: &VerifyingKey,
    platform: &str,
) -> Result<AuthenticatedRelease> {
    if digest(manifest_bytes) != channel.manifest_sha256 {
        bail!("release manifest SHA256 does not match the signed channel");
    }
    let manifest: Manifest =
        serde_json::from_slice(manifest_bytes).context("invalid signed release manifest")?;
    let controller = select_artifact(&manifest, channel, key, platform, "treer-agent-server")?;
    let cli = select_artifact(&manifest, channel, key, platform, "treer")?;
    Ok(AuthenticatedRelease {
        #[cfg(unix)]
        version: channel.version.clone(),
        controller: controller.clone(),
        cli: cli.clone(),
        #[cfg(any(windows, test))]
        manifest_sha256: digest(manifest_bytes),
    })
}

/// Offline admission of independently supplied signed documents. Reuses the
/// same signature, channel, manifest and artifact binding checks as networking.
/// The key is separately trusted input, never read from either document.
#[cfg(any(windows, test))]
pub(super) fn authenticate_documents(
    channel_bytes: &[u8],
    channel_signature: &[u8],
    manifest_bytes: &[u8],
    manifest_signature: &[u8],
    channel_name: &str,
    key: &VerifyingKey,
    platform: &str,
) -> Result<AuthenticatedRelease> {
    if [
        channel_bytes,
        channel_signature,
        manifest_bytes,
        manifest_signature,
    ]
    .iter()
    .any(|bytes| bytes.len() > MAX_METADATA_BYTES)
    {
        bail!("signed update metadata exceeds limit");
    }
    validate_channel_name(channel_name)?;
    verify_signed(channel_bytes, channel_signature, key)?;
    let channel: Channel = serde_json::from_slice(channel_bytes)?;
    validate_channel(&channel, channel_name)?;
    verify_signed(manifest_bytes, manifest_signature, key)?;
    authenticated_manifest(&channel, manifest_bytes, key, platform)
}

pub(super) async fn download(
    client: &reqwest::Client,
    base: Url,
    channel_name: &str,
    key: &VerifyingKey,
    platform: &str,
) -> Result<(VerifiedBinary, VerifiedBinary)> {
    let base = normalize_base_url(base)?;
    let release = authenticate(client, base.clone(), channel_name, key, platform).await?;
    tokio::try_join!(
        fetch_binary(client, &base, &release.controller),
        fetch_binary(client, &base, &release.cli)
    )
}

#[cfg(test)]
#[path = "release_update_tests.rs"]
mod tests;
