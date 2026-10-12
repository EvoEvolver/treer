use super::*;

pub(in crate::api) async fn list_virtual_network_hosts(
    Extension(auth): Extension<AuthStore>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    Ok(Json(json!({
        "hosts": auth.list_virtual_network_hosts(&workspace_id).await?
    })))
}

pub(in crate::api) async fn create_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateVirtualNetworkHostRequest>,
) -> Result<Json<Value>, ApiFailure> {
    let _ = (&state, &auth, &session, &workspace_id, &request);
    Err(ApiFailure::bad_request(
        "virtual_hosts_retired",
        "register services by stable ID; platform assigns public domains",
    ))
}

pub(in crate::api) async fn delete_virtual_network_host(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, hostname)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    auth.delete_virtual_network_host(&workspace_id, &hostname)
        .await?;
    publish_virtual_network_hosts(&state, &auth, &workspace_id).await?;
    Ok(Json(json!({ "deleted": true })))
}

pub(in crate::api) async fn list_service_ingresses(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Path(workspace_id): Path<String>,
) -> Result<Json<Value>, ApiFailure> {
    let ingresses = auth
        .list_service_ingresses(&workspace_id)
        .await?
        .into_iter()
        .map(|ingress| service_ingress_json(&config, ingress))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({ "ingresses": ingresses })))
}

pub(in crate::api) async fn create_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(provider_store): Extension<crate::policy_provider_store::PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    Path(workspace_id): Path<String>,
    Json(request): Json<CreateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if request.access == ServiceIngressAccess::Public
        && provider_store
            .get(&workspace_id)
            .await
            .map_err(|error| {
                ApiFailure::internal("policy_provider_store_failed", &error.to_string())
            })?
            .is_some_and(|provider| provider.service_id == request.service_id)
    {
        return Err(ApiFailure::bad_request(
            "policy_provider_must_be_private",
            "the active Policy Provider service cannot have public ingress",
        ));
    }
    let ingress = auth
        .create_service_ingress(
            &workspace_id,
            &session.user_id,
            config.base_domain()?,
            request,
        )
        .await?;
    Ok(Json(json!({
        "ingress": service_ingress_json(&config, ingress)?
    })))
}

pub(in crate::api) async fn update_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Extension(config): Extension<IngressConfig>,
    Extension(provider_store): Extension<crate::policy_provider_store::PolicyProviderStore>,
    Extension(session): Extension<CurrentSession>,
    Path((workspace_id, ingress_id)): Path<(String, String)>,
    Json(request): Json<UpdateServiceIngressRequest>,
) -> Result<Json<Value>, ApiFailure> {
    if request.access == Some(ServiceIngressAccess::Public) {
        let current = auth
            .resolve_service_ingress(&workspace_id, &ingress_id)
            .await?;
        if provider_store
            .get(&workspace_id)
            .await
            .map_err(|error| {
                ApiFailure::internal("policy_provider_store_failed", &error.to_string())
            })?
            .is_some_and(|provider| provider.service_id == current.ingress.service_id)
        {
            return Err(ApiFailure::bad_request(
                "policy_provider_must_be_private",
                "the active Policy Provider service cannot have public ingress",
            ));
        }
    }
    let ingress = auth
        .update_service_ingress(&workspace_id, &ingress_id, &session.user_id, request)
        .await?;
    Ok(Json(json!({
        "ingress": service_ingress_json(&config, ingress)?
    })))
}

pub(in crate::api) async fn delete_service_ingress(
    Extension(auth): Extension<AuthStore>,
    Path((workspace_id, ingress_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiFailure> {
    let ingress = auth
        .delete_service_ingress(&workspace_id, &ingress_id)
        .await?;
    Ok(Json(json!({
        "deleted": true,
        "ingress_id": ingress.ingress_id,
        "hostname": ingress.hostname,
    })))
}

pub(in crate::api) fn service_ingress_json(
    config: &IngressConfig,
    ingress: ServiceIngress,
) -> Result<Value, ApiFailure> {
    let public_url = config.url_for_hostname(&ingress.hostname)?.to_string();
    let mut value = serde_json::to_value(ingress)
        .map_err(|error| ApiFailure::internal("serialization_error", &error.to_string()))?;
    value
        .as_object_mut()
        .expect("service ingress serializes as an object")
        .insert("url".to_string(), Value::String(public_url));
    Ok(value)
}
