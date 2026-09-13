use super::*;
pub(super) async fn verify_app_identity(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Json(request): Json<AppIdentityVerifyRequest>,
) -> Response {
    let mut verified = identity.verify_app(&request.token, request.audience.trim());
    if let Some(claims) = verified.claims.as_mut() {
        let service_active = auth
            .resolve_machine_service(&claims.workspace_id, &claims.service_id)
            .await
            .is_ok();
        let membership_active = claims.principal_kind != AppPrincipalKind::Human
            || match auth
                .workspace_member_role(&claims.workspace_id, &claims.sub)
                .await
            {
                Ok(role) => {
                    claims.role = Some(role);
                    true
                }
                Err(_) => false,
            };
        if !service_active || !membership_active {
            verified.active = false;
            verified.claims = None;
        }
    }
    ([(header::CACHE_CONTROL, "no-store")], Json(verified)).into_response()
}

#[derive(Debug, Deserialize)]
pub(super) struct AppOAuthAuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct AppOAuthTokenRequest {
    grant_type: String,
    code: String,
    client_id: String,
    redirect_uri: String,
    code_verifier: String,
}

pub(super) async fn authorize_workspace_app(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    headers: HeaderMap,
    OriginalUri(original_uri): OriginalUri,
    Query(query): Query<AppOAuthAuthorizeQuery>,
) -> Result<Response, ApiFailure> {
    if query.response_type != "code"
        || query.code_challenge_method != "S256"
        || query.state.is_empty()
        || query.state.len() > 512
    {
        return Err(ApiFailure::bad_request(
            "invalid_app_oauth_request",
            "app OAuth requires response_type=code, S256 PKCE, and a bounded state",
        ));
    }
    let (redirect_uri, resolved) = resolve_app_redirect(
        &auth,
        &config,
        query.client_id.trim(),
        query.redirect_uri.trim(),
    )
    .await?;
    let session = match auth::authenticate_request(&auth, &headers).await {
        Ok(session) => session,
        Err(error) => {
            let (status, error) = error.into_parts();
            if status != StatusCode::UNAUTHORIZED {
                return Err(ApiFailure { status, error });
            }
            let mut return_to = config.proxy_public_url.clone();
            return_to.set_path(original_uri.path());
            return_to.set_query(original_uri.query());
            let mut login = config.app_public_url.clone();
            login.set_query(None);
            login
                .query_pairs_mut()
                .append_pair("return_to", return_to.as_str());
            return Ok(Redirect::to(login.as_str()).into_response());
        }
    };
    let role = auth
        .workspace_member_role(&resolved.ingress.workspace_id, &session.user_id)
        .await?;
    let code = auth
        .create_app_oauth_code(
            &auth::AppOAuthGrant {
                workspace_id: resolved.ingress.workspace_id,
                service_id: resolved.service.service_id,
                user_id: session.user_id,
                preferred_name: session.preferred_name,
                role,
            },
            redirect_uri.as_str(),
            query.code_challenge.trim(),
        )
        .await?;
    let mut callback = redirect_uri;
    callback
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &query.state);
    Ok(Redirect::to(callback.as_str()).into_response())
}

pub(super) async fn exchange_workspace_app_code(
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Form(request): Form<AppOAuthTokenRequest>,
) -> Result<Response, ApiFailure> {
    if request.grant_type != "authorization_code" {
        return Err(ApiFailure::bad_request(
            "unsupported_grant_type",
            "app OAuth supports only the authorization_code grant",
        ));
    }
    let grant = auth
        .consume_app_oauth_code(
            request.code.trim(),
            request.client_id.trim(),
            request.redirect_uri.trim(),
            request.code_verifier.trim(),
        )
        .await?;
    let token = identity
        .issue_human(
            &grant.workspace_id,
            &grant.user_id,
            &grant.preferred_name,
            &grant.role,
            &grant.service_id,
        )
        .map_err(|error| {
            tracing::error!(%error, "failed to sign app human identity token");
            ApiFailure::internal(
                "identity_signing_failed",
                "failed to sign app identity token",
            )
        })?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(token)).into_response())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn workspace_app_directory(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiFailure> {
    let claims = authenticate_workspace_app(&auth, &identity, &headers, &service_id).await?;
    let principals = workspace_app_principals(&state, &auth, &claims.workspace_id).await?;
    Ok(Json(json!({ "principals": principals })))
}

pub(super) async fn resolve_workspace_app_recipients(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ResolveAppRecipientsRequest>,
) -> Result<Json<ResolveAppRecipientsResponse>, ApiFailure> {
    if request.recipients.is_empty() || request.recipients.len() > 32 {
        return Err(ApiFailure::bad_request(
            "invalid_app_recipients",
            "recipient resolution requires 1-32 targets",
        ));
    }
    let claims = authenticate_workspace_app(&auth, &identity, &headers, &service_id).await?;
    let principals = workspace_app_principals(&state, &auth, &claims.workspace_id).await?;
    let sender = principals
        .iter()
        .find(|principal| principal.id == claims.sub && principal.kind == claims.principal_kind)
        .cloned()
        .unwrap_or(AppPrincipal {
            kind: claims.principal_kind,
            id: claims.sub.clone(),
            name: claims.name.clone(),
            role: claims.role.clone(),
        });
    let mut seen = HashSet::new();
    let mut recipients = Vec::new();
    for raw_target in request.recipients {
        let target = raw_target.trim();
        let target = if matches!(target, "self" | ".") {
            claims.sub.as_str()
        } else {
            target
        };
        let recipient = resolve_app_principal(&principals, target)?;
        if seen.insert((recipient.kind, recipient.id.clone())) {
            recipients.push(recipient);
        }
    }
    Ok(Json(ResolveAppRecipientsResponse { sender, recipients }))
}

pub(super) async fn resolve_app_redirect(
    auth: &AuthStore,
    config: &IngressConfig,
    service_id: &str,
    redirect_uri: &str,
) -> Result<(Url, auth::ResolvedServiceIngress), ApiFailure> {
    let redirect = Url::parse(redirect_uri)
        .map_err(|_| ApiFailure::bad_request("invalid_redirect_uri", "redirect URI is invalid"))?;
    if !matches!(redirect.scheme(), "http" | "https")
        || redirect.host_str().is_none()
        || !redirect.username().is_empty()
        || redirect.password().is_some()
        || redirect.fragment().is_some()
    {
        return Err(ApiFailure::bad_request(
            "invalid_redirect_uri",
            "redirect URI must be an absolute HTTP URL without credentials or a fragment",
        ));
    }
    let hostname = redirect.host_str().expect("redirect host checked above");
    let resolved = auth
        .resolve_service_ingress_hostname(hostname)
        .await?
        .filter(|resolved| {
            resolved.ingress.enabled
                && resolved.ingress.access == ServiceIngressAccess::Workspace
                && resolved.service.service_id == service_id
        })
        .ok_or_else(|| {
            ApiFailure::bad_request(
                "invalid_redirect_uri",
                "redirect URI is not an enabled workspace ingress for this service",
            )
        })?;
    let expected_origin = config.url_for_hostname(hostname)?.origin();
    if redirect.origin() != expected_origin {
        return Err(ApiFailure::bad_request(
            "invalid_redirect_uri",
            "redirect URI origin does not match the registered service ingress",
        ));
    }
    Ok((redirect, resolved))
}

pub(super) async fn authenticate_workspace_app(
    auth: &AuthStore,
    identity: &IdentityIssuer,
    headers: &HeaderMap,
    service_id: &str,
) -> Result<treer_protocol::AppIdentityClaims, ApiFailure> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            ApiFailure::unauthorized(
                "app_authentication_required",
                "a service-audience Treer identity token is required",
            )
        })?;
    let mut claims = identity
        .verify_app(token, service_id)
        .claims
        .ok_or_else(|| {
            ApiFailure::unauthorized(
                "app_authentication_required",
                "the Treer identity token is invalid for this service",
            )
        })?;
    auth.resolve_machine_service(&claims.workspace_id, service_id)
        .await
        .map_err(|_| {
            ApiFailure::unauthorized(
                "app_authentication_required",
                "the target workspace service is no longer active",
            )
        })?;
    if claims.principal_kind == AppPrincipalKind::Human {
        claims.role = Some(
            auth.workspace_member_role(&claims.workspace_id, &claims.sub)
                .await
                .map_err(|_| {
                    ApiFailure::unauthorized(
                        "app_authentication_required",
                        "the human identity is no longer a workspace member",
                    )
                })?,
        );
    }
    Ok(claims)
}

pub(super) async fn workspace_app_principals(
    state: &AppState,
    auth: &AuthStore,
    workspace_id: &str,
) -> Result<Vec<AppPrincipal>, ApiFailure> {
    let snapshot = state.snapshot(workspace_id).await?;
    let humans = auth.list_workspace_humans(workspace_id).await?;
    Ok(snapshot
        .agents
        .into_iter()
        .map(|agent| AppPrincipal {
            kind: AppPrincipalKind::Agent,
            id: agent.agent_id,
            name: agent.name,
            role: None,
        })
        .chain(humans.into_iter().map(|human| AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: human.user_id,
            name: human.preferred_name,
            role: Some(human.role),
        }))
        .collect())
}

pub(super) fn resolve_app_principal(
    principals: &[AppPrincipal],
    target: &str,
) -> Result<AppPrincipal, ProtocolError> {
    let mut matches = principals
        .iter()
        .filter(|principal| principal.id == target)
        .cloned()
        .collect::<Vec<_>>();
    if matches.is_empty() {
        matches.extend(
            principals
                .iter()
                .filter(|principal| principal.name == target)
                .cloned(),
        );
    }
    match matches.as_slice() {
        [] => Err(ProtocolError::new(
            "recipient_not_found",
            format!("no Agent or human recipient matches {target}"),
        )),
        [recipient] => Ok(recipient.clone()),
        _ => Err(ProtocolError::new(
            "recipient_ambiguous",
            format!("more than one Agent or human is named {target}; use a stable id"),
        )),
    }
}

pub(super) async fn app_message_identity(
    state: &AppState,
    auth: &AuthStore,
    identity: &IdentityIssuer,
    headers: &HeaderMap,
    service_id: &str,
) -> Result<(String, PolicySubject, MessagePrincipal), ApiFailure> {
    let claims = authenticate_workspace_app(auth, identity, headers, service_id).await?;
    let workspace_id = claims.workspace_id.clone();
    let (subject, principal) = match claims.principal_kind {
        AppPrincipalKind::Human => (
            PolicySubject::Human {
                user_id: claims.sub.clone(),
            },
            MessagePrincipal {
                kind: MessagePrincipalKind::Human,
                id: claims.sub,
                name: claims.name,
                role: claims.role,
            },
        ),
        AppPrincipalKind::Agent => {
            let server_id = claims.machine_id.ok_or_else(|| {
                ApiFailure::unauthorized(
                    "app_authentication_required",
                    "the Agent App identity is missing its machine binding",
                )
            })?;
            let agent = state
                .resolve_agent(&claims.workspace_id, &claims.sub)
                .await
                .map_err(|_| {
                    ApiFailure::unauthorized(
                        "app_authentication_required",
                        "the Agent App identity is no longer active",
                    )
                })?;
            if agent.server_id != server_id {
                return Err(ApiFailure::unauthorized(
                    "app_authentication_required",
                    "the Agent App identity no longer matches its machine",
                ));
            }
            (
                PolicySubject::Agent {
                    server_id,
                    agent_id: agent.agent_id.clone(),
                },
                MessagePrincipal {
                    kind: MessagePrincipalKind::Agent,
                    id: agent.agent_id,
                    name: agent.name,
                    role: None,
                },
            )
        }
    };
    Ok((workspace_id, subject, principal))
}
