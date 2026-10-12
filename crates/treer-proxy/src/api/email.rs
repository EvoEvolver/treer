use super::*;
use sha2::{Digest, Sha256};
use treer_protocol::{SendAppEmailRequest, SendAppEmailResponse};

pub(super) async fn send_app_email(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Extension(policy): Extension<PolicyEngine>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SendAppEmailRequest>,
) -> Result<Json<SendAppEmailResponse>, ApiFailure> {
    let claims = authenticate_workspace_app(&auth, &identity, &headers, &service_id).await?;
    if claims.principal_kind != AppPrincipalKind::Agent {
        return Err(ApiFailure::forbidden(
            "email_app_required",
            "only a Managed App runtime may send email",
        ));
    }
    let app = auth
        .resolve_app_deployment_by_runtime(&claims.workspace_id, &claims.sub)
        .await?
        .filter(|app| {
            app.service_id == service_id && claims.machine_id.as_deref() == Some(&app.server_id)
        })
        .ok_or_else(|| {
            ApiFailure::forbidden(
                "email_app_required",
                "identity does not own this Managed App service",
            )
        })?;
    let recipient_hash = format!(
        "{:x}",
        Sha256::digest(request.to.to_ascii_lowercase().as_bytes())
    );
    let result = async {
        authorize_control(
            &policy,
            &claims.workspace_id,
            Some(&PolicySubject::Agent {
                server_id: app.server_id,
                agent_id: claims.sub,
            }),
            crate::policy::ACTION_EMAIL_SEND,
            PolicyResource::new(RESOURCE_MACHINE_SERVICE, &service_id),
        )
        .await?;
        auth.send_app_email(
            &claims.workspace_id,
            &app.app_id,
            &request,
            &std::env::var("TREER_EMAIL_ALLOWLIST").unwrap_or_default(),
        )
        .await
        .map(Json)
    }
    .await;
    let outcome = match &result {
        Ok(_) => "sent",
        Err(error) => error.error.code.as_str(),
    };
    tracing::info!(workspace_id = %claims.workspace_id, app_id = %app.app_id,
        recipient_hash = %recipient_hash, outcome, "App email request outcome");
    result
}
