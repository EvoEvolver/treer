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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Provenance {
    release_sha: String,
    artifact_source_sha: String,
    sums_sha256: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AclPolicy {
    OwnerSystemProtected,
}

// This draft deliberately matches the native adapter's NTFS-only boundary.
// The label is not evidence: a future observer must verify the filesystem
// through its pinned volume handle before trusting the BY_HANDLE identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Filesystem {
    Ntfs,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    old: Object,
    new: Object,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    rollback_reason: ReasonClaim,
}

// Required key: unlike Option, absence is not accepted as null by serde.
// Both variants are untrusted serialized claims, never verified capabilities.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum ReasonClaim {
    None(()),
    Reference(ReasonReference),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReasonKind {
    ControllerActivationFailed,
    NewControllerUnhealthy,
    CliActivationFailed,
    FinalHealthFailed,
    DeadlineExceeded,
    HumanAbort,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReasonReference {
    kind: ReasonKind,
    entry_revision: u64,
    predecessor_receipt_sha256: String,
    evidence_sha256: String,
}

impl ReasonClaim {
    fn valid(&self, revision: u64) -> bool {
        match self {
            Self::None(()) => true,
            Self::Reference(reason) => {
                reason.entry_revision > 1
                    && reason.entry_revision <= revision
                    && hex(&reason.predecessor_receipt_sha256, 64)
                    && hex(&reason.evidence_sha256, 64)
            }
        }
    }
}

// Untrusted-label compatibility only, not evidence verification or permission.
fn reason_kind_matches_entry(
    kind: ReasonKind,
    prior: &ClaimedState,
    slots: [[layout::Object; 3]; 2],
) -> bool {
    use layout::{Step::*, Transaction::*};
    if inspect_claim(prior, slots[0], slots[1]) != layout::PairInspection::Consistent {
        return false;
    }
    let operation_started =
        |step| matches!(step, MoveOldIntent | OldMoved | PublishIntent | Published);
    match kind {
        ReasonKind::ControllerActivationFailed => {
            prior.transaction == Activating && operation_started(prior.controller_step)
        }
        ReasonKind::NewControllerUnhealthy => {
            prior.transaction == Activating && prior.controller_step == Published
        }
        ReasonKind::CliActivationFailed => {
            prior.transaction == CoreHealthy && operation_started(prior.cli_step)
        }
        ReasonKind::FinalHealthFailed => {
            prior.transaction == CoreHealthy && prior.cli_step == Published
        }
        ReasonKind::DeadlineExceeded | ReasonKind::HumanAbort => {
            matches!(prior.transaction, Prepared | Activating | CoreHealthy)
        }
    }
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
            && self.rollback_reason.valid(self.binding.revision)
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

// An independently supplied expected head in this model only. There is no
// implemented protected source, durable storage, restart ownership or CAS.
struct ExpectedHead {
    binding: Binding,
    receipt_sha256: String,
}

// Partial successor guard, NOT state-transition acceptance. Both pairs must
// parse, the predecessor must match the separately supplied exact-byte head,
// and only revision/state claims may vary. A later graph/evidence check must
// still refuse invalid state changes; this grants no publication authority.
fn check_successor_fields(
    previous: (&[u8], &[u8]),
    candidate: (&[u8], &[u8]),
    head: &ExpectedHead,
) -> Result<DraftFields, Refusal> {
    if !head.binding.valid()
        || !hex(&head.receipt_sha256, 64)
        || previous.0.len() > crate::windows_update_envelope::LIMIT
        || digest(previous.0) != head.receipt_sha256
    {
        return Err("predecessor_head_mismatch");
    }
    let old = parse_pair(previous.0, previous.1, &head.binding)?;
    let mut next_binding = head.binding.clone();
    next_binding.revision = head
        .binding
        .revision
        .checked_add(1)
        .ok_or("revision_exhausted")?;
    let next = parse_pair(candidate.0, candidate.1, &next_binding)?;
    // Exhaustive on purpose: adding ANY receipt field requires an explicit
    // immutability/transition decision here, rather than silently making it
    // mutable. Binding is checked by parse_pair above; state needs the later
    // phase/evidence guard and is not accepted by this partial check.
    let DraftFields {
        binding: _,
        provenance: old_provenance,
        images: old_images,
        state: _,
        rollback_reason: old_reason,
    } = &old;
    let DraftFields {
        binding: _,
        provenance: next_provenance,
        images: next_images,
        state: _,
        rollback_reason: next_reason,
    } = &next;
    if next_provenance != old_provenance || next_images != old_images {
        return Err("transaction_fields_changed");
    }
    // Conservative schema slice: creation at entry is deliberately not wired.
    if next_reason != old_reason {
        return Err("rollback_reason_changed");
    }
    Ok(next)
}

// Forward-only graph. Explicit rollback edges and authenticated health evidence
// are separate increments. Matching this graph never authorizes publication.
fn next_forward_claim(old: &ClaimedState) -> Option<ClaimedState> {
    use layout::{Step::*, Transaction::*};
    let advance = |step| match step {
        Original => Some(MoveOldIntent),
        MoveOldIntent => Some(OldMoved),
        OldMoved => Some(PublishIntent),
        PublishIntent => Some(Published),
        Published | MoveNewAsideIntent | NewAside | RestoreOldIntent | Restored => None,
    };
    let mut next = old.clone();
    match old.transaction {
        Prepared => next.transaction = Activating,
        Activating if old.controller_step == Published => {
            next.transaction = CoreHealthy;
            next.new_health_recorded = true;
        }
        Activating => next.controller_step = advance(old.controller_step)?,
        CoreHealthy if old.cli_step == Published => {
            next.transaction = Complete;
            next.final_health_recorded = true;
        }
        CoreHealthy => next.cli_step = advance(old.cli_step)?,
        Complete | RollbackCli | RollbackController | RolledBack => return None,
    }
    Some(next)
}

fn check_forward_successor(
    previous: (&[u8], &[u8]),
    candidate: (&[u8], &[u8]),
    head: &ExpectedHead,
    previous_slots: [[layout::Object; 3]; 2],
    candidate_slots: [[layout::Object; 3]; 2],
) -> Result<(), Refusal> {
    let next = check_successor_fields(previous, candidate, head)?;
    let old = parse_pair(previous.0, previous.1, &head.binding)?;
    if inspect_claim(&old.state, previous_slots[0], previous_slots[1])
        != layout::PairInspection::Consistent
        || inspect_claim(&next.state, candidate_slots[0], candidate_slots[1])
            != layout::PairInspection::Consistent
    {
        return Err("transition_layout_conflict");
    }
    if next_forward_claim(&old.state).as_ref() != Some(&next.state) {
        return Err("invalid_forward_transition");
    }
    // Only a synthetic graph check: bare health claims are not authenticated,
    // and the ExpectedHead still has no durable/independent implementation.
    Ok(())
}

// Inert entry classification only. Both claims use the SAME current snapshot;
// no rename, actor, quiescence proof or authenticated health exists here.
fn check_rollback_entry(
    previous: (&[u8], &[u8]),
    candidate: (&[u8], &[u8]),
    head: &ExpectedHead,
    slots: [[layout::Object; 3]; 2],
) -> Result<(), Refusal> {
    use layout::{Object::*, Step::*, Transaction::*};
    let next = check_successor_fields(previous, candidate, head)?;
    let old = parse_pair(previous.0, previous.1, &head.binding)?;
    if inspect_claim(&old.state, slots[0], slots[1]) != layout::PairInspection::Consistent
        || inspect_claim(&next.state, slots[0], slots[1]) != layout::PairInspection::Consistent
    {
        return Err("transition_layout_conflict");
    }
    let mut expected = old.state.clone();
    let moving_step = match old.state.transaction {
        Prepared | Activating => {
            expected.transaction = RollbackController;
            &mut expected.controller_step
        }
        CoreHealthy => {
            expected.transaction = RollbackCli;
            &mut expected.cli_step
        }
        Complete | RollbackCli | RollbackController | RolledBack => {
            return Err("invalid_rollback_entry");
        }
    };
    let moving_slots = slots[usize::from(old.state.transaction == CoreHealthy)];
    *moving_step = match moving_slots {
        [Old, New, Absent] => Original,
        [Absent, New, Old] => RestoreOldIntent,
        [New, Absent, Old] => MoveNewAsideIntent,
        _ => return Err("transition_layout_conflict"),
    };
    if expected != next.state {
        return Err("invalid_rollback_entry");
    }
    Ok(())
}

// Structural proposal only. Intent results still require a real post-operation
// observation, and the last edge needs authenticated health in a future API.
fn next_rollback_claim(old: &ClaimedState) -> Option<ClaimedState> {
    use layout::{Step::*, Transaction::*};
    if old.old_health_recorded || old.final_health_recorded {
        return None;
    }
    let mut next = old.clone();
    let step = match old.transaction {
        RollbackCli => old.cli_step,
        RollbackController => old.controller_step,
        Prepared | Activating | CoreHealthy | Complete | RolledBack => return None,
    };
    let result = match step {
        MoveNewAsideIntent => NewAside,
        NewAside => RestoreOldIntent,
        RestoreOldIntent => Restored,
        Original | Restored => {
            if old.transaction == RollbackCli {
                next.transaction = RollbackController;
                next.controller_step = MoveNewAsideIntent;
            } else {
                next.transaction = RolledBack;
                next.old_health_recorded = true;
            }
            return Some(next);
        }
        MoveOldIntent | OldMoved | PublishIntent | Published => return None,
    };
    if old.transaction == RollbackCli {
        next.cli_step = result;
    } else {
        next.controller_step = result;
    }
    Some(next)
}

fn check_rollback_successor(
    previous: (&[u8], &[u8]),
    candidate: (&[u8], &[u8]),
    head: &ExpectedHead,
    current_slots: [[layout::Object; 3]; 2],
) -> Result<(), Refusal> {
    let next = check_successor_fields(previous, candidate, head)?;
    let old = parse_pair(previous.0, previous.1, &head.binding)?;
    if inspect_claim(&old.state, current_slots[0], current_slots[1])
        != layout::PairInspection::Consistent
        || inspect_claim(&next.state, current_slots[0], current_slots[1])
            != layout::PairInspection::Consistent
    {
        return Err("transition_layout_conflict");
    }
    if next_rollback_claim(&old.state).as_ref() != Some(&next.state) {
        return Err("invalid_rollback_transition");
    }
    // No actor, reason verifier, health verifier, head storage or mutation API.
    Ok(())
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
        rollback_reason: ReasonClaim::None(()),
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

#[test]
fn successor_fields_require_exact_predecessor_and_one_revision_step() {
    let old = fixture();
    let (old_receipt, old_marker) = envelopes(&serde_json::to_vec(&old).unwrap(), &old.binding);
    let head = ExpectedHead {
        binding: old.binding.clone(),
        receipt_sha256: digest(&old_receipt),
    };
    let mut next = old.clone();
    next.binding.revision += 1;
    let (receipt, marker) = envelopes(&serde_json::to_vec(&next).unwrap(), &next.binding);
    assert!(
        check_successor_fields((&old_receipt, &old_marker), (&receipt, &marker), &head).is_ok()
    );
    // There is deliberately no phase-graph acceptance yet: unchanged state
    // claims at the next revision pass only this partial field guard.
    for revision in [0, 1, 3, u64::MAX] {
        next.binding.revision = revision;
        let (receipt, marker) = envelopes(&serde_json::to_vec(&next).unwrap(), &next.binding);
        assert!(
            check_successor_fields((&old_receipt, &old_marker), (&receipt, &marker), &head)
                .is_err()
        );
    }
    let mut changed_prior = old.clone();
    changed_prior.provenance.release_sha = "e".repeat(40);
    let (other_receipt, other_marker) = envelopes(
        &serde_json::to_vec(&changed_prior).unwrap(),
        &changed_prior.binding,
    );
    assert_eq!(
        check_successor_fields((&other_receipt, &other_marker), (&receipt, &marker), &head)
            .unwrap_err(),
        "predecessor_head_mismatch"
    );
    assert!(check_successor_fields((&old_receipt, &[]), (&receipt, &marker), &head).is_err());
    let mut exhausted = old;
    exhausted.binding.revision = u64::MAX;
    let (full_receipt, full_marker) =
        envelopes(&serde_json::to_vec(&exhausted).unwrap(), &exhausted.binding);
    let exhausted_head = ExpectedHead {
        binding: exhausted.binding.clone(),
        receipt_sha256: digest(&full_receipt),
    };
    assert_eq!(
        check_successor_fields(
            (&full_receipt, &full_marker),
            (&receipt, &marker),
            &exhausted_head
        )
        .unwrap_err(),
        "revision_exhausted"
    );
    // Replacing both pairs AND the synthetic head remains possible in this
    // model. Passing this check therefore does not establish head freshness.
    let other_head = ExpectedHead {
        binding: changed_prior.binding.clone(),
        receipt_sha256: digest(&other_receipt),
    };
    changed_prior.binding.revision += 1;
    let (other_next, other_next_marker) = envelopes(
        &serde_json::to_vec(&changed_prior).unwrap(),
        &changed_prior.binding,
    );
    assert!(check_successor_fields(
        (&other_receipt, &other_marker),
        (&other_next, &other_next_marker),
        &other_head
    )
    .is_ok());
}

#[test]
fn successor_fields_refuse_valid_but_retargeted_provenance_or_image_records() {
    let old = fixture();
    let (old_receipt, old_marker) = envelopes(&serde_json::to_vec(&old).unwrap(), &old.binding);
    let head = ExpectedHead {
        binding: old.binding.clone(),
        receipt_sha256: digest(&old_receipt),
    };
    let mut next = old.clone();
    next.binding.revision += 1;
    let value = serde_json::to_value(&next).unwrap();
    let mut mutations = vec![
        (
            "/provenance/release_sha".to_owned(),
            serde_json::json!("e".repeat(40)),
        ),
        (
            "/provenance/artifact_source_sha".to_owned(),
            serde_json::json!("f".repeat(40)),
        ),
        (
            "/provenance/sums_sha256".to_owned(),
            serde_json::json!("e".repeat(64)),
        ),
    ];
    for image in ["controller", "cli"] {
        for version in ["old", "new"] {
            for (field, replacement) in [
                ("volume_serial", serde_json::json!(8)),
                ("file_id", serde_json::json!(99)),
                ("bytes", serde_json::json!(124)),
                ("sha256", serde_json::json!("e".repeat(64))),
                ("attributes", serde_json::json!(0x01)),
            ] {
                mutations.push((format!("/images/{image}/{version}/{field}"), replacement));
            }
        }
    }
    for (pointer, replacement) in mutations {
        let mut changed = value.clone();
        *changed.pointer_mut(&pointer).unwrap() = replacement;
        let (receipt, marker) = envelopes(&serde_json::to_vec(&changed).unwrap(), &next.binding);
        // Every changed record is valid by itself; this is predecessor
        // immutability rejection, not a malformed-field or pair-digest error.
        assert!(parse_pair(&receipt, &marker, &next.binding).is_ok());
        assert_eq!(
            check_successor_fields((&old_receipt, &old_marker), (&receipt, &marker), &head)
                .unwrap_err(),
            "transaction_fields_changed",
            "{pointer}"
        );
    }
    for (field, replacement) in [
        ("workspace_id", "ws_other"),
        ("server_id", "srv_other"),
        ("job_id", "22222222-2222-4222-8222-222222222222"),
    ] {
        let mut changed = value.clone();
        changed["binding"][field] = replacement.into();
        let changed: DraftFields = serde_json::from_value(changed).unwrap();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&changed).unwrap(), &changed.binding);
        assert!(parse_pair(&receipt, &marker, &changed.binding).is_ok());
        assert!(
            check_successor_fields((&old_receipt, &old_marker), (&receipt, &marker), &head)
                .is_err()
        );
    }
}

fn forward_fixture_trace() -> Vec<(ClaimedState, [[layout::Object; 3]; 2])> {
    use layout::{Object::*, Step::*, Transaction::*};
    let original = [Old, New, Absent];
    let published = [New, Absent, Old];
    let mut trace = vec![(fixture().state, [original, original])];
    // Explicit expected trace, independent of next_forward_claim.
    for transaction in [Activating, CoreHealthy] {
        for (step, slots) in [
            (Original, original),
            (MoveOldIntent, original),
            (OldMoved, [Absent, New, Old]),
            (PublishIntent, [Absent, New, Old]),
            (Published, published),
        ] {
            let core_healthy = transaction == CoreHealthy;
            trace.push((
                ClaimedState {
                    transaction,
                    controller_step: if core_healthy { Published } else { step },
                    cli_step: if core_healthy { step } else { Original },
                    new_health_recorded: core_healthy,
                    final_health_recorded: false,
                    old_health_recorded: false,
                },
                if core_healthy {
                    [published, slots]
                } else {
                    [slots, original]
                },
            ));
        }
    }
    trace.push((
        ClaimedState {
            transaction: Complete,
            controller_step: Published,
            cli_step: Published,
            new_health_recorded: true,
            final_health_recorded: true,
            old_health_recorded: false,
        },
        [published, published],
    ));
    trace
}

fn forward_fixture_check(
    old: &ClaimedState,
    next: &ClaimedState,
    old_slots: [[layout::Object; 3]; 2],
    next_slots: [[layout::Object; 3]; 2],
) -> Result<(), Refusal> {
    let mut prior = fixture();
    prior.state = old.clone();
    let (old_receipt, old_marker) = envelopes(&serde_json::to_vec(&prior).unwrap(), &prior.binding);
    let head = ExpectedHead {
        binding: prior.binding.clone(),
        receipt_sha256: digest(&old_receipt),
    };
    let mut candidate = prior;
    candidate.binding.revision += 1;
    candidate.state = next.clone();
    let (receipt, marker) = envelopes(&serde_json::to_vec(&candidate).unwrap(), &candidate.binding);
    check_forward_successor(
        (&old_receipt, &old_marker),
        (&receipt, &marker),
        &head,
        old_slots,
        next_slots,
    )
}

#[test]
fn forward_graph_accepts_only_adjacent_claims_not_noops_skips_or_reversals() {
    let trace = forward_fixture_trace();
    assert_eq!(trace.len(), 12);
    for (old_index, (old, old_slots)) in trace.iter().enumerate() {
        for (next_index, (next, next_slots)) in trace.iter().enumerate() {
            // Every state is independently well-formed and layout-consistent.
            assert_eq!(
                inspect_claim(old, old_slots[0], old_slots[1]),
                layout::PairInspection::Consistent
            );
            assert_eq!(
                inspect_claim(next, next_slots[0], next_slots[1]),
                layout::PairInspection::Consistent
            );
            let result = forward_fixture_check(old, next, *old_slots, *next_slots);
            assert_eq!(
                result.is_ok(),
                next_index == old_index + 1,
                "{old_index}->{next_index}: {result:?}"
            );
        }
    }
    // A valid rollback layout/claim is outside this forward-only graph.
    let mut rollback = trace[0].0.clone();
    rollback.transaction = layout::Transaction::RollbackController;
    assert_eq!(
        inspect_claim(&rollback, trace[0].1[0], trace[0].1[1]),
        layout::PairInspection::Consistent
    );
    assert_eq!(
        forward_fixture_check(&trace[0].0, &rollback, trace[0].1, trace[0].1),
        Err("invalid_forward_transition")
    );
    assert_eq!(
        forward_fixture_check(&rollback, &trace[0].0, trace[0].1, trace[0].1),
        Err("invalid_forward_transition")
    );
}

#[test]
fn forward_graph_checks_both_layouts_and_health_changes_at_each_edge() {
    use layout::Object::{Unknown, Unreadable};
    let trace = forward_fixture_trace();
    for edge in trace.windows(2) {
        let (old, old_slots) = &edge[0];
        let (next, next_slots) = &edge[1];
        for image in 0..2 {
            for slot in 0..3 {
                for uncertain in [Unknown, Unreadable] {
                    let mut bad_old = *old_slots;
                    bad_old[image][slot] = uncertain;
                    assert_eq!(
                        forward_fixture_check(old, next, bad_old, *next_slots),
                        Err("transition_layout_conflict")
                    );
                    let mut bad_next = *next_slots;
                    bad_next[image][slot] = uncertain;
                    assert_eq!(
                        forward_fixture_check(old, next, *old_slots, bad_next),
                        Err("transition_layout_conflict")
                    );
                }
            }
        }
        for flags in 0..8 {
            let mut altered = next.clone();
            altered.new_health_recorded = flags & 1 != 0;
            altered.final_health_recorded = flags & 2 != 0;
            altered.old_health_recorded = flags & 4 != 0;
            assert_eq!(
                forward_fixture_check(old, &altered, *old_slots, *next_slots).is_ok(),
                altered == *next
            );
        }
    }
}

#[test]
fn rollback_entry_uses_current_layout_and_preserves_other_image_and_health() {
    use layout::{Object::*, Step::*, Transaction::*};
    let trace = forward_fixture_trace();
    let mut cases = Vec::new();
    // Explicit expected exits for the 12 canonical forward trace states.
    for (index, (state, slots)) in trace.iter().enumerate() {
        let step = [
            Original,
            Original,
            Original,
            RestoreOldIntent,
            RestoreOldIntent,
            MoveNewAsideIntent,
            Original,
            Original,
            RestoreOldIntent,
            RestoreOldIntent,
            MoveNewAsideIntent,
            MoveNewAsideIntent,
        ][index];
        cases.push((state.clone(), *slots, step));
        // Each intent also has an independently legal post-operation layout.
        if matches!(index, 2 | 4 | 7 | 9) {
            let mut post = *slots;
            let image = usize::from(index >= 6);
            post[image] = if matches!(index, 2 | 7) {
                [Absent, New, Old]
            } else {
                [New, Absent, Old]
            };
            cases.push((
                state.clone(),
                post,
                if matches!(index, 2 | 7) {
                    RestoreOldIntent
                } else {
                    MoveNewAsideIntent
                },
            ));
        }
    }
    assert_eq!(cases.len(), 16);
    for (old_state, slots, expected_step) in cases {
        let mut old = fixture();
        old.state = old_state.clone();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&old).unwrap(), &old.binding);
        let head = ExpectedHead {
            binding: old.binding.clone(),
            receipt_sha256: digest(&receipt),
        };
        let mut candidate = old.clone();
        candidate.binding.revision += 1;
        let cli = old_state.transaction == CoreHealthy;
        candidate.state.transaction = if cli { RollbackCli } else { RollbackController };
        if cli {
            candidate.state.cli_step = expected_step;
        } else {
            candidate.state.controller_step = expected_step;
        }
        let check = |fields: &DraftFields, observed| {
            let (next_receipt, next_marker) =
                envelopes(&serde_json::to_vec(fields).unwrap(), &fields.binding);
            check_rollback_entry(
                (&receipt, &marker),
                (&next_receipt, &next_marker),
                &head,
                observed,
            )
        };
        if old_state.transaction == Complete {
            assert!(check(&candidate, slots).is_err());
            continue;
        }
        assert_eq!(check(&candidate, slots), Ok(()));
        for phase in [
            Prepared,
            Activating,
            CoreHealthy,
            Complete,
            RollbackCli,
            RollbackController,
            RolledBack,
        ] {
            for step in [
                Original,
                MoveOldIntent,
                OldMoved,
                PublishIntent,
                Published,
                MoveNewAsideIntent,
                NewAside,
                RestoreOldIntent,
                Restored,
            ] {
                let mut changed = candidate.clone();
                changed.state.transaction = phase;
                if cli {
                    changed.state.cli_step = step;
                } else {
                    changed.state.controller_step = step;
                }
                assert_eq!(
                    check(&changed, slots).is_ok(),
                    changed.state == candidate.state
                );
            }
        }
        // No-op, forward continuation, premature health and damage to either
        // image must not become entry authorization.
        let mut noop = candidate.clone();
        noop.state = old_state;
        assert!(check(&noop, slots).is_err());
        for flags in 0..8 {
            let mut changed = candidate.clone();
            changed.state.new_health_recorded = flags & 1 != 0;
            changed.state.final_health_recorded = flags & 2 != 0;
            changed.state.old_health_recorded = flags & 4 != 0;
            assert_eq!(
                check(&changed, slots).is_ok(),
                changed.state == candidate.state
            );
        }
        for image in 0..2 {
            for slot in 0..3 {
                for uncertain in [Unknown, Unreadable] {
                    let mut changed = slots;
                    changed[image][slot] = uncertain;
                    assert_eq!(
                        check(&candidate, changed),
                        Err("transition_layout_conflict")
                    );
                }
            }
        }
        let mut wrong_other = candidate.clone();
        if cli {
            wrong_other.state.controller_step = MoveNewAsideIntent;
        } else {
            wrong_other.state.cli_step = Restored;
        }
        assert!(check(&wrong_other, slots).is_err());
    }
}

#[test]
fn rollback_continuation_classifies_every_consistent_tuple() {
    use layout::{Object::*, Step::*, Transaction::*};
    // Independent edge table: phase, moving step, next phase, next moving
    // step. Handoff changes the Controller step rather than the CLI step.
    let rows = [
        (
            RollbackCli,
            Original,
            RollbackController,
            MoveNewAsideIntent,
        ),
        (
            RollbackCli,
            Restored,
            RollbackController,
            MoveNewAsideIntent,
        ),
        (RollbackCli, MoveNewAsideIntent, RollbackCli, NewAside),
        (RollbackCli, NewAside, RollbackCli, RestoreOldIntent),
        (RollbackCli, RestoreOldIntent, RollbackCli, Restored),
        (RollbackController, Original, RolledBack, Original),
        (RollbackController, Restored, RolledBack, Restored),
        (
            RollbackController,
            MoveNewAsideIntent,
            RollbackController,
            NewAside,
        ),
        (
            RollbackController,
            NewAside,
            RollbackController,
            RestoreOldIntent,
        ),
        (
            RollbackController,
            RestoreOldIntent,
            RollbackController,
            Restored,
        ),
    ];
    let steps = [
        Original,
        MoveOldIntent,
        OldMoved,
        PublishIntent,
        Published,
        MoveNewAsideIntent,
        NewAside,
        RestoreOldIntent,
        Restored,
    ];
    let layouts = [[Old, New, Absent], [Absent, New, Old], [New, Absent, Old]];
    let mut hits = [0; 10];
    let mut observed_stops = std::collections::BTreeSet::new();
    for phase in [
        Prepared,
        Activating,
        CoreHealthy,
        Complete,
        RollbackCli,
        RollbackController,
        RolledBack,
    ] {
        for c in steps {
            for l in steps {
                for cs in layouts {
                    for ls in layouts {
                        for flags in 0..8 {
                            let old = ClaimedState {
                                transaction: phase,
                                controller_step: c,
                                cli_step: l,
                                new_health_recorded: flags & 1 != 0,
                                final_health_recorded: flags & 2 != 0,
                                old_health_recorded: flags & 4 != 0,
                            };
                            if inspect_claim(&old, cs, ls) != layout::PairInspection::Consistent {
                                continue;
                            }
                            let moving = if phase == RollbackCli { l } else { c };
                            let matching: Vec<_> = rows
                                .iter()
                                .enumerate()
                                .filter(|(_, row)| {
                                    row.0 == phase && row.1 == moving && flags & 6 == 0
                                })
                                .collect();
                            assert!(matching.len() <= 1);
                            let expected = matching.first().map(|(index, row)| {
                                hits[*index] += 1;
                                let mut next = old.clone();
                                next.transaction = row.2;
                                if row.2 == RolledBack {
                                    next.old_health_recorded = true;
                                } else if row.2 == RollbackController {
                                    next.controller_step = row.3;
                                } else {
                                    next.cli_step = row.3;
                                }
                                next
                            });
                            // Independent, closed stop inventory: never let
                            // absence from BOTH graph and table invent a stop.
                            let mut reasons = Vec::new();
                            if flags & 2 != 0 {
                                reasons.push("final_health");
                            }
                            if flags & 4 != 0 {
                                reasons.push("old_health");
                            }
                            match (phase, moving) {
                                (Prepared | Activating | CoreHealthy, _) => {
                                    reasons.push("non_rollback")
                                }
                                (Complete, _) => reasons.push("terminal_complete"),
                                (RolledBack, _) => reasons.push("terminal_rolled_back"),
                                (RollbackCli, Published) => reasons.push("cli_published"),
                                (RollbackController, MoveOldIntent) => {
                                    reasons.push("controller_move_old_intent")
                                }
                                (RollbackController, OldMoved) => {
                                    reasons.push("controller_old_moved")
                                }
                                (RollbackController, PublishIntent) => {
                                    reasons.push("controller_publish_intent")
                                }
                                (RollbackController, Published) => {
                                    reasons.push("controller_published")
                                }
                                _ => {}
                            }
                            assert_eq!(
                                expected.is_none(),
                                !reasons.is_empty(),
                                "missing edge or unexpected stop: {old:?}"
                            );
                            observed_stops.extend(reasons);
                            assert_eq!(
                                next_rollback_claim(&old),
                                expected,
                                "{old:?}, slots={cs:?}/{ls:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(hits.into_iter().all(|count| count > 0));
    assert_eq!(
        observed_stops,
        std::collections::BTreeSet::from([
            "final_health",
            "old_health",
            "non_rollback",
            "terminal_complete",
            "terminal_rolled_back",
            "cli_published",
            "controller_move_old_intent",
            "controller_old_moved",
            "controller_publish_intent",
            "controller_published",
        ])
    );
}

fn rollback_fixture_traces() -> [Vec<(ClaimedState, [[layout::Object; 3]; 2])>; 6] {
    use layout::{Object::*, Step::*, Transaction::*};
    let original = [Old, New, Absent];
    let gap = [Absent, New, Old];
    let published = [New, Absent, Old];
    let mut trace = Vec::new();
    for phase in [RollbackCli, RollbackController] {
        for (step, slots) in [
            (MoveNewAsideIntent, published),
            (NewAside, gap),
            (RestoreOldIntent, gap),
            (Restored, original),
        ] {
            trace.push((
                ClaimedState {
                    transaction: phase,
                    controller_step: if phase == RollbackCli {
                        Published
                    } else {
                        step
                    },
                    cli_step: if phase == RollbackCli { step } else { Restored },
                    new_health_recorded: true,
                    final_health_recorded: false,
                    old_health_recorded: false,
                },
                if phase == RollbackCli {
                    [published, slots]
                } else {
                    [slots, original]
                },
            ));
        }
    }
    let mut terminal = trace.last().unwrap().0.clone();
    terminal.transaction = RolledBack;
    terminal.old_health_recorded = true;
    trace.push((terminal, [original, original]));
    // Independently select the shorter branches instead of deriving them from
    // next_rollback_claim. Preserve an untouched CLI as Original at handoff.
    let mut untouched_cli = trace[3..].to_vec();
    for (claim, _) in &mut untouched_cli {
        claim.cli_step = Original;
    }
    let mut no_move = fixture().state;
    no_move.transaction = RollbackController;
    let mut no_move_terminal = no_move.clone();
    no_move_terminal.transaction = RolledBack;
    no_move_terminal.old_health_recorded = true;
    let no_move_trace = vec![
        (no_move, [original, original]),
        (no_move_terminal, [original, original]),
    ];
    let mut cli_already_restored = no_move_trace.clone();
    for (claim, _) in &mut cli_already_restored {
        claim.cli_step = Restored;
    }
    [
        trace.clone(),
        untouched_cli,
        trace[2..].to_vec(), // Direct CLI gap restoration.
        trace[6..].to_vec(), // Direct Controller gap restoration.
        no_move_trace,
        cli_already_restored,
    ]
}

#[test]
fn rollback_record_edges_require_actual_results_and_exact_health_claims() {
    for branch in rollback_fixture_traces() {
        for historical_new_health in [false, true] {
            let mut branch = branch.clone();
            for (claim, slots) in &mut branch {
                claim.new_health_recorded = historical_new_health;
                assert_eq!(
                    inspect_claim(claim, slots[0], slots[1]),
                    layout::PairInspection::Consistent
                );
            }
            assert_rollback_record_trace(&branch);
        }
    }
}

#[test]
fn rollback_chains_advance_exact_heads_and_refuse_replay_or_rewriting() {
    for branch in rollback_fixture_traces() {
        for historical_new_health in [false, true] {
            let records: Vec<_> = branch
                .iter()
                .enumerate()
                .map(|(index, (claim, _))| {
                    let mut fields = fixture();
                    fields.binding.revision = index as u64 + 1;
                    fields.state = claim.clone();
                    fields.state.new_health_recorded = historical_new_health;
                    let pair = envelopes(&serde_json::to_vec(&fields).unwrap(), &fields.binding);
                    (fields, pair)
                })
                .collect();
            let mut head = ExpectedHead {
                binding: records[0].0.binding.clone(),
                receipt_sha256: digest(&records[0].1 .0),
            };
            for index in 1..records.len() {
                let (old, (old_receipt, old_marker)) = &records[index - 1];
                let (next, (receipt, marker)) = &records[index];
                let slots = branch[index].1;
                let check = |prior: (&[u8], &[u8]), candidate: (&[u8], &[u8])| {
                    check_rollback_successor(prior, candidate, &head, slots)
                };
                assert_eq!(check((old_receipt, old_marker), (receipt, marker)), Ok(()));
                // Earlier records remain syntactically valid but are no
                // longer the predecessor, even with their matching marker.
                if index > 1 {
                    assert_eq!(
                        check((&records[0].1 .0, &records[0].1 .1), (receipt, marker)),
                        Err("predecessor_head_mismatch")
                    );
                }
                assert!(check((old_receipt, old_marker), (old_receipt, old_marker)).is_err());
                let mut skipped = next.clone();
                skipped.binding.revision += 1;
                let (r, m) = envelopes(&serde_json::to_vec(&skipped).unwrap(), &skipped.binding);
                assert!(check((old_receipt, old_marker), (&r, &m)).is_err());
                let mut replaced = old.clone();
                replaced.provenance.release_sha = "e".repeat(40);
                let (r, m) = envelopes(&serde_json::to_vec(&replaced).unwrap(), &replaced.binding);
                assert!(parse_pair(&r, &m, &replaced.binding).is_ok());
                assert_eq!(
                    check((&r, &m), (receipt, marker)),
                    Err("predecessor_head_mismatch")
                );
                let mut changed = next.clone();
                changed.images.controller.old.sha256 = "e".repeat(64);
                let (r, m) = envelopes(&serde_json::to_vec(&changed).unwrap(), &changed.binding);
                assert!(parse_pair(&r, &m, &changed.binding).is_ok());
                assert_eq!(
                    check((old_receipt, old_marker), (&r, &m)),
                    Err("transaction_fields_changed")
                );
                // Advance only this synthetic in-memory test head after the
                // successful edge. This is not a durable CAS or restart test.
                head = ExpectedHead {
                    binding: next.binding.clone(),
                    receipt_sha256: digest(receipt),
                };
            }
            let (terminal, (receipt, marker)) = records.last().unwrap();
            assert_eq!(head.binding, terminal.binding);
            assert_eq!(head.receipt_sha256, digest(receipt));
            let mut retry = terminal.clone();
            retry.binding.revision += 1;
            let (r, m) = envelopes(&serde_json::to_vec(&retry).unwrap(), &retry.binding);
            assert_eq!(
                check_rollback_successor(
                    (receipt, marker),
                    (&r, &m),
                    &head,
                    branch.last().unwrap().1
                ),
                Err("invalid_rollback_transition")
            );
        }
    }
}

#[test]
fn reason_schema_requires_explicit_key_and_strict_bounded_reference() {
    let mut fields = fixture();
    fields.binding.revision = 2;
    let value = serde_json::to_value(&fields).unwrap();
    assert!(value["rollback_reason"].is_null());
    let accepts = |value: &serde_json::Value| {
        let (r, m) = envelopes(&serde_json::to_vec(value).unwrap(), &fields.binding);
        parse_pair(&r, &m, &fields.binding).is_ok()
    };
    let mut missing = value.clone();
    missing.as_object_mut().unwrap().remove("rollback_reason");
    assert!(!accepts(&missing));
    for kind in [
        "controller_activation_failed",
        "new_controller_unhealthy",
        "cli_activation_failed",
        "final_health_failed",
        "deadline_exceeded",
        "human_abort",
    ] {
        let mut valid = value.clone();
        valid["rollback_reason"] = serde_json::json!({
            "kind":kind, "entry_revision":2,
            "predecessor_receipt_sha256":"a".repeat(64), "evidence_sha256":"b".repeat(64)
        });
        assert!(accepts(&valid)); // Shape only, even in a forward claim.
        for key in [
            "kind",
            "entry_revision",
            "predecessor_receipt_sha256",
            "evidence_sha256",
        ] {
            let mut bad = valid.clone();
            bad["rollback_reason"].as_object_mut().unwrap().remove(key);
            assert!(!accepts(&bad));
        }
        for (key, replacement) in [
            ("kind", serde_json::json!("unknown")),
            ("entry_revision", serde_json::json!(0)),
            ("entry_revision", serde_json::json!(1)),
            ("entry_revision", serde_json::json!(3)),
            ("entry_revision", serde_json::json!("2")),
            ("entry_revision", serde_json::json!(-1)),
            ("evidence_sha256", serde_json::json!("B".repeat(64))),
            (
                "predecessor_receipt_sha256",
                serde_json::json!("a".repeat(63)),
            ),
            ("extra", serde_json::json!(true)),
        ] {
            let mut bad = valid.clone();
            bad["rollback_reason"][key] = replacement;
            assert!(!accepts(&bad), "{key}");
        }
        let encoded = serde_json::to_string(&valid).unwrap();
        let duplicate = encoded.replace(
            "\"entry_revision\":2",
            "\"entry_revision\":2,\"entry_revision\":2",
        );
        assert!(serde_json::from_str::<serde_json::Value>(&duplicate).is_ok());
        let (r, m) = envelopes(duplicate.as_bytes(), &fields.binding);
        assert!(parse_pair(&r, &m, &fields.binding).is_err());
    }
    for bad in [
        serde_json::json!([]),
        serde_json::json!(false),
        serde_json::json!("null"),
    ] {
        let mut malformed = value.clone();
        malformed["rollback_reason"] = bad;
        assert!(!accepts(&malformed));
    }
}

#[test]
fn reason_schema_slice_preserves_reference_but_refuses_creation_or_replacement() {
    for has_reason in [false, true] {
        let mut old = fixture();
        old.binding.revision = 2;
        let reference = ReasonClaim::Reference(ReasonReference {
            kind: ReasonKind::DeadlineExceeded,
            entry_revision: 2,
            predecessor_receipt_sha256: "a".repeat(64),
            evidence_sha256: "b".repeat(64),
        });
        if has_reason {
            old.rollback_reason = reference.clone();
        }
        let (r, m) = envelopes(&serde_json::to_vec(&old).unwrap(), &old.binding);
        let head = ExpectedHead {
            binding: old.binding.clone(),
            receipt_sha256: digest(&r),
        };
        let mut next = old.clone();
        next.binding.revision += 1;
        let check = |fields: &DraftFields| {
            let (nr, nm) = envelopes(&serde_json::to_vec(fields).unwrap(), &fields.binding);
            assert!(parse_pair(&nr, &nm, &fields.binding).is_ok());
            check_successor_fields((&r, &m), (&nr, &nm), &head).map(|_| ())
        };
        assert_eq!(check(&next), Ok(()));
        let mut changed = next.clone();
        changed.rollback_reason = if has_reason {
            ReasonClaim::None(())
        } else {
            reference
        };
        assert_eq!(check(&changed), Err("rollback_reason_changed"));
        if has_reason {
            for field in [
                "kind",
                "entry_revision",
                "predecessor_receipt_sha256",
                "evidence_sha256",
            ] {
                let mut changed = serde_json::to_value(&next).unwrap();
                changed["rollback_reason"][field] = match field {
                    "kind" => serde_json::json!("human_abort"),
                    "entry_revision" => serde_json::json!(3),
                    _ => serde_json::json!("c".repeat(64)),
                };
                let changed = serde_json::from_value(changed).unwrap();
                assert_eq!(check(&changed), Err("rollback_reason_changed"));
            }
        }
    }
}

#[test]
fn reason_kind_table_requires_matching_forward_phase_step_and_layout() {
    use layout::{Object::*, Step::*, Transaction::*};
    // Independent explicit row inventory, not the helper's range predicate.
    let rows = [
        (
            Activating,
            MoveOldIntent,
            ReasonKind::ControllerActivationFailed,
        ),
        (Activating, OldMoved, ReasonKind::ControllerActivationFailed),
        (
            Activating,
            PublishIntent,
            ReasonKind::ControllerActivationFailed,
        ),
        (
            Activating,
            Published,
            ReasonKind::ControllerActivationFailed,
        ),
        (Activating, Published, ReasonKind::NewControllerUnhealthy),
        (CoreHealthy, MoveOldIntent, ReasonKind::CliActivationFailed),
        (CoreHealthy, OldMoved, ReasonKind::CliActivationFailed),
        (CoreHealthy, PublishIntent, ReasonKind::CliActivationFailed),
        (CoreHealthy, Published, ReasonKind::CliActivationFailed),
        (CoreHealthy, Published, ReasonKind::FinalHealthFailed),
    ];
    let kinds = [
        ReasonKind::ControllerActivationFailed,
        ReasonKind::NewControllerUnhealthy,
        ReasonKind::CliActivationFailed,
        ReasonKind::FinalHealthFailed,
        ReasonKind::DeadlineExceeded,
        ReasonKind::HumanAbort,
    ];
    let steps = [
        Original,
        MoveOldIntent,
        OldMoved,
        PublishIntent,
        Published,
        MoveNewAsideIntent,
        NewAside,
        RestoreOldIntent,
        Restored,
    ];
    let layouts = [[Old, New, Absent], [Absent, New, Old], [New, Absent, Old]];
    let mut accepted = [0; 6];
    let mut refused = [0; 6];
    for phase in [
        Prepared,
        Activating,
        CoreHealthy,
        Complete,
        RollbackCli,
        RollbackController,
        RolledBack,
    ] {
        for c in steps {
            for l in steps {
                for cs in layouts {
                    for ls in layouts {
                        for flags in 0..8 {
                            let state = ClaimedState {
                                transaction: phase,
                                controller_step: c,
                                cli_step: l,
                                new_health_recorded: flags & 1 != 0,
                                final_health_recorded: flags & 2 != 0,
                                old_health_recorded: flags & 4 != 0,
                            };
                            let consistent =
                                inspect_claim(&state, cs, ls) == layout::PairInspection::Consistent;
                            for (index, kind) in kinds.into_iter().enumerate() {
                                let moving = if phase == CoreHealthy { l } else { c };
                                let expected_label = if index >= 4 {
                                    [Prepared, Activating, CoreHealthy].contains(&phase)
                                } else {
                                    rows.contains(&(phase, moving, kind))
                                };
                                let expected = consistent && expected_label;
                                assert_eq!(
                                    reason_kind_matches_entry(kind, &state, [cs, ls]),
                                    expected,
                                    "{kind:?}: {state:?} {cs:?}/{ls:?}"
                                );
                                if expected {
                                    accepted[index] += 1;
                                } else {
                                    refused[index] += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(accepted.into_iter().all(|count| count > 0));
    assert!(refused.into_iter().all(|count| count > 0));
    for (state, slots) in forward_fixture_trace() {
        for kind in kinds {
            for image in 0..2 {
                for slot in 0..3 {
                    for uncertain in [Unknown, Unreadable] {
                        let mut damaged = slots;
                        damaged[image][slot] = uncertain;
                        assert!(!reason_kind_matches_entry(kind, &state, damaged));
                    }
                }
            }
        }
    }
}

fn assert_rollback_record_trace(trace: &[(ClaimedState, [[layout::Object; 3]; 2])]) {
    use layout::Object::{Unknown, Unreadable};
    for (i, (old, _)) in trace.iter().enumerate() {
        let mut fields = fixture();
        fields.state = old.clone();
        let (receipt, marker) = envelopes(&serde_json::to_vec(&fields).unwrap(), &fields.binding);
        let head = ExpectedHead {
            binding: fields.binding.clone(),
            receipt_sha256: digest(&receipt),
        };
        for (j, (next, slots)) in trace.iter().enumerate() {
            let mut candidate = fields.clone();
            candidate.binding.revision += 1;
            candidate.state = next.clone();
            let check = |claim: &DraftFields, observed| {
                let (r, m) = envelopes(&serde_json::to_vec(claim).unwrap(), &claim.binding);
                check_rollback_successor((&receipt, &marker), (&r, &m), &head, observed)
            };
            assert_eq!(check(&candidate, *slots).is_ok(), j == i + 1, "{i}->{j}");
            if j != i + 1 {
                continue;
            }
            for flags in 0..8 {
                let mut changed = candidate.clone();
                changed.state.new_health_recorded = flags & 1 != 0;
                changed.state.final_health_recorded = flags & 2 != 0;
                changed.state.old_health_recorded = flags & 4 != 0;
                assert_eq!(
                    check(&changed, *slots).is_ok(),
                    changed.state == candidate.state
                );
            }
            for image in 0..2 {
                for slot in 0..3 {
                    for uncertain in [Unknown, Unreadable] {
                        let mut changed = *slots;
                        changed[image][slot] = uncertain;
                        assert_eq!(
                            check(&candidate, changed),
                            Err("transition_layout_conflict")
                        );
                    }
                }
            }
        }
    }
}
