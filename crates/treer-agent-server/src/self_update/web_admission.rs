//! Private operator-pin admission evidence, not browser or receipt authority.
//! A fresh submission persists it before artifact IO. Only the original worker,
//! holding the transferred SAME locks, can use a rechecked catalog before Core.
use std::io::{Read, Write};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use treer_protocol::controller_update_runtime::DispatchBinding;

use super::{
    selection::{Selected, SelectionLocks},
    Journal, Source,
};
use crate::controller_update::unix::Catalog;

const NAME: &str = "web-admission.json";
const LIMIT: u64 = 64 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Admission {
    pub(crate) schema_version: u8,
    pub(crate) binding: DispatchBinding,
    pub(crate) catalog_sha256: String,
    pub(crate) from_sequence: u64,
    pub(crate) to_sequence: u64,
    pub(crate) sums_sha256: String,
    pub(crate) allow_pinned: bool,
    pub(crate) allow_artifact_origin: bool,
    pub(crate) accept_shared_binaries: bool,
}

impl Admission {
    fn check(&self, journal: &Journal) -> Result<()> {
        let receipt = journal.read()?;
        super::web_adapter::check(&receipt, &self.binding)?;
        let hex = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.schema_version != 1
            || !hex(&self.catalog_sha256)
            || !hex(&self.sums_sha256)
            || self.from_sequence == 0
            || self.to_sequence <= self.from_sequence
            || self.to_sequence >= 1 << 53
            || !self.allow_pinned
            || self.binding.plan_digest != self.binding.expected_plan_digest()?
            || !matches!(&receipt.source, Source::Pin {
                sums_sha256, allow_pinned: true, allow_artifact_origin,
                allow_same: false, allow_downgrade: false,
            } if sums_sha256 == &self.sums_sha256
                && *allow_artifact_origin == self.allow_artifact_origin)
        {
            bail!("controller_update_admission_invalid");
        }
        Ok(())
    }

    pub(super) fn persist(&self, journal: &Journal) -> Result<()> {
        self.check(journal)?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() as u64 > LIMIT {
            bail!("controller_update_admission_invalid");
        }
        let mut file = journal.directory.create_file(NAME)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        journal.directory.sync()?;
        if read(journal)? != *self {
            bail!("controller_update_admission_invalid");
        }
        Ok(())
    }
}

fn read(journal: &Journal) -> Result<Admission> {
    let mut bytes = Vec::new();
    journal
        .directory
        .read_file(NAME)?
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        bail!("controller_update_admission_invalid");
    }
    let proof: Admission = serde_json::from_slice(&bytes)?;
    proof.check(journal)?;
    Ok(proof)
}

fn matching(journal: &Journal, catalog: &Catalog) -> Result<Admission> {
    let saved = read(journal)?;
    if saved != catalog.admission(&saved.binding)? {
        bail!("controller_update_preview_changed");
    }
    Ok(saved)
}

pub(super) async fn verify(
    journal: &Journal,
    selected: &Selected,
    locks: &SelectionLocks,
) -> Result<()> {
    let catalog = Catalog::local()?;
    let proof = matching(journal, &catalog)?;
    // Reauthenticate installed bytes before executing either version probe.
    // Catalog comparison, sequence, target, SUMS pin and consent are rechecked
    // with ownership retained; neither a copied proof nor an old plan suffices.
    let current = catalog.current(selected, locks).await?;
    if current.target.identity != proof.binding.current {
        bail!("controller_update_preview_changed");
    }
    catalog.target(&proof.binding, current)?;
    matching(journal, &Catalog::local()?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use treer_platform::private_directory::PrivateDirectory;
    use treer_protocol::controller_update as wire;

    #[test]
    fn exclusive_predownload_proof_reopens_and_rejects_any_catalog_change() {
        let f = super::super::selection::tests::Fixture::new();
        let jobs = PrivateDirectory::open(&f.path)
            .unwrap()
            .create_child("jobs")
            .unwrap();
        let (mut receipt, mut binding) = super::super::web_adapter::tests::bound();
        if let Source::Pin { allow_same, .. } = &mut receipt.source {
            *allow_same = false;
        }
        binding.target.platform = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => wire::Platform::DarwinAarch64,
            ("macos", "x86_64") => wire::Platform::DarwinX86_64,
            ("linux", "aarch64") => wire::Platform::LinuxAarch64,
            _ => wire::Platform::LinuxX86_64,
        };
        binding.plan_digest = binding.expected_plan_digest().unwrap();
        let source_pin = match &receipt.source {
            Source::Pin { sums_sha256, .. } => sums_sha256.clone(),
            _ => unreachable!(),
        };
        let document = serde_json::json!({"schema_version":1,"revision":binding.catalog_revision,
        "accept_shared_binaries":false,"entries":[
            {"release_sequence":1,"target":{"target_id":"original","identity":binding.current,
                "platform":binding.target.platform,"version_label":"original"},
                "sums_sha256":"1".repeat(64),"artifact_base":"https://artifacts.example.test/",
                "allow_pinned":true,"allow_artifact_origin":false},
            {"release_sequence":2,"target":binding.target,"sums_sha256":source_pin,
                "artifact_base":"https://artifacts.example.test/","allow_pinned":true,
                "allow_artifact_origin":false}
        ]});
        let bytes = serde_json::to_vec(&document).unwrap();
        let catalog = Catalog::parse(&bytes).unwrap();
        let proof = catalog.admission(&binding).unwrap();
        let journal = Journal::create(&jobs, &receipt).unwrap();
        assert!(matching(&journal, &catalog).is_err());
        proof.persist(&journal).unwrap();
        assert!(proof.persist(&journal).is_err());
        assert!(matching(&journal, &catalog).unwrap() == proof);
        // Even unchanged revision/semantics cannot substitute different local
        // catalog bytes between admission and execution.
        let mut whitespace = bytes.clone();
        whitespace.push(b' ');
        assert!(matching(&journal, &Catalog::parse(&whitespace).unwrap()).is_err());
        for field in ["revision", "sequence", "pin", "consent", "origin"] {
            let mut changed = document.clone();
            match field {
                "revision" => changed["revision"] = serde_json::json!(binding.catalog_revision + 1),
                "sequence" => changed["entries"][1]["release_sequence"] = serde_json::json!(3),
                "pin" => changed["entries"][1]["sums_sha256"] = serde_json::json!("2".repeat(64)),
                "consent" => changed["accept_shared_binaries"] = serde_json::json!(true),
                _ => changed["entries"][1]["allow_artifact_origin"] = serde_json::json!(true),
            }
            let changed = Catalog::parse(&serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(matching(&journal, &changed).is_err(), "{field}");
        }
        drop(journal);
        let reopened = Journal::open(&jobs, &binding.operation_id).unwrap();
        assert!(matching(&reopened, &catalog).unwrap() == proof);
        assert_eq!(reopened.read().unwrap(), receipt);
        let mut invalid = proof.clone();
        invalid.to_sequence = invalid.from_sequence;
        assert!(invalid.check(&reopened).is_err());
        invalid = proof;
        invalid.sums_sha256 = "3".repeat(64);
        assert!(invalid.check(&reopened).is_err());
    }
}
