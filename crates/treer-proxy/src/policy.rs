use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use treer_protocol::ProtocolError;

pub const ACTION_NETWORK_CONNECT: &str = "network.connect";
pub const ACTION_IDENTITY_TOKEN_ISSUE: &str = "identity.token.issue";
pub const ACTION_MAIL_SEND: &str = "mail.send";
pub const ACTION_MAIL_READ: &str = "mail.read";
pub const ACTION_SERVICE_LIST: &str = "service.list";
pub const ACTION_SERVICE_CREATE: &str = "service.create";
pub const ACTION_SERVICE_UPDATE: &str = "service.update";
pub const ACTION_SERVICE_DELETE: &str = "service.delete";
pub const ACTION_SERVICE_PROBE: &str = "service.probe";
pub const ACTION_VIRTUAL_HOST_LIST: &str = "virtual_host.list";
pub const ACTION_VIRTUAL_HOST_CREATE: &str = "virtual_host.create";
pub const ACTION_VIRTUAL_HOST_DELETE: &str = "virtual_host.delete";
pub const RESOURCE_NETWORK_ENDPOINT: &str = "network.endpoint";
pub const RESOURCE_AGENT_MAILBOX: &str = "agent.mailbox";
pub const RESOURCE_MACHINE_SERVICE: &str = "machine.service";
pub const RESOURCE_VIRTUAL_HOST: &str = "virtual_host";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicySubject {
    Agent { server_id: String, agent_id: String },
    Machine { server_id: String },
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
        host: &str,
        port: u16,
    ) -> Self {
        let mut attributes = BTreeMap::new();
        attributes.insert(
            "destination_server_id".to_string(),
            destination_server_id.to_string(),
        );
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

pub trait PolicyEvaluator: Send + Sync {
    fn evaluate<'a>(&'a self, request: &'a PolicyRequest) -> PolicyFuture<'a>;
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

    struct StaticEvaluator(PolicyEvaluation);

    impl PolicyEvaluator for StaticEvaluator {
        fn evaluate<'a>(&'a self, _request: &'a PolicyRequest) -> PolicyFuture<'a> {
            Box::pin(async { Ok(self.0.clone()) })
        }
    }

    fn request() -> PolicyRequest {
        PolicyRequest::network_connect(
            "workspace-a",
            "source-machine",
            Some("agent-a"),
            "destination-machine",
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

    #[test]
    fn network_request_preserves_subject_action_and_resource_context() {
        let request = request();
        assert_eq!(request.workspace_id, "workspace-a");
        assert_eq!(request.action, ACTION_NETWORK_CONNECT);
        assert_eq!(request.resource.kind, RESOURCE_NETWORK_ENDPOINT);
        assert_eq!(request.resource.attributes["port"], "8080");
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
}
