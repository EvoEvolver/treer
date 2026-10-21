use std::io::Read;

use crate::self_update::selection::{Inventory, Selected, SelectionLocks};
use anyhow::{bail, Result};
use serde::Deserialize;
use treer_platform::private_directory::PrivateDirectory;
use treer_protocol::{
    controller_update as wire,
    controller_update_runtime::{DispatchBinding, Inspection},
};

const NAME: &str = "controller-update-catalog.json";
const LIMIT: u64 = 64 * 1024;

/// Local operator authorization, not a publisher signature or browser choice.
/// Same-release/downgrade overrides intentionally do not exist in this format.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    schema_version: u8,
    pub(crate) revision: u64,
    pub(crate) accept_shared_binaries: bool,
    pub(crate) entries: Vec<Entry>,
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
fn platform() -> Result<wire::Platform> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok(wire::Platform::LinuxX86_64),
        ("linux", "aarch64") => Ok(wire::Platform::LinuxAarch64),
        ("macos", "x86_64") => Ok(wire::Platform::DarwinX86_64),
        ("macos", "aarch64") => Ok(wire::Platform::DarwinAarch64),
        _ => bail!("controller_update_unsupported"),
    }
}
impl Catalog {
    pub(crate) fn local() -> Result<Self> {
        let directory = crate::service::controller_update_config_directory()?;
        let directory = PrivateDirectory::open(&directory)?;
        let mut bytes = Vec::new();
        directory
            .read_file(NAME)?
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)?;
        Self::parse(&bytes)
    }
    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() as u64 > LIMIT {
            bail!("controller_update_catalog_invalid");
        }
        let catalog: Self = serde_json::from_slice(bytes)?;
        if catalog.schema_version != 1
            || catalog.revision == 0
            || catalog.revision >= 1 << 53
            || catalog.entries.is_empty()
            || catalog.entries.len() > wire::MAX_TARGETS + 1
        {
            bail!("controller_update_catalog_invalid");
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
            if entry.target.platform != platform()?
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
        Ok(catalog)
    }
    pub(crate) fn select(
        &self,
        workspace: &str,
        server: &str,
    ) -> Result<(Selected, SelectionLocks)> {
        let selected = Inventory::local()?.select(Some(workspace), self.accept_shared_binaries)?;
        if selected.server_id() != server {
            bail!("controller_update_scope_mismatch");
        }
        let locks = selected.lock()?;
        Ok((selected, locks))
    }
    /// Both installed digests must match one independently provisioned identity.
    /// Never choose provenance by hashing arbitrary local bytes then trusting it.
    pub(crate) async fn current<'a>(
        &'a self,
        selected: &Selected,
        locks: &SelectionLocks,
    ) -> Result<&'a Entry> {
        let (controller_digest, cli_digest) =
            crate::self_update::web_adapter::installed_pair(selected, locks)?;
        let entry = self
            .entries
            .iter()
            .find(|entry| {
                entry.target.identity.controller_sha256 == controller_digest
                    && entry.target.identity.cli_sha256 == cli_digest
            })
            .ok_or_else(|| anyhow::anyhow!("controller_update_current_unknown"))?;
        // The selected installed pair is authenticated against local pins before
        // either --version probe executes. Probes never infer their own trust.
        crate::self_update::web_adapter::inspect_pair(selected, locks, &entry.target.identity)
            .await?;
        Ok(entry)
    }
    pub(crate) fn target<'a>(
        &'a self,
        binding: &DispatchBinding,
        current: &Entry,
    ) -> Result<&'a Entry> {
        if !binding.validate()
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

pub(super) async fn inspect(workspace: &str, server: &str) -> Result<Inspection> {
    let catalog = Catalog::local()?;
    let (selected, locks) = catalog.select(workspace, server)?;
    let current = catalog.current(&selected, &locks).await?;
    Ok(Inspection {
        platform: platform()?,
        current: current.target.identity.clone(),
        observed_at: chrono::Utc::now(),
        // Preparation and read-only recovery do not establish a full executor.
        // No platform is advertised until its worker/launcher path is verified.
        native_ready: super::native_accepted(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn document() -> serde_json::Value {
        let platform = serde_json::to_value(platform().unwrap()).unwrap();
        serde_json::json!({"schema_version":1,"revision":2,"accept_shared_binaries":false,"entries":[{
            "release_sequence":1,"target":{"target_id":"release-a","identity":{"release_sha":"a".repeat(40),"artifact_source_sha":"a".repeat(40),"controller_sha256":"a".repeat(64),"cli_sha256":"b".repeat(64)},"platform":platform,"version_label":"1.0"},
            "sums_sha256":"c".repeat(64),"artifact_base":"https://artifacts.example.test/fixed/","allow_pinned":true,"allow_artifact_origin":false
        }]})
    }
    #[test]
    fn local_catalog_rejects_implicit_pin_url_tricks_unknown_fields_and_oversize() {
        let good = document();
        assert!(Catalog::parse(&serde_json::to_vec(&good).unwrap()).is_ok());
        for url in [
            "http://artifacts.example.test/",
            "https://user@example.test/",
            "https://example.test/?x=y",
            "https://example.test/#x",
        ] {
            let mut bad = good.clone();
            bad["entries"][0]["artifact_base"] = serde_json::json!(url);
            assert!(Catalog::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        let mut bad = good.clone();
        bad["entries"][0]["allow_pinned"] = serde_json::json!(false);
        assert!(Catalog::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        bad = good;
        bad["entries"][0]["allow_same"] = serde_json::json!(true);
        assert!(Catalog::parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        assert!(Catalog::parse(&vec![b' '; LIMIT as usize + 1]).is_err());
    }
    #[test]
    fn admission_rechecks_full_binding_monotone_order_and_canonical_plan() {
        let mut doc = document();
        let mut next = doc["entries"][0].clone();
        next["release_sequence"] = serde_json::json!(2);
        next["target"]["target_id"] = serde_json::json!("release-b");
        next["target"]["identity"]["release_sha"] = serde_json::json!("b".repeat(40));
        next["target"]["identity"]["controller_sha256"] = serde_json::json!("d".repeat(64));
        doc["entries"].as_array_mut().unwrap().push(next);
        let catalog = Catalog::parse(&serde_json::to_vec(&doc).unwrap()).unwrap();
        let current = &catalog.entries[0];
        let mut binding = DispatchBinding {
            operation_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: "ws_a".into(),
            server_id: "srv_a".into(),
            current: current.target.identity.clone(),
            target: catalog.entries[1].target.clone(),
            catalog_revision: catalog.revision,
            plan_digest: "0".repeat(64),
            created_at: chrono::Utc::now(),
        };
        assert!(catalog.target(&binding, current).is_err());
        binding.plan_digest = binding.expected_plan_digest().unwrap();
        assert!(catalog.target(&binding, current).is_ok());
        let good = binding.clone();
        for case in 0..5 {
            let mut changed = good.clone();
            match case {
                0 => changed.catalog_revision += 1,
                1 => changed.current.cli_sha256 = "e".repeat(64),
                2 => changed.target.version_label = "untrusted rename".into(),
                3 => changed.target = current.target.clone(),
                _ => {
                    changed.current = catalog.entries[1].target.identity.clone();
                    changed.target = current.target.clone();
                }
            }
            changed.plan_digest = changed.expected_plan_digest().unwrap();
            assert!(catalog
                .target(
                    &changed,
                    if case == 4 {
                        &catalog.entries[1]
                    } else {
                        current
                    }
                )
                .is_err());
        }
    }
}
