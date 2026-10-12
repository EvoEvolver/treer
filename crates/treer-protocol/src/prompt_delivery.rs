//! Durable AIS prompt delivery contract. Sender is attested by the control
//! plane, never accepted from the public prompt request.
use super::*;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDelivery {
    #[default]
    Queue,
    Steer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSender {
    pub agent_id: String,
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptOutcome {
    Done,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptNotify {
    pub on: Vec<PromptOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

impl Default for PromptNotify {
    fn default() -> Self {
        Self {
            on: vec![
                PromptOutcome::Done,
                PromptOutcome::Failed,
                PromptOutcome::Interrupted,
            ],
            to: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInterfacePromptRequest {
    pub operation_id: String,
    pub text: String,
    pub mode: String,
    pub delivery: PromptDelivery,
    pub sender: Option<PromptSender>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<PromptNotify>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptDisposition {
    Queued,
    Sent,
    Steered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSubmissionReceipt {
    pub id: String,
    pub disposition: PromptDisposition,
    pub position: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInterfacePromptResponse {
    pub submission: PromptSubmissionReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSubmissionState {
    Queued,
    Running,
    Done,
    Failed,
    Interrupted,
}

impl PromptSubmissionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Interrupted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSubmission {
    pub id: String,
    pub sender: Option<PromptSender>,
    pub created_at: DateTime<Utc>,
    pub position: Option<usize>,
    pub text_preview: String,
    pub state: PromptSubmissionState,
    pub turn_id: Option<String>,
    pub assistant_preview: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQueueResponse {
    pub submissions: Vec<PromptSubmission>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_delivery_contract_round_trips() {
        let request = AgentInterfacePromptRequest {
            operation_id: "cmd-1".into(),
            text: "task".into(),
            mode: "prompt".into(),
            delivery: PromptDelivery::Queue,
            sender: Some(PromptSender {
                agent_id: "ag_sender".into(),
                path: "/team/sender".into(),
                name: "sender".into(),
            }),
            notify: Some(PromptNotify::default()),
        };
        assert_eq!(
            serde_json::from_value::<AgentInterfacePromptRequest>(
                serde_json::to_value(&request).unwrap()
            )
            .unwrap(),
            request
        );
        let receipt = AgentInterfacePromptResponse {
            submission: PromptSubmissionReceipt {
                id: "sub-1".into(),
                disposition: PromptDisposition::Queued,
                position: Some(1),
            },
        };
        assert_eq!(
            serde_json::to_value(receipt).unwrap()["submission"]["disposition"],
            "queued"
        );
        assert!(PromptSubmissionState::Interrupted.is_terminal());
        assert!(!PromptSubmissionState::Queued.is_terminal());
    }
}
