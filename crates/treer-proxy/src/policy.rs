use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use treer_protocol::{
    AgentCommand, PolicyEffect, PolicyMode, PolicyPrincipalKind, PolicyPrincipalRef,
    PolicyProviderBundle, PolicyProviderFailureMode, PolicyResourceSelector, PolicySubjectSelector,
    ProtocolError, WorkspacePolicy, WorkspacePolicyRule, POLICY_PROVIDER_PROTOCOL_V1,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

use crate::auth::AuthStore;
use crate::policy_provider_store::PolicyProviderStore;
use crate::state::AppState;
#[path = "policy_path_match.rs"]
mod path_match;
#[cfg(test)]
#[path = "policy_path_tests.rs"]
mod path_tests;

pub const ACTION_NETWORK_CONNECT: &str = "network.connect";
pub const ACTION_AGENT_DISCOVER: &str = "agent.discover";
pub const ACTION_WORKSPACE_AUDIT_READ: &str = "workspace.audit.read";
pub const ACTION_AGENT_METADATA_READ: &str = "agent.metadata.read";
pub const ACTION_AGENT_OUTPUT_READ: &str = "agent.output.read";
pub const ACTION_AGENT_PROMPT: &str = "agent.prompt";
pub const ACTION_AGENT_QUEUE_READ: &str = "agent.queue.read";
pub const ACTION_AGENT_QUEUE_CANCEL: &str = "agent.queue.cancel";
pub const ACTION_AGENT_INPUT: &str = "agent.input";
pub const ACTION_AGENT_STOP: &str = "agent.stop";
pub const ACTION_AGENT_ABORT: &str = "agent.abort";
pub const ACTION_AGENT_UPDATE: &str = "agent.update";
pub const ACTION_AGENT_DELETE: &str = "agent.delete";
pub const ACTION_AGENT_CREATE: &str = "agent.create";
pub const ACTION_AGENT_STARTUP_READ: &str = "agent.startup.read";
pub const ACTION_AGENT_STARTUP_MANAGE: &str = "agent.startup.manage";
pub const ACTION_AGENT_TREE_READ: &str = "agent.tree.read";
pub const ACTION_AGENT_TREE_MANAGE: &str = "agent.tree.manage";
pub const ACTION_LAUNCH_PROFILE_LIST: &str = "launch_profile.list";
pub const ACTION_LAUNCH_PROFILE_READ: &str = "launch_profile.read";
pub const ACTION_LAUNCH_PROFILE_CREATE: &str = "launch_profile.create";
pub const ACTION_LAUNCH_PROFILE_UPDATE: &str = "launch_profile.update";
pub const ACTION_LAUNCH_PROFILE_DELETE: &str = "launch_profile.delete";
pub const ACTION_LAUNCH_PROFILE_USE: &str = "launch_profile.use";
pub const ACTION_MACHINE_UPDATE: &str = "machine.update";
pub const ACTION_MACHINE_DELETE: &str = "machine.delete";
pub const ACTION_MACHINE_EXEC: &str = "machine.exec";
pub const ACTION_MACHINE_FILE_WRITE: &str = "machine.file.write";
pub const ACTION_MACHINE_FILE_READ: &str = "machine.file.read";
pub const ACTION_IDENTITY_TOKEN_ISSUE: &str = "identity.token.issue";
pub const ACTION_HUMAN_LIST: &str = "human.list";
pub const ACTION_SERVICE_LIST: &str = "service.list";
pub const ACTION_SERVICE_CREATE: &str = "service.create";
pub const ACTION_SERVICE_UPDATE: &str = "service.update";
pub const ACTION_SERVICE_DELETE: &str = "service.delete";
pub const ACTION_SERVICE_PROBE: &str = "service.probe";
pub const ACTION_VIRTUAL_HOST_LIST: &str = "virtual_host.list";
pub const ACTION_VIRTUAL_HOST_CREATE: &str = "virtual_host.create";
pub const ACTION_VIRTUAL_HOST_DELETE: &str = "virtual_host.delete";
pub const ACTION_INGRESS_LIST: &str = "ingress.list";
pub const ACTION_INGRESS_CREATE: &str = "ingress.create";
pub const ACTION_INGRESS_UPDATE: &str = "ingress.update";
pub const ACTION_INGRESS_DELETE: &str = "ingress.delete";
pub const ACTION_MESSAGE_SEND: &str = "message.send";
pub const ACTION_MESSAGE_READ: &str = "message.read";
pub const ACTION_MESSAGE_RECEIVE: &str = "message.receive";
pub const ACTION_MESSAGE_ACK: &str = "message.ack";
pub const ACTION_MESSAGE_IMPORT: &str = "message.import";
pub const RESOURCE_NETWORK_ENDPOINT: &str = "network.endpoint";
pub const RESOURCE_AGENT: &str = "agent";
pub const RESOURCE_WORKSPACE: &str = "workspace";
pub const RESOURCE_AGENT_TREE: &str = "agent.tree";
pub const RESOURCE_AGENT_LAUNCH_PROFILE: &str = "agent.launch_profile";
pub const RESOURCE_MACHINE: &str = "machine";
pub const RESOURCE_HUMAN_DIRECTORY: &str = "human.directory";
pub const RESOURCE_MACHINE_SERVICE: &str = "machine.service";
pub const RESOURCE_VIRTUAL_HOST: &str = "virtual_host";
pub const RESOURCE_SERVICE_INGRESS: &str = "service.ingress";
pub const RESOURCE_MESSAGE: &str = "message";
pub const RESOURCE_MESSAGE_MAILBOX: &str = "message.mailbox";
pub const RESOURCE_MESSAGE_DELIVERY: &str = "message.delivery";
pub const RESOURCE_MESSAGE_IMPORT: &str = "message.import";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicySubject {
    Agent { server_id: String, agent_id: String },
    Machine { server_id: String },
    Human { user_id: String },
    Service { service_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyResource {
    pub kind: String,
    pub id: String,
    pub attributes: BTreeMap<String, String>,
}

impl PolicyResource {
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            attributes: BTreeMap::new(),
        }
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyRequest {
    pub workspace_id: String,
    pub subject: PolicySubject,
    pub action: String,
    pub resource: PolicyResource,
    pub subject_tree: Option<treer_protocol::PolicyTreeContext>,
    pub resource_tree: Option<treer_protocol::PolicyTreeContext>,
    pub tree_revision: Option<u64>,
}

impl PolicyRequest {
    pub fn new(
        workspace_id: impl Into<String>,
        subject: PolicySubject,
        action: impl Into<String>,
        resource: PolicyResource,
    ) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            subject,
            action: action.into(),
            resource,
            subject_tree: None,
            resource_tree: None,
            tree_revision: None,
        }
    }

    pub fn network_connect(
        workspace_id: &str,
        source_server_id: &str,
        source_agent_id: Option<&str>,
        destination_server_id: &str,
        destination_agent_id: Option<&str>,
        host: &str,
        port: u16,
    ) -> Self {
        let mut attributes = BTreeMap::new();
        attributes.insert(
            "destination_server_id".to_string(),
            destination_server_id.to_string(),
        );
        if let Some(agent_id) = destination_agent_id {
            attributes.insert("destination_agent_id".to_string(), agent_id.to_string());
        }
        attributes.insert("host".to_string(), host.to_string());
        attributes.insert("port".to_string(), port.to_string());
        let subject = source_agent_id.map_or_else(
            || PolicySubject::Machine {
                server_id: source_server_id.to_string(),
            },
            |agent_id| PolicySubject::Agent {
                server_id: source_server_id.to_string(),
                agent_id: agent_id.to_string(),
            },
        );
        Self::new(
            workspace_id,
            subject,
            ACTION_NETWORK_CONNECT,
            PolicyResource {
                kind: RESOURCE_NETWORK_ENDPOINT.to_string(),
                id: format!("{destination_server_id}:{host}:{port}"),
                attributes,
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDenial {
    pub code: String,
    pub message: String,
}

impl PolicyDenial {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    fn into_error(self) -> ProtocolError {
        ProtocolError::new(self.code, self.message)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny(PolicyDenial),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyEvaluation {
    Abstain,
    Decide(PolicyDecision),
}

pub type PolicyFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PolicyEvaluation, ProtocolError>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyBatchEvaluation {
    pub explanations: Vec<PolicyExplanation>,
    pub evaluations: Vec<PolicyEvaluation>,
    pub revision: Option<u64>,
}

pub type PolicyBatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PolicyBatchEvaluation, ProtocolError>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PolicyBatchAuthorization {
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyBatchDenial {
    pub request_index: usize,
    pub error: ProtocolError,
}

/// What an evaluator knows about the workspace Policy revision it would apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRevision {
    /// No revisioned document here (fixed engines, or no Policy published).
    Unrevisioned,
    Known(u64),
    /// The document could not be loaded right now; decisions keyed on a
    /// revision must not be remembered or reused in this state.
    Unavailable,
}

pub trait PolicyEvaluator: Send + Sync {
    fn explain<'a>(
        &'a self,
        _request: &'a PolicyRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PolicyExplanation>, ProtocolError>> + Send + 'a>>
    {
        Box::pin(async { Ok(None) })
    }
    fn evaluate<'a>(&'a self, request: &'a PolicyRequest) -> PolicyFuture<'a>;

    /// The revision of the workspace Policy this evaluator would apply now,
    /// when it has one; durable and Provider evaluators answer from the same
    /// bounded cache their decisions use, so a cache key built on it changes
    /// exactly when their decisions can.
    fn revision<'a>(
        &'a self,
        _workspace_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = PolicyRevision> + Send + 'a>> {
        Box::pin(async { PolicyRevision::Unrevisioned })
    }

    fn evaluate_batch<'a>(&'a self, requests: &'a [PolicyRequest]) -> PolicyBatchFuture<'a> {
        Box::pin(async move {
            let mut evaluations = Vec::with_capacity(requests.len());
            for request in requests {
                evaluations.push(self.evaluate(request).await?);
            }
            Ok(PolicyBatchEvaluation {
                explanations: Vec::new(),
                evaluations,
                revision: None,
            })
        })
    }
}

#[derive(Clone)]
pub struct PolicyEngine {
    evaluators: Arc<[Arc<dyn PolicyEvaluator>]>,
    default_decision: PolicyDecision,
    provider: Option<Arc<ProviderPolicyEvaluator>>,
    tree_pool: Option<sqlx::PgPool>,
    records: Arc<Mutex<VecDeque<PolicyDecisionRecord>>>,
}

impl PolicyEngine {
    async fn check_tree_revision(&self, request: &PolicyRequest) -> Result<(), ProtocolError> {
        if let (Some(pool), Some(revision)) = (&self.tree_pool, request.tree_revision) {
            let current: Option<i64> = sqlx::query_scalar(
                "SELECT tree_revision FROM workspaces WHERE workspace_id=$1 AND deleted_at IS NULL",
            )
            .bind(&request.workspace_id)
            .fetch_optional(pool)
            .await
            .map_err(|_| {
                ProtocolError::new(
                    "policy_tree_unavailable",
                    "cannot validate current Tree revision",
                )
            })?;
            if current.and_then(|v| u64::try_from(v).ok()) != Some(revision) {
                return Err(ProtocolError::new(
                    "policy_tree_changed",
                    "Tree changed during authorization; retry request",
                ));
            }
        }
        Ok(())
    }
    async fn enrich(&self, request: &PolicyRequest) -> Result<PolicyRequest, ProtocolError> {
        match &self.tree_pool {
            Some(pool) => Ok(
                crate::policy_tree::TreeSnapshot::load(pool, &request.workspace_id)
                    .await?
                    .enrich(request),
            ),
            None => Ok(request.clone()),
        }
    }

    pub async fn explain_request(
        &self,
        request: &PolicyRequest,
    ) -> Result<PolicyExplanation, ProtocolError> {
        let request = self.enrich(request).await?;
        for evaluator in self.evaluators.iter() {
            if let Some(mut explanation) = evaluator.explain(&request).await? {
                explanation.tree_revision = request.tree_revision;
                return Ok(explanation);
            }
            match evaluator.evaluate(&request).await? {
                PolicyEvaluation::Abstain => {}
                PolicyEvaluation::Decide(decision) => {
                    return Ok(PolicyExplanation::baseline(decision, request.tree_revision))
                }
            }
        }
        Ok(PolicyExplanation::baseline(
            self.default_decision.clone(),
            request.tree_revision,
        ))
    }

    async fn record(&self, request: &PolicyRequest, explanation: &PolicyExplanation) {
        if explanation.mode != PolicyMode::Monitor
            || explanation.computed_effect != PolicyEffect::Deny
        {
            return;
        }
        let (kind, id, _) = subject_parts(&request.subject);
        let record = PolicyDecisionRecord {
            workspace_id: request.workspace_id.clone(),
            actor_kind: kind,
            actor_id: id.into(),
            action: request.action.clone(),
            resource_kind: request.resource.kind.clone(),
            resource_id: request.resource.id.clone(),
            occurred_at: chrono::Utc::now(),
            explanation: explanation.clone(),
        };
        tracing::info!(workspace_id=%record.workspace_id,actor_id=%record.actor_id,action=%record.action,resource_id=%record.resource_id,rule_id=?explanation.matched_rule_id,policy_revision=explanation.policy_revision,tree_revision=?request.tree_revision,"policy monitor would deny");
        let mut records = self.records.lock().await;
        if records.len() == 2048 {
            records.pop_front();
        }
        records.push_back(record);
    }

    pub async fn decision_records(&self, workspace: &str) -> Vec<PolicyDecisionRecord> {
        self.records
            .lock()
            .await
            .iter()
            .rev()
            .filter(|r| r.workspace_id == workspace)
            .take(100)
            .cloned()
            .collect()
    }
    pub fn allow_all() -> Self {
        Self::new(PolicyDecision::Allow, Vec::new())
    }

    pub fn durable(store: WorkspacePolicyStore) -> Self {
        let pool = store.pool();
        let mut engine = Self::new(
            PolicyDecision::Allow,
            vec![Arc::new(DurablePolicyEvaluator::new(store))],
        );
        engine.tree_pool = Some(pool);
        engine
    }

    pub(crate) fn durable_with_provider(
        store: WorkspacePolicyStore,
        provider_store: PolicyProviderStore,
        auth: AuthStore,
        state: AppState,
    ) -> Self {
        let pool = store.pool();
        let provider = Arc::new(ProviderPolicyEvaluator::new(provider_store, auth, state));
        Self {
            evaluators: vec![
                provider.clone() as Arc<dyn PolicyEvaluator>,
                Arc::new(DurablePolicyEvaluator::new(store)),
            ]
            .into(),
            default_decision: PolicyDecision::Allow,
            provider: Some(provider),
            tree_pool: Some(pool),
            records: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn new(
        default_decision: PolicyDecision,
        evaluators: Vec<Arc<dyn PolicyEvaluator>>,
    ) -> Self {
        Self {
            evaluators: evaluators.into(),
            default_decision,
            provider: None,
            tree_pool: None,
            records: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// The workspace Policy revision of the first evaluator that has one
    /// (#143 Phase 2), in the same order `authorize` consults them, so it is
    /// the revision that decides. Callers that remember a decision key it with
    /// this value; `Unavailable` means the document cannot be loaded now and
    /// nothing may be remembered or reused.
    pub async fn policy_revision(&self, workspace_id: &str) -> PolicyRevision {
        for evaluator in self.evaluators.iter() {
            match evaluator.revision(workspace_id).await {
                PolicyRevision::Unrevisioned => continue,
                known_or_unavailable => return known_or_unavailable,
            }
        }
        PolicyRevision::Unrevisioned
    }

    pub async fn invalidate_provider(&self, workspace_id: &str) {
        if let Some(provider) = &self.provider {
            provider.invalidate(workspace_id).await;
        }
    }

    pub async fn provider_cache_status(&self, workspace_id: &str) -> PolicyProviderCacheStatus {
        match &self.provider {
            Some(provider) => provider.cache_status(workspace_id).await,
            None => PolicyProviderCacheStatus::default(),
        }
    }

    pub async fn authorize(&self, request: &PolicyRequest) -> Result<(), ProtocolError> {
        let request = self.enrich(request).await?;
        for evaluator in self.evaluators.iter() {
            if let Some(explanation) = evaluator.explain(&request).await? {
                self.check_tree_revision(&request).await?;
                self.record(&request, &explanation).await;
                return explanation.into_decision().into_result();
            }
            match evaluator.evaluate(&request).await? {
                PolicyEvaluation::Abstain => {}
                PolicyEvaluation::Decide(decision) => return decision.into_result(),
            }
        }
        self.default_decision.clone().into_result()
    }

    /// Sensitive reads have a role baseline, not the permissive Core fallback.
    /// Only a matched, enforced allow rule can expand that baseline. Monitor
    /// decisions and action defaults cannot grant access to a non-owner.
    pub async fn authorize_sensitive_read(
        &self,
        request: &PolicyRequest,
        role_allowed: bool,
    ) -> Result<(), ProtocolError> {
        let request = self.enrich(request).await?;
        for evaluator in self.evaluators.iter() {
            if let Some(explanation) = evaluator.explain(&request).await? {
                self.check_tree_revision(&request).await?;
                self.record(&request, &explanation).await;
                let explicitly_allowed = explanation.mode == PolicyMode::Enforce
                    && explanation.matched_rule_id.is_some()
                    && explanation.effective_decision == PolicyEffect::Allow;
                explanation.into_decision().into_result()?;
                return sensitive_read_baseline(role_allowed || explicitly_allowed);
            }
            match evaluator.evaluate(&request).await? {
                PolicyEvaluation::Abstain => {}
                PolicyEvaluation::Decide(decision) => {
                    self.check_tree_revision(&request).await?;
                    decision.into_result()?;
                    return sensitive_read_baseline(role_allowed);
                }
            }
        }
        self.default_decision.clone().into_result()?;
        self.check_tree_revision(&request).await?;
        sensitive_read_baseline(role_allowed)
    }

    pub async fn authorize_batch(
        &self,
        requests: &[PolicyRequest],
    ) -> Result<PolicyBatchAuthorization, PolicyBatchDenial> {
        if requests.is_empty() {
            return Ok(PolicyBatchAuthorization { revision: None });
        }
        let enriched = if let Some(pool) = &self.tree_pool {
            let snapshot = crate::policy_tree::TreeSnapshot::load(pool, &requests[0].workspace_id)
                .await
                .map_err(|error| PolicyBatchDenial {
                    request_index: 0,
                    error,
                })?;
            requests
                .iter()
                .map(|r| snapshot.enrich(r))
                .collect::<Vec<_>>()
        } else {
            requests.to_vec()
        };
        let requests = enriched.as_slice();
        let workspace_id = &requests[0].workspace_id;
        if requests
            .iter()
            .any(|request| request.workspace_id != *workspace_id)
        {
            return Err(PolicyBatchDenial {
                request_index: 0,
                error: ProtocolError::new(
                    "policy_batch_workspace_mismatch",
                    "a policy batch must belong to one workspace",
                ),
            });
        }

        let mut unresolved = vec![true; requests.len()];
        let mut revision = None;
        for evaluator in self.evaluators.iter() {
            if unresolved.iter().all(|value| !value) {
                break;
            }
            let batch =
                evaluator
                    .evaluate_batch(requests)
                    .await
                    .map_err(|error| PolicyBatchDenial {
                        request_index: 0,
                        error,
                    })?;
            if batch.evaluations.len() != requests.len() {
                return Err(PolicyBatchDenial {
                    request_index: 0,
                    error: ProtocolError::new(
                        "policy_batch_invalid",
                        "a policy evaluator returned an invalid batch result",
                    ),
                });
            }
            if let Some(batch_revision) = batch.revision {
                if revision.is_some_and(|current| current != batch_revision) {
                    return Err(PolicyBatchDenial {
                        request_index: 0,
                        error: ProtocolError::new(
                            "policy_batch_revision_conflict",
                            "policy evaluators did not use one pinned revision",
                        ),
                    });
                }
                revision = Some(batch_revision);
            }
            self.check_tree_revision(&requests[0])
                .await
                .map_err(|error| PolicyBatchDenial {
                    request_index: 0,
                    error,
                })?;
            for (index, explanation) in batch.explanations.iter().enumerate() {
                if unresolved.get(index) == Some(&true) {
                    self.record(&requests[index], explanation).await;
                }
            }
            for (index, evaluation) in batch.evaluations.into_iter().enumerate() {
                if !unresolved[index] {
                    continue;
                }
                match evaluation {
                    PolicyEvaluation::Abstain => {}
                    PolicyEvaluation::Decide(PolicyDecision::Allow) => unresolved[index] = false,
                    PolicyEvaluation::Decide(PolicyDecision::Deny(denial)) => {
                        return Err(PolicyBatchDenial {
                            request_index: index,
                            error: denial.into_error(),
                        });
                    }
                }
            }
        }
        for (index, is_unresolved) in unresolved.into_iter().enumerate() {
            if !is_unresolved {
                continue;
            }
            if let PolicyDecision::Deny(denial) = self.default_decision.clone() {
                return Err(PolicyBatchDenial {
                    request_index: index,
                    error: denial.into_error(),
                });
            }
        }
        Ok(PolicyBatchAuthorization { revision })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PolicyExplanation {
    pub computed_effect: PolicyEffect,
    pub effective_decision: PolicyEffect,
    pub mode: PolicyMode,
    pub matched_rule_id: Option<String>,
    pub reason: String,
    pub policy_revision: u64,
    pub tree_revision: Option<u64>,
    pub source: String,
}
impl PolicyExplanation {
    fn into_decision(self) -> PolicyDecision {
        if self.effective_decision == PolicyEffect::Allow {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(PolicyDenial::new(
                "policy_denied",
                "workspace policy denied this operation",
            ))
        }
    }
    fn baseline(decision: PolicyDecision, tree_revision: Option<u64>) -> Self {
        let effect = if matches!(decision, PolicyDecision::Allow) {
            PolicyEffect::Allow
        } else {
            PolicyEffect::Deny
        };
        Self {
            computed_effect: effect,
            effective_decision: effect,
            mode: PolicyMode::Monitor,
            matched_rule_id: None,
            reason: "Core fallback".into(),
            policy_revision: 0,
            tree_revision,
            source: "baseline".into(),
        }
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct PolicyDecisionRecord {
    pub workspace_id: String,
    pub actor_kind: PolicyPrincipalKind,
    pub actor_id: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_id: String,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub explanation: PolicyExplanation,
}

const POLICY_CACHE_TTL: Duration = Duration::from_secs(5);
const POLICY_PROVIDER_BUNDLE_MAX_BYTES: u32 = 300 * 1024;
const POLICY_PROVIDER_FETCH_TIMEOUT_MS: u64 = 3_000;

#[derive(Debug, Clone, Default, Serialize)]
pub struct PolicyProviderCacheStatus {
    pub revision: Option<u64>,
    pub age_seconds: Option<u64>,
    pub last_error: Option<String>,
}

struct CachedProviderPolicy {
    app_id: String,
    checked_at: Instant,
    loaded_at: Option<Instant>,
    policy: Option<Arc<CompiledWorkspacePolicy>>,
    last_error: Option<String>,
}

struct ProviderPolicyEvaluator {
    store: PolicyProviderStore,
    auth: AuthStore,
    state: AppState,
    cache: RwLock<HashMap<String, CachedProviderPolicy>>,
    fetch_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ProviderPolicyEvaluator {
    fn new(store: PolicyProviderStore, auth: AuthStore, state: AppState) -> Self {
        Self {
            store,
            auth,
            state,
            cache: RwLock::new(HashMap::new()),
            fetch_locks: Mutex::new(HashMap::new()),
        }
    }

    async fn invalidate(&self, workspace_id: &str) {
        self.cache.write().await.remove(workspace_id);
    }

    async fn cache_status(&self, workspace_id: &str) -> PolicyProviderCacheStatus {
        self.cache
            .read()
            .await
            .get(workspace_id)
            .map(|cached| PolicyProviderCacheStatus {
                revision: cached.policy.as_ref().map(|policy| policy.revision),
                age_seconds: cached.loaded_at.map(|loaded| loaded.elapsed().as_secs()),
                last_error: cached.last_error.clone(),
            })
            .unwrap_or_default()
    }

    async fn fetch_lock(&self, workspace_id: &str) -> Arc<Mutex<()>> {
        self.fetch_locks
            .lock()
            .await
            .entry(workspace_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn compiled(
        &self,
        workspace_id: &str,
    ) -> Result<Option<Arc<CompiledWorkspacePolicy>>, ProtocolError> {
        if let Some(cached) = self.cache.read().await.get(workspace_id) {
            if cached.checked_at.elapsed() < POLICY_CACHE_TTL {
                return cached_provider_result(cached);
            }
        }
        let fetch_lock = self.fetch_lock(workspace_id).await;
        let _guard = fetch_lock.lock().await;
        if let Some(cached) = self.cache.read().await.get(workspace_id) {
            if cached.checked_at.elapsed() < POLICY_CACHE_TTL {
                return cached_provider_result(cached);
            }
        }

        let provider = self.store.get(workspace_id).await.map_err(|error| {
            ProtocolError::new("policy_provider_store_failed", error.to_string())
        })?;
        let Some(provider) = provider else {
            self.cache.write().await.insert(
                workspace_id.to_string(),
                CachedProviderPolicy {
                    app_id: String::new(),
                    checked_at: Instant::now(),
                    loaded_at: None,
                    policy: None,
                    last_error: None,
                },
            );
            return Ok(None);
        };

        let previous = self.cache.write().await.remove(workspace_id);
        let fetched = self.fetch_bundle(&provider.app_id, workspace_id).await;
        match fetched {
            Ok(bundle) if bundle.revision >= provider.revision_hint => {
                let policy = Arc::new(CompiledWorkspacePolicy::compile(WorkspacePolicy {
                    workspace_id: workspace_id.to_string(),
                    revision: bundle.revision,
                    mode: bundle.mode,
                    document: bundle.document,
                    updated_at: bundle.generated_at,
                    updated_by: PolicyPrincipalRef {
                        kind: PolicyPrincipalKind::Service,
                        id: provider.service_id,
                    },
                }));
                self.cache.write().await.insert(
                    workspace_id.to_string(),
                    CachedProviderPolicy {
                        app_id: provider.app_id,
                        checked_at: Instant::now(),
                        loaded_at: Some(Instant::now()),
                        policy: Some(policy.clone()),
                        last_error: None,
                    },
                );
                Ok(Some(policy))
            }
            Ok(bundle) => {
                let error = format!(
                    "Policy App returned revision {} below announced revision {}",
                    bundle.revision, provider.revision_hint
                );
                self.provider_failure(workspace_id, provider, previous, error)
                    .await
            }
            Err(error) => {
                // Incompatible contracts are not transient outages: never downgrade
                // unknown rules through fail-open or a cached v1 policy.
                if provider_contract_error(&error) {
                    return Err(error);
                }
                self.provider_failure(workspace_id, provider, previous, error.message)
                    .await
            }
        }
    }

    async fn fetch_bundle(
        &self,
        app_id: &str,
        workspace_id: &str,
    ) -> Result<PolicyProviderBundle, ProtocolError> {
        let app = self
            .auth
            .resolve_app_deployment(workspace_id, app_id)
            .await
            .map_err(|error| error.into_parts().1)?;
        let manifest_value = self
            .state
            .send_command(
                workspace_id,
                &app.server_id,
                AgentCommand::FetchServiceHttp {
                    policy_schema_version: None,
                    port: app.port,
                    path: "/v1/manifest".into(),
                    max_bytes: 32 * 1024,
                    timeout_ms: POLICY_PROVIDER_FETCH_TIMEOUT_MS,
                },
            )
            .await?;
        let manifest: treer_protocol::PolicyProviderManifest =
            serde_json::from_value(manifest_value).map_err(|_| {
                ProtocolError::new(
                    "unsupported_policy_provider",
                    "invalid Policy Provider manifest",
                )
            })?;
        let supports_v2 = manifest
            .capabilities
            .iter()
            .any(|c| c == treer_protocol::POLICY_PROVIDER_BUNDLE_CAPABILITY_V2);
        if manifest.protocol != POLICY_PROVIDER_PROTOCOL_V1
            || !supports_v2
                && !manifest
                    .capabilities
                    .iter()
                    .any(|c| c == treer_protocol::POLICY_PROVIDER_BUNDLE_CAPABILITY_V1)
        {
            return Err(ProtocolError::new(
                "unsupported_policy_provider",
                "unsupported Policy Provider capabilities",
            ));
        }
        let query = {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("workspace_id", workspace_id);
            query.append_pair("max_schema_version", "2");
            query.finish()
        };
        let value = self
            .state
            .send_command(
                workspace_id,
                &app.server_id,
                AgentCommand::FetchServiceHttp {
                    policy_schema_version: supports_v2.then_some(2),
                    port: app.port,
                    path: format!("/v1/policy/bundle?{query}"),
                    max_bytes: POLICY_PROVIDER_BUNDLE_MAX_BYTES,
                    timeout_ms: POLICY_PROVIDER_FETCH_TIMEOUT_MS,
                },
            )
            .await?;
        let bundle: PolicyProviderBundle = serde_json::from_value(value).map_err(|_| {
            ProtocolError::new(
                "invalid_policy_contract",
                "invalid Policy Provider bundle fields or types",
            )
        })?;
        if bundle.protocol != POLICY_PROVIDER_PROTOCOL_V1 {
            return Err(ProtocolError::new(
                "unsupported_policy_provider_protocol",
                format!("Policy App uses unsupported protocol {}", bundle.protocol),
            ));
        }
        if bundle.workspace_id != workspace_id {
            return Err(ProtocolError::new(
                "policy_provider_workspace_mismatch",
                "Policy App returned a bundle for another workspace",
            ));
        }
        treer_proxy::policy_store::validate_document(&bundle.document)
            .map_err(|error| ProtocolError::new("invalid_policy_contract", error.code()))?;
        if bundle.document.schema_version == 2 && !supports_v2 {
            return Err(ProtocolError::new(
                "unsupported_policy_provider",
                "schema v2 requires policy.bundle.v2 capability",
            ));
        }
        Ok(bundle)
    }

    async fn provider_failure(
        &self,
        workspace_id: &str,
        provider: treer_protocol::WorkspacePolicyProvider,
        previous: Option<CachedProviderPolicy>,
        error: String,
    ) -> Result<Option<Arc<CompiledWorkspacePolicy>>, ProtocolError> {
        let stale = previous.and_then(|cached| {
            (cached.app_id == provider.app_id
                && cached
                    .loaded_at
                    .is_some_and(|loaded| loaded.elapsed().as_secs() <= provider.max_stale_seconds))
            .then_some(cached)
        });
        if let Some(mut cached) = stale {
            cached.checked_at = Instant::now();
            cached.last_error = Some(error);
            let policy = cached.policy.clone();
            self.cache
                .write()
                .await
                .insert(workspace_id.to_string(), cached);
            return Ok(policy);
        }
        if provider.failure_mode == PolicyProviderFailureMode::FailOpen {
            let policy = Arc::new(CompiledWorkspacePolicy::allow(provider.revision_hint));
            self.cache.write().await.insert(
                workspace_id.to_string(),
                CachedProviderPolicy {
                    app_id: provider.app_id,
                    checked_at: Instant::now(),
                    loaded_at: None,
                    policy: Some(policy.clone()),
                    last_error: Some(error),
                },
            );
            return Ok(Some(policy));
        }
        self.cache.write().await.insert(
            workspace_id.to_string(),
            CachedProviderPolicy {
                app_id: provider.app_id,
                checked_at: Instant::now(),
                loaded_at: None,
                policy: None,
                last_error: Some(error.clone()),
            },
        );
        Err(ProtocolError::new("policy_provider_unavailable", error))
    }
}

fn provider_contract_error(error: &ProtocolError) -> bool {
    matches!(
        error.code.as_str(),
        "unsupported_command"
            | "unsupported_policy_provider_protocol"
            | "unsupported_policy_schema"
            | "unsupported_policy_provider"
            | "invalid_policy_contract"
            | "policy_provider_workspace_mismatch"
            | "invalid_service_json"
            | "service_http_response_too_large"
    )
}

fn cached_provider_result(
    cached: &CachedProviderPolicy,
) -> Result<Option<Arc<CompiledWorkspacePolicy>>, ProtocolError> {
    if cached.app_id.is_empty() || cached.policy.is_some() {
        Ok(cached.policy.clone())
    } else {
        Err(ProtocolError::new(
            "policy_provider_unavailable",
            cached
                .last_error
                .clone()
                .unwrap_or_else(|| "Policy App is unavailable".to_string()),
        ))
    }
}

impl PolicyEvaluator for ProviderPolicyEvaluator {
    fn revision<'a>(
        &'a self,
        workspace_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = PolicyRevision> + Send + 'a>> {
        Box::pin(async move {
            match self.compiled(workspace_id).await {
                Ok(Some(policy)) => PolicyRevision::Known(policy.revision),
                Ok(None) => PolicyRevision::Unrevisioned,
                Err(_) => PolicyRevision::Unavailable,
            }
        })
    }
    fn explain<'a>(
        &'a self,
        request: &'a PolicyRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PolicyExplanation>, ProtocolError>> + Send + 'a>>
    {
        Box::pin(async move {
            let Some(policy) = self.compiled(&request.workspace_id).await? else {
                return Ok(None);
            };
            let mut explanation = policy.explain(request);
            explanation.source = "provider".into();
            if let Some(cached) = self.cache.read().await.get(&request.workspace_id) {
                if cached.last_error.is_some() {
                    explanation.source = if cached.loaded_at.is_some() {
                        "provider_stale"
                    } else {
                        "provider_fail_open"
                    }
                    .into();
                }
            }
            Ok(Some(explanation))
        })
    }
    fn evaluate<'a>(&'a self, request: &'a PolicyRequest) -> PolicyFuture<'a> {
        Box::pin(async move {
            let Some(policy) = self.compiled(&request.workspace_id).await? else {
                return Ok(PolicyEvaluation::Abstain);
            };
            Ok(PolicyEvaluation::Decide(policy.evaluate(request)))
        })
    }

    fn evaluate_batch<'a>(&'a self, requests: &'a [PolicyRequest]) -> PolicyBatchFuture<'a> {
        Box::pin(async move {
            let Some(first) = requests.first() else {
                return Ok(PolicyBatchEvaluation {
                    explanations: Vec::new(),
                    evaluations: Vec::new(),
                    revision: None,
                });
            };
            if requests
                .iter()
                .any(|request| request.workspace_id != first.workspace_id)
            {
                return Err(ProtocolError::new(
                    "policy_batch_workspace_mismatch",
                    "a policy batch must belong to one workspace",
                ));
            }
            let Some(policy) = self.compiled(&first.workspace_id).await? else {
                return Ok(PolicyBatchEvaluation {
                    explanations: Vec::new(),
                    evaluations: vec![PolicyEvaluation::Abstain; requests.len()],
                    revision: None,
                });
            };
            Ok(PolicyBatchEvaluation {
                explanations: requests.iter().map(|r| policy.explain(r)).collect(),
                evaluations: requests
                    .iter()
                    .map(|request| PolicyEvaluation::Decide(policy.evaluate(request)))
                    .collect(),
                revision: Some(policy.revision),
            })
        })
    }
}

struct CachedWorkspacePolicy {
    loaded_at: Instant,
    policy: Option<Arc<CompiledWorkspacePolicy>>,
}

#[derive(Debug)]
pub(crate) struct CompiledWorkspacePolicy {
    revision: u64,
    mode: PolicyMode,
    defaults: BTreeMap<String, PolicyEffect>,
    groups: HashMap<String, Vec<PolicyPrincipalRef>>,
    rules_by_action: HashMap<String, Vec<Arc<WorkspacePolicyRule>>>,
}

struct DurablePolicyEvaluator {
    store: WorkspacePolicyStore,
    cache: RwLock<HashMap<String, CachedWorkspacePolicy>>,
}

impl DurablePolicyEvaluator {
    fn new(store: WorkspacePolicyStore) -> Self {
        Self {
            store,
            cache: RwLock::new(HashMap::new()),
        }
    }

    async fn compiled(
        &self,
        workspace_id: &str,
    ) -> Result<Option<Arc<CompiledWorkspacePolicy>>, ProtocolError> {
        if let Some(cached) = self.cache.read().await.get(workspace_id) {
            if cached.loaded_at.elapsed() < POLICY_CACHE_TTL {
                return Ok(cached.policy.clone());
            }
        }
        let policy = self
            .store
            .get(workspace_id)
            .await
            .map_err(|error| ProtocolError::new(error.code(), error.to_string()))?
            .map(CompiledWorkspacePolicy::compile)
            .map(Arc::new);
        self.cache.write().await.insert(
            workspace_id.to_string(),
            CachedWorkspacePolicy {
                loaded_at: Instant::now(),
                policy: policy.clone(),
            },
        );
        Ok(policy)
    }
}

impl PolicyEvaluator for DurablePolicyEvaluator {
    fn revision<'a>(
        &'a self,
        workspace_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = PolicyRevision> + Send + 'a>> {
        Box::pin(async move {
            match self.compiled(workspace_id).await {
                Ok(Some(policy)) => PolicyRevision::Known(policy.revision),
                Ok(None) => PolicyRevision::Unrevisioned,
                Err(_) => PolicyRevision::Unavailable,
            }
        })
    }
    fn explain<'a>(
        &'a self,
        request: &'a PolicyRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Option<PolicyExplanation>, ProtocolError>> + Send + 'a>>
    {
        Box::pin(async move {
            Ok(self
                .compiled(&request.workspace_id)
                .await?
                .map(|policy| policy.explain(request)))
        })
    }
    fn evaluate<'a>(&'a self, request: &'a PolicyRequest) -> PolicyFuture<'a> {
        Box::pin(async move {
            let Some(policy) = self.compiled(&request.workspace_id).await? else {
                return Ok(PolicyEvaluation::Abstain);
            };
            Ok(PolicyEvaluation::Decide(policy.evaluate(request)))
        })
    }

    fn evaluate_batch<'a>(&'a self, requests: &'a [PolicyRequest]) -> PolicyBatchFuture<'a> {
        Box::pin(async move {
            let Some(first) = requests.first() else {
                return Ok(PolicyBatchEvaluation {
                    explanations: Vec::new(),
                    evaluations: Vec::new(),
                    revision: None,
                });
            };
            if requests
                .iter()
                .any(|request| request.workspace_id != first.workspace_id)
            {
                return Err(ProtocolError::new(
                    "policy_batch_workspace_mismatch",
                    "a policy batch must belong to one workspace",
                ));
            }
            let Some(policy) = self.compiled(&first.workspace_id).await? else {
                return Ok(PolicyBatchEvaluation {
                    explanations: Vec::new(),
                    evaluations: vec![PolicyEvaluation::Abstain; requests.len()],
                    revision: None,
                });
            };
            Ok(PolicyBatchEvaluation {
                explanations: requests.iter().map(|r| policy.explain(r)).collect(),
                evaluations: requests
                    .iter()
                    .map(|request| PolicyEvaluation::Decide(policy.evaluate(request)))
                    .collect(),
                revision: Some(policy.revision),
            })
        })
    }
}

impl CompiledWorkspacePolicy {
    pub(crate) fn explain_document(
        document: treer_protocol::WorkspacePolicyDocument,
        mode: PolicyMode,
        request: &PolicyRequest,
    ) -> PolicyExplanation {
        Self::compile(WorkspacePolicy {
            workspace_id: request.workspace_id.clone(),
            revision: 0,
            mode,
            document,
            updated_at: chrono::Utc::now(),
            updated_by: PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "simulation".into(),
            },
        })
        .explain(request)
    }
    fn allow(revision: u64) -> Self {
        Self {
            revision,
            mode: PolicyMode::Monitor,
            defaults: BTreeMap::new(),
            groups: HashMap::new(),
            rules_by_action: HashMap::new(),
        }
    }

    fn compile(policy: WorkspacePolicy) -> Self {
        let stable_ties = policy.document.schema_version == 2;
        let mut rules_by_action: HashMap<String, Vec<Arc<WorkspacePolicyRule>>> = HashMap::new();
        for rule in policy.document.rules {
            let rule = Arc::new(rule);
            let mut seen = HashSet::new();
            for action in &rule.actions {
                if !seen.insert(action) {
                    continue;
                }
                rules_by_action
                    .entry(action.clone())
                    .or_default()
                    .push(rule.clone());
            }
        }
        for rules in rules_by_action.values_mut() {
            rules.sort_by(|left, right| {
                right
                    .priority
                    .cmp(&left.priority)
                    .then_with(|| effect_order(right.effect).cmp(&effect_order(left.effect)))
                    .then_with(|| {
                        if stable_ties {
                            left.id.cmp(&right.id)
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
            });
        }
        Self {
            revision: policy.revision,
            mode: policy.mode,
            defaults: policy.document.defaults,
            groups: policy
                .document
                .groups
                .into_iter()
                .map(|(name, group)| (name, group.principals))
                .collect(),
            rules_by_action,
        }
    }

    fn evaluate(&self, request: &PolicyRequest) -> PolicyDecision {
        self.explain(request).into_decision()
    }

    fn explain(&self, request: &PolicyRequest) -> PolicyExplanation {
        let matched = self.rules_by_action.get(&request.action).and_then(|rules| {
            rules.iter().find(|rule| {
                rule.subjects
                    .iter()
                    .any(|s| self.subject_matches(s, request))
                    && rule
                        .resources
                        .iter()
                        .any(|r| self.resource_matches(r, request))
                    && path_match::conditions_match(rule, request)
            })
        });
        let effect = matched
            .map(|r| r.effect)
            .or_else(|| self.defaults.get(&request.action).copied())
            .unwrap_or(PolicyEffect::Allow);
        PolicyExplanation {
            computed_effect: effect,
            effective_decision: if self.mode == PolicyMode::Monitor {
                PolicyEffect::Allow
            } else {
                effect
            },
            mode: self.mode,
            matched_rule_id: matched.map(|r| r.id.clone()),
            reason: if matched.is_some() {
                "rule matched"
            } else {
                "no rule matched; action default"
            }
            .into(),
            policy_revision: self.revision,
            tree_revision: request.tree_revision,
            source: "workspace_policy".into(),
        }
    }

    fn subject_matches(&self, selector: &PolicySubjectSelector, request: &PolicyRequest) -> bool {
        let (kind, id, machine_id) = subject_parts(&request.subject);
        selector.kind.is_none_or(|expected| expected == kind)
            && path_match::selector_matches(selector.tree.as_ref(), request.subject_tree.as_ref())
            && selector.id.as_deref().is_none_or(|expected| expected == id)
            && selector
                .machine_id
                .as_deref()
                .is_none_or(|expected| expected == machine_id)
            && selector
                .group
                .as_deref()
                .is_none_or(|group| self.group_contains(group, kind, id))
            && (!selector.is_self || request.resource.id == id)
    }

    fn resource_matches(&self, selector: &PolicyResourceSelector, request: &PolicyRequest) -> bool {
        path_match::selector_matches(selector.tree.as_ref(), request.resource_tree.as_ref())
            && selector
                .kind
                .as_deref()
                .is_none_or(|kind| kind == request.resource.kind)
            && selector
                .id
                .as_deref()
                .is_none_or(|id| id == request.resource.id)
            && selector.principal_group.as_deref().is_none_or(|group| {
                resource_principal(&request.resource)
                    .is_some_and(|(kind, id)| self.group_contains(group, kind, id))
            })
    }

    fn group_contains(&self, group: &str, kind: PolicyPrincipalKind, id: &str) -> bool {
        self.groups.get(group).is_some_and(|principals| {
            principals
                .iter()
                .any(|principal| principal.kind == kind && principal.id == id)
        })
    }
}

const fn effect_order(effect: PolicyEffect) -> u8 {
    match effect {
        PolicyEffect::Allow => 0,
        PolicyEffect::Deny => 1,
    }
}

fn subject_parts(subject: &PolicySubject) -> (PolicyPrincipalKind, &str, &str) {
    match subject {
        PolicySubject::Agent {
            server_id,
            agent_id,
        } => (PolicyPrincipalKind::Agent, agent_id, server_id),
        PolicySubject::Machine { server_id } => {
            (PolicyPrincipalKind::Machine, server_id, server_id)
        }
        PolicySubject::Human { user_id } => (PolicyPrincipalKind::Human, user_id, ""),
        PolicySubject::Service { service_id } => (PolicyPrincipalKind::Service, service_id, ""),
    }
}

fn resource_principal(resource: &PolicyResource) -> Option<(PolicyPrincipalKind, &str)> {
    match resource.kind.as_str() {
        RESOURCE_AGENT => Some((PolicyPrincipalKind::Agent, &resource.id)),
        RESOURCE_MACHINE => Some((PolicyPrincipalKind::Machine, &resource.id)),
        RESOURCE_MESSAGE_MAILBOX => match resource.attributes.get("principal_kind")?.as_str() {
            "agent" => Some((PolicyPrincipalKind::Agent, &resource.id)),
            "human" => Some((PolicyPrincipalKind::Human, &resource.id)),
            "machine" => Some((PolicyPrincipalKind::Machine, &resource.id)),
            "service" => Some((PolicyPrincipalKind::Service, &resource.id)),
            _ => None,
        },
        _ => None,
    }
}

fn sensitive_read_baseline(allowed: bool) -> Result<(), ProtocolError> {
    if allowed {
        Ok(())
    } else {
        Err(ProtocolError::new(
            "policy_denied",
            "workspace audit requires an owner or an explicit enforced allow rule",
        ))
    }
}

impl PolicyDecision {
    fn into_result(self) -> Result<(), ProtocolError> {
        match self {
            Self::Allow => Ok(()),
            Self::Deny(denial) => Err(denial.into_error()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use treer_protocol::{PolicyPrincipalGroup, WorkspacePolicyDocument, POLICY_SCHEMA_VERSION};

    struct StaticEvaluator(PolicyEvaluation);

    impl PolicyEvaluator for StaticEvaluator {
        fn evaluate<'a>(&'a self, _request: &'a PolicyRequest) -> PolicyFuture<'a> {
            Box::pin(async { Ok(self.0.clone()) })
        }
    }

    struct PinnedBatchEvaluator {
        calls: Arc<AtomicUsize>,
        revision: u64,
        evaluations: Vec<PolicyEvaluation>,
    }

    impl PolicyEvaluator for PinnedBatchEvaluator {
        fn evaluate<'a>(&'a self, _request: &'a PolicyRequest) -> PolicyFuture<'a> {
            Box::pin(async {
                panic!("batch authorization must not fall back to per-item evaluation")
            })
        }

        fn evaluate_batch<'a>(&'a self, _requests: &'a [PolicyRequest]) -> PolicyBatchFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Ok(PolicyBatchEvaluation {
                    explanations: Vec::new(),
                    evaluations: self.evaluations.clone(),
                    revision: Some(self.revision),
                })
            })
        }
    }

    fn request() -> PolicyRequest {
        PolicyRequest::network_connect(
            "workspace-a",
            "source-machine",
            Some("agent-a"),
            "destination-machine",
            Some("agent-b"),
            "127.0.0.1",
            8080,
        )
    }

    #[tokio::test]
    async fn default_engine_allows_every_request() {
        PolicyEngine::allow_all()
            .authorize(&request())
            .await
            .expect("default policy should allow access");
    }

    #[tokio::test]
    async fn first_explicit_decision_wins_after_abstentions() {
        let engine = PolicyEngine::new(
            PolicyDecision::Allow,
            vec![
                Arc::new(StaticEvaluator(PolicyEvaluation::Abstain)),
                Arc::new(StaticEvaluator(PolicyEvaluation::Decide(
                    PolicyDecision::Deny(PolicyDenial::new("policy_denied", "blocked by test")),
                ))),
                Arc::new(StaticEvaluator(PolicyEvaluation::Decide(
                    PolicyDecision::Allow,
                ))),
            ],
        );
        let error = engine
            .authorize(&request())
            .await
            .expect_err("explicit deny should stop evaluation");
        assert_eq!(error.code, "policy_denied");
    }

    #[tokio::test]
    async fn batch_authorization_uses_one_evaluator_snapshot_and_reports_denied_index() {
        let requests = vec![request(), request(), request()];
        let calls = Arc::new(AtomicUsize::new(0));
        let engine = PolicyEngine::new(
            PolicyDecision::Allow,
            vec![Arc::new(PinnedBatchEvaluator {
                calls: calls.clone(),
                revision: 42,
                evaluations: vec![
                    PolicyEvaluation::Decide(PolicyDecision::Allow),
                    PolicyEvaluation::Decide(PolicyDecision::Allow),
                    PolicyEvaluation::Decide(PolicyDecision::Allow),
                ],
            })],
        );
        let authorized = engine
            .authorize_batch(&requests)
            .await
            .expect("authorize one pinned batch");
        assert_eq!(authorized.revision, Some(42));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let denied = PolicyEngine::new(
            PolicyDecision::Allow,
            vec![Arc::new(PinnedBatchEvaluator {
                calls,
                revision: 43,
                evaluations: vec![
                    PolicyEvaluation::Decide(PolicyDecision::Allow),
                    PolicyEvaluation::Decide(PolicyDecision::Deny(PolicyDenial::new(
                        "policy_denied",
                        "blocked by test",
                    ))),
                    PolicyEvaluation::Decide(PolicyDecision::Allow),
                ],
            })],
        )
        .authorize_batch(&requests)
        .await
        .expect_err("batch denial should identify its request");
        assert_eq!(denied.request_index, 1);
        assert_eq!(denied.error.code, "policy_denied");
    }

    #[test]
    fn network_request_preserves_subject_action_and_resource_context() {
        let request = request();
        assert_eq!(request.workspace_id, "workspace-a");
        assert_eq!(request.action, ACTION_NETWORK_CONNECT);
        assert_eq!(request.resource.kind, RESOURCE_NETWORK_ENDPOINT);
        assert_eq!(request.resource.attributes["port"], "8080");
        assert_eq!(
            request.resource.attributes["destination_agent_id"],
            "agent-b"
        );
        assert_eq!(
            request.subject,
            PolicySubject::Agent {
                server_id: "source-machine".to_string(),
                agent_id: "agent-a".to_string(),
            }
        );
    }

    #[test]
    fn requests_without_an_agent_use_a_machine_subject() {
        let request = PolicyRequest::network_connect(
            "workspace-a",
            "source-machine",
            None,
            "destination-machine",
            None,
            "127.0.0.1",
            8080,
        );
        assert_eq!(
            request.subject,
            PolicySubject::Machine {
                server_id: "source-machine".to_string(),
            }
        );
    }

    #[test]
    fn generic_requests_support_future_actions_and_resource_attributes() {
        let request = PolicyRequest::new(
            "workspace-a",
            PolicySubject::Agent {
                server_id: "machine-a".to_string(),
                agent_id: "agent-a".to_string(),
            },
            ACTION_VIRTUAL_HOST_CREATE,
            PolicyResource::new(RESOURCE_VIRTUAL_HOST, "api.internal")
                .with_attribute("destination_server_id", "machine-b")
                .with_attribute("target_port", "8080"),
        );
        assert_eq!(request.action, ACTION_VIRTUAL_HOST_CREATE);
        assert_eq!(request.resource.kind, RESOURCE_VIRTUAL_HOST);
        assert_eq!(request.resource.id, "api.internal");
        assert_eq!(
            request.resource.attributes["destination_server_id"],
            "machine-b"
        );
    }

    #[test]
    fn configured_provider_failure_does_not_abstain_into_legacy_allow() {
        let failed = CachedProviderPolicy {
            app_id: "app_policy".to_string(),
            checked_at: Instant::now(),
            loaded_at: None,
            policy: None,
            last_error: Some("offline".to_string()),
        };
        let error = cached_provider_result(&failed).expect_err("configured failure must deny");
        assert_eq!(error.code, "policy_provider_unavailable");

        let absent = CachedProviderPolicy {
            app_id: String::new(),
            checked_at: Instant::now(),
            loaded_at: None,
            policy: None,
            last_error: None,
        };
        assert!(cached_provider_result(&absent)
            .expect("missing provider abstains")
            .is_none());
    }

    #[test]
    fn compiled_policy_applies_priority_deny_ties_and_monitor_mode() {
        let document = WorkspacePolicyDocument {
            schema_version: POLICY_SCHEMA_VERSION,
            defaults: BTreeMap::from([(ACTION_AGENT_PROMPT.to_string(), PolicyEffect::Deny)]),
            groups: BTreeMap::from([(
                "reviewers".to_string(),
                PolicyPrincipalGroup {
                    principals: vec![PolicyPrincipalRef {
                        kind: PolicyPrincipalKind::Agent,
                        id: "target".to_string(),
                    }],
                },
            )]),
            rules: vec![
                WorkspacePolicyRule {
                    bindings: Default::default(),
                    conditions: Vec::new(),
                    id: "allow-reviewer".to_string(),
                    priority: 10,
                    effect: PolicyEffect::Allow,
                    subjects: vec![PolicySubjectSelector {
                        tree: None,
                        kind: Some(PolicyPrincipalKind::Agent),
                        id: Some("source".to_string()),
                        machine_id: Some("machine-a".to_string()),
                        group: None,
                        is_self: false,
                    }],
                    actions: vec![ACTION_AGENT_PROMPT.to_string()],
                    resources: vec![PolicyResourceSelector {
                        tree: None,
                        kind: Some(RESOURCE_AGENT.to_string()),
                        id: None,
                        principal_group: Some("reviewers".to_string()),
                    }],
                },
                WorkspacePolicyRule {
                    bindings: Default::default(),
                    conditions: Vec::new(),
                    id: "deny-tie".to_string(),
                    priority: 10,
                    effect: PolicyEffect::Deny,
                    subjects: vec![PolicySubjectSelector {
                        tree: None,
                        kind: Some(PolicyPrincipalKind::Agent),
                        id: Some("source".to_string()),
                        machine_id: None,
                        group: None,
                        is_self: false,
                    }],
                    actions: vec![ACTION_AGENT_PROMPT.to_string()],
                    resources: vec![PolicyResourceSelector {
                        tree: None,
                        kind: Some(RESOURCE_AGENT.to_string()),
                        id: Some("target".to_string()),
                        principal_group: None,
                    }],
                },
            ],
        };
        let request = PolicyRequest::new(
            "workspace-a",
            PolicySubject::Agent {
                server_id: "machine-a".to_string(),
                agent_id: "source".to_string(),
            },
            ACTION_AGENT_PROMPT,
            PolicyResource::new(RESOURCE_AGENT, "target"),
        );
        let enforce = CompiledWorkspacePolicy::compile(WorkspacePolicy {
            workspace_id: "workspace-a".to_string(),
            revision: 1,
            mode: PolicyMode::Enforce,
            document: document.clone(),
            updated_at: Utc::now(),
            updated_by: PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".to_string(),
            },
        });
        assert!(matches!(
            enforce.evaluate(&request),
            PolicyDecision::Deny(_)
        ));

        let monitor = CompiledWorkspacePolicy::compile(WorkspacePolicy {
            workspace_id: "workspace-a".to_string(),
            revision: 1,
            mode: PolicyMode::Monitor,
            document,
            updated_at: Utc::now(),
            updated_by: PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".to_string(),
            },
        });
        assert_eq!(monitor.evaluate(&request), PolicyDecision::Allow);
    }
}
