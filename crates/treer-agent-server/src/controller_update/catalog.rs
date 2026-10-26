//! Shared local-catalog validation only; data is not caller authorization,
//! signed input admission, an installation selection or execution permission.
use anyhow::{bail, Result};
use serde::Deserialize;
use treer_protocol::{controller_update as wire, controller_update_runtime::DispatchBinding};

pub(super) const LIMIT: u64 = 64 * 1024;

/// Local operator authorization, not a publisher signature or browser choice.
/// Same-release/downgrade overrides intentionally do not exist in this format.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    #[serde(skip)]
    pub(super) digest: String,
    schema_version: u8,
    pub(crate) revision: u64,
    pub(crate) accept_shared_binaries: bool,
    pub(crate) entries: Vec<Entry>,
    /// Normal local settings for the signed Windows consumer. Not command data.
    #[serde(default)]
    pub(crate) signed_release: Option<SignedRelease>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SignedRelease {
    #[serde(deserialize_with = "deserialize_url")]
    pub(crate) base_url: url::Url,
    pub(crate) channel: String,
    pub(crate) public_key: Option<std::path::PathBuf>,
    pub(crate) confirmed_shared_servers: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub(crate) release_sequence: u64,
    pub(crate) target: wire::Target,
    pub(crate) sums_sha256: String,
    #[serde(deserialize_with = "deserialize_url")]
    pub(crate) artifact_base: url::Url,
    pub(crate) allow_pinned: bool,
    pub(crate) allow_artifact_origin: bool,
}
fn deserialize_url<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<url::Url, D::Error> {
    url::Url::parse(&String::deserialize(d)?)
        .map_err(|_| serde::de::Error::custom("invalid artifact origin"))
}
fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn platform() -> Result<wire::Platform> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(wire::Platform::LinuxX86_64),
        ("linux", "aarch64") => Ok(wire::Platform::LinuxAarch64),
        ("macos", "x86_64") => Ok(wire::Platform::DarwinX86_64),
        ("macos", "aarch64") => Ok(wire::Platform::DarwinAarch64),
        ("windows", "x86_64") => Ok(wire::Platform::WindowsX86_64),
        _ => bail!("controller_update_unsupported"),
    }
}
impl Catalog {
    #[cfg(any(windows, test))]
    pub(crate) fn raw_digest(&self) -> &str {
        &self.digest
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self> {
        Self::parse_for(bytes, platform()?)
    }
    fn parse_for(bytes: &[u8], expected: wire::Platform) -> Result<Self> {
        if bytes.len() as u64 > LIMIT {
            bail!("controller_update_catalog_invalid");
        }
        let mut catalog: Self = serde_json::from_slice(bytes)?;
        if catalog.schema_version != 1
            || catalog.revision == 0
            || catalog.revision >= 1 << 53
            || catalog.entries.is_empty()
            || catalog.entries.len() > wire::MAX_TARGETS + 1
        {
            bail!("controller_update_catalog_invalid");
        }
        if let Some(settings) = &catalog.signed_release {
            let base = &settings.base_url;
            let test_loopback = cfg!(test)
                && base.scheme() == "http"
                && matches!(base.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback());
            if (base.scheme() != "https" && !test_loopback)
                || base.host_str().is_none()
                || !base.username().is_empty()
                || base.password().is_some()
                || base.query().is_some()
                || base.fragment().is_some()
                || !base.path().ends_with('/')
                || settings.channel.len() > 64
                || crate::release_update::validate_channel_name(&settings.channel).is_err()
                || settings
                    .public_key
                    .as_ref()
                    .is_some_and(|p| !p.is_absolute())
                || settings.confirmed_shared_servers.len() > 1024
                || settings.confirmed_shared_servers.iter().any(|s| {
                    !s.strip_prefix("srv_").is_some_and(|id| {
                        !id.is_empty()
                            && id.len() <= 128
                            && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    })
                })
                || settings
                    .confirmed_shared_servers
                    .windows(2)
                    .any(|p| p[0] >= p[1])
            {
                bail!("controller_update_catalog_invalid");
            }
        }
        let mut sequences = std::collections::HashSet::new();
        let mut ids = std::collections::HashSet::new();
        let mut identities = std::collections::HashSet::new();
        for entry in &catalog.entries {
            entry
                .target
                .validate()
                .map_err(|_| anyhow::anyhow!("controller_update_catalog_invalid"))?;
            let base = &entry.artifact_base;
            if entry.target.platform != expected
                || entry.release_sequence == 0
                || entry.release_sequence >= 1 << 53
                || !entry.allow_pinned
                || !hex(&entry.sums_sha256, 64)
                || base.scheme() != "https"
                || base.host_str().is_none()
                || !base.username().is_empty()
                || base.password().is_some()
                || base.query().is_some()
                || base.fragment().is_some()
                || !base.path().ends_with('/')
                || !sequences.insert(entry.release_sequence)
                || !ids.insert(&entry.target.target_id)
                || !identities.insert(serde_json::to_string(&entry.target.identity)?)
            {
                bail!("controller_update_catalog_invalid");
            }
        }
        use sha2::{Digest, Sha256};
        catalog.digest = format!("{:x}", Sha256::digest(bytes));
        Ok(catalog)
    }
    /// Match the complete confirmed plan against independently provisioned
    /// catalog records. This does not authenticate the caller, select an
    /// installation or validate the installed/signed bytes.
    pub(crate) fn select_plan(&self, binding: &DispatchBinding) -> Result<(&Entry, &Entry)> {
        let current = self
            .entries
            .iter()
            .find(|entry| entry.target.identity == binding.current)
            .ok_or_else(|| anyhow::anyhow!("controller_update_current_unknown"))?;
        Ok((current, self.target(binding, current)?))
    }

    pub(crate) fn target<'a>(
        &'a self,
        binding: &DispatchBinding,
        current: &Entry,
    ) -> Result<&'a Entry> {
        if !self
            .entries
            .iter()
            .any(|entry| std::ptr::eq(entry, current))
            || !binding.validate()
            || binding.catalog_revision != self.revision
            || binding.current != current.target.identity
            || binding.plan_digest != binding.expected_plan_digest()?
        {
            bail!("controller_update_preview_changed");
        }
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.target == binding.target)
            .ok_or_else(|| anyhow::anyhow!("controller_update_target_unavailable"))?;
        if entry.release_sequence <= current.release_sequence {
            bail!("controller_update_target_not_upgrade");
        }
        Ok(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn windows_update_consumer_normal_signed_configuration_is_explicit_and_strict() {
        let mut doc = document(wire::Platform::WindowsX86_64);
        assert!(
            parse_windows(&doc).unwrap().signed_release.is_none(),
            "legacy config never guesses a signed publisher"
        );
        doc["signed_release"] = json!({"base_url":"https://releases.treer.ai/","channel":"stable","public_key":null,"confirmed_shared_servers":["srv_a","srv_b"]});
        assert!(parse_windows(&doc).is_ok());
        for (field, bad) in [
            ("base_url", json!("https://user:secret@example.test/")),
            ("channel", json!("../canary")),
            ("public_key", json!("relative.pem")),
            ("confirmed_shared_servers", json!(["srv_b", "srv_a"])),
            ("confirmed_shared_servers", json!(["srv_a", "srv_a"])),
        ] {
            let mut bad_doc = doc.clone();
            bad_doc["signed_release"][field] = bad;
            assert!(parse_windows(&bad_doc).is_err(), "{field}");
        }
    }

    fn document(platform: wire::Platform) -> Value {
        let entry = |sequence, id: &str, hash: &str| {
            json!({
                "release_sequence": sequence,
                "target": {"target_id": id, "identity": {
                    "release_sha": hash.repeat(40), "artifact_source_sha": hash.repeat(40),
                    "controller_sha256": hash.repeat(64), "cli_sha256": "c".repeat(64)
                }, "platform": platform, "version_label": id},
                "sums_sha256": "d".repeat(64),
                "artifact_base": "https://artifacts.example.test/fixed/",
                "allow_pinned": true, "allow_artifact_origin": false
            })
        };
        json!({"schema_version":1,"revision":2,"accept_shared_binaries":false,
            "entries":[entry(1,"release-a","a"),entry(2,"release-b","b")]})
    }

    fn parse_windows(doc: &Value) -> Result<Catalog> {
        Catalog::parse_for(&serde_json::to_vec(doc)?, wire::Platform::WindowsX86_64)
    }

    fn binding(catalog: &Catalog) -> DispatchBinding {
        let mut binding = DispatchBinding {
            operation_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            current: catalog.entries[0].target.identity.clone(),
            target: catalog.entries[1].target.clone(),
            catalog_revision: catalog.revision,
            plan_digest: "0".repeat(64),
            created_at: chrono::Utc::now(),
        };
        binding.plan_digest = binding.expected_plan_digest().unwrap();
        binding
    }

    #[test]
    fn windows_update_catalog_platform_is_host_selected_not_document_selected() {
        for expected in [
            wire::Platform::LinuxX86_64,
            wire::Platform::LinuxAarch64,
            wire::Platform::DarwinX86_64,
            wire::Platform::DarwinAarch64,
            wire::Platform::WindowsX86_64,
        ] {
            let doc = document(expected);
            let bytes = serde_json::to_vec(&doc).unwrap();
            assert!(Catalog::parse_for(&bytes, expected).is_ok());
            assert_eq!(
                Catalog::parse(&bytes).is_ok(),
                expected == platform().unwrap()
            );
            let foreign = if expected == wire::Platform::WindowsX86_64 {
                wire::Platform::LinuxX86_64
            } else {
                wire::Platform::WindowsX86_64
            };
            assert!(Catalog::parse_for(&bytes, foreign).is_err());
        }
        assert!(!super::super::capability_available());
    }

    #[test]
    fn windows_update_catalog_complete_plan_and_monotone_records() {
        let catalog = parse_windows(&document(wire::Platform::WindowsX86_64)).unwrap();
        let good = binding(&catalog);
        let (current, target) = catalog.select_plan(&good).unwrap();
        assert_eq!((current.release_sequence, target.release_sequence), (1, 2));
        for case in 0..8 {
            let mut bad = good.clone();
            match case {
                0 => bad.catalog_revision += 1,
                1 => bad.current.cli_sha256 = "e".repeat(64),
                2 => bad.target.identity.artifact_source_sha = "e".repeat(40),
                3 => bad.target.version_label = "browser-override".into(),
                4 => bad.target.platform = wire::Platform::LinuxX86_64,
                5 => bad.target = current.target.clone(),
                6 => {
                    bad.current = target.target.identity.clone();
                    bad.target = current.target.clone();
                }
                _ => bad.operation_id = "not-an-operation".into(),
            }
            bad.plan_digest = bad.expected_plan_digest().unwrap();
            assert!(catalog.select_plan(&bad).is_err(), "case {case}");
        }
        let mut bad = good.clone();
        bad.plan_digest = "0".repeat(64);
        assert!(catalog.select_plan(&bad).is_err());
        let independent = parse_windows(&document(wire::Platform::WindowsX86_64)).unwrap();
        assert!(catalog.target(&good, &independent.entries[0]).is_err());
    }

    #[test]
    fn windows_update_catalog_local_contract_and_complete_raw_digest() {
        let good = document(wire::Platform::WindowsX86_64);
        let original = parse_windows(&good).unwrap();
        let pretty = serde_json::to_vec_pretty(&good).unwrap();
        let changed = Catalog::parse_for(&pretty, wire::Platform::WindowsX86_64).unwrap();
        assert_ne!(original.digest, changed.digest);
        for url in [
            "http://example.test/",
            "https://user@example.test/",
            "https://example.test/?query",
            "https://example.test/#fragment",
            "https://example.test/not-a-directory",
        ] {
            let mut bad = good.clone();
            bad["entries"][0]["artifact_base"] = json!(url);
            assert!(parse_windows(&bad).is_err());
        }
        for case in 0..7 {
            let mut bad = good.clone();
            match case {
                0 => bad["entries"][0]["allow_pinned"] = json!(false),
                1 => bad["entries"][0]["release_sequence"] = json!(2),
                2 => bad["entries"][0]["target"]["target_id"] = json!("release-b"),
                3 => {
                    bad["entries"][0]["target"]["identity"] =
                        good["entries"][1]["target"]["identity"].clone()
                }
                4 => bad["revision"] = json!(0),
                5 => bad["entries"][0]["allow_same"] = json!(true),
                _ => bad["entries"][0]["sums_sha256"] = json!("D".repeat(64)),
            }
            assert!(parse_windows(&bad).is_err(), "case {case}");
        }
        assert!(Catalog::parse_for(
            &vec![b' '; LIMIT as usize + 1],
            wire::Platform::WindowsX86_64
        )
        .is_err());
    }
}
