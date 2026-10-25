//! Proposed #222 browser contract. Defining these models does not advertise
//! support or authorize execution. No command, artifact URL, path or trust
//! override is accepted. Call `validate` at response boundaries after decoding;
//! serde alone checks types and rejects extra fields, not semantic evidence.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// Intentionally absent from CONTROLLER_CAPABILITIES until #111 integration
/// and native acceptance exist. Proxy support must be advertised separately.
pub const CAPABILITY: &str = "machine.controller-update.v1";
pub const MAX_TARGETS: usize = 16;
pub const MAX_DISCOVERY: usize = 16;
pub const CONFIRMATION_TTL_SECONDS: u64 = 120;
/// Mapping only: never extends confirmation authority beyond the original TTL.
pub const CONSUMED_TOKEN_LOOKUP_SECONDS: u64 = CONFIRMATION_TTL_SECONDS + 300;
pub const MAX_LIVE_TOKENS: usize = 4;
pub const PREPARE_ATTEMPTS_PER_MINUTE: usize = 10;
pub const FAILURE_COOLDOWN_SECONDS: u64 = 60;
pub const REPORT_GRACE_SECONDS: u64 = 600;
pub const RESOLVED_RETENTION_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const MAX_RESOLVED_OPERATIONS: usize = 64;
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

// Unlike serde's default Option handling, absence is an error; explicit null
// means unknown. Keep the same strict wire requirement in the browser decoder.
// Reject a seventeenth collection element while decoding, rather than first
// allocating a caller-controlled vector and only then running validation.
fn bounded_items<'de, D: Deserializer<'de>, T: Deserialize<'de>, const N: usize>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    struct Items<T, const N: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const N: usize> serde::de::Visitor<'de> for Items<T, N> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "at most {N} items")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|n| n > N) {
                return Err(serde::de::Error::custom("collection_too_large"));
            }
            let mut result = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(N));
            while let Some(item) = seq.next_element::<T>()? {
                if result.len() == N {
                    return Err(serde::de::Error::custom("collection_too_large"));
                }
                result.push(item);
            }
            Ok(result)
        }
    }
    d.deserialize_seq(Items::<T, N>(std::marker::PhantomData))
}
fn bounded_six<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Vec<T>, D::Error> {
    bounded_items::<D, T, 6>(d)
}
fn bounded_sixteen<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    bounded_items::<D, T, 16>(d)
}

fn required_option<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

fn token(s: &str) -> bool {
    s.len() >= 32 && opaque(s, 256)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidControllerUpdateModel;
type Checked = Result<(), InvalidControllerUpdateModel>;

fn require(ok: bool) -> Checked {
    ok.then_some(()).ok_or(InvalidControllerUpdateModel)
}

fn hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn opaque(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn job_id(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && s.as_bytes()[14] == b'4'
        && matches!(s.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    #[serde(rename = "darwin-aarch64")]
    DarwinAarch64,
    #[serde(rename = "darwin-x86_64")]
    DarwinX86_64,
    #[serde(rename = "linux-aarch64")]
    LinuxAarch64,
    #[serde(rename = "linux-x86_64")]
    LinuxX86_64,
    #[serde(rename = "windows-x86_64")]
    WindowsX86_64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub release_sha: String,
    pub artifact_source_sha: String,
    pub controller_sha256: String,
    pub cli_sha256: String,
}

impl Identity {
    pub fn validate(&self) -> Checked {
        require(
            hex(&self.release_sha, 40)
                && hex(&self.artifact_source_sha, 40)
                && hex(&self.controller_sha256, 64)
                && hex(&self.cli_sha256, 64),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub target_id: String,
    pub identity: Identity,
    pub platform: Platform,
    pub version_label: String,
}

impl Target {
    pub fn validate(&self) -> Checked {
        self.identity.validate()?;
        require(
            opaque(&self.target_id, 128)
                && !self.version_label.is_empty()
                && self.version_label.len() <= 128
                && self
                    .version_label
                    .bytes()
                    .all(|b| b.is_ascii_graphic() || b == b' '),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareRequest {
    pub target_id: String,
}

impl PrepareRequest {
    pub fn validate(&self) -> Checked {
        require(opaque(&self.target_id, 128))
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmRequest {
    pub confirmation_token: String,
}

impl std::fmt::Debug for ConfirmRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConfirmRequest { confirmation_token: [REDACTED] }")
    }
}

impl ConfirmRequest {
    pub fn validate(&self) -> Checked {
        require(token(&self.confirmation_token))
    }
}

macro_rules! wire_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}

wire_enum!(Support {
    Supported,
    ManualUpdateRequired
});
wire_enum!(Availability {
    Ready,
    Offline,
    Busy,
    CatalogUnavailable
});
wire_enum!(UpdateStep {
    VerifyArtifacts,
    Stage,
    ReplaceControllerAndCli,
    VerifyHealth,
    ReconcileLauncher,
    Finalize
});
pub const PLAN_STEPS: [UpdateStep; 6] = [
    UpdateStep::VerifyArtifacts,
    UpdateStep::Stage,
    UpdateStep::ReplaceControllerAndCli,
    UpdateStep::VerifyHealth,
    UpdateStep::ReconcileLauncher,
    UpdateStep::Finalize,
];
wire_enum!(State {
    Accepted,
    Updating,
    Succeeded,
    Failed,
    RolledBack,
    RollbackFailed,
    Partial,
    Unconfirmed
});
wire_enum!(Phase {
    Preparing,
    Prepared,
    Activating,
    CoreHealthy,
    LauncherInstalling,
    RollingBack,
    Complete,
    Failed,
    RolledBack,
    RollbackFailed,
    Partial
});
wire_enum!(Observation {
    Available,
    WaitingForReconnect,
    Unavailable
});
wire_enum!(Health {
    Unknown,
    Healthy,
    Unhealthy
});
wire_enum!(UpdateError {
    Unsupported,
    Offline,
    Busy,
    CatalogUnavailable,
    TargetUnavailable,
    TargetNotUpgrade,
    PreviewExpired,
    PreviewChanged,
    ConfirmationInvalid,
    RateLimited,
    CapacityExhausted,
    VerificationFailed,
    HealthFailed,
    ReceiptInvalid,
    RecoveryRequired,
    ResultUnconfirmed
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub support: Support,
    pub availability: Availability,
    #[serde(deserialize_with = "required_option")]
    pub current: Option<Identity>,
    #[serde(deserialize_with = "required_option")]
    pub observed_at: Option<DateTime<Utc>>,
    pub platform: Platform,
    pub catalog_revision: u64,
    #[serde(deserialize_with = "bounded_sixteen")]
    pub targets: Vec<Target>,
}

impl Catalog {
    pub fn validate(&self) -> Checked {
        require(
            self.catalog_revision > 0
                && self.catalog_revision <= MAX_SAFE_INTEGER
                && self.targets.len() <= MAX_TARGETS,
        )?;
        if let Some(identity) = &self.current {
            identity.validate()?;
        }
        require(self.current.is_some() == self.observed_at.is_some())?;
        require(
            self.availability != Availability::Ready
                || (self.support == Support::Supported && self.current.is_some()),
        )?;
        require(
            (self.support == Support::Supported && self.availability == Availability::Ready)
                || self.targets.is_empty(),
        )?;
        for (i, target) in self.targets.iter().enumerate() {
            target.validate()?;
            require(
                target.platform == self.platform
                    && !self.targets[..i]
                        .iter()
                        .any(|t| t.target_id == target.target_id),
            )?;
        }
        Ok(())
    }
}

// No Debug: preview contains a confirmation secret. Do not log or persist it
// in browser storage. Stable operation metadata is served separately below.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preview {
    pub operation_id: String,
    pub current: Identity,
    pub target: Target,
    pub plan_digest: String,
    #[serde(deserialize_with = "bounded_six")]
    pub steps: Vec<UpdateStep>,
    pub confirmation_token: String,
    pub expires_at: DateTime<Utc>,
}

impl Preview {
    /// Structural checks only. Catalog authority, time, upgrade ordering and
    /// scope/token bindings remain responsibilities of the accepting server.
    pub fn validate(&self) -> Checked {
        self.current.validate()?;
        self.target.validate()?;
        require(
            job_id(&self.operation_id)
                && hex(&self.plan_digest, 64)
                && self.steps == PLAN_STEPS
                && token(&self.confirmation_token)
                && self.current != self.target.identity,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub operation_id: String,
    pub current: Identity,
    pub target: Target,
    pub state: State,
    #[serde(deserialize_with = "required_option")]
    pub phase: Option<Phase>,
    pub observation: Observation,
    #[serde(deserialize_with = "required_option")]
    pub observed_identity: Option<Identity>,
    #[serde(deserialize_with = "required_option")]
    pub restored_identity: Option<Identity>,
    pub health: Health,
    #[serde(deserialize_with = "required_option")]
    pub receipt_revision: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(deserialize_with = "required_option")]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(deserialize_with = "required_option")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(deserialize_with = "required_option")]
    pub error_code: Option<UpdateError>,
}

impl Operation {
    /// Validates the projection, never proves receipt authenticity or health.
    /// The reporting adapter must validate native evidence before constructing
    /// this value; callers must not infer success from a connected machine.
    pub fn validate(&self) -> Checked {
        self.current.validate()?;
        self.target.validate()?;
        for identity in [&self.observed_identity, &self.restored_identity]
            .into_iter()
            .flatten()
        {
            identity.validate()?;
        }
        require(
            job_id(&self.operation_id)
                && self.created_at <= self.updated_at
                && self
                    .observed_at
                    .is_none_or(|t| t >= self.created_at && t <= self.updated_at)
                && self
                    .finished_at
                    .is_none_or(|t| t >= self.created_at && t <= self.updated_at)
                && self.phase.is_some() == self.receipt_revision.is_some()
                && self.receipt_revision.is_none_or(|r| r <= MAX_SAFE_INTEGER),
        )?;
        match self.state {
            State::Succeeded => require(
                self.phase == Some(Phase::Complete)
                    && self.observed_identity.as_ref() == Some(&self.target.identity)
                    && self.observed_at.is_some()
                    && self.health == Health::Healthy
                    && self.finished_at.is_some()
                    && self.restored_identity.is_none()
                    && self.error_code.is_none(),
            ),
            State::RolledBack => require(
                self.phase == Some(Phase::RolledBack)
                    && self.restored_identity.as_ref() == Some(&self.current)
                    && self.observed_identity.as_ref() == Some(&self.current)
                    && self.observed_at.is_some()
                    && self.health == Health::Healthy
                    && self.finished_at.is_some(),
            ),
            State::Failed => require(
                self.phase.is_none_or(|p| p == Phase::Failed)
                    && self.finished_at.is_some()
                    && self.error_code.is_some(),
            ),
            State::Partial => require(
                self.phase == Some(Phase::Partial)
                    && self.finished_at.is_some()
                    && self.health != Health::Healthy
                    && self.error_code.is_some(),
            ),
            State::RollbackFailed => require(
                self.phase == Some(Phase::RollbackFailed)
                    && self.finished_at.is_some()
                    && self.health != Health::Healthy
                    && self.error_code.is_some(),
            ),
            State::Accepted => require(
                self.phase.is_none()
                    && self.finished_at.is_none()
                    && self.error_code.is_none()
                    && self.health == Health::Unknown
                    && self.observed_identity.is_none()
                    && self.restored_identity.is_none()
                    && self.observed_at.is_none(),
            ),
            State::Updating => require(
                matches!(
                    self.phase,
                    Some(
                        Phase::Preparing
                            | Phase::Prepared
                            | Phase::Activating
                            | Phase::CoreHealthy
                            | Phase::LauncherInstalling
                            | Phase::RollingBack
                    )
                ) && self.finished_at.is_none()
                    && self.error_code.is_none(),
            ),
            State::Unconfirmed => require(
                self.finished_at.is_none()
                    && self.error_code.is_some()
                    && self.health != Health::Healthy,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery {
    #[serde(deserialize_with = "bounded_sixteen")]
    pub operations: Vec<Operation>,
    pub blocked_by_other_operation: bool,
}

impl Discovery {
    pub fn validate(&self) -> Checked {
        require(self.operations.len() <= MAX_DISCOVERY)?;
        require(
            self.operations
                .iter()
                .filter(|op| {
                    matches!(
                        op.state,
                        State::Accepted
                            | State::Updating
                            | State::Unconfirmed
                            | State::Partial
                            | State::RollbackFailed
                    )
                })
                .count()
                <= 1,
        )?;
        for (i, operation) in self.operations.iter().enumerate() {
            operation.validate()?;
            require(
                !self.operations[..i]
                    .iter()
                    .any(|other| other.operation_id == operation.operation_id),
            )?;
            if let Some(previous) = i.checked_sub(1).map(|j| &self.operations[j]) {
                require(
                    (previous.created_at, &previous.operation_id)
                        >= (operation.created_at, &operation.operation_id),
                )?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
