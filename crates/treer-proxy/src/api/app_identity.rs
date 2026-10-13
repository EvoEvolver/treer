use super::*;

/// Recheck current membership and profile at the point of forwarding.
/// Do not take role, name, or user ID from client identity headers.
pub(super) async fn issue_browser_app_identity(
    auth: &AuthStore,
    identity: &IdentityIssuer,
    workspace_id: &str,
    user_id: &str,
    service_id: &str,
) -> Result<String, ApiFailure> {
    let role = auth.workspace_member_role(workspace_id, user_id).await?;
    let human = auth
        .list_workspace_humans(workspace_id)
        .await?
        .into_iter()
        .find(|human| human.user_id == user_id)
        .ok_or_else(|| {
            ApiFailure::unauthorized(
                "app_authentication_required",
                "current workspace membership required",
            )
        })?;
    identity
        .issue_human_request(
            workspace_id,
            user_id,
            &human.preferred_name,
            &role,
            service_id,
        )
        .map(|token| token.access_token)
        .map_err(|_| ApiFailure::internal("identity_token_failed", "failed to issue App identity"))
}

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
    Extension(policy): Extension<PolicyEngine>,
    Extension(identity): Extension<IdentityIssuer>,
    Path(service_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiFailure> {
    let claims = authenticate_workspace_app(&auth, &identity, &headers, &service_id).await?;
    let principals = visible_workspace_app_principals(&state, &auth, &policy, &claims).await?;
    Ok(Json(json!({
        "principals": principals,
        "capabilities": [treer_protocol::APP_DIRECTORY_PATH_CAPABILITY],
    })))
}

pub(super) async fn resolve_workspace_app_recipients(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(policy): Extension<PolicyEngine>,
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
    let principals = visible_workspace_app_principals(&state, &auth, &policy, &claims).await?;
    let sender = principals
        .iter()
        .find(|principal| principal.id == claims.sub && principal.kind == claims.principal_kind)
        .cloned()
        .unwrap_or(AppPrincipal {
            kind: claims.principal_kind,
            id: claims.sub.clone(),
            name: claims.name.clone(),
            path: None,
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
        let tree = auth.workload_tree(&claims.workspace_id).await?;
        let caller =
            (claims.principal_kind == AppPrincipalKind::Agent).then_some(claims.sub.as_str());
        let recipient = resolve_tree_principal(&principals, &tree, caller, target)?;
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
    let paths = auth.canonical_workload_paths(workspace_id).await?;
    Ok(snapshot
        .agents
        .into_iter()
        .map(|agent| {
            let canonical = paths.get(&agent.agent_id);
            AppPrincipal {
                kind: AppPrincipalKind::Agent,
                path: canonical.map(|(_, path)| path.clone()),
                id: agent.agent_id,
                name: canonical
                    .map(|(name, _)| name.clone())
                    .unwrap_or(agent.name),
                role: None,
            }
        })
        .chain(humans.into_iter().map(|human| AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: human.user_id,
            name: human.preferred_name,
            path: None,
            role: Some(human.role),
        }))
        .collect())
}

/// Do not let a service-audience directory bypass ordinary Core discovery.
/// The verified caller's own identity remains available without enumeration,
/// so Provider authentication does not depend on delegated human-list rules.
async fn visible_workspace_app_principals(
    state: &AppState,
    auth: &AuthStore,
    policy: &PolicyEngine,
    claims: &treer_protocol::AppIdentityClaims,
) -> Result<Vec<AppPrincipal>, ApiFailure> {
    let subject = match claims.principal_kind {
        AppPrincipalKind::Human => PolicySubject::Human {
            user_id: claims.sub.clone(),
        },
        AppPrincipalKind::Agent => PolicySubject::Agent {
            agent_id: claims.sub.clone(),
            server_id: claims.machine_id.clone().ok_or_else(|| {
                ApiFailure::unauthorized("app_authentication_required", "machine binding required")
            })?,
        },
    };
    let all = workspace_app_principals(state, auth, &claims.workspace_id).await?;
    let mut visible = Vec::new();
    for principal in all {
        if principal.kind == AppPrincipalKind::Human
            && claims.principal_kind == AppPrincipalKind::Human
            && principal.id == claims.sub
        {
            visible.push(principal);
            continue;
        }
        let checks = if principal.kind == AppPrincipalKind::Agent {
            vec![
                PolicyRequest::new(
                    &claims.workspace_id,
                    subject.clone(),
                    ACTION_AGENT_DISCOVER,
                    PolicyResource::new(RESOURCE_AGENT, &principal.id),
                ),
                PolicyRequest::new(
                    &claims.workspace_id,
                    subject.clone(),
                    ACTION_AGENT_METADATA_READ,
                    PolicyResource::new(RESOURCE_AGENT, &principal.id),
                ),
            ]
        } else {
            vec![PolicyRequest::new(
                &claims.workspace_id,
                subject.clone(),
                ACTION_HUMAN_LIST,
                PolicyResource::new(RESOURCE_HUMAN_DIRECTORY, &claims.workspace_id),
            )]
        };
        if policy.authorize_batch(&checks).await.is_ok() {
            visible.push(principal);
        }
    }
    Ok(visible)
}

#[cfg(test)]
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
                path: None,
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
                    path: None,
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

pub(super) fn resolve_tree_principal(
    principals: &[AppPrincipal],
    tree: &treer_protocol::WorkloadTree,
    caller: Option<&str>,
    target: &str,
) -> Result<AppPrincipal, ProtocolError> {
    if let Some(principal) = principals.iter().find(|p| p.id == target) {
        return Ok(principal.clone());
    }
    let rows = principals
        .iter()
        .filter(|p| p.kind == AppPrincipalKind::Agent)
        .map(|p| treer_protocol::WorkloadTarget {
            id: p.id.clone(),
            name: p.name.clone(),
        })
        .collect::<Vec<_>>();
    let agent = tree.resolve_target(&rows, caller, target);
    let humans = principals
        .iter()
        .filter(|p| p.kind == AppPrincipalKind::Human && p.name == target)
        .collect::<Vec<_>>();
    match (agent, humans.as_slice()) {
        (Ok(id), []) => principals
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| ProtocolError::new("recipient_not_found", "recipient is unavailable")),
        (Err(_), [human]) if !target.contains('/') && !matches!(target, "self" | ".") => {
            Ok((*human).clone())
        }
        (Err(error), []) => Err(error),
        _ => Err(ProtocolError::new(
            "recipient_ambiguous",
            "more than one recipient matches; use a path or stable ID",
        )),
    }
}

#[cfg(test)]
mod tree_recipient_tests {
    use super::*;
    use treer_protocol::{WorkloadFolder, WorkloadPlacement, WorkloadTree};
    #[test]
    fn message_targets_select_siblings_or_paths_without_global_fallback() {
        let tree = WorkloadTree {
            folders: vec![WorkloadFolder {
                folder_id: "team".into(),
                name: "team".into(),
                parent_id: None,
            }],
            placements: vec![
                WorkloadPlacement {
                    agent_id: "ag_caller".into(),
                    folder_id: Some("team".into()),
                    position: 0,
                },
                WorkloadPlacement {
                    agent_id: "ag_peer".into(),
                    folder_id: Some("team".into()),
                    position: 1,
                },
            ],
        };
        let principals = vec![
            AppPrincipal {
                kind: AppPrincipalKind::Agent,
                id: "ag_peer".into(),
                name: "reviewer".into(),
                path: None,
                role: None,
            },
            AppPrincipal {
                kind: AppPrincipalKind::Agent,
                id: "ag_root".into(),
                name: "reviewer".into(),
                path: None,
                role: None,
            },
            AppPrincipal {
                kind: AppPrincipalKind::Agent,
                id: "ag_other".into(),
                name: "other".into(),
                path: None,
                role: None,
            },
            AppPrincipal {
                kind: AppPrincipalKind::Human,
                id: "usr_owner".into(),
                name: "Owner".into(),
                path: None,
                role: Some("owner".into()),
            },
        ];
        for (target, expected) in [
            ("reviewer", "ag_peer"),
            ("./reviewer", "ag_peer"),
            ("../reviewer", "ag_root"),
            ("/reviewer", "ag_root"),
            ("/team/reviewer", "ag_peer"),
            ("ag_root", "ag_root"),
            ("Owner", "usr_owner"),
        ] {
            assert_eq!(
                resolve_tree_principal(&principals, &tree, Some("ag_caller"), target)
                    .unwrap()
                    .id,
                expected
            );
        }
        let error =
            resolve_tree_principal(&principals, &tree, Some("ag_caller"), "other").unwrap_err();
        assert_eq!(error.code, "agent_not_in_folder");
        assert_eq!(error.hint.unwrap(), ["/other"]);
    }
}
