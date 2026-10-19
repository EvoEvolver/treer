//! Inert draft private-record fields and claimed state/layout consistency.
//! No real observations, health validation, recovery or mutation authority.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::windows_update_envelope::{decode, Kind};
use crate::windows_update_layout_model as layout;

// Remote derives keep one classifier and its original portable regressions.
// These spellings are a draft format, never trusted evidence by themselves.
#[derive(Clone, Serialize, Deserialize)]
#[serde(remote = "layout::Step", rename_all = "snake_case")]
enum StepWire {
    Original,
    MoveOldIntent,
    OldMoved,
    PublishIntent,
    Published,
    MoveNewAsideIntent,
    NewAside,
    RestoreOldIntent,
    Restored,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "layout::Transaction", rename_all = "snake_case")]
enum TransactionWire {
    Prepared,
    Activating,
    CoreHealthy,
    Complete,
    RollbackCli,
    RollbackController,
    RolledBack,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimedState {
    #[serde(with = "TransactionWire")]
    transaction: layout::Transaction,
    #[serde(with = "StepWire")]
    controller_step: layout::Step,
    #[serde(with = "StepWire")]
    cli_step: layout::Step,
    new_health_recorded: bool,
    final_health_recorded: bool,
    old_health_recorded: bool,
}

// Called only AFTER parse_pair. Slots are separate synthetic observations;
// never accept self-reported slot labels from the receipt. Consistency is not
// proof of health, a permitted transition, completion or permission to mutate.
fn inspect_claim(
    state: &ClaimedState,
    controller: [layout::Object; 3],
    cli: [layout::Object; 3],
) -> layout::PairInspection {
    layout::inspect_pair(
        state.transaction,
        layout::Image {
            step: state.controller_step,
            slots: controller,
        },
        layout::Image {
            step: state.cli_step,
            slots: cli,
        },
        state.new_health_recorded,
        state.final_health_recorded,
        state.old_health_recorded,
    )
}

type Refusal = &'static str;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    workspace_id: String,
    server_id: String,
    job_id: String,
    revision: u64,
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn identifier(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 128
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    })
}

impl Binding {
    fn valid(&self) -> bool {
        identifier(&self.workspace_id, "ws_")
            && identifier(&self.server_id, "srv_")
            && self.revision > 0
            && Uuid::parse_str(&self.job_id)
                .is_ok_and(|id| !id.is_nil() && id.to_string() == self.job_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    binding: Binding,
    // Digest of the WHOLE receipt envelope, including its version/layout/kind.
    receipt_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    release_sha: String,
    artifact_source_sha: String,
    sums_sha256: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AclPolicy {
    OwnerSystemProtected,
}

// This draft deliberately matches the native adapter's NTFS-only boundary.
// The label is not evidence: a future observer must verify the filesystem
// through its pinned volume handle before trusting the BY_HANDLE identity.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Filesystem {
    Ntfs,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Object {
    filesystem: Filesystem,
    volume_serial: u32,
    file_id: u64,
    bytes: u64,
    sha256: String,
    links: u32,
    attributes: u32,
    acl_policy: AclPolicy,
}

impl Object {
    fn valid(&self) -> bool {
        self.bytes > 0
            && self.bytes <= 256 * 1024 * 1024
            && hex(&self.sha256, 64)
            && self.links == 1
            // NORMAL alone, or ARCHIVE/READONLY (including their combination).
            // Every other attribute, and NORMAL mixed with flags, is refused.
            && matches!(self.attributes, 0x80 | 0x20 | 0x01 | 0x21)
    }

    fn identity(&self) -> (u32, u64) {
        (self.volume_serial, self.file_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    old: Object,
    new: Object,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Images {
    // Fixed keys, no caller-controlled paths or array of duplicate image names.
    controller: Image,
    cli: Image,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftFields {
    binding: Binding,
    provenance: Provenance,
    images: Images,
    state: ClaimedState,
}

impl DraftFields {
    fn valid(&self) -> bool {
        let p = &self.provenance;
        let objects = [
            &self.images.controller.old,
            &self.images.controller.new,
            &self.images.cli.old,
            &self.images.cli.new,
        ];
        self.binding.valid()
            && hex(&p.release_sha, 40)
            && hex(&p.artifact_source_sha, 40)
            && hex(&p.sums_sha256, 64)
            && objects.iter().all(|object| object.valid())
            && objects.iter().enumerate().all(|(index, object)| {
                objects[..index]
                    .iter()
                    .all(|prior| prior.identity() != object.identity())
            })
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Returns parsed draft fields only. The caller's expected binding is synthetic
// here; it is not the unimplemented external durable anchor or a lock proof.
fn parse_pair(receipt: &[u8], marker: &[u8], expected: &Binding) -> Result<DraftFields, Refusal> {
    if !expected.valid() {
        return Err("invalid_expected_binding");
    }
    let marker_payload = decode(marker, Kind::Marker)?;
    let marker: Marker =
        serde_json::from_slice(marker_payload).map_err(|_| "invalid_marker_fields")?;
    if !marker.binding.valid() || &marker.binding != expected || !hex(&marker.receipt_sha256, 64) {
        return Err("marker_binding_mismatch");
    }
    // Checks both the cap and header before hashing; no receipt payload parse
    // may move above the pair digest check.
    let payload = decode(receipt, Kind::Receipt)?;
    if digest(receipt) != marker.receipt_sha256 {
        return Err("record_pair_digest_mismatch");
    }
    let fields: DraftFields =
        serde_json::from_slice(payload).map_err(|_| "invalid_receipt_fields")?;
    if &fields.binding != expected || !fields.valid() {
        return Err("invalid_receipt_fields");
    }
    Ok(fields)
}

fn fixture() -> DraftFields {
    let object = |file_id| Object {
        filesystem: Filesystem::Ntfs,
        volume_serial: 7,
        file_id,
        bytes: 123,
        sha256: "a".repeat(64), // Equal hashes may still refer to distinct objects.
        links: 1,
        attributes: 0x20,
        acl_policy: AclPolicy::OwnerSystemProtected,
    };
    DraftFields {
        binding: Binding {
            workspace_id: "ws_fixture".into(),
            server_id: "srv_fixture".into(),
            job_id: "11111111-1111-4111-8111-111111111111".into(),
            revision: 1,
        },
        provenance: Provenance {
            release_sha: "b".repeat(40),
            artifact_source_sha: "c".repeat(40),
            sums_sha256: "d".repeat(64),
        },
        images: Images {
            controller: Image {
                old: object(1),
                new: object(2),
            },
            cli: Image {
                old: object(3),
                new: object(4),
            },
        },
        state: ClaimedState {
            transaction: layout::Transaction::Prepared,
            controller_step: layout::Step::Original,
            cli_step: layout::Step::Original,
            new_health_recorded: false,
            final_health_recorded: false,
            old_health_recorded: false,
        },
    }
}

fn envelopes(payload: &[u8], binding: &Binding) -> (Vec<u8>, Vec<u8>) {
    use crate::windows_update_envelope::encode;
    let receipt = encode(payload, Kind::Receipt).unwrap();
    let marker = Marker {
        binding: binding.clone(),
        receipt_sha256: digest(&receipt),
    };
    let marker = encode(&serde_json::to_vec(&marker).unwrap(), Kind::Marker).unwrap();
    (receipt, marker)
}

#[test]
fn draft_fields_require_exact_pair_before_receipt_payload_parse() {
    let fields = fixture();
    let (receipt, marker) = envelopes(&serde_json::to_vec(&fields).unwrap(), &fields.binding);
    assert_eq!(
        parse_pair(&receipt, &marker, &fields.binding)
            .unwrap()
            .binding,
        fields.binding
    );
    let (malformed, _) = envelopes(b"invalid JSON", &fields.binding);
    assert_eq!(
        parse_pair(&malformed, &marker, &fields.binding).unwrap_err(),
        "record_pair_digest_mismatch"
    );
    let (malformed, matching) = envelopes(b"invalid JSON", &fields.binding);
    assert_eq!(
        parse_pair(&malformed, &matching, &fields.binding).unwrap_err(),
        "invalid_receipt_fields"
    );
    let mut damaged = receipt.clone();
    *damaged.last_mut().unwrap() ^= 1;
    assert_eq!(
        parse_pair(&damaged, &marker, &fields.binding).unwrap_err(),
        "record_pair_digest_mismatch"
    );
    assert!(parse_pair(&receipt, &[], &fields.binding).is_err());
    assert!(parse_pair(&receipt, &receipt, &fields.binding).is_err());
}

#[test]
fn draft_fields_reject_unknown_duplicate_missing_and_trailing_json() {
    let fields = fixture();
    let value = serde_json::to_value(&fields).unwrap();
    for pointer in [
        "",
        "/binding",
        "/provenance",
        "/images",
        "/images/controller",
        "/images/controller/new",
        "/state",
    ] {
        let mut bad = value.clone();
        bad.pointer_mut(pointer).unwrap()["unknown"] = true.into();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&bad).unwrap(), &fields.binding);
        assert!(
            parse_pair(&receipt, &marker, &fields.binding).is_err(),
            "{pointer}"
        );
    }
    let mut missing = value.clone();
    missing["images"].as_object_mut().unwrap().remove("cli");
    let (receipt, marker) = envelopes(&serde_json::to_vec(&missing).unwrap(), &fields.binding);
    assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    let encoded = serde_json::to_string(&fields).unwrap();
    let duplicate = encoded.replacen("\"revision\":1", "\"revision\":1,\"revision\":1", 1);
    let duplicate_image = encoded.replacen(
        "\"images\":{",
        &format!(
            "\"images\":{{\"controller\":{},",
            value["images"]["controller"]
        ),
        1,
    );
    // These are syntactically valid JSON; refusal must come from duplicate
    // typed fields, not an accidentally malformed test input.
    assert!(serde_json::from_str::<serde_json::Value>(&duplicate).is_ok());
    assert!(serde_json::from_str::<serde_json::Value>(&duplicate_image).is_ok());
    for bad in [
        duplicate,
        duplicate_image,
        format!("{encoded} {{}}"),
        encoded[..encoded.len() - 1].into(),
    ] {
        let (receipt, marker) = envelopes(bad.as_bytes(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    }
}

#[test]
fn draft_marker_fields_are_strict_before_receipt_use() {
    use crate::windows_update_envelope::encode;
    let fields = fixture();
    let (receipt, marker) = envelopes(&serde_json::to_vec(&fields).unwrap(), &fields.binding);
    let payload = decode(&marker, Kind::Marker).unwrap();
    let value: serde_json::Value = serde_json::from_slice(payload).unwrap();
    let mut unknown = value.clone();
    unknown["unknown"] = true.into();
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("binding");
    let encoded = std::str::from_utf8(payload).unwrap();
    let duplicate = encoded.replacen("\"revision\":1", "\"revision\":1,\"revision\":1", 1);
    assert!(serde_json::from_str::<serde_json::Value>(&duplicate).is_ok());
    for bad in [
        serde_json::to_vec(&unknown).unwrap(),
        serde_json::to_vec(&missing).unwrap(),
        duplicate.into_bytes(),
    ] {
        let bad = encode(&bad, Kind::Marker).unwrap();
        assert_eq!(
            parse_pair(&receipt, &bad, &fields.binding).unwrap_err(),
            "invalid_marker_fields"
        );
    }
    for replacement in ["A".repeat(64), "0".repeat(63)] {
        let mut bad = value.clone();
        bad["receipt_sha256"] = replacement.into();
        let bad = encode(&serde_json::to_vec(&bad).unwrap(), Kind::Marker).unwrap();
        assert_eq!(
            parse_pair(&receipt, &bad, &fields.binding).unwrap_err(),
            "marker_binding_mismatch"
        );
    }
}

#[test]
fn draft_fields_reject_wrong_bindings_in_marker_and_receipt() {
    let fields = fixture();
    for field in ["workspace_id", "server_id", "job_id", "revision"] {
        let mut changed = serde_json::to_value(&fields.binding).unwrap();
        changed[field] = match field {
            "workspace_id" => "ws_other".into(),
            "server_id" => "srv_other".into(),
            "job_id" => "22222222-2222-4222-8222-222222222222".into(),
            _ => 2.into(),
        };
        let changed: Binding = serde_json::from_value(changed).unwrap();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&fields).unwrap(), &changed);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
        let altered = DraftFields {
            binding: changed,
            ..fields.clone()
        };
        let (receipt, marker) = envelopes(&serde_json::to_vec(&altered).unwrap(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    }
}

#[test]
fn draft_fields_reject_bad_provenance_objects_and_paths() {
    let fields = fixture();
    let value = serde_json::to_value(&fields).unwrap();
    for (pointer, replacement) in [
        ("/binding/revision", serde_json::json!(0)),
        ("/binding/job_id", serde_json::json!("../job")),
        ("/provenance/release_sha", serde_json::json!("A".repeat(40))),
        ("/provenance/sums_sha256", serde_json::json!("../sums")),
        ("/images/controller/new/bytes", serde_json::json!(0)),
        (
            "/images/controller/new/bytes",
            serde_json::json!(268435457_u64),
        ),
        ("/images/controller/new/links", serde_json::json!(2)),
        (
            "/images/controller/new/attributes",
            serde_json::json!(0x400),
        ),
        ("/images/controller/new/attributes", serde_json::json!(0x10)),
        (
            "/images/controller/new/acl_policy",
            serde_json::json!("arbitrary"),
        ),
        ("/images/controller/new/file_id", serde_json::json!(1)),
        ("/images/cli/new/file_id", serde_json::json!(2)),
        (
            "/images/controller/new/sha256",
            serde_json::json!("secret\nvalue"),
        ),
    ] {
        let mut bad = value.clone();
        *bad.pointer_mut(pointer).unwrap() = replacement;
        let (receipt, marker) = envelopes(&serde_json::to_vec(&bad).unwrap(), &fields.binding);
        assert!(
            parse_pair(&receipt, &marker, &fields.binding).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn draft_objects_require_ntfs_and_only_plain_image_attributes() {
    let fields = fixture();
    let value = serde_json::to_value(&fields).unwrap();
    for attributes in [0x80, 0x20, 0x01, 0x21] {
        let mut good = value.clone();
        good["images"]["controller"]["new"]["attributes"] = attributes.into();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&good).unwrap(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_ok());
    }
    // Each bit outside ARCHIVE/READONLY/NORMAL must refuse even with ARCHIVE.
    // This includes unknown future bits, not just today's problematic flags.
    for bit in 0..32 {
        let flag = 1_u32 << bit;
        if flag & (0x80 | 0x20 | 0x01) != 0 {
            continue;
        }
        let mut bad = value.clone();
        bad["images"]["controller"]["new"]["attributes"] = (0x20 | flag).into();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&bad).unwrap(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    }
    for attributes in [0, 0x81, 0xa0, 0xa1] {
        let mut bad = value.clone();
        bad["images"]["controller"]["new"]["attributes"] = attributes.into();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&bad).unwrap(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    }
    for filesystem in ["refs", "fat32", "NTFS", ""] {
        let mut bad = value.clone();
        bad["images"]["controller"]["new"]["filesystem"] = filesystem.into();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&bad).unwrap(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    }
    let mut missing = value;
    missing["images"]["controller"]["new"]
        .as_object_mut()
        .unwrap()
        .remove("filesystem");
    let (receipt, marker) = envelopes(&serde_json::to_vec(&missing).unwrap(), &fields.binding);
    assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
}

#[test]
fn draft_pair_rejects_uppercase_digest_spellings() {
    use crate::windows_update_envelope::encode;
    let fields = fixture();
    let (receipt, marker) = envelopes(&serde_json::to_vec(&fields).unwrap(), &fields.binding);
    let mut upper_marker: Marker =
        serde_json::from_slice(decode(&marker, Kind::Marker).unwrap()).unwrap();
    upper_marker.receipt_sha256.make_ascii_uppercase();
    assert_ne!(upper_marker.receipt_sha256, digest(&receipt));
    let upper_marker = encode(&serde_json::to_vec(&upper_marker).unwrap(), Kind::Marker).unwrap();
    assert_eq!(
        parse_pair(&receipt, &upper_marker, &fields.binding).unwrap_err(),
        "marker_binding_mismatch"
    );
    let mut upper_receipt = fields.clone();
    upper_receipt
        .images
        .controller
        .new
        .sha256
        .make_ascii_uppercase();
    // Recompute the whole-envelope marker so refusal proves the inner digest
    // spelling check, rather than a mismatched envelope digest.
    let (receipt, marker) = envelopes(
        &serde_json::to_vec(&upper_receipt).unwrap(),
        &fields.binding,
    );
    assert_eq!(
        parse_pair(&receipt, &marker, &fields.binding).unwrap_err(),
        "invalid_receipt_fields"
    );
}

#[test]
fn parsed_claims_require_independent_layouts_and_ordered_health_flags() {
    use layout::{Object::*, PairInspection::*};
    let original = [Old, New, Absent];
    let published = [New, Absent, Old];
    let gap = [Absent, New, Old];
    for (phase, c_step, l_step, c_slots, l_slots, allowed_flags) in [
        (
            "prepared",
            "original",
            "original",
            original,
            original,
            &[0][..],
        ),
        (
            "activating",
            "publish_intent",
            "original",
            gap,
            original,
            &[0][..],
        ),
        (
            "core_healthy",
            "published",
            "publish_intent",
            published,
            gap,
            &[1][..],
        ),
        (
            "complete",
            "published",
            "published",
            published,
            published,
            &[3][..],
        ),
        (
            "rollback_cli",
            "published",
            "restored",
            published,
            original,
            &[0, 1][..],
        ),
        (
            "rollback_controller",
            "restored",
            "original",
            original,
            original,
            &[0, 1, 4, 5][..],
        ),
        (
            "rolled_back",
            "restored",
            "original",
            original,
            original,
            &[4, 5][..],
        ),
        // Both names may look right but a CLI publish before Controller
        // health, or old-health before Controller restore, must conflict.
        (
            "activating",
            "published",
            "published",
            published,
            published,
            &[][..],
        ),
        (
            "rollback_controller",
            "publish_intent",
            "original",
            gap,
            original,
            &[0, 1][..],
        ),
    ] {
        for flags in 0..8 {
            let fields = fixture();
            let mut value = serde_json::to_value(&fields).unwrap();
            value["state"] = serde_json::json!({
                "transaction": phase, "controller_step": c_step, "cli_step": l_step,
                "new_health_recorded": flags & 1 != 0,
                "final_health_recorded": flags & 2 != 0,
                "old_health_recorded": flags & 4 != 0,
            });
            let (receipt, marker) =
                envelopes(&serde_json::to_vec(&value).unwrap(), &fields.binding);
            let parsed = parse_pair(&receipt, &marker, &fields.binding).unwrap();
            assert_eq!(
                inspect_claim(&parsed.state, c_slots, l_slots) == Consistent,
                allowed_flags.contains(&flags)
            );
            // Unknown/unreadable observed files never become absence just
            // because the claimed state and pair digest parse successfully.
            for uncertain in [Unknown, Unreadable] {
                assert_eq!(
                    inspect_claim(&parsed.state, [uncertain, c_slots[1], c_slots[2]], l_slots),
                    Conflict
                );
                assert_eq!(
                    inspect_claim(&parsed.state, c_slots, [uncertain, l_slots[1], l_slots[2]]),
                    Conflict
                );
            }
        }
    }
}

#[test]
fn claimed_state_rejects_missing_unknown_duplicate_and_mistyped_fields() {
    let fields = fixture();
    let value = serde_json::to_value(&fields).unwrap();
    let state = value["state"].as_object().unwrap();
    for key in state.keys() {
        let mut missing = value.clone();
        missing["state"].as_object_mut().unwrap().remove(key);
        let (receipt, marker) = envelopes(&serde_json::to_vec(&missing).unwrap(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    }
    for (key, bad) in [
        ("transaction", serde_json::json!("recover")),
        ("controller_step", serde_json::json!("atomic_replace")),
        ("cli_step", serde_json::json!("Original")),
        ("new_health_recorded", serde_json::json!(1)),
        ("slots", serde_json::json!(["old", "new", "absent"])),
    ] {
        let mut bad_value = value.clone();
        bad_value["state"][key] = bad;
        let (receipt, marker) =
            envelopes(&serde_json::to_vec(&bad_value).unwrap(), &fields.binding);
        assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
    }
    let duplicate = serde_json::to_string(&fields).unwrap().replacen(
        "\"transaction\":\"prepared\"",
        "\"transaction\":\"prepared\",\"transaction\":\"prepared\"",
        1,
    );
    assert!(serde_json::from_str::<serde_json::Value>(&duplicate).is_ok());
    let (receipt, marker) = envelopes(duplicate.as_bytes(), &fields.binding);
    assert!(parse_pair(&receipt, &marker, &fields.binding).is_err());
}
