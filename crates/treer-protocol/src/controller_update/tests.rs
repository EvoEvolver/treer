use super::*;
use serde_json::{json, Value};

fn identity(digit: char) -> Identity {
    Identity {
        release_sha: digit.to_string().repeat(40),
        artifact_source_sha: digit.to_string().repeat(40),
        controller_sha256: digit.to_string().repeat(64),
        cli_sha256: digit.to_string().repeat(64),
    }
}

fn target() -> Target {
    Target {
        target_id: "release-target-2".into(),
        identity: identity('b'),
        platform: Platform::LinuxX86_64,
        version_label: "2026.10.20".into(),
    }
}

fn operation() -> Operation {
    let now: DateTime<Utc> = "2026-10-20T18:00:00Z".parse().unwrap();
    Operation {
        operation_id: "12345678-1234-4234-8234-123456789abc".into(),
        current: identity('a'),
        target: target(),
        state: State::Succeeded,
        phase: Some(Phase::Complete),
        observation: Observation::Available,
        observed_identity: Some(identity('b')),
        restored_identity: None,
        health: Health::Healthy,
        receipt_revision: Some(6),
        created_at: now,
        updated_at: now,
        observed_at: Some(now),
        finished_at: Some(now),
        error_code: None,
    }
}

#[test]
fn requests_reject_execution_inputs_and_tokens_are_redacted() {
    for extra in [
        "argv",
        "url",
        "path",
        "command",
        "sha256",
        "allow_downgrade",
    ] {
        let mut prepare = json!({"target_id":"target-1"});
        prepare[extra] = json!("injected");
        assert!(serde_json::from_value::<PrepareRequest>(prepare).is_err());
        let mut confirm = json!({"confirmation_token":"secret-1"});
        confirm[extra] = json!("injected");
        assert!(serde_json::from_value::<ConfirmRequest>(confirm).is_err());
    }
    for invalid in ["", "../target", "https://example.test/release", "id\n"] {
        assert!(PrepareRequest {
            target_id: invalid.into()
        }
        .validate()
        .is_err());
    }
    let confirm = ConfirmRequest {
        confirmation_token: "secret-1".into(),
    };
    assert!(confirm.validate().is_ok());
    assert!(!format!("{confirm:?}").contains("secret-1"));
    assert!(ConfirmRequest {
        confirmation_token: "x".repeat(257)
    }
    .validate()
    .is_err());
}

#[test]
fn success_requires_complete_identity_health_and_evidence_metadata() {
    let good = operation();
    assert!(good.validate().is_ok());
    for phase in [
        None,
        Some(Phase::CoreHealthy),
        Some(Phase::LauncherInstalling),
    ] {
        let mut op = good.clone();
        op.phase = phase;
        assert!(op.validate().is_err());
    }
    for observed in [None, Some(identity('a'))] {
        let mut op = good.clone();
        op.observed_identity = observed;
        assert!(op.validate().is_err());
    }
    for health in [Health::Unknown, Health::Unhealthy] {
        let mut op = good.clone();
        op.health = health;
        assert!(op.validate().is_err());
    }
    let mut wire = serde_json::to_value(&good).unwrap();
    for field in ["receipt_revision", "observed_at", "finished_at"] {
        let saved = wire[field].take();
        let parsed: Operation = serde_json::from_value(wire.clone()).unwrap();
        assert!(parsed.validate().is_err(), "missing {field}");
        wire[field] = saved;
    }
    wire["error_code"] = json!("process_output_or_unknown_code");
    assert!(serde_json::from_value::<Operation>(wire).is_err());
}

#[test]
fn connectivity_never_changes_durable_success_and_unconfirmed_is_not_finished() {
    let mut op = operation();
    op.observation = Observation::Unavailable;
    assert!(op.validate().is_ok()); // recorded success survives an outage
    let discovery = Discovery {
        operations: vec![op.clone()],
        blocked_by_other_operation: true,
    };
    assert!(discovery.validate().is_ok());
    op.state = State::Unconfirmed;
    op.phase = None;
    op.receipt_revision = None;
    op.health = Health::Unknown;
    op.observed_identity = None;
    op.error_code = Some(UpdateError::ResultUnconfirmed);
    assert!(op.validate().is_err()); // cannot call an unresolved run finished
    op.finished_at = None;
    assert!(op.validate().is_ok());
    op.state = State::Accepted;
    assert!(op.validate().is_ok());
    op.state = State::Updating;
    assert!(op.validate().is_err()); // no receipt yet
    op.phase = Some(Phase::CoreHealthy);
    op.receipt_revision = Some(3);
    assert!(op.validate().is_ok()); // still not whole-update success
}

#[test]
fn rollback_needs_restoration_identity_and_health_and_outcomes_stay_distinct() {
    let mut op = operation();
    op.state = State::RolledBack;
    op.phase = Some(Phase::RolledBack);
    op.observed_identity = Some(op.current.clone());
    assert!(op.validate().is_err());
    op.restored_identity = Some(op.current.clone());
    assert!(op.validate().is_ok());
    op.health = Health::Unknown;
    assert!(op.validate().is_err());
    for (state, phase) in [
        (State::Partial, Phase::Partial),
        (State::RollbackFailed, Phase::RollbackFailed),
        (State::Failed, Phase::Failed),
    ] {
        op.state = state;
        op.phase = Some(phase);
        op.error_code = Some(UpdateError::RecoveryRequired);
        assert!(op.validate().is_ok());
        op.phase = Some(Phase::Complete);
        assert!(op.validate().is_err());
    }
}

#[test]
fn catalog_rejects_wrong_platform_duplicates_invalid_hashes_and_unsafe_revisions() {
    let mut catalog = Catalog {
        support: Support::Supported,
        availability: Availability::Ready,
        current: Some(identity('a')),
        observed_at: Some(operation().created_at),
        platform: Platform::LinuxX86_64,
        catalog_revision: 1,
        targets: vec![target()],
    };
    assert!(catalog.validate().is_ok());
    catalog.targets.push(target());
    assert!(catalog.validate().is_err());
    catalog.targets.pop();
    catalog.targets[0].platform = Platform::WindowsX86_64;
    assert!(catalog.validate().is_err());
    catalog.targets[0].platform = catalog.platform;
    catalog.targets[0].identity.controller_sha256 = "A".repeat(64);
    assert!(catalog.validate().is_err());
    catalog.targets = vec![target()];
    for revision in [0, MAX_SAFE_INTEGER + 1] {
        catalog.catalog_revision = revision;
        assert!(catalog.validate().is_err());
    }
    catalog.catalog_revision = 1;
    catalog.support = Support::ManualUpdateRequired;
    assert!(catalog.validate().is_err()); // unsupported cannot claim ready
    catalog.availability = Availability::Offline;
    assert!(catalog.validate().is_ok());
}

#[test]
fn preview_requires_v4_selector_fixed_plan_and_changed_target() {
    let op = operation();
    let mut preview = Preview {
        operation_id: op.operation_id,
        current: op.current,
        target: op.target,
        plan_digest: "a".repeat(64),
        steps: PLAN_STEPS.to_vec(),
        confirmation_token: "synthetic-token".into(),
        expires_at: op.created_at,
    };
    assert!(preview.validate().is_ok());
    for id in [
        "../job",
        "00000000-0000-0000-0000-000000000000",
        "12345678-1234-1234-8234-123456789abc",
        "12345678-1234-4234-7234-123456789abc",
    ] {
        preview.operation_id = id.into();
        assert!(preview.validate().is_err());
    }
    preview.operation_id = "12345678-1234-4234-8234-123456789abc".into();
    preview.steps.swap(0, 1);
    assert!(preview.validate().is_err());
    preview.steps = PLAN_STEPS.to_vec();
    preview.target.identity = preview.current.clone();
    assert!(preview.validate().is_err());
}

#[test]
fn discovery_is_bounded_unique_and_wire_enums_fail_closed() {
    let op = operation();
    let mut discovery = Discovery {
        operations: vec![op.clone(), op],
        blocked_by_other_operation: false,
    };
    assert!(discovery.validate().is_err());
    discovery.operations = vec![operation(); MAX_DISCOVERY + 1];
    assert!(discovery.validate().is_err());
    let mut wire: Value = serde_json::to_value(operation()).unwrap();
    wire["phase"] = json!("ready_for_any_shell_command");
    assert!(serde_json::from_value::<Operation>(wire).is_err());
    assert!(!crate::CONTROLLER_CAPABILITIES.contains(&CAPABILITY));
}
