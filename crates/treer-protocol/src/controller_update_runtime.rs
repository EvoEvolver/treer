//! Typed Controller adapter contract. Targets refer to the provisioned catalog;
//! no remote caller can provide an executable, URL, path or trust override.
use crate::controller_update::{Identity, Target};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchBinding {
    pub operation_id: String,
    pub workspace_id: String,
    pub server_id: String,
    pub current: Identity,
    pub target: Target,
    pub catalog_revision: u64,
    pub plan_digest: String,
    pub created_at: DateTime<Utc>,
}

impl DispatchBinding {
    /// Canonical plan binding shared by Proxy confirmation and native admission.
    pub fn expected_plan_digest(&self) -> Result<String, serde_json::Error> {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(&(
            &self.current,
            &self.target,
            self.catalog_revision,
            crate::controller_update::PLAN_STEPS,
        ))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn validate(&self) -> bool {
        use crate::controller_update::{Preview, PLAN_STEPS};
        self.catalog_revision > 0
            && self.catalog_revision < (1 << 53)
            && !self.workspace_id.is_empty()
            && self.workspace_id.len() <= 128
            && !self.server_id.is_empty()
            && self.server_id.len() <= 128
            && Preview {
                operation_id: self.operation_id.clone(),
                current: self.current.clone(),
                target: self.target.clone(),
                plan_digest: self.plan_digest.clone(),
                steps: PLAN_STEPS.to_vec(),
                confirmation_token: "0".repeat(32),
                expires_at: self.created_at,
            }
            .validate()
            .is_ok()
    }
}

/// Read-only installed identity, obtained from selected installation images.
/// `native_ready` means the platform's reviewed entry and recovery are present;
/// it is never inferred from the existence of files or a live connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub platform: crate::controller_update::Platform,
    pub current: Identity,
    pub observed_at: DateTime<Utc>,
    pub native_ready: bool,
}

#[cfg(test)]
mod tests {
    #[test]
    fn typed_update_commands_are_separately_gated_and_not_advertised() {
        use crate::{AgentCommand, CONTROLLER_CAPABILITIES};
        for command in [
            AgentCommand::ControllerUpdateInspect {},
            AgentCommand::ControllerUpdateOperation {
                operation_id: "12345678-1234-4234-8234-123456789abc".into(),
            },
        ] {
            assert_eq!(
                command.required_capability(),
                Some(crate::controller_update::CAPABILITY)
            );
        }
        assert!(!CONTROLLER_CAPABILITIES.contains(&crate::controller_update::CAPABILITY));
    }
}
