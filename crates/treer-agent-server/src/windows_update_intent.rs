//! Single-use Windows update intent, not execution or recovery authority.
//! A valid record proves only that these inputs were reserved. It does not prove
//! publisher authenticity, backup, replacement, process ownership or health.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAGIC: &[u8; 8] = b"TRWININT";
const LIMIT: usize = 4096;

/// Physical installation identity and exact input digests supplied by the
/// future verified admission layer. No path, argv, credential or free-form error
/// belongs in this record. The caller must compare against independently held
/// inputs; reading a record cannot authenticate those inputs by itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub operation_id: Uuid,
    pub workspace_id: String,
    pub server_id: String,
    pub installation_volume: u64,
    pub installation_file_id: u64,
    pub old_controller_sha256: String,
    pub old_cli_sha256: String,
    pub controller_sha256: String,
    pub cli_sha256: String,
    pub signed_manifest_sha256: String,
}

fn identifier(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 128
            && rest
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

impl Binding {
    pub fn validate(&self) -> Result<()> {
        if self.operation_id.get_version_num() != 4
            || self.operation_id.get_variant() != uuid::Variant::RFC4122
            || !identifier(&self.workspace_id, "ws_")
            || !identifier(&self.server_id, "srv_")
            || self.installation_file_id == 0
            || [
                &self.old_controller_sha256,
                &self.old_cli_sha256,
                &self.controller_sha256,
                &self.cli_sha256,
                &self.signed_manifest_sha256,
            ]
            .iter()
            .any(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        {
            bail!("invalid_windows_update_intent");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    // This schema has exactly one state. No caller can mark images published,
    // rollback successful or a worker healthy by changing a phase string.
    phase: IntentPhase,
    binding: Binding,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum IntentPhase {
    Reserved,
}

pub fn encode(binding: &Binding) -> Result<Vec<u8>> {
    binding.validate()?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(&Record {
        schema_version: 1,
        phase: IntentPhase::Reserved,
        binding: binding.clone(),
    })?);
    if bytes.len() > LIMIT {
        bail!("invalid_windows_update_intent");
    }
    Ok(bytes)
}

/// Read-only reconciliation. Exact comparison is mandatory; successful decoding
/// never grants permission to execute or retry the operation.
pub fn inspect(bytes: &[u8], expected: &Binding) -> Result<()> {
    expected.validate()?;
    if bytes.len() > LIMIT || !bytes.starts_with(MAGIC) {
        bail!("invalid_windows_update_intent");
    }
    let record: Record = serde_json::from_slice(&bytes[MAGIC.len()..])
        .map_err(|_| anyhow::anyhow!("invalid_windows_update_intent"))?;
    record.binding.validate()?;
    if record.schema_version != 1 || record.binding != *expected {
        bail!("windows_update_intent_binding_conflict");
    }
    Ok(())
}

/// Consume the private store's exclusive holder for a single reservation. An
/// existing record (including a matching one) is never resubmitted. An error
/// after publication begins has unknown outcome: retain all evidence, do not
/// retry. Reopening is for inspect only, not a recovery execution entry.
#[cfg(windows)]
pub fn reserve(
    store: treer_platform::windows_update_store::UpdateStore,
    binding: &Binding,
) -> Result<()> {
    let bytes = encode(binding)?;
    match store.read() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => bail!("windows_update_intent_exists_or_unreadable"),
    }
    store.publish(&bytes)?;
    inspect(&store.read()?, binding)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            operation_id: Uuid::new_v4(),
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            installation_volume: 42,
            installation_file_id: 10,
            old_controller_sha256: "1".repeat(64),
            old_cli_sha256: "2".repeat(64),
            controller_sha256: "3".repeat(64),
            cli_sha256: "4".repeat(64),
            signed_manifest_sha256: "5".repeat(64),
        }
    }

    #[test]
    fn windows_update_intent_binds_every_field_and_refuses_foreign_records() {
        let expected = binding();
        let bytes = encode(&expected).unwrap();
        inspect(&bytes, &expected).unwrap();
        assert!(serde_json::from_slice::<serde_json::Value>(&bytes).is_err());
        let value = serde_json::to_value(&expected).unwrap();
        for (field, original) in value.as_object().unwrap() {
            let mut changed = value.clone();
            changed[field] = match original {
                serde_json::Value::Number(n) => serde_json::json!(n.as_u64().unwrap() + 1),
                serde_json::Value::String(_) if field == "operation_id" => {
                    serde_json::json!(Uuid::new_v4())
                }
                serde_json::Value::String(s) => serde_json::json!(if s.len() == 64 {
                    "6".repeat(64)
                } else {
                    format!("{s}_other")
                }),
                _ => unreachable!(),
            };
            let changed = serde_json::from_value(changed).unwrap();
            assert!(inspect(&bytes, &changed).is_err(), "{field}");
        }
        for end in 0..bytes.len() {
            assert!(inspect(&bytes[..end], &expected).is_err());
        }
        assert!(inspect(&vec![0; LIMIT + 1], &expected).is_err());
    }

    #[test]
    fn windows_update_intent_refuses_schema_phase_unknown_fields_and_duplicates() {
        let expected = binding();
        let bytes = encode(&expected).unwrap();
        let json = std::str::from_utf8(&bytes[MAGIC.len()..]).unwrap();
        for invalid in [
            json.replace("\"schema_version\":1", "\"schema_version\":2"),
            json.replace("reserved", "complete"),
            json.replacen('{', "{\"untrusted\":true,", 1),
            json.replacen('{', "{\"schema_version\":1,", 1),
            json.replace("\"binding\":{", "\"binding\":{\"unexpected\":true,"),
            json.replace("\"binding\":{", "\"binding\":{\"installation_volume\":42,"),
            format!("{json}{{}}"),
        ] {
            let mut bad = MAGIC.to_vec();
            bad.extend(invalid.bytes());
            assert!(inspect(&bad, &expected).is_err());
        }
        let mut invalid = expected.clone();
        invalid.operation_id = Uuid::nil();
        assert!(encode(&invalid).is_err());
        invalid = expected;
        invalid.cli_sha256 = "A".repeat(64);
        assert!(encode(&invalid).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_update_intent_native_single_reservation_and_read_only_reconciliation() {
        use treer_platform::windows_update_store::UpdateStore;
        let root = std::env::temp_dir().join(format!("treer-update-intent-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        treer_platform::secure_directory(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                std::fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let _cleanup = Cleanup(root.clone());
        let expected = binding();
        reserve(UpdateStore::open(&root).unwrap(), &expected).unwrap();
        let before = UpdateStore::open(&root).unwrap().read().unwrap();
        assert!(reserve(UpdateStore::open(&root).unwrap(), &expected).is_err());
        let after = UpdateStore::open(&root).unwrap().read().unwrap();
        assert_eq!(before, after);
        inspect(&after, &expected).unwrap();
        assert!(inspect(&after, &binding()).is_err());
        assert!(!root.join("receipt.next").exists());
    }
}
