use super::*;

/// Request-scoped identity and authorization context for Agent endpoints.
///
/// Keeping the resolved subject together with the workspace and policy engine
/// makes it harder for a handler to authorize one target and operate on
/// another, and gives all Agent-facing routes the same ownership checks.
pub(crate) struct AgentRequestContext {
    state: AppState,
    policy: PolicyEngine,
    auth: AuthStore,
    caller: Option<String>,
    workspace_id: String,
    subject: Option<PolicySubject>,
}

impl AgentRequestContext {
    pub(crate) async fn new(
        state: &AppState,
        policy: &PolicyEngine,
        auth: &AuthStore,
        machine: Option<&MachineSession>,
        headers: &HeaderMap,
        workspace_id: &str,
    ) -> Result<Self, ApiFailure> {
        let subject = control_policy_subject(state, machine, headers, workspace_id).await?;
        Ok(Self {
            state: state.clone(),
            policy: policy.clone(),
            auth: auth.clone(),
            caller: headers
                .get(AGENT_ID_HEADER)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
            workspace_id: workspace_id.to_string(),
            subject,
        })
    }

    pub(crate) fn state(&self) -> &AppState {
        &self.state
    }

    pub(crate) fn policy_subject(&self) -> Option<&PolicySubject> {
        self.subject.as_ref()
    }

    pub(crate) fn policy(&self) -> &PolicyEngine {
        &self.policy
    }

    pub(crate) fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    pub(crate) async fn resolve_agent(&self, target: &str) -> Result<AgentInfo, ApiFailure> {
        resolve_workload_at(
            &self.state,
            &self.auth,
            &self.workspace_id,
            self.caller.as_deref(),
            target,
        )
        .await
    }

    pub(crate) async fn authorize(
        &self,
        action: &str,
        resource: PolicyResource,
    ) -> Result<(), ApiFailure> {
        authorize_control(
            &self.policy,
            &self.workspace_id,
            self.subject.as_ref(),
            action,
            resource,
        )
        .await
    }

    pub(crate) async fn authorize_agent(
        &self,
        agent: &AgentInfo,
        action: &str,
    ) -> Result<(), ApiFailure> {
        require_machine_target(self.subject.as_ref(), &agent.server_id)?;
        self.authorize(action, agent_policy_resource(agent)).await
    }
}
