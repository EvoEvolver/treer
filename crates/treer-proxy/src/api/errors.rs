use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Error as JsonError;
use treer_protocol::{ApiError, ProtocolError};

use crate::auth;
use crate::message_store::MessageStoreError;
#[derive(Debug)]
pub struct ApiFailure {
    pub(crate) status: StatusCode,
    pub(crate) error: ProtocolError,
}

impl ApiFailure {
    pub(crate) fn unauthorized(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn forbidden(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn bad_request(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn not_found(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn bad_gateway(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn service_unavailable(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            error: ProtocolError::new(code, message),
        }
    }

    pub(crate) fn internal(code: &str, message: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: ProtocolError::new(code, message),
        }
    }
}

impl From<ProtocolError> for ApiFailure {
    fn from(error: ProtocolError) -> Self {
        let status = match error.code.as_str() {
            "workspace_not_found"
            | "server_not_found"
            | "agent_not_found"
            | "recipient_not_found" => StatusCode::NOT_FOUND,
            "workspace_exists" | "agent_ambiguous" | "server_ambiguous" | "recipient_ambiguous" => {
                StatusCode::CONFLICT
            }
            "machine_file_exists" => StatusCode::CONFLICT,
            "agent_startup_not_found" => StatusCode::NOT_FOUND,
            "policy_denied" | "policy_subject_mismatch" | "agent_identity_mismatch" => {
                StatusCode::FORBIDDEN
            }
            "server_offline" | "no_online_server" | "ssh_unsupported" | "scp_unsupported" => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            "invalid_agent_identity"
            | "invalid_name"
            | "invalid_request"
            | "invalid_machine_exec"
            | "invalid_machine_exec_timeout"
            | "invalid_machine_path"
            | "invalid_machine_file_name"
            | "invalid_machine_upload"
            | "machine_upload_chunk_too_large" => StatusCode::BAD_REQUEST,
            "invalid_agent_startup" | "invalid_agent_startup_validation" => StatusCode::BAD_REQUEST,
            _ => StatusCode::BAD_GATEWAY,
        };
        Self { status, error }
    }
}

impl From<auth::AuthFailure> for ApiFailure {
    fn from(error: auth::AuthFailure) -> Self {
        let (status, error) = error.into_parts();
        Self { status, error }
    }
}

impl From<MessageStoreError> for ApiFailure {
    fn from(error: MessageStoreError) -> Self {
        match error {
            MessageStoreError::Contract { code, message } => {
                let status = match code {
                    "message_not_found"
                    | "message_context_not_found"
                    | "message_delivery_not_found" => StatusCode::NOT_FOUND,
                    "message_idempotency_conflict"
                    | "message_ack_idempotency_conflict"
                    | "message_import_idempotency_conflict"
                    | "message_import_conflict" => StatusCode::CONFLICT,
                    _ => StatusCode::BAD_REQUEST,
                };
                Self {
                    status,
                    error: ProtocolError::new(code, message),
                }
            }
            MessageStoreError::Database(_) => {
                tracing::error!("Core Message database operation failed");
                Self::service_unavailable(
                    "message_store_unavailable",
                    "Core Message storage is unavailable",
                )
            }
            MessageStoreError::Corrupt => {
                tracing::error!("Core Message storage returned invalid data");
                Self::internal(
                    "message_store_corrupt",
                    "Core Message storage contains invalid data",
                )
            }
        }
    }
}

impl From<JsonError> for ApiFailure {
    fn from(error: JsonError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            error: ProtocolError::new("encode_error", error.to_string()),
        }
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        (self.status, Json(ApiError { error: self.error })).into_response()
    }
}
