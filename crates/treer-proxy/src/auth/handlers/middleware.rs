use super::*;
pub(crate) async fn require_user(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    // Match authenticate_request's selection exactly: malformed Authorization
    // falls back to the cookie; an invalid selected Bearer fails authentication
    // rather than falling back. Admin authentication below is cookie-only.
    if !auth.disabled
        && bearer_token(request.headers()).is_none()
        && browser_request_needs_origin(&request)
    {
        if let Err(error) = require_cookie_request_origin(&auth, &request) {
            return error.into_response();
        }
    }
    match authenticate_request(&auth, request.headers()).await {
        Ok(session) => {
            let actor = crate::audit::MutationActor {
                kind: "user",
                id: Some(session.user_id.clone()),
            };
            request.extensions_mut().insert(session);
            crate::audit::MUTATION_ACTOR
                .scope(actor, next.run(request))
                .await
        }
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn require_workspace_access(
    State(auth): State<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    request: Request,
    next: Next,
) -> Response {
    let Some(workspace_id) = workspace_id_from_api_path(request.uri().path()) else {
        return next.run(request).await;
    };
    match auth
        .require_workspace_member(&workspace_id, &session.user_id)
        .await
    {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn require_admin(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    if !auth.disabled && browser_request_needs_origin(&request) {
        if let Err(error) = require_console_origin(&auth, request.headers()) {
            return error.into_response();
        }
    }
    if auth.disabled {
        request.extensions_mut().insert(AdminSession {
            token: "local-admin".to_string(),
        });
        return next.run(request).await;
    }
    let result = cookie_value(request.headers(), ADMIN_SESSION_COOKIE).ok_or_else(|| {
        AuthFailure::unauthorized(
            "admin_authentication_required",
            "administrator authentication required",
        )
    });
    match result {
        Ok(token) => match auth.admin_session(&token).await {
            Ok(Some(session)) => {
                request.extensions_mut().insert(session);
                next.run(request).await
            }
            Ok(None) => AuthFailure::unauthorized(
                "admin_authentication_required",
                "administrator authentication required",
            )
            .into_response(),
            Err(error) => error.into_response(),
        },
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn require_machine(
    State(auth): State<AuthStore>,
    mut request: Request,
    next: Next,
) -> Response {
    match auth.authenticate_machine(request.headers()).await {
        Ok(session) if machine_workspace_matches(&session, request.uri().path()) => {
            match auth.authenticate_agent(&session, request.headers()).await {
                Ok(agent) => {
                    let actor = match &agent {
                        Some(agent) => crate::audit::MutationActor {
                            kind: "agent",
                            id: Some(agent.agent_id.clone()),
                        },
                        None => crate::audit::MutationActor {
                            kind: "machine",
                            id: session.server_id.clone(),
                        },
                    };
                    request.extensions_mut().insert(session);
                    if let Some(agent) = agent {
                        request.extensions_mut().insert(agent);
                    }
                    crate::audit::MUTATION_ACTOR
                        .scope(actor, next.run(request))
                        .await
                }
                Err(error) => error.into_response(),
            }
        }
        Ok(_) => AuthFailure::forbidden(
            "machine_workspace_mismatch",
            "machine credentials do not grant access to this workspace",
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
}
