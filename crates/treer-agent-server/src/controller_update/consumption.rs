//! Existing confirmed-operation projection, not an authorization or receipt.
use treer_protocol::{controller_update as wire, controller_update_runtime::DispatchBinding};

pub(super) fn validate(workspace: &str, server: &str, binding: &DispatchBinding) -> bool {
    let now = chrono::Utc::now();
    binding.workspace_id == workspace
        && binding.server_id == server
        && binding.validate()
        && binding
            .expected_plan_digest()
            .is_ok_and(|d| d == binding.plan_digest)
        && binding.created_at <= now + chrono::Duration::seconds(30)
        && binding.created_at >= now - chrono::Duration::minutes(2)
}

pub(super) fn projection(binding: &DispatchBinding, unresolved: bool) -> wire::Operation {
    wire::Operation {
        operation_id: binding.operation_id.clone(),
        current: binding.current.clone(),
        target: binding.target.clone(),
        state: if unresolved {
            wire::State::Unconfirmed
        } else {
            wire::State::Accepted
        },
        phase: None,
        observation: wire::Observation::Available,
        observed_identity: None,
        restored_identity: None,
        health: wire::Health::Unknown,
        receipt_revision: None,
        created_at: binding.created_at,
        updated_at: chrono::Utc::now().max(binding.created_at),
        observed_at: None,
        finished_at: None,
        error_code: unresolved.then_some(wire::UpdateError::ResultUnconfirmed),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_update_consumer_projection_never_claims_receipt_or_update_success() {
        use super::*;
        use treer_protocol::controller_update::{Identity, Platform, Target};
        let mut binding = DispatchBinding {
            operation_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: "ws_test".into(),
            server_id: "srv_test".into(),
            current: Identity {
                release_sha: "a".repeat(40),
                artifact_source_sha: "a".repeat(40),
                controller_sha256: "a".repeat(64),
                cli_sha256: "a".repeat(64),
            },
            target: Target {
                target_id: "next".into(),
                version_label: "v0.1.16".into(),
                platform: Platform::WindowsX86_64,
                identity: Identity {
                    release_sha: "b".repeat(40),
                    artifact_source_sha: "b".repeat(40),
                    controller_sha256: "b".repeat(64),
                    cli_sha256: "b".repeat(64),
                },
            },
            catalog_revision: 1,
            plan_digest: String::new(),
            created_at: chrono::Utc::now(),
        };
        binding.plan_digest = binding.expected_plan_digest().unwrap();
        assert!(validate("ws_test", "srv_test", &binding));
        assert!(!validate("ws_other", "srv_test", &binding));
        for unresolved in [false, true] {
            let op = projection(&binding, unresolved);
            op.validate().unwrap();
            assert_eq!(op.health, wire::Health::Unknown);
            assert!(
                op.phase.is_none() && op.receipt_revision.is_none() && op.finished_at.is_none()
            );
        }
        binding.created_at -= chrono::Duration::minutes(3);
        assert!(!validate("ws_test", "srv_test", &binding));
        binding.created_at = chrono::Utc::now();
        binding.plan_digest = "0".repeat(64);
        assert!(!validate("ws_test", "srv_test", &binding));
    }
}
