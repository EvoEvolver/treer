use super::*;

#[allow(clippy::too_many_arguments)]
pub fn router(
    state: AppState,
    bootstrap: BootstrapConfig,
    auth_store: AuthStore,
    policy: PolicyEngine,
    provider_store: crate::policy_provider_store::PolicyProviderStore,
    identity: IdentityIssuer,
    browser: BrowserAccess,
    ingress: IngressConfig,
    messages: MessageStore,
    rollout: CapabilityRollout,
    updater: UpdaterClient,
    voice: VoiceServices,
) -> Router {
    let cors = browser.cors_layer();
    let workload_identity = WorkloadIdentityApi {
        auth: auth_store.clone(),
        policy: policy.clone(),
        issuer: identity.clone(),
    };
    let service_ingress = ServiceIngressApi {
        auth: auth_store.clone(),
        policy: policy.clone(),
        config: ingress.clone(),
    };
    let agent_control = Router::new()
        .route("/agent/machine/identity", post(bind_machine_identity))
        .route(
            "/agent/workspaces/{workspace_id}/snapshot",
            get(workspace_snapshot),
        )
        .route(
            "/agent/workspaces/{workspace_id}/identity/token",
            post(agent_issue_identity_token),
        )
        .route(
            "/agent/workspaces/{workspace_id}/humans",
            get(agent_list_humans),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps",
            get(list_app_deployments).post(create_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}",
            get(get_app_deployment).delete(delete_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}/start",
            post(start_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}/stop",
            post(stop_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/apps/{app_id}/restart",
            post(restart_app_deployment),
        )
        .route(
            "/agent/workspaces/{workspace_id}/policy-provider/invalidate",
            post(invalidate_policy_provider),
        )
        .route(
            "/agent/workspaces/{workspace_id}/launch-profiles",
            get(list_agent_launch_profiles).post(create_agent_launch_profile),
        )
        .route(
            "/agent/workspaces/{workspace_id}/launch-profiles/{profile_id}",
            get(get_agent_launch_profile)
                .patch(update_agent_launch_profile)
                .delete(delete_agent_launch_profile),
        )
        .route(
            "/agent/workspaces/{workspace_id}/launch-profiles/{profile_id}/launch",
            post(launch_agent_profile),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}",
            get(get_agent).patch(rename_agent).delete(delete_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/startup",
            get(get_agent_startup)
                .put(set_agent_startup)
                .delete(clear_agent_startup),
        )
        .route(
            "/agent/workspaces/{workspace_id}/machines/{server_id}/startup/validate",
            post(validate_agent_startup),
        )
        .route(
            "/agent/workspaces/{workspace_id}/servers/{server_id}",
            axum::routing::patch(rename_server).delete(delete_server),
        )
        .route(
            "/agent/workspaces/{workspace_id}/machines/{server_id}/exec",
            post(exec_machine),
        )
        .route(
            "/agent/workspaces/{workspace_id}/machines/{server_id}/files",
            post(upload_machine_file).layer(DefaultBodyLimit::max(MAX_MACHINE_UPLOAD_BODY_BYTES)),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services",
            get(agent_list_machine_services).post(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services/{service_id}",
            axum::routing::patch(agent_network_publication_forbidden)
                .delete(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/services/{service_id}/probe",
            post(agent_probe_machine_service),
        )
        .route(
            "/agent/workspaces/{workspace_id}/virtual-hosts",
            get(agent_list_virtual_network_hosts).post(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/virtual-hosts/{hostname}",
            axum::routing::delete(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/ingresses",
            get(agent_list_service_ingresses).post(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/ingresses/{ingress_id}",
            axum::routing::patch(agent_network_publication_forbidden)
                .delete(agent_network_publication_forbidden),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/prompt",
            post(prompt_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/input",
            post(input_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/output",
            get(read_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/transcript",
            get(read_agent_transcript),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/stop",
            post(stop_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/abort",
            post(abort_agent),
        )
        .route(
            "/agent/workspaces/{workspace_id}/agents/{agent_id}/terminal",
            get(agent_terminal),
        );
    let agent_control = if rollout.core_messages {
        agent_control
            .route(
                "/agent/workspaces/{workspace_id}/messages",
                get(list_core_messages).post(send_core_message),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/receive",
                post(receive_core_messages),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/ack",
                post(acknowledge_core_messages),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/import",
                post(import_core_messages),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/{message_id}",
                get(get_core_message),
            )
    } else {
        agent_control
            .route(
                "/agent/workspaces/{workspace_id}/messages",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/receive",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/ack",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/import",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/agent/workspaces/{workspace_id}/messages/{message_id}",
                any(core_messages_rollout_disabled),
            )
    };
    let agent_control = agent_control.route_layer(middleware::from_fn_with_state(
        auth_store.clone(),
        auth::require_machine,
    ));
    let app_messages = if rollout.core_messages {
        Router::new()
            .route(
                "/api/apps/{service_id}/messages",
                get(list_app_messages).post(send_app_message),
            )
            .route(
                "/api/apps/{service_id}/messages/receive",
                post(receive_app_messages),
            )
            .route(
                "/api/apps/{service_id}/messages/ack",
                post(acknowledge_app_messages),
            )
            .route(
                "/api/apps/{service_id}/messages/{message_id}",
                get(get_app_message),
            )
    } else {
        Router::new()
            .route(
                "/api/apps/{service_id}/messages",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/api/apps/{service_id}/messages/receive",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/api/apps/{service_id}/messages/ack",
                any(core_messages_rollout_disabled),
            )
            .route(
                "/api/apps/{service_id}/messages/{message_id}",
                any(core_messages_rollout_disabled),
            )
    };
    let authenticated = Router::new()
        .route(
            "/api/organizations",
            get(auth::organizations).post(auth::create_organization_handler),
        )
        .route(
            "/api/organizations/{organization_id}",
            axum::routing::patch(auth::rename_organization_handler),
        )
        .route(
            "/api/organizations/{organization_id}/members",
            get(auth::members),
        )
        .route(
            "/api/organizations/{organization_id}/groups",
            get(auth::organization_groups).post(auth::create_organization_group_handler),
        )
        .route(
            "/api/organizations/{organization_id}/groups/{group_id}",
            axum::routing::delete(auth::delete_organization_group_handler),
        )
        .route(
            "/api/organizations/{organization_id}/groups/{group_id}/members/{user_id}",
            axum::routing::put(auth::add_organization_group_member_handler)
                .delete(auth::remove_organization_group_member_handler),
        )
        .route(
            "/api/organizations/{organization_id}/audit-events",
            get(auth::audit_events),
        )
        .route(
            "/api/organizations/{organization_id}/members/{user_id}",
            axum::routing::patch(auth::update_member_role_handler)
                .delete(auth::remove_member_handler),
        )
        .route(
            "/api/organizations/{organization_id}/invitations",
            post(auth::create_invitation),
        )
        .route(
            "/api/workspaces/{workspace_id}/bootstrap",
            post(bootstrap_info),
        )
        .route(
            "/api/workspaces",
            get(list_workspaces).post(create_workspace),
        )
        .route(
            "/api/workspaces/{workspace_id}",
            axum::routing::patch(rename_workspace).delete(delete_workspace),
        )
        .route(
            "/api/workspaces/{workspace_id}/access",
            get(auth::workspace_access).patch(auth::update_workspace_access),
        )
        .route(
            "/api/workspaces/{workspace_id}/access/users/{user_id}",
            axum::routing::put(auth::update_workspace_user_grant)
                .delete(auth::delete_workspace_user_grant),
        )
        .route(
            "/api/workspaces/{workspace_id}/access/groups/{group_id}",
            axum::routing::put(auth::update_workspace_group_grant)
                .delete(auth::delete_workspace_group_grant),
        )
        .route(
            "/api/workspaces/{workspace_id}/snapshot",
            get(workspace_snapshot),
        )
        .route("/api/workspaces/{workspace_id}/servers", get(list_servers))
        .route(
            "/api/workspaces/{workspace_id}/apps",
            get(list_app_deployments).post(create_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}",
            get(get_app_deployment).delete(delete_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/access",
            axum::routing::patch(update_app_access),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/start",
            post(start_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/stop",
            post(stop_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/apps/{app_id}/restart",
            post(restart_app_deployment),
        )
        .route(
            "/api/workspaces/{workspace_id}/policy-provider",
            get(get_policy_provider)
                .put(set_policy_provider)
                .delete(clear_policy_provider),
        )
        .route(
            "/api/workspaces/{workspace_id}/policy-provider/default-app",
            post(install_default_policy_app),
        )
        .route(
            "/api/workspaces/{workspace_id}/services",
            get(list_machine_services).post(create_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/services/{service_id}",
            axum::routing::patch(update_machine_service).delete(delete_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/services/{service_id}/probe",
            post(probe_machine_service),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts",
            get(list_virtual_network_hosts).post(create_virtual_network_host),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}",
            axum::routing::delete(delete_virtual_network_host),
        )
        .route(
            "/api/workspaces/{workspace_id}/ingresses",
            get(list_service_ingresses).post(create_service_ingress),
        )
        .route(
            "/api/workspaces/{workspace_id}/ingresses/{ingress_id}",
            axum::routing::patch(update_service_ingress).delete(delete_service_ingress),
        )
        .route(
            "/api/workspaces/{workspace_id}/traffic",
            get(list_machine_traffic),
        )
        .route(
            "/api/workspaces/{workspace_id}/traffic/agents",
            get(list_agent_traffic),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy",
            any(proxy_virtual_network_host_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/",
            any(proxy_virtual_network_host_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/virtual-hosts/{hostname}/proxy/{*path}",
            any(proxy_virtual_network_host_path),
        )
        .route(
            "/api/workspaces/{workspace_id}/servers/{server_id}",
            axum::routing::patch(rename_server).delete(delete_server),
        )
        .route(
            "/api/workspaces/{workspace_id}/machines/{server_id}/exec",
            post(exec_machine),
        )
        .route(
            "/api/workspaces/{workspace_id}/machines/{server_id}/files",
            post(upload_machine_file).layer(DefaultBodyLimit::max(MAX_MACHINE_UPLOAD_BODY_BYTES)),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents",
            get(list_agents).post(create_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/launch-profiles",
            get(list_agent_launch_profiles).post(create_agent_launch_profile),
        )
        .route(
            "/api/workspaces/{workspace_id}/launch-profiles/{profile_id}",
            get(get_agent_launch_profile)
                .patch(update_agent_launch_profile)
                .delete(delete_agent_launch_profile),
        )
        .route(
            "/api/workspaces/{workspace_id}/launch-profiles/{profile_id}/launch",
            post(launch_agent_profile),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}",
            get(get_agent).patch(rename_agent).delete(delete_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui",
            any(proxy_agent_interface_ui_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/",
            any(proxy_agent_interface_ui_root),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/interface/ui/{*path}",
            any(proxy_agent_interface_ui_path),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/prompt",
            post(prompt_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/input",
            post(input_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/output",
            get(read_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/transcript",
            get(read_agent_transcript),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/stop",
            post(stop_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/abort",
            post(abort_agent),
        )
        .route(
            "/api/workspaces/{workspace_id}/agents/{agent_id}/terminal",
            get(agent_terminal),
        )
        .route(
            "/api/workspaces/{workspace_id}/events",
            get(workspace_events),
        )
        .route(
            "/api/workspaces/{workspace_id}/voice/asr",
            get(voice_asr_status),
        )
        .route(
            "/api/workspaces/{workspace_id}/voice/asr/stream",
            get(voice_asr_stream),
        )
        .route(
            "/api/workspaces/{workspace_id}/voice/command",
            get(voice_command_status).post(voice_command),
        )
        .route("/api/auth/me", get(auth::me))
        .route(
            "/api/auth/profile",
            axum::routing::patch(auth::update_profile),
        )
        .route("/api/auth/logout", post(auth::logout))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_workspace_access,
        ))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_user,
        ));
    let admin = admin::routes()
        .route("/api/admin/me", get(auth::admin_me))
        .route("/api/admin/logout", post(auth::admin_logout))
        .route(
            "/api/admin/update",
            get(crate::updater::status).post(crate::updater::apply),
        )
        .route("/api/admin/update/check", get(crate::updater::check))
        .route_layer(middleware::from_fn_with_state(
            auth_store.clone(),
            auth::require_admin,
        ));
    let control = Router::new()
        .route("/install.sh", get(install_script))
        .route("/api/machines/enroll", post(enroll_machine))
        .route("/artifacts/{platform}/{binary}", get(download_artifact))
        .route("/api/health", get(health))
        .route("/.well-known/jwks.json", get(workload_identity_jwks))
        .route("/.treer/identity/verify", post(verify_workload_identity))
        .route("/.treer/apps/identity/verify", post(verify_app_identity))
        .route("/api/apps/oauth/authorize", get(authorize_workspace_app))
        .route("/api/apps/oauth/token", post(exchange_workspace_app_code))
        .route(
            "/api/apps/{service_id}/directory",
            get(workspace_app_directory),
        )
        .route(
            "/api/apps/{service_id}/recipients/resolve",
            post(resolve_workspace_app_recipients),
        )
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/config", get(auth::oauth_config))
        .route("/api/auth/oauth/{provider}/start", get(auth::oauth_start))
        .route(
            "/api/auth/oauth/{provider}/callback",
            get(auth::oauth_callback),
        )
        .route(
            "/api/auth/request-password-reset",
            post(auth::request_password_reset),
        )
        .route("/api/auth/reset-password", post(auth::reset_password))
        .route("/api/auth/register", post(auth::register))
        .route("/api/admin/login", post(auth::admin_login))
        .route("/agent/connect", get(agent_socket::upgrade))
        .merge(agent_control)
        .merge(app_messages)
        .merge(authenticated)
        .merge(admin)
        .layer(cors);
    Router::new()
        .merge(control)
        .route("/.treer/ingress/authorize", get(authorize_service_ingress))
        .fallback(any(proxy_service_ingress))
        .layer(Extension(bootstrap))
        .layer(Extension(policy))
        .layer(Extension(identity))
        .layer(Extension(workload_identity))
        .layer(Extension(provider_store))
        .layer(Extension(service_ingress))
        .layer(Extension(auth_store))
        .layer(Extension(browser))
        .layer(Extension(ingress))
        .layer(Extension(messages))
        .layer(Extension(updater))
        .layer(Extension(voice.asr))
        .layer(Extension(voice.llm))
        .with_state(state)
}
