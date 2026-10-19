//! Synthetic verifier composition only, reachable solely through the cfg(test)
//! parent. No native source, ownership proof, publication or production token.
#![cfg(test)]
use super::*;
use std::cell::Cell;

const EVIDENCE_LIMIT: usize = 8192;

// These numeric identities are synthetic stand-ins, NOT native handles or
// authenticated monitor/ownership identities. No serde constructor for a live
// attempt or its single-use verification result exists.
struct LiveAttempt {
    ownership: u64,
    monitor: u64,
    alive: Cell<bool>,
    entered: Cell<bool>,
}

struct EntryContext<'a> {
    binding: Binding,
    predecessor: String,
    images: Images,
    prior: ClaimedState,
    slots: [[layout::Object; 3]; 2],
    live: &'a LiveAttempt,
    process: Process,
    deadline: u64,
    operation_intent_revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Process {
    handle_identity: u64,
    start_instance: u64,
    image_sha256: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ImageRole {
    Controller,
    Cli,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FailureStage {
    MoveOld,
    Publish,
    VerifyImage,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HealthPredicate {
    IntendedBuild,
    ProxyConnected,
    HostUnchanged,
    LiveAgentTuples,
    CliImageHash,
}

fn required_predicate(kind: ReasonKind, predicate: HealthPredicate) -> bool {
    use HealthPredicate::*;
    matches!(
        (kind, predicate),
        (
            ReasonKind::NewControllerUnhealthy | ReasonKind::FinalHealthFailed,
            IntendedBuild | ProxyConnected | HostUnchanged | LiveAgentTuples
        ) | (ReasonKind::FinalHealthFailed, CliImageHash)
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    OperationFailed {
        image: ImageRole,
        stage: FailureStage,
        intent_revision: u64,
    },
    HealthFailed {
        process: Process,
        predicate: HealthPredicate,
    },
    Deadline {
        observed: u64,
        original_deadline: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    binding: Binding,
    predecessor: String,
    images: Images,
    kind: ReasonKind,
    ownership: u64,
    monitor: u64,
    event: Event,
}

enum SourceResult {
    Read { declared: usize, bytes: Vec<u8> },
    Missing,
    Error,
}

// Fixed-slot source: no receipt-selected path/URL/command/selector argument.
// Only this test module constructs it. Its observations are not OS evidence.
struct SyntheticSource {
    result: SourceResult,
    reads: usize,
}
impl SyntheticSource {
    fn read(&mut self) -> Result<&[u8], Refusal> {
        self.reads += 1;
        match &self.result {
            SourceResult::Read { declared, bytes }
                if *declared > 0 && *declared <= EVIDENCE_LIMIT && bytes.len() == *declared =>
            {
                Ok(bytes)
            }
            SourceResult::Read { .. } | SourceResult::Missing | SourceResult::Error => {
                Err("reason_source_unavailable")
            }
        }
    }
}

// No Clone, Serialize, Deserialize or Default. This value borrows the exact
// live attempt/context and is moved into the inert entry check once.
struct VerifiedReason<'a> {
    context: &'a EntryContext<'a>,
    reference: ReasonReference,
}

fn verify<'a>(
    context: &'a EntryContext<'a>,
    reference: &ReasonReference,
    source: &mut SyntheticSource,
) -> Result<VerifiedReason<'a>, Refusal> {
    if !context.live.alive.get() || context.live.entered.get() {
        return Err("reason_attempt_inactive");
    }
    if !context.binding.valid()
        || context.live.ownership == 0
        || context.live.monitor == 0
        || reference.entry_revision != context.binding.revision
        || reference.entry_revision <= 1
        || !hex(&context.predecessor, 64)
        || reference.predecessor_receipt_sha256 != context.predecessor
        || !hex(&reference.evidence_sha256, 64)
        || !reason_kind_matches_entry(reference.kind, &context.prior, context.slots)
    {
        return Err("reason_context_mismatch");
    }
    let bytes = source.read()?;
    if digest(bytes) != reference.evidence_sha256 {
        return Err("reason_evidence_digest");
    }
    let observation: Observation =
        serde_json::from_slice(bytes).map_err(|_| "reason_evidence_schema")?;
    if observation.binding != context.binding
        || observation.predecessor != context.predecessor
        || observation.images != context.images
        || observation.kind != reference.kind
        || observation.ownership != context.live.ownership
        || observation.monitor != context.live.monitor
    {
        return Err("reason_observation_mismatch");
    }
    use layout::Step::*;
    let operation = |step, stage, intent: u64| {
        intent == context.operation_intent_revision
            && (match step {
                MoveOldIntent | PublishIntent => intent.checked_add(1),
                OldMoved | Published => intent.checked_add(2),
                _ => None,
            }) == Some(context.binding.revision)
            && matches!(
                (step, stage),
                (MoveOldIntent, FailureStage::MoveOld)
                    | (PublishIntent, FailureStage::Publish)
                    | (OldMoved | Published, FailureStage::VerifyImage)
            )
    };
    let valid = match (&observation.kind, &observation.event) {
        (
            ReasonKind::ControllerActivationFailed,
            Event::OperationFailed {
                image: ImageRole::Controller,
                stage,
                intent_revision,
            },
        ) => operation(context.prior.controller_step, *stage, *intent_revision),
        (
            ReasonKind::CliActivationFailed,
            Event::OperationFailed {
                image: ImageRole::Cli,
                stage,
                intent_revision,
            },
        ) => operation(context.prior.cli_step, *stage, *intent_revision),
        (
            ReasonKind::NewControllerUnhealthy | ReasonKind::FinalHealthFailed,
            Event::HealthFailed { process, predicate },
        ) => {
            process == &context.process
                && process.handle_identity != 0
                && process.start_instance != 0
                && process.image_sha256 == context.images.controller.new.sha256
                && required_predicate(observation.kind, *predicate)
        }
        (
            ReasonKind::DeadlineExceeded,
            Event::Deadline {
                observed,
                original_deadline,
            },
        ) => {
            context.deadline > 0
                && *original_deadline == context.deadline
                && *observed >= context.deadline
        }
        // No protected human channel, including in this synthetic source.
        _ => false,
    };
    if !valid {
        return Err("reason_semantics_mismatch");
    }
    Ok(VerifiedReason {
        context,
        reference: reference.clone(),
    })
}

fn live_attempt() -> LiveAttempt {
    LiveAttempt {
        ownership: 11,
        monitor: 22,
        alive: Cell::new(true),
        entered: Cell::new(false),
    }
}

fn entry_fixture(live: &LiveAttempt, kind: ReasonKind) -> (EntryContext<'_>, DraftFields, Event) {
    use layout::{Object::*, Step::*, Transaction::*};
    let mut old = fixture();
    old.binding.revision = 10;
    let original = [Old, New, Absent];
    let published = [New, Absent, Old];
    let mut slots = [original; 2];
    let process = Process {
        handle_identity: 33,
        start_instance: 44,
        image_sha256: old.images.controller.new.sha256.clone(),
    };
    let event = match kind {
        ReasonKind::ControllerActivationFailed => {
            old.state.transaction = Activating;
            old.state.controller_step = MoveOldIntent;
            Event::OperationFailed {
                image: ImageRole::Controller,
                stage: FailureStage::MoveOld,
                intent_revision: 10,
            }
        }
        ReasonKind::NewControllerUnhealthy | ReasonKind::FinalHealthFailed => {
            old.state.transaction = Activating;
            old.state.controller_step = Published;
            slots[0] = published;
            if kind == ReasonKind::FinalHealthFailed {
                old.state.transaction = CoreHealthy;
                old.state.cli_step = Published;
                old.state.new_health_recorded = true;
                slots[1] = published;
            }
            Event::HealthFailed {
                process: process.clone(),
                predicate: HealthPredicate::IntendedBuild,
            }
        }
        ReasonKind::CliActivationFailed => {
            old.state.transaction = CoreHealthy;
            old.state.controller_step = Published;
            old.state.cli_step = MoveOldIntent;
            old.state.new_health_recorded = true;
            slots[0] = published;
            Event::OperationFailed {
                image: ImageRole::Cli,
                stage: FailureStage::MoveOld,
                intent_revision: 10,
            }
        }
        ReasonKind::DeadlineExceeded | ReasonKind::HumanAbort => Event::Deadline {
            observed: 100,
            original_deadline: 100,
        },
    };
    let pair = envelopes(&serde_json::to_vec(&old).unwrap(), &old.binding);
    let mut binding = old.binding.clone();
    binding.revision += 1;
    (
        EntryContext {
            binding,
            predecessor: digest(&pair.0),
            images: old.images.clone(),
            prior: old.state.clone(),
            slots,
            live,
            process,
            deadline: 100,
            operation_intent_revision: 10,
        },
        old,
        event,
    )
}

fn observation(context: &EntryContext<'_>, kind: ReasonKind, event: Event) -> Observation {
    Observation {
        binding: context.binding.clone(),
        predecessor: context.predecessor.clone(),
        images: context.images.clone(),
        kind,
        ownership: context.live.ownership,
        monitor: context.live.monitor,
        event,
    }
}

fn evidence(
    context: &EntryContext<'_>,
    kind: ReasonKind,
    bytes: Vec<u8>,
) -> (ReasonReference, SyntheticSource) {
    (
        ReasonReference {
            kind,
            entry_revision: context.binding.revision,
            predecessor_receipt_sha256: context.predecessor.clone(),
            evidence_sha256: digest(&bytes),
        },
        SyntheticSource {
            result: SourceResult::Read {
                declared: bytes.len(),
                bytes,
            },
            reads: 0,
        },
    )
}

#[test]
fn independent_reason_is_required_and_consumed_once_without_head_mutation() {
    use layout::{Step::*, Transaction::*};
    for kind in [
        ReasonKind::ControllerActivationFailed,
        ReasonKind::NewControllerUnhealthy,
        ReasonKind::CliActivationFailed,
        ReasonKind::FinalHealthFailed,
        ReasonKind::DeadlineExceeded,
    ] {
        let live = live_attempt();
        let (context, old, event) = entry_fixture(&live, kind);
        let bytes = serde_json::to_vec(&observation(&context, kind, event)).unwrap();
        let (reference, mut source) = evidence(&context, kind, bytes);
        let previous = envelopes(&serde_json::to_vec(&old).unwrap(), &old.binding);
        let head = ExpectedHead {
            binding: old.binding.clone(),
            receipt_sha256: digest(&previous.0),
        };
        let mut next = old.clone();
        next.binding = context.binding.clone();
        next.rollback_reason = ReasonClaim::Reference(reference.clone());
        if old.state.transaction == CoreHealthy {
            next.state.transaction = RollbackCli;
            next.state.cli_step = if old.state.cli_step == Published {
                MoveNewAsideIntent
            } else {
                Original
            };
        } else {
            next.state.transaction = RollbackController;
            next.state.controller_step = if old.state.controller_step == Published {
                MoveNewAsideIntent
            } else {
                Original
            };
        }
        let candidate = envelopes(&serde_json::to_vec(&next).unwrap(), &next.binding);
        assert!(check_rollback_entry(
            (&previous.0, &previous.1),
            (&candidate.0, &candidate.1),
            &head,
            context.slots
        )
        .is_ok());
        // Even a well-formed receipt and matching evidence digest cannot
        // replace the independent fixed-slot observation.
        for result in [SourceResult::Missing, SourceResult::Error] {
            let mut absent = SyntheticSource { result, reads: 0 };
            assert!(verify(&context, &reference, &mut absent).is_err());
            assert_eq!(absent.reads, 1);
        }
        let first = verify(&context, &reference, &mut source).unwrap();
        let second = verify(&context, &reference, &mut source).unwrap();
        assert!(!live.entered.get());
        first
            .accept_entry(
                &context,
                (&previous.0, &previous.1),
                (&candidate.0, &candidate.1),
                &head,
            )
            .unwrap();
        // Model a failed publication immediately after consumption. The head
        // is deliberately not advanced; a caller must preserve this latch,
        // diagnose and stop instead of retrying with another verified value.
        let publication = || -> Result<(), Refusal> {
            assert!(live.entered.get());
            Err("injected_publication_failure")
        };
        assert_eq!(publication(), Err("injected_publication_failure"));
        assert!(second
            .accept_entry(
                &context,
                (&previous.0, &previous.1),
                (&candidate.0, &candidate.1),
                &head
            )
            .is_err());
        assert!(verify(&context, &reference, &mut source).is_err());
        assert_eq!(source.reads, 2);
        assert_eq!(head.receipt_sha256, digest(&previous.0));
        assert_eq!(
            parse_pair(&previous.0, &previous.1, &head.binding)
                .unwrap()
                .state,
            old.state
        );
        assert_eq!(live.ownership, 11);
        assert_eq!(live.monitor, 22);
    }
}

#[test]
fn independent_reason_rejects_each_rebound_field_even_with_matching_digest() {
    let live = live_attempt();
    let kind = ReasonKind::NewControllerUnhealthy;
    let (context, _, event) = entry_fixture(&live, kind);
    let value = serde_json::to_value(observation(&context, kind, event)).unwrap();
    let mut changes = vec![
        (
            "/binding/workspace_id".to_owned(),
            serde_json::json!("ws_other"),
        ),
        (
            "/binding/server_id".to_owned(),
            serde_json::json!("srv_other"),
        ),
        (
            "/binding/job_id".to_owned(),
            serde_json::json!("22222222-2222-4222-8222-222222222222"),
        ),
        ("/binding/revision".to_owned(), serde_json::json!(12)),
        ("/predecessor".to_owned(), serde_json::json!("b".repeat(64))),
        (
            "/kind".to_owned(),
            serde_json::json!("controller_activation_failed"),
        ),
        ("/ownership".to_owned(), serde_json::json!(12)),
        ("/monitor".to_owned(), serde_json::json!(23)),
        (
            "/event/process/handle_identity".to_owned(),
            serde_json::json!(34),
        ),
        (
            "/event/process/start_instance".to_owned(),
            serde_json::json!(45),
        ),
        (
            "/event/process/image_sha256".to_owned(),
            serde_json::json!("b".repeat(64)),
        ),
    ];
    for image in ["controller", "cli"] {
        for version in ["old", "new"] {
            for (field, original) in value["images"][image][version].as_object().unwrap() {
                let replacement = match original {
                    serde_json::Value::Number(number) => {
                        serde_json::json!(number.as_u64().unwrap() + 1)
                    }
                    serde_json::Value::String(_) if field == "sha256" => {
                        serde_json::json!("b".repeat(64))
                    }
                    serde_json::Value::String(string) => {
                        serde_json::json!(format!("{string}_other"))
                    }
                    _ => unreachable!(),
                };
                changes.push((format!("/images/{image}/{version}/{field}"), replacement));
            }
        }
    }
    for (pointer, replacement) in changes {
        let mut changed = value.clone();
        *changed.pointer_mut(&pointer).unwrap() = replacement;
        let (reference, mut source) =
            evidence(&context, kind, serde_json::to_vec(&changed).unwrap());
        assert!(
            verify(&context, &reference, &mut source).is_err(),
            "{pointer}"
        );
        assert_eq!(source.reads, 1);
        assert!(!live.entered.get());
    }
}

#[test]
fn independent_reason_bounds_schema_and_refuses_truncated_or_failed_reads() {
    let live = live_attempt();
    let kind = ReasonKind::NewControllerUnhealthy;
    let (context, _, event) = entry_fixture(&live, kind);
    let value = serde_json::to_value(observation(&context, kind, event)).unwrap();
    let valid = serde_json::to_vec(&value).unwrap();
    let mut unknown = value.clone();
    unknown["event"]["diagnostic"] = "not an allowed free-text field".into();
    let mut predicate = value.clone();
    predicate["event"]["predicate"] = "unknown".into();
    for bytes in [
        vec![],
        vec![b' '; EVIDENCE_LIMIT + 1],
        valid[..valid.len() - 1].to_vec(),
        serde_json::to_vec(&unknown).unwrap(),
        serde_json::to_vec(&predicate).unwrap(),
    ] {
        let (reference, mut source) = evidence(&context, kind, bytes);
        assert!(verify(&context, &reference, &mut source).is_err());
        assert_eq!(source.reads, 1);
    }
    let (reference, mut source) = evidence(&context, kind, valid.clone());
    source.result = SourceResult::Read {
        declared: valid.len() + 1,
        bytes: valid,
    };
    assert_eq!(
        verify(&context, &reference, &mut source).err(),
        Some("reason_source_unavailable")
    );
    assert_eq!(source.reads, 1);
}

#[test]
fn independent_reason_semantics_and_live_instance_are_not_receipt_claims() {
    for kind in [
        ReasonKind::ControllerActivationFailed,
        ReasonKind::CliActivationFailed,
        ReasonKind::DeadlineExceeded,
        ReasonKind::HumanAbort,
    ] {
        let live = live_attempt();
        let (context, _, event) = entry_fixture(&live, kind);
        let mut observation = observation(&context, kind, event);
        let bad_event = match kind {
            ReasonKind::ControllerActivationFailed | ReasonKind::CliActivationFailed => {
                Event::OperationFailed {
                    image: ImageRole::Controller,
                    stage: FailureStage::Publish,
                    intent_revision: 9,
                }
            }
            _ => Event::Deadline {
                observed: 99,
                original_deadline: 100,
            },
        };
        observation.event = bad_event;
        let (reference, mut source) =
            evidence(&context, kind, serde_json::to_vec(&observation).unwrap());
        assert!(verify(&context, &reference, &mut source).is_err());
    }
    let live = live_attempt();
    let kind = ReasonKind::NewControllerUnhealthy;
    let (context, old, event) = entry_fixture(&live, kind);
    let (reference, mut source) = evidence(
        &context,
        kind,
        serde_json::to_vec(&observation(&context, kind, event)).unwrap(),
    );
    let token = verify(&context, &reference, &mut source).unwrap();
    let replacement = live_attempt();
    let (other, _, _) = entry_fixture(&replacement, kind);
    let pair = envelopes(&serde_json::to_vec(&old).unwrap(), &old.binding);
    let head = ExpectedHead {
        binding: old.binding.clone(),
        receipt_sha256: digest(&pair.0),
    };
    assert_eq!(
        token.accept_entry(&other, (&pair.0, &pair.1), (&pair.0, &pair.1), &head),
        Err("reason_attempt_inactive")
    );
    let token = verify(&context, &reference, &mut source).unwrap();
    live.alive.set(false);
    assert_eq!(
        token.accept_entry(&context, (&pair.0, &pair.1), (&pair.0, &pair.1), &head),
        Err("reason_attempt_inactive")
    );
    assert!(verify(&context, &reference, &mut source).is_err());
    assert_eq!(source.reads, 2);
    assert!(!live.entered.get());
}

#[test]
fn independent_reason_checks_each_semantic_component_without_budget_fallback() {
    use serde_json::json;
    for (kind, mutations) in [
        (
            ReasonKind::ControllerActivationFailed,
            vec![
                ("/event/image", json!("cli")),
                ("/event/stage", json!("publish")),
                ("/event/intent_revision", json!(9)),
                ("/event/intent_revision", json!(u64::MAX)),
            ],
        ),
        (
            ReasonKind::DeadlineExceeded,
            vec![
                ("/event/observed", json!(99)),
                ("/event/original_deadline", json!(101)),
            ],
        ),
    ] {
        let live = live_attempt();
        let (context, _, event) = entry_fixture(&live, kind);
        let value = serde_json::to_value(observation(&context, kind, event)).unwrap();
        for (pointer, replacement) in mutations {
            let mut changed = value.clone();
            *changed.pointer_mut(pointer).unwrap() = replacement;
            let (reference, mut source) =
                evidence(&context, kind, serde_json::to_vec(&changed).unwrap());
            assert_eq!(
                verify(&context, &reference, &mut source).err(),
                Some("reason_semantics_mismatch"),
                "{pointer}"
            );
            assert_eq!(source.reads, 1);
        }
    }
    let live = live_attempt();
    let kind = ReasonKind::NewControllerUnhealthy;
    let (context, _, _) = entry_fixture(&live, kind);
    for predicate in [
        HealthPredicate::IntendedBuild,
        HealthPredicate::ProxyConnected,
        HealthPredicate::HostUnchanged,
        HealthPredicate::LiveAgentTuples,
    ] {
        let event = Event::HealthFailed {
            process: context.process.clone(),
            predicate,
        };
        let bytes = serde_json::to_vec(&observation(&context, kind, event)).unwrap();
        let (reference, mut source) = evidence(&context, kind, bytes.clone());
        assert!(verify(&context, &reference, &mut source).is_ok());
        for field in [
            "entry_revision",
            "predecessor_receipt_sha256",
            "evidence_sha256",
        ] {
            let mut changed = serde_json::to_value(&reference).unwrap();
            changed[field] = if field == "entry_revision" {
                json!(12)
            } else {
                json!("b".repeat(64))
            };
            let changed: ReasonReference = serde_json::from_value(changed).unwrap();
            assert!(verify(&context, &changed, &mut source).is_err());
        }
    }
    let live = live_attempt();
    let (context, _, event) = entry_fixture(&live, ReasonKind::HumanAbort);
    let (reference, mut source) = evidence(
        &context,
        ReasonKind::HumanAbort,
        serde_json::to_vec(&observation(&context, ReasonKind::HumanAbort, event)).unwrap(),
    );
    assert_eq!(
        verify(&context, &reference, &mut source).err(),
        Some("reason_semantics_mismatch")
    );
    assert_eq!(source.reads, 1);
}

#[test]
fn independent_reason_operation_results_keep_the_original_intent_revision() {
    use layout::{Object::*, Step::*};
    for (step, stage, slots, intent_revision) in [
        (MoveOldIntent, FailureStage::MoveOld, [Old, New, Absent], 10),
        (PublishIntent, FailureStage::Publish, [Absent, New, Old], 10),
        (OldMoved, FailureStage::VerifyImage, [Absent, New, Old], 9),
        (Published, FailureStage::VerifyImage, [New, Absent, Old], 9),
    ] {
        let live = live_attempt();
        let kind = ReasonKind::ControllerActivationFailed;
        let (mut context, _, _) = entry_fixture(&live, kind);
        context.prior.controller_step = step;
        context.slots[0] = slots;
        context.operation_intent_revision = intent_revision;
        for claimed_revision in [intent_revision, intent_revision + 1] {
            let event = Event::OperationFailed {
                image: ImageRole::Controller,
                stage,
                intent_revision: claimed_revision,
            };
            let (reference, mut source) = evidence(
                &context,
                kind,
                serde_json::to_vec(&observation(&context, kind, event)).unwrap(),
            );
            assert_eq!(
                verify(&context, &reference, &mut source).is_ok(),
                claimed_revision == intent_revision,
                "{step:?}"
            );
        }
    }
}

#[test]
fn independent_reason_accepts_only_predicates_required_for_its_health_phase() {
    for kind in [
        ReasonKind::NewControllerUnhealthy,
        ReasonKind::FinalHealthFailed,
    ] {
        let live = live_attempt();
        let (context, _, _) = entry_fixture(&live, kind);
        for (predicate, allowed) in [
            (HealthPredicate::IntendedBuild, true),
            (HealthPredicate::ProxyConnected, true),
            (HealthPredicate::HostUnchanged, true),
            (HealthPredicate::LiveAgentTuples, true),
            // The CLI has not been activated at initial Controller health.
            (
                HealthPredicate::CliImageHash,
                kind == ReasonKind::FinalHealthFailed,
            ),
        ] {
            let event = Event::HealthFailed {
                process: context.process.clone(),
                predicate,
            };
            let (reference, mut source) = evidence(
                &context,
                kind,
                serde_json::to_vec(&observation(&context, kind, event)).unwrap(),
            );
            assert_eq!(
                verify(&context, &reference, &mut source).is_ok(),
                allowed,
                "{kind:?}/{predicate:?}"
            );
            assert_eq!(source.reads, 1);
            assert!(!live.entered.get());
        }
    }
}

impl VerifiedReason<'_> {
    fn accept_entry(
        self,
        current: &EntryContext<'_>,
        previous: (&[u8], &[u8]),
        candidate: (&[u8], &[u8]),
        head: &ExpectedHead,
    ) -> Result<(), Refusal> {
        if !std::ptr::eq(self.context, current)
            || !current.live.alive.get()
            || current.live.entered.get()
        {
            return Err("reason_attempt_inactive");
        }
        let old = parse_pair(previous.0, previous.1, &head.binding)?;
        let next = parse_pair(candidate.0, candidate.1, &current.binding)?;
        if head.receipt_sha256 != current.predecessor
            || old.images != current.images
            || old.state != current.prior
            || next.rollback_reason != ReasonClaim::Reference(self.reference)
        {
            return Err("reason_entry_mismatch");
        }
        check_rollback_entry(previous, candidate, head, current.slots)?;
        current.live.entered.set(true);
        Ok(()) // In-memory consumption only; NO entry publication or ownership.
    }
}
