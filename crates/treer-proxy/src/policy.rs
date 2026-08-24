use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use treer_protocol::{
    PolicyEffect, PolicyMode, PolicyPrincipalKind, PolicyPrincipalRef, PolicyResourceSelector,
    PolicySubjectSelector, ProtocolError, WorkspacePolicy, WorkspacePolicyRule,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

pub const ACTION_NETWORK_CONNECT: &str = "network.connect";
pub const ACTION_AGENT_DISCOVER: &str = "agent.discover";
pub const ACTION_AGENT_METADATA_READ: &str = "agent.metadata.read";
pub const ACTION_AGENT_OUTPUT_READ: &str = "agent.output.read";
pub const ACTION_AGENT_PROMPT: &str = "agent.prompt";
pub const ACTION_AGENT_INPUT: &str = "agent.input";
pub const ACTION_AGENT_STOP: &str = "agent.stop";
pub const ACTION_AGENT_UPDATE: &str = "agent.update";
pub const ACTION_AGENT_DELETE: &str = "agent.delete";
pub const ACTION_AGENT_CREATE: &str = "agent.create";
pub const ACTION_LAUNCH_PROFILE_LIST: &str = "launch_profile.list";
pub const ACTION_LAUNCH_PROFILE_READ: &str = "launch_profile.read";
pub const ACTION_LAUNCH_PROFILE_CREATE: &str = "launch_profile.create";
pub const ACTION_LAUNCH_PROFILE_UPDATE: &str = "launch_profile.update";
pub const ACTION_LAUNCH_PROFILE_DELETE: &str = "launch_profile.delete";
pub const ACTION_LAUNCH_PROFILE_USE: &str = "launch_profile.use";
pub const ACTION_MACHINE_UPDATE: &str = "machine.update";
pub const ACTION_MACHINE_DELETE: &str = "machine.delete";
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

pub trait PolicyEvaluator: Send + Sync {
    fn evaluate<'a>(&'a self, request: &'a PolicyRequest) -> PolicyFuture<'a>;

    fn evaluate_batch<'a>(&'a self, requests: &'a [PolicyRequest]) -> PolicyBatchFuture<'a> {
        Box::pin(async move {
            let mut evaluations = Vec::with_capacity(requests.len());
            for request in requests {
                evaluations.push(self.evaluate(request).await?);
            }
            Ok(PolicyBatchEvaluation {
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
}

impl PolicyEngine {
    pub fn allow_all() -> Self {
        Self::new(PolicyDecision::Allow, Vec::new())
    }

    pub fn durable(store: WorkspacePolicyStore) -> Self {
        Self::new(
            PolicyDecision::Allow,
            vec![Arc::new(DurablePolicyEvaluator::new(store))],
        )
    }

    pub fn new(
        default_decision: PolicyDecision,
        evaluators: Vec<Arc<dyn PolicyEvaluator>>,
    ) -> Self {
        Self {
            evaluators: evaluators.into(),
            default_decision,
        }
    }

    pub async fn authorize(&self, request: &PolicyRequest) -> Result<(), ProtocolError> {
        for evaluator in self.evaluators.iter() {
            match evaluator.evaluate(request).await? {
                PolicyEvaluation::Abstain => {}
                PolicyEvaluation::Decide(decision) => return decision.into_result(),
            }
        }
        self.default_decision.clone().into_result()
    }

    pub async fn authorize_batch(
        &self,
        requests: &[PolicyRequest],
    ) -> Result<PolicyBatchAuthorization, PolicyBatchDenial> {
        if requests.is_empty() {
            return Ok(PolicyBatchAuthorization { revision: None });
        }
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

const POLICY_CACHE_TTL: Duration = Duration::from_secs(5);

struct CachedWorkspacePolicy {
    loaded_at: Instant,
    policy: Option<Arc<CompiledWorkspacePolicy>>,
}

struct CompiledWorkspacePolicy {
    revision: u64,
    mode: PolicyMode,
    defaults: BTreeMap<String, PolicyEffect>,
    groups: HashMap<String, Vec<PolicyPrincipalRef>>,
    rules_by_action: HashMap<String, Vec<WorkspacePolicyRule>>,
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
                    evaluations: vec![PolicyEvaluation::Abstain; requests.len()],
                    revision: None,
                });
            };
            Ok(PolicyBatchEvaluation {
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
    fn compile(policy: WorkspacePolicy) -> Self {
        let mut rules_by_action: HashMap<String, Vec<WorkspacePolicyRule>> = HashMap::new();
        for rule in policy.document.rules {
            for action in &rule.actions {
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
        let effect = self
            .rules_by_action
            .get(&request.action)
            .and_then(|rules| {
                rules.iter().find(|rule| {
                    rule.subjects
                        .iter()
                        .any(|selector| self.subject_matches(selector, request))
                        && rule
                            .resources
                            .iter()
                            .any(|selector| self.resource_matches(selector, request))
                })
            })
            .map(|rule| rule.effect)
            .or_else(|| self.defaults.get(&request.action).copied())
            .unwrap_or(PolicyEffect::Allow);
        if self.mode == PolicyMode::Monitor || effect == PolicyEffect::Allow {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny(PolicyDenial::new(
                "policy_denied",
                "workspace policy denied this operation",
            ))
        }
    }

    fn subject_matches(&self, selector: &PolicySubjectSelector, request: &PolicyRequest) -> bool {
        let (kind, id, machine_id) = subject_parts(&request.subject);
        selector.kind.is_none_or(|expected| expected == kind)
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
        selector
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
                    id: "allow-reviewer".to_string(),
                    priority: 10,
                    effect: PolicyEffect::Allow,
                    subjects: vec![PolicySubjectSelector {
                        kind: Some(PolicyPrincipalKind::Agent),
                        id: Some("source".to_string()),
                        machine_id: Some("machine-a".to_string()),
                        group: None,
                        is_self: false,
                    }],
                    actions: vec![ACTION_AGENT_PROMPT.to_string()],
                    resources: vec![PolicyResourceSelector {
                        kind: Some(RESOURCE_AGENT.to_string()),
                        id: None,
                        principal_group: Some("reviewers".to_string()),
                    }],
                },
                WorkspacePolicyRule {
                    id: "deny-tie".to_string(),
                    priority: 10,
                    effect: PolicyEffect::Deny,
                    subjects: vec![PolicySubjectSelector {
                        kind: Some(PolicyPrincipalKind::Agent),
                        id: Some("source".to_string()),
                        machine_id: None,
                        group: None,
                        is_self: false,
                    }],
                    actions: vec![ACTION_AGENT_PROMPT.to_string()],
                    resources: vec![PolicyResourceSelector {
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
