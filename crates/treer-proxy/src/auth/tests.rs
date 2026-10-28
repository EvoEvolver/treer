use super::*;
use axum::extract::Form;
use axum::routing::{get, post};
use axum::Router;
use treer_protocol::{AgentInfo, AgentStatus, CreateVirtualNetworkHostRequest, ServerStatus};

fn bearer(token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    headers
}

async fn machine_ok(store: &AuthStore, token: &str) -> bool {
    store.authenticate_machine(&bearer(token)).await.is_ok()
}

async fn enroll(
    store: &AuthStore,
    workspace: &str,
    installation: Option<&str>,
    prior_server: Option<&str>,
) -> Result<MachineEnrollmentClaim, AuthFailure> {
    let key = store
        .create_machine_enrollment(workspace, "admin")
        .await
        .unwrap();
    store
        .claim_machine_enrollment_for_installation(
            &key,
            installation,
            Some("Builder"),
            prior_server,
        )
        .await
}

async fn machine_row(store: &AuthStore, server_id: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT installation_id, revoked_at FROM machines WHERE server_id = $1")
        .bind(server_id)
        .fetch_one(&store.pool)
        .await
        .unwrap()
}

fn actor(id: &str) -> ProfileMutationActor<'_> {
    ProfileMutationActor {
        kind: "user",
        id: Some(id),
        label: id,
    }
}

fn profile_request(name: &str, args: &[&str]) -> CreateAgentLaunchProfileRequest {
    CreateAgentLaunchProfileRequest {
        name: name.to_string(),
        description: String::new(),
        cwd: ".".to_string(),
        command: "codex".to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
}

async fn bootstrap_owner(store: &AuthStore, email: &str, name: &str) -> CurrentSession {
    let (invite, _) = store.create_personal_invitation().await.unwrap();
    store
        .register(Some(&invite), email, name, "password123")
        .await
        .unwrap()
}

/// Registers `email` from an invitation `inviter` creates and accepts it.
async fn join_organization(
    store: &AuthStore,
    organization_id: &str,
    inviter: &str,
    email: &str,
) -> CurrentSession {
    let (invite, _) = store
        .create_invitation(organization_id, inviter)
        .await
        .unwrap();
    let user = store
        .register_with_client(Some(&invite), email, "Member", "password123", None, true)
        .await
        .unwrap();
    store
        .accept_invitation(&invite, &user.user_id)
        .await
        .unwrap();
    user
}

/// An owner with a fresh organization `org` holding workspace `workspace`.
async fn organization_fixture(store: &AuthStore, workspace: &str) -> (CurrentSession, String) {
    let owner = bootstrap_owner(store, "owner@example.com", "Owner").await;
    let organization = store
        .create_organization(&owner.user_id, "Engineering")
        .await
        .unwrap();
    store
        .create_workspace(
            &organization.organization_id,
            workspace,
            workspace,
            &owner.user_id,
        )
        .await
        .unwrap();
    (owner, organization.organization_id)
}

async fn credential_hash(store: &AuthStore, agent: &str) -> (String, Option<String>) {
    sqlx::query_as("SELECT secret_hash, revoked_at FROM agent_credentials WHERE agent_id = $1")
        .bind(agent)
        .fetch_one(&store.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn lifecycle_credential_rotation_is_scope_bound_and_never_revives_revoked_identity() {
    let store = AuthStore::for_test("password").await;
    store.seed_test_workspace("rotation").await;
    let first = store
        .create_agent_credential("rotation", "machine-a", "agent-a")
        .await
        .unwrap();
    let (hash, _) = credential_hash(&store, "agent-a").await;
    for (workspace, machine, agent) in [
        ("other", "machine-a", "agent-a"),
        ("rotation", "machine-b", "agent-a"),
        ("rotation", "machine-a", "invented"),
    ] {
        assert!(store
            .rotate_agent_credential(workspace, machine, agent)
            .await
            .is_err());
    }
    assert_eq!(credential_hash(&store, "agent-a").await.0, hash);
    let second = store
        .rotate_agent_credential("rotation", "machine-a", "agent-a")
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        credential_hash(&store, "agent-a").await.0,
        fast_secret_hash(&second)
    );
    store.delete_agent("rotation", "agent-a").await.unwrap();
    assert!(store
        .rotate_agent_credential("rotation", "machine-a", "agent-a")
        .await
        .is_err());
    let (after, revoked) = credential_hash(&store, "agent-a").await;
    assert_eq!(after, fast_secret_hash(&second));
    assert!(revoked.is_some());
}

#[test]
fn platform_service_hostnames_are_stable_and_dns_safe() {
    assert_eq!(
        platform_service_hostname("svc_abcdef1234567890", "apps.treer.test").unwrap(),
        "svc-abcdef1234567890.apps.treer.test"
    );
    assert!(platform_service_hostname("svc_", "apps.treer.test").is_err());
    assert!(platform_service_hostname("svc_invalid.name", "apps.treer.test").is_err());
}

#[tokio::test]
async fn app_oauth_codes_require_pkce_and_are_single_use() {
    let store = AuthStore::for_test("admin-password").await;
    store.seed_test_workspace("app-oauth").await;
    let owner = bootstrap_owner(&store, "ada@example.com", "Ada").await;
    let now = Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
         VALUES('org_app-oauth', $1, 'owner', $2)",
    )
    .bind(&owner.user_id)
    .bind(&now)
    .execute(&store.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO machine_services(service_id, workspace_id, server_id, name, target_host, \
         target_port, protocol, created_at, created_by, updated_at, updated_by) \
         VALUES('service-mail', 'app-oauth', 'machine-a', 'Mail', '127.0.0.1', 8788, 'http', $1, 'user-a', $1, 'user-a')",
    )
    .bind(&now)
    .execute(&store.pool)
    .await
    .unwrap();
    let verifier = "v".repeat(64);
    let callback = "https://mail.example/api/auth/callback";
    let code = store
        .create_app_oauth_code(
            &AppOAuthGrant {
                workspace_id: "app-oauth".to_string(),
                service_id: "service-mail".to_string(),
                user_id: owner.user_id.clone(),
                preferred_name: "Ada".to_string(),
                role: "owner".to_string(),
            },
            callback,
            &pkce_challenge(&verifier),
        )
        .await
        .unwrap();
    let wrong_verifier = "x".repeat(64);
    for (service, redirect, proof) in [
        ("other-service", callback, verifier.as_str()),
        (
            "service-mail",
            "https://evil.example/api/auth/callback",
            &verifier,
        ),
        ("service-mail", callback, &wrong_verifier),
    ] {
        assert!(store
            .consume_app_oauth_code(&code, service, redirect, proof)
            .await
            .is_err());
    }
    let grant = store
        .consume_app_oauth_code(&code, "service-mail", callback, &verifier)
        .await
        .unwrap();
    assert_eq!(grant.user_id, owner.user_id);
    assert!(store
        .consume_app_oauth_code(&code, "service-mail", callback, &verifier)
        .await
        .is_err());
}

#[tokio::test]
async fn agent_launch_profiles_are_workspace_scoped_unique_and_validated() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("profiles").await;
    store.seed_test_workspace("other-workspace").await;
    let created = store
        .create_agent_launch_profile(
            "profiles",
            actor("user-a"),
            profile_request("Reviewer", &[]),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .resolve_agent_launch_profile("profiles", "reviewer")
            .await
            .unwrap(),
        created
    );
    let code = |failure: AuthFailure| failure.error.code;
    assert_eq!(
        code(
            store
                .resolve_agent_launch_profile("other-workspace", &created.profile_id)
                .await
                .unwrap_err()
        ),
        "launch_profile_not_found"
    );
    assert_eq!(
        code(
            store
                .create_agent_launch_profile(
                    "profiles",
                    actor("user-a"),
                    profile_request("REVIEWER", &[])
                )
                .await
                .unwrap_err()
        ),
        "launch_profile_exists"
    );
    let invalid = store
        .update_agent_launch_profile(
            "profiles",
            &created.profile_id,
            actor("user-a"),
            UpdateAgentLaunchProfileRequest {
                args: Some(vec!["bad\0argument".to_string()]),
                ..UpdateAgentLaunchProfileRequest::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(invalid), "invalid_launch_profile");
    let updated = store
        .update_agent_launch_profile(
            "profiles",
            &created.profile_id,
            actor("user-b"),
            UpdateAgentLaunchProfileRequest {
                name: Some("Code reviewer".to_string()),
                ..UpdateAgentLaunchProfileRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (updated.name.as_str(), updated.updated_by.as_str()),
        ("Code reviewer", "user-b")
    );
    store
        .delete_agent_launch_profile("profiles", &created.profile_id, actor("user-a"))
        .await
        .unwrap();
    assert!(store
        .list_agent_launch_profiles("profiles")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn network_schema_upgrades_legacy_service_and_traffic_constraints() {
    let store = AuthStore::for_test("owner-password").await;
    sqlx::raw_sql("ALTER TABLE machine_services DROP CONSTRAINT machine_services_protocol_check;
            ALTER TABLE machine_services ADD CONSTRAINT machine_services_protocol_check CHECK(protocol IN ('tcp','http'));
            ALTER TABLE traffic_usage_hourly DROP CONSTRAINT traffic_usage_hourly_traffic_class_check;
            ALTER TABLE traffic_usage_hourly ADD CONSTRAINT traffic_usage_hourly_traffic_class_check CHECK(traffic_class IN ('virtual_network','service_ingress','virtual_host','agent_interface'));
            ALTER TABLE traffic_usage_hourly DROP CONSTRAINT traffic_usage_hourly_source_type_check;
            ALTER TABLE traffic_usage_hourly ADD CONSTRAINT traffic_usage_hourly_source_type_check CHECK(source_type IN ('client','machine'));
            ALTER TABLE traffic_usage_hourly DROP CONSTRAINT traffic_usage_hourly_destination_type_check;
            ALTER TABLE traffic_usage_hourly ADD CONSTRAINT traffic_usage_hourly_destination_type_check CHECK(destination_type IN ('client','machine'));")
            .execute(&store.pool).await.unwrap();
    store.initialize_schema().await.unwrap();
    store.initialize_schema().await.unwrap();
    for (table, constraint, value) in [
        ("machine_services", "machine_services_protocol_check", "udp"),
        (
            "traffic_usage_hourly",
            "traffic_usage_hourly_traffic_class_check",
            "direct_network",
        ),
        (
            "traffic_usage_hourly",
            "traffic_usage_hourly_source_type_check",
            "internet",
        ),
        (
            "traffic_usage_hourly",
            "traffic_usage_hourly_destination_type_check",
            "internet",
        ),
    ] {
        let definition: String = sqlx::query_scalar("SELECT pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid=$1::regclass AND conname=$2")
                .bind(table).bind(constraint).fetch_one(&store.pool).await.unwrap();
        assert!(definition.contains(value), "{constraint}: {definition}");
    }
}

#[tokio::test]
async fn invitation_registration_and_login_round_trip() {
    let store = AuthStore::for_test("owner-password").await;
    let admin = store.admin_login("owner-password").await.unwrap();
    assert!(store.admin_session(&admin.token).await.unwrap().is_some());
    let (invite, url) = store.create_personal_invitation().await.unwrap();
    assert!(url
        .as_str()
        .starts_with(&format!("https://app.treer.example/?invite={invite}")));
    let registered = store
        .register(Some(&invite), "Alice@Example.com", "Alice", "password123")
        .await
        .unwrap();
    assert_eq!(registered.email, "alice@example.com");
    let organizations = store.list_organizations(&registered.user_id).await.unwrap();
    assert_eq!(organizations.len(), 1);
    assert_eq!(organizations[0].role, "owner");
    assert!(store
        .register(Some(&invite), "bob@example.com", "Bob", "password123")
        .await
        .is_err());
    let login = store
        .login("ALICE@EXAMPLE.COM", "password123")
        .await
        .unwrap();
    store
        .update_profile(&login.user_id, "alicia@example.com", "Alicia")
        .await
        .unwrap();
    assert!(store
        .login("alice@example.com", "password123")
        .await
        .is_err());
    assert!(store
        .login("alicia@example.com", "password123")
        .await
        .is_ok());
}

async fn oauth_token(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
    assert_eq!(
        form.get("client_secret").map(String::as_str),
        Some("client-secret")
    );
    assert_eq!(form.get("code").map(String::as_str), Some("oauth-code"));
    Json(json!({ "access_token": "provider-access-token" }))
}

fn assert_oauth_bearer(headers: &HeaderMap) {
    assert_eq!(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
        Some("Bearer provider-access-token")
    );
}

#[tokio::test]
async fn oauth_provider_exchange_uses_verified_provider_profiles() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/token", post(oauth_token))
        .route(
            "/github/user",
            get(|headers: HeaderMap| async move {
                assert_oauth_bearer(&headers);
                Json(json!({ "id": 12345, "login": "octocat", "name": "Octo Cat" }))
            }),
        )
        .route(
            "/github/emails",
            get(|headers: HeaderMap| async move {
                assert_oauth_bearer(&headers);
                Json(json!([
                    { "email": "unverified@example.com", "primary": true, "verified": false },
                    { "email": "octo@example.com", "primary": false, "verified": true }
                ]))
            }),
        )
        .route(
            "/google/user",
            get(|headers: HeaderMap| async move {
                assert_oauth_bearer(&headers);
                Json(json!({
                    "sub": "google-subject", "email": "google@example.com",
                    "email_verified": true, "name": "Google User"
                }))
            }),
        );
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let provider = |user: &str, emails: Option<&str>| {
        OAuthProviderConfig::new(
            "client-id".to_string(),
            "client-secret".to_string(),
            &format!("{base}/authorize"),
            &format!("{base}/token"),
            &format!("{base}/{user}"),
            emails.map(|path| format!("{base}/{path}")).as_deref(),
        )
        .unwrap()
    };
    let mut store = AuthStore::for_test("owner-password").await;
    store.oauth = Arc::new(OAuthConfig::new(
        Some(provider("github/user", Some("github/emails"))),
        Some(provider("google/user", None)),
        true,
    ));
    let github = store
        .exchange_oauth_code("github", "oauth-code")
        .await
        .unwrap();
    assert_eq!(
        (github.subject.as_str(), github.email.as_str()),
        ("12345", "octo@example.com")
    );
    let google = store
        .exchange_oauth_code("google", "oauth-code")
        .await
        .unwrap();
    assert_eq!(
        (google.subject.as_str(), google.email.as_str()),
        ("google-subject", "google@example.com")
    );
    server.abort();
}

#[tokio::test]
async fn oauth_state_is_provider_scoped_and_single_use() {
    let github =
        OAuthProviderConfig::github("client-id".to_string(), "client-secret".to_string()).unwrap();
    let mut store = AuthStore::for_test("owner-password").await;
    store.oauth = Arc::new(OAuthConfig::new(Some(github), None, true));
    let (invite, _) = store.create_personal_invitation().await.unwrap();
    let authorization = store
        .oauth_authorization_url("github", Some(&invite), false, "browser-fixture")
        .await
        .unwrap();
    assert!(authorization.query_pairs().any(|(key, value)| {
        key == "redirect_uri"
            && value == "https://proxy.treer.example/api/auth/oauth/github/callback"
    }));
    let state = authorization
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .unwrap();
    for (provider, browser) in [("google", "browser-fixture"), ("github", "wrong-browser")] {
        assert!(store
            .consume_oauth_state(provider, &state, browser)
            .await
            .is_err());
    }
    let consumed = store
        .consume_oauth_state("github", &state, "browser-fixture")
        .await
        .unwrap();
    assert_eq!(consumed.invite.as_deref(), Some(invite.as_str()));
    assert!(store
        .consume_oauth_state("github", &state, "browser-fixture")
        .await
        .is_err());
}

fn oauth_profile(provider: &'static str, subject: &str, email: &str) -> OAuthProfile {
    OAuthProfile {
        provider,
        subject: subject.to_string(),
        email: email.to_string(),
        preferred_name: "Provider Name".to_string(),
    }
}

#[tokio::test]
async fn oauth_merges_verified_email_and_keeps_stable_provider_identity() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let merged = store
        .complete_oauth_login(
            oauth_profile("github", "github-123", "OWNER@example.com"),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(merged.user_id, owner.user_id);
    assert!(store.session(&owner.token).await.unwrap().is_none());
    assert!(store
        .login("owner@example.com", "password123")
        .await
        .is_err());
    let stable = store
        .complete_oauth_login(
            oauth_profile("github", "github-123", "changed@example.com"),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(stable.user_id, owner.user_id);
    assert_eq!(stable.email, "owner@example.com");
}

#[tokio::test]
async fn invitation_switch_controls_new_password_and_oauth_accounts() {
    let mut store = AuthStore::for_test("owner-password").await;
    let profile = || oauth_profile("google", "new-google-user", "new@example.com");
    assert!(store
        .complete_oauth_login(profile(), None, false)
        .await
        .is_err());
    assert!(store
        .register(None, "password@example.com", "Password User", "password123")
        .await
        .is_err());
    store.oauth = Arc::new(OAuthConfig::new(None, None, false));
    let oauth_user = store
        .complete_oauth_login(profile(), None, false)
        .await
        .unwrap();
    let password_user = store
        .register(None, "password@example.com", "Password User", "password123")
        .await
        .unwrap();
    for user in [oauth_user, password_user] {
        assert_eq!(
            store.list_organizations(&user.user_id).await.unwrap().len(),
            1
        );
    }
}

#[tokio::test]
async fn password_reset_is_single_use_rate_limited_and_revokes_sessions() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let reset_token = |url: &Url| {
        url.query_pairs()
            .find_map(|(key, value)| (key == "reset").then(|| value.into_owned()))
            .unwrap()
    };
    let pending = store
        .create_password_reset("OWNER@example.com")
        .await
        .unwrap()
        .expect("known user reset");
    assert!(store
        .create_password_reset("owner@example.com")
        .await
        .unwrap()
        .is_none());
    assert!(store
        .create_password_reset("missing@example.com")
        .await
        .unwrap()
        .is_none());
    let token = reset_token(&pending.url);
    let stored_hash: String =
        sqlx::query_scalar("SELECT secret_hash FROM password_reset_tokens WHERE token_id = $1")
            .bind(&pending.token_id)
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert!(!stored_hash.contains(&token));
    store
        .reset_password(&token, "new-password-123")
        .await
        .unwrap();
    assert!(store.session(&owner.token).await.unwrap().is_none());
    assert!(store
        .login("owner@example.com", "password123")
        .await
        .is_err());
    assert!(store
        .login("owner@example.com", "new-password-123")
        .await
        .is_ok());
    assert!(store
        .reset_password(&token, "another-password")
        .await
        .is_err());
    // A reset issued before an email change no longer applies.
    let pending = store
        .create_password_reset("owner@example.com")
        .await
        .unwrap()
        .unwrap();
    store
        .update_profile(&owner.user_id, "renamed@example.com", "Owner")
        .await
        .unwrap();
    assert!(store
        .reset_password(&reset_token(&pending.url), "newer-password")
        .await
        .is_err());
}

#[tokio::test]
async fn virtual_network_hosts_are_normalized_resolved_and_cleaned_up() {
    let mut store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("default").await;
    let initial = store
        .virtual_network_hosts_snapshot("default")
        .await
        .expect("initial virtual-host snapshot");
    let service = store
        .create_machine_service(
            "default",
            "admin",
            CreateMachineServiceRequest {
                name: "development API".to_string(),
                server_id: "destination".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create machine service");
    let record = store
        .create_virtual_network_host(
            "default",
            "admin",
            CreateVirtualNetworkHostRequest {
                hostname: "API.Dev.Example.".to_string(),
                service_id: service.service_id.clone(),
            },
        )
        .await
        .expect("create virtual host");
    let created = store
        .virtual_network_hosts_snapshot("default")
        .await
        .expect("created virtual-host snapshot");
    assert!(created.revision > initial.revision);
    assert_eq!(created.hosts, std::slice::from_ref(&record));
    assert_eq!(record.hostname, "api.dev.example");
    assert_eq!(record.service_id, service.service_id);
    assert_eq!(record.destination_server_id, "destination");
    assert_eq!(record.target_port, Some(8080));
    assert_eq!(
        store
            .resolve_virtual_network_host("default", "API.DEV.EXAMPLE")
            .await
            .expect("resolve virtual host"),
        Some(record.clone())
    );
    assert!(store
        .create_virtual_network_host(
            "default",
            "admin",
            CreateVirtualNetworkHostRequest {
                hostname: "api.dev.example".to_string(),
                service_id: service.service_id.clone(),
            },
        )
        .await
        .is_err());
    store
        .create_virtual_network_host(
            "default",
            "admin",
            CreateVirtualNetworkHostRequest {
                hostname: "host.via.machine.treer".to_string(),
                service_id: service.service_id.clone(),
            },
        )
        .await
        .expect("virtual host names have no reserved routing suffixes");
    store
        .create_virtual_network_host(
            "default",
            "admin",
            CreateVirtualNetworkHostRequest {
                hostname: "git.via.example".to_string(),
                service_id: service.service_id.clone(),
            },
        )
        .await
        .expect("via label outside a Treer direct route is valid");
    store.disabled = true;
    store
        .delete_machine("default", "destination", &[])
        .await
        .expect("delete destination machine");
    let deleted = store
        .virtual_network_hosts_snapshot("default")
        .await
        .expect("deleted virtual-host snapshot");
    assert!(deleted.revision > created.revision);
    assert!(deleted.hosts.is_empty());
    assert!(store
        .list_machine_services("default")
        .await
        .expect("list machine services")
        .is_empty());
}

#[tokio::test]
async fn unchanged_virtual_host_refresh_preserves_revision_and_remote_changes_advance_it() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("default").await;
    let initial = store
        .virtual_network_hosts_snapshot("default")
        .await
        .unwrap();
    store.refresh_virtual_network_hosts().await.unwrap();
    assert_eq!(
        store
            .virtual_network_hosts_snapshot("default")
            .await
            .unwrap(),
        initial
    );
    let service = store
        .create_machine_service(
            "default",
            "admin",
            CreateMachineServiceRequest {
                name: "revision-test".into(),
                server_id: "destination".into(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".into(),
                target_port: 8080,
                protocol: MachineServiceProtocol::Http,
            },
        )
        .await
        .unwrap();
    store
        .create_virtual_network_host(
            "default",
            "admin",
            CreateVirtualNetworkHostRequest {
                hostname: "revision.example".into(),
                service_id: service.service_id.clone(),
            },
        )
        .await
        .unwrap();
    let created = store
        .virtual_network_hosts_snapshot("default")
        .await
        .unwrap();
    for _ in 0..3 {
        store.refresh_virtual_network_hosts().await.unwrap();
        assert_eq!(
            store
                .virtual_network_hosts_snapshot("default")
                .await
                .unwrap(),
            created
        );
    }
    // A different Proxy's durable service change must still reach this cache.
    sqlx::query("UPDATE machine_services SET target_port = 8081 WHERE service_id = $1")
        .bind(&service.service_id)
        .execute(&store.pool())
        .await
        .unwrap();
    store.refresh_virtual_network_hosts().await.unwrap();
    let changed = store
        .virtual_network_hosts_snapshot("default")
        .await
        .unwrap();
    assert!(changed.revision > created.revision);
    assert_eq!(changed.hosts[0].target_port, Some(8081));
    store.refresh_virtual_network_hosts().await.unwrap();
    assert_eq!(
        store
            .virtual_network_hosts_snapshot("default")
            .await
            .unwrap(),
        changed
    );
    store
        .delete_machine_service("default", &service.service_id)
        .await
        .unwrap();
    let deleted = store
        .virtual_network_hosts_snapshot("default")
        .await
        .unwrap();
    assert!(deleted.hosts.is_empty());
    for _ in 0..3 {
        store.refresh_virtual_network_hosts().await.unwrap();
        assert_eq!(
            store
                .virtual_network_hosts_snapshot("default")
                .await
                .unwrap(),
            deleted
        );
    }
}

#[tokio::test]
async fn machine_service_updates_refresh_aliases_and_delete_cascades() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("default").await;
    let service = store
        .create_machine_service(
            "default",
            "admin",
            CreateMachineServiceRequest {
                name: "web".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 3000,
                protocol: MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    store
        .create_virtual_network_host(
            "default",
            "admin",
            CreateVirtualNetworkHostRequest {
                hostname: "web.internal".to_string(),
                service_id: service.service_id.clone(),
            },
        )
        .await
        .expect("create alias");
    let updated = store
        .update_machine_service(
            "default",
            "web",
            "admin",
            UpdateMachineServiceRequest {
                target_port: Some(4000),
                ..UpdateMachineServiceRequest::default()
            },
        )
        .await
        .expect("update service");
    assert_eq!(updated.target_port, 4000);
    let alias = store
        .resolve_virtual_network_host("default", "web.internal")
        .await
        .expect("resolve alias")
        .expect("alias exists");
    assert_eq!(alias.target_port, Some(4000));

    store
        .delete_machine_service("default", &service.service_id)
        .await
        .expect("delete service");
    assert!(store
        .resolve_virtual_network_host("default", "web.internal")
        .await
        .expect("resolve deleted alias")
        .is_none());
}

#[tokio::test]
async fn managed_app_owns_a_stable_service_and_virtual_host_across_runtime_restarts() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("apps").await;
    let app = store
        .create_app_deployment(
            "apps",
            "owner",
            "machine-a".to_string(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".to_string()),
                name: "Soul".to_string(),
                command: "python3".to_string(),
                args: vec!["apps/soul/soul.py".to_string()],
                cwd: ".".to_string(),
                port: 9420,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .expect("create App deployment");
    let service = store
        .resolve_machine_service("apps", &app.service_id)
        .await
        .expect("resolve App service");
    assert_eq!(service.target_agent_id, None);
    assert_eq!(service.target_port, 9420);
    let host = store
        .resolve_virtual_network_host("apps", &app.hostname)
        .await
        .expect("resolve App virtual host")
        .expect("App virtual host");
    assert_eq!(host.service_id, app.service_id);
    let ingress = store
        .ensure_app_ingress(
            &app,
            "owner",
            "apps.treer.test",
            Some(ServiceIngressAccess::Workspace),
        )
        .await
        .expect("create App ingress");
    assert_eq!(ingress.service_id.as_deref(), Some(app.service_id.as_str()));
    assert_eq!(ingress.access, ServiceIngressAccess::Workspace);
    assert_eq!(
        ingress.hostname,
        platform_service_hostname(&app.service_id, "apps.treer.test").unwrap()
    );
    assert!(ingress.hostname.ends_with(".apps.treer.test"));
    assert_eq!(
        store
            .ensure_app_ingress(&app, "owner", "apps.treer.test", None)
            .await
            .expect("reuse App ingress")
            .ingress_id,
        ingress.ingress_id
    );

    let public_app = store
        .create_app_deployment(
            "apps",
            "owner",
            "machine-a".to_string(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".to_string()),
                name: "Public Docs".to_string(),
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "http.server".to_string()],
                cwd: ".".to_string(),
                port: 8080,
                hostname: String::new(),
                public: true,
            },
        )
        .await
        .expect("create public App deployment");
    let public_ingress = store
        .ensure_app_ingress(
            &public_app,
            "owner",
            "apps.treer.test",
            Some(ServiceIngressAccess::Public),
        )
        .await
        .expect("create public App ingress");
    assert_eq!(public_ingress.access, ServiceIngressAccess::Public);
    assert_eq!(
        store
            .set_app_ingress_access(
                &public_app,
                "owner",
                "apps.treer.test",
                ServiceIngressAccess::Workspace,
            )
            .await
            .expect("protect public App ingress")
            .access,
        ServiceIngressAccess::Workspace
    );
    assert_eq!(
        store
            .set_app_ingress_access(
                &public_app,
                "owner",
                "apps.treer.test",
                ServiceIngressAccess::Public,
            )
            .await
            .expect("publish App ingress")
            .access,
        ServiceIngressAccess::Public
    );
    store
        .ensure_managed_app_ingresses("reconciler", "apps.treer.test")
        .await
        .expect("reconcile App ingresses");
    assert_eq!(
        store
            .ensure_app_ingress(&public_app, "reconciler", "apps.treer.test", None)
            .await
            .expect("reuse public ingress after reconciliation")
            .access,
        ServiceIngressAccess::Public
    );

    let first = store.bind_app_workload("apps", &app.app_id).await.unwrap();
    assert_eq!(first.runtime_agent_id.as_deref(), Some(app.app_id.as_str()));
    let second = store.bind_app_workload("apps", &app.app_id).await.unwrap();
    assert_eq!(second.runtime_agent_id, first.runtime_agent_id);
    assert_eq!(second.restart_count, 0);
    assert_eq!(second.service_id, app.service_id);
    assert_eq!(second.hostname, app.hostname);
    sqlx::query("UPDATE app_deployments SET runtime_agent_id='appw_legacy' WHERE app_id=$1")
        .bind(&app.app_id)
        .execute(&store.pool)
        .await
        .unwrap();
    let legacy = store.bind_app_workload("apps", &app.app_id).await.unwrap();
    assert_eq!(legacy.runtime_agent_id.as_deref(), Some("appw_legacy"));
    let credential = store
        .create_agent_credential("apps", "machine-a", "appw_legacy")
        .await
        .unwrap();
    assert!(!credential.is_empty());
    let active = store.bind_app_workload("apps", &app.app_id).await.unwrap();
    assert_eq!(active.runtime_agent_id.as_deref(), Some("appw_legacy"));
    sqlx::query("UPDATE agent_credentials SET revoked_at='old-stop' WHERE agent_id='appw_legacy'")
        .execute(&store.pool)
        .await
        .unwrap();
    let migrated = store.bind_app_workload("apps", &app.app_id).await.unwrap();
    assert_eq!(
        migrated.runtime_agent_id.as_deref(),
        Some(app.app_id.as_str())
    );
    let repeated = store.bind_app_workload("apps", &app.app_id).await.unwrap();
    assert_eq!(repeated.runtime_agent_id, migrated.runtime_agent_id);
    let revoked: Option<String> =
        sqlx::query_scalar("SELECT revoked_at FROM agent_credentials WHERE agent_id='appw_legacy'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(revoked.as_deref(), Some("old-stop"));

    let stopped = store
        .set_app_desired_state("apps", &app.app_id, AppDesiredState::Stopped, "owner")
        .await
        .expect("stop App");
    assert_eq!(stopped.desired_state, AppDesiredState::Stopped);
    store
        .delete_app_deployment("apps", &app.app_id)
        .await
        .expect("delete App");
    assert!(store
        .resolve_machine_service("apps", &app.service_id)
        .await
        .is_err());
    assert!(store
        .resolve_virtual_network_host("apps", &app.hostname)
        .await
        .expect("resolve deleted host")
        .is_none());
    assert!(store
        .resolve_service_ingress_hostname(&ingress.hostname)
        .await
        .expect("resolve deleted ingress")
        .is_none());
}

#[tokio::test]
async fn primary_agent_services_are_unique_http_scoped_and_keep_their_identity() {
    let store = AuthStore::for_test("password").await;
    store.seed_test_workspace("primary").await;
    store.seed_test_workspace("other").await;
    let request = CreateMachineServiceRequest {
        name: "review-api".into(),
        server_id: "machine-a".into(),
        target_agent_id: Some("agent-a".into()),
        primary: true,
        target_host: "127.0.0.1".into(),
        target_port: 4180,
        protocol: MachineServiceProtocol::Http,
    };
    let mut invalid = request.clone();
    invalid.target_agent_id = None;
    assert_eq!(
        store
            .create_machine_service("primary", "test", invalid)
            .await
            .unwrap_err()
            .code(),
        "invalid_primary_service"
    );
    let mut invalid = request.clone();
    invalid.protocol = MachineServiceProtocol::Tcp;
    assert_eq!(
        store
            .create_machine_service("primary", "test", invalid)
            .await
            .unwrap_err()
            .code(),
        "invalid_primary_service"
    );

    let mut second = request.clone();
    second.name = "another-api".into();
    let (left, right) = tokio::join!(
        store.create_machine_service("primary", "test", request.clone()),
        store.create_machine_service("primary", "test", second),
    );
    assert_ne!(
        left.is_ok(),
        right.is_ok(),
        "concurrent registration must produce exactly one primary"
    );
    let service = left.or(right).unwrap();
    let updated = store
        .update_machine_service(
            "primary",
            &service.service_id,
            "test",
            UpdateMachineServiceRequest {
                name: Some("renamed-api".into()),
                target_port: Some(4199),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(updated.service_id, service.service_id);
    assert!(updated.primary);
    assert_eq!(updated.target_port, 4199);
    let resolved = store
        .resolve_machine_service("primary", "renamed-api")
        .await
        .unwrap();
    assert_eq!(resolved, updated);
    assert!(store
        .resolve_machine_service("other", &service.service_id)
        .await
        .is_err());
    assert!(store
        .update_machine_service(
            "primary",
            &service.service_id,
            "test",
            UpdateMachineServiceRequest {
                protocol: Some(MachineServiceProtocol::Tcp),
                ..Default::default()
            }
        )
        .await
        .is_err());
    // The same Agent ID is independent in another workspace; other Agents may
    // each register a primary inside this workspace.
    assert!(store
        .create_machine_service("other", "test", request.clone())
        .await
        .is_ok());
    let mut other_agent = request;
    other_agent.target_agent_id = Some("agent-b".into());
    assert!(store
        .create_machine_service("primary", "test", other_agent)
        .await
        .is_ok());
    store.delete_agent("primary", "agent-a").await.unwrap();
    assert!(store
        .resolve_machine_service("primary", &service.service_id)
        .await
        .is_err());
}

#[tokio::test]
async fn agent_services_keep_their_scope_and_delete_with_the_agent() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("default").await;
    let service = store
        .create_machine_service(
            "default",
            "agent:agent-a",
            CreateMachineServiceRequest {
                name: "Agent app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: Some("agent-a".to_string()),
                primary: false,
                target_host: "localhost".to_string(),
                target_port: 3000,
                protocol: MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create Agent service");
    assert_eq!(service.target_agent_id.as_deref(), Some("agent-a"));
    assert_eq!(service.target_host, "127.0.0.1");

    let host = store
        .create_virtual_network_host(
            "default",
            "agent:agent-a",
            CreateVirtualNetworkHostRequest {
                hostname: "agent-app.internal".to_string(),
                service_id: service.service_id.clone(),
            },
        )
        .await
        .expect("create Agent virtual host");
    assert_eq!(host.destination_agent_id.as_deref(), Some("agent-a"));

    let error = store
        .update_machine_service(
            "default",
            &service.service_id,
            "agent:agent-a",
            UpdateMachineServiceRequest {
                server_id: Some("machine-b".to_string()),
                ..UpdateMachineServiceRequest::default()
            },
        )
        .await
        .expect_err("Agent service scope must be immutable");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);

    store
        .delete_agent("default", "agent-a")
        .await
        .expect("delete Agent");
    store
        .refresh_virtual_network_hosts()
        .await
        .expect("refresh virtual hosts");
    assert!(store
        .resolve_machine_service("default", &service.service_id)
        .await
        .is_err());
    assert!(store
        .virtual_network_hosts_snapshot("default")
        .await
        .expect("virtual hosts")
        .hosts
        .is_empty());
}

#[tokio::test]
async fn workspace_deletion_requires_a_manager_retains_history_and_revokes_credentials() {
    let store = AuthStore::for_test("owner-password").await;
    let (owner, organization_id) = organization_fixture(&store, "ws_deletable").await;
    let member = join_organization(
        &store,
        &organization_id,
        &owner.user_id,
        "member@example.com",
    )
    .await;
    assert!(store
        .delete_workspace("ws_deletable", &member.user_id)
        .await
        .is_err());
    let machine = enroll(&store, "ws_deletable", None, None).await.unwrap();
    store
        .create_agent_credential("ws_deletable", &machine.server_id, "agent-1")
        .await
        .unwrap();
    crate::message_store::MessageStore::open(store.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO core_messages(message_id, workspace_id, sender_kind, sender_id, sender_name, body, created_at) \
         VALUES('msg-history', 'ws_deletable', 'agent', 'agent-1', 'Agent One', 'retained', $1)",
    )
    .bind(Utc::now().to_rfc3339())
    .execute(&store.pool)
    .await
    .unwrap();
    let unused_enrollment = store
        .create_machine_enrollment("ws_deletable", &owner.user_id)
        .await
        .unwrap();

    let (status, error) = store
        .delete_workspace("ws_deletable", &owner.user_id)
        .await
        .expect_err("an active machine blocks workspace deletion")
        .into_parts();
    assert_eq!(
        (status, error.code.as_str()),
        (StatusCode::CONFLICT, "workspace_has_machines")
    );
    store
        .delete_machine("ws_deletable", &machine.server_id, &["agent-1".to_string()])
        .await
        .unwrap();
    let deleted = store
        .delete_workspace("ws_deletable", &owner.user_id)
        .await
        .unwrap();
    assert_eq!((deleted.machine_count, deleted.agent_count), (0, 1));
    assert!(store
        .list_workspaces(&organization_id, &owner.user_id)
        .await
        .unwrap()
        .is_empty());
    let (deleted_by, machines, messages): (Option<String>, i64, i64) = sqlx::query_as(
        "SELECT deleted_by, \
         (SELECT COUNT(*) FROM machines WHERE workspace_id = $1), \
         (SELECT COUNT(*) FROM core_messages WHERE workspace_id = $1) \
         FROM workspaces WHERE workspace_id = $1",
    )
    .bind("ws_deletable")
    .fetch_one(&store.pool)
    .await
    .unwrap();
    assert_eq!(
        (deleted_by, machines, messages),
        (Some(owner.user_id.clone()), 1, 1)
    );
    assert!(store
        .claim_machine_enrollment(&unused_enrollment)
        .await
        .is_err());
    assert!(store
        .delete_workspace("ws_deletable", &owner.user_id)
        .await
        .is_err());
    let audit = store
        .list_audit_events(&organization_id, &owner.user_id, None, None, 100)
        .await
        .unwrap();
    assert!(audit
        .iter()
        .any(|event| event.action == "workspace.deleted" && event.resource_id == "ws_deletable"));
}

#[tokio::test]
async fn organization_roles_control_members_and_share_workspaces() {
    let store = AuthStore::for_test("owner-password").await;
    let (owner, organization_id) = organization_fixture(&store, "ws_engineering").await;
    let alice = join_organization(
        &store,
        &organization_id,
        &owner.user_id,
        "alice@example.com",
    )
    .await;
    let workspaces = store
        .list_workspaces(&organization_id, &alice.user_id)
        .await
        .unwrap();
    assert_eq!(workspaces[0].workspace_id, "ws_engineering");
    store
        .rename_workspace("ws_engineering", &alice.user_id, "Platform")
        .await
        .expect("members may rename workspaces");
    store
        .create_workspace(&organization_id, "ws_product", "Product", &alice.user_id)
        .await
        .expect("members may create workspaces");
    assert!(store
        .create_invitation(&organization_id, &alice.user_id)
        .await
        .is_err());
    assert!(store
        .list_audit_events(
            &organization_id,
            &alice.user_id,
            Some("ws_engineering"),
            None,
            100
        )
        .await
        .is_err());

    store
        .update_member_role(&organization_id, &owner.user_id, &alice.user_id, "admin")
        .await
        .unwrap();
    let bob = join_organization(&store, &organization_id, &alice.user_id, "bob@example.com").await;
    store
        .remove_member(&organization_id, &alice.user_id, &bob.user_id)
        .await
        .expect("admin removes member");
    assert!(store
        .remove_member(&organization_id, &alice.user_id, &owner.user_id)
        .await
        .is_err());
    let audit = store
        .list_audit_events(
            &organization_id,
            &owner.user_id,
            Some("ws_engineering"),
            None,
            100,
        )
        .await
        .unwrap();
    for action in ["workspace.renamed", "member.role_updated"] {
        assert!(audit.iter().any(|event| event.action == action), "{action}");
    }
}

#[tokio::test]
async fn workspace_access_is_limited_to_organization_members() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let personal = store
        .list_organizations(&owner.user_id)
        .await
        .unwrap()
        .remove(0);
    let alice = join_organization(
        &store,
        &personal.organization_id,
        &owner.user_id,
        "alice@example.com",
    )
    .await;
    let private = store
        .create_organization(&owner.user_id, "Private")
        .await
        .unwrap();
    store
        .create_workspace(
            &private.organization_id,
            "ws_private",
            "Private",
            &owner.user_id,
        )
        .await
        .unwrap();
    let error = store
        .require_workspace_member("ws_private", &alice.user_id)
        .await
        .expect_err("cross-organization access must fail");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn restricted_workspaces_support_user_and_group_grants() {
    let store = AuthStore::for_test("owner-password").await;
    let (owner, organization_id) = organization_fixture(&store, "ws_restricted").await;
    let alice = join_organization(
        &store,
        &organization_id,
        &owner.user_id,
        "alice@example.com",
    )
    .await;
    let bob = join_organization(&store, &organization_id, &owner.user_id, "bob@example.com").await;
    store
        .update_workspace_access_mode("ws_restricted", &owner.user_id, "restricted")
        .await
        .unwrap();
    assert!(store
        .list_workspaces(&organization_id, &alice.user_id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store
            .require_workspace_member("ws_restricted", &alice.user_id)
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    store
        .upsert_workspace_user_grant("ws_restricted", &owner.user_id, &alice.user_id, "member")
        .await
        .unwrap();
    assert_eq!(
        store
            .workspace_member_role("ws_restricted", &alice.user_id)
            .await
            .unwrap(),
        "member"
    );
    assert!(store
        .delete_workspace("ws_restricted", &alice.user_id)
        .await
        .is_err());
    let group = store
        .create_organization_group(&organization_id, &owner.user_id, "Reviewers")
        .await
        .unwrap();
    store
        .set_organization_group_member(
            &organization_id,
            &owner.user_id,
            &group.group_id,
            &bob.user_id,
            true,
        )
        .await
        .unwrap();
    store
        .upsert_workspace_group_grant("ws_restricted", &owner.user_id, &group.group_id, "member")
        .await
        .unwrap();
    assert_eq!(
        store
            .workspace_member_role("ws_restricted", &bob.user_id)
            .await
            .unwrap(),
        "member"
    );
    store
        .upsert_workspace_user_grant("ws_restricted", &owner.user_id, &alice.user_id, "owner")
        .await
        .unwrap();
    store
        .delete_workspace("ws_restricted", &alice.user_id)
        .await
        .expect("workspace owner may delete");
}

#[tokio::test]
async fn service_ingress_and_human_authorization_are_durable_and_scoped() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let organization = store
        .list_organizations(&owner.user_id)
        .await
        .expect("list organizations")
        .into_iter()
        .next()
        .expect("personal organization");
    store
        .create_workspace(
            &organization.organization_id,
            "published",
            "Published",
            &owner.user_id,
        )
        .await
        .expect("create workspace");
    let service = store
        .create_machine_service(
            "published",
            &owner.user_id,
            CreateMachineServiceRequest {
                name: "Issue Tracker".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 3000,
                protocol: MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    let ingress = store
        .create_service_ingress(
            "published",
            &owner.user_id,
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id.clone(),
                slug: None,
                access: ServiceIngressAccess::Workspace,
            },
        )
        .await
        .expect("create ingress");
    assert_eq!(
        ingress.hostname,
        platform_service_hostname(&service.service_id, "apps.treer.ai")
            .expect("platform service address")
    );
    let republished = store
        .create_service_ingress(
            "published",
            &owner.user_id,
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id.clone(),
                slug: None,
                access: ServiceIngressAccess::Workspace,
            },
        )
        .await
        .expect("idempotent publication");
    assert_eq!(republished.ingress_id, ingress.ingress_id);
    let custom = store
        .create_service_ingress(
            "published",
            &owner.user_id,
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id.clone(),
                slug: Some("custom".into()),
                access: ServiceIngressAccess::Public,
            },
        )
        .await
        .expect_err("custom service domains retired");
    assert_eq!(custom.error.code, "platform_domain_required");
    assert!(ingress.hostname.ends_with(".apps.treer.ai"));
    assert_eq!(
        store
            .resolve_service_ingress_hostname(&ingress.hostname)
            .await
            .expect("resolve ingress")
            .expect("stored ingress")
            .service
            .service_id,
        service.service_id
    );

    let code = store
        .create_ingress_auth_code(
            &ingress,
            &owner.user_id,
            "/issues?mine=1",
            &ingress_nonce_hash(&"a".repeat(64)),
        )
        .await
        .expect("create authorization code");
    let authorization = store
        .consume_ingress_auth_code(&ingress.hostname, &code, Some(&"a".repeat(64)))
        .await
        .expect("consume authorization code");
    assert_eq!(authorization.return_path, "/issues?mine=1");
    assert_eq!(
        store
            .authenticate_ingress_session(&ingress.hostname, &authorization.session_token)
            .await
            .expect("authenticate ingress session")
            .as_deref(),
        Some(owner.user_id.as_str())
    );
    assert!(store
        .consume_ingress_auth_code(&ingress.hostname, &code, Some(&"a".repeat(64)))
        .await
        .is_err());

    let disabled = store
        .update_service_ingress(
            "published",
            &ingress.ingress_id,
            &owner.user_id,
            UpdateServiceIngressRequest {
                access: None,
                enabled: Some(false),
            },
        )
        .await
        .expect("disable ingress");
    assert!(!disabled.enabled);
    assert!(store
        .authenticate_ingress_session(&ingress.hostname, &authorization.session_token)
        .await
        .expect("disabled session lookup")
        .is_none());
}

#[tokio::test]
async fn sessions_authenticate_by_bearer_and_logout_invalidates_them() {
    let store = AuthStore::for_test("owner-password").await;
    let session = bootstrap_owner(&store, "bearer@example.com", "Bearer").await;
    let authenticated = authenticate_request(&store, &bearer(&session.token))
        .await
        .unwrap();
    assert_eq!(authenticated.user_id, session.user_id);
    store.logout(&session.token).await.unwrap();
    assert!(store.session(&session.token).await.unwrap().is_none());
    assert!(authenticate_request(&store, &bearer(&session.token))
        .await
        .is_err());
}

#[tokio::test]
async fn session_json_includes_token_only_for_native_clients() {
    let store = AuthStore::for_test("owner-password").await;
    let session = bootstrap_owner(&store, "ios@example.com", "iOS").await;
    for (client, token_visible) in [
        (Some("mobile_ios"), true),
        (Some("browser"), false),
        (None, false),
    ] {
        let mut headers = HeaderMap::new();
        if let Some(client) = client {
            headers.insert(NATIVE_CLIENT_HEADER, HeaderValue::from_static(client));
        }
        let response = session_response(&store, &session, &headers);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["user_id"], session.user_id);
        assert_eq!(json.get("token").is_some(), token_visible, "{client:?}");
    }
}

#[tokio::test]
async fn disabled_auth_injects_a_local_user() {
    let mut store = AuthStore::for_test("owner-password").await;
    store.disabled = true;
    let session = authenticate_request(&store, &HeaderMap::new())
        .await
        .unwrap();
    assert_eq!(session.user_id, "local");
}

#[tokio::test]
async fn recovery_stopped_intent_is_durable_and_workspace_bound() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("workspace-a").await;
    let claim = enroll(&store, "workspace-a", None, None).await.unwrap();
    store
        .create_agent_credential("workspace-a", &claim.server_id, "agent-recovery")
        .await
        .unwrap();
    let ids = vec!["agent-recovery".to_string()];
    let stopped = |server: String| {
        let store = store.clone();
        let ids = ids.clone();
        async move {
            store
                .stopped_agent_recovery_ids("workspace-a", &server, &ids)
                .await
                .unwrap()
        }
    };
    store
        .set_agent_recovery_enabled("another-workspace", "agent-recovery", false)
        .await
        .unwrap();
    assert!(stopped(claim.server_id.clone()).await.is_empty());
    store
        .set_agent_recovery_enabled("workspace-a", "agent-recovery", false)
        .await
        .unwrap();
    assert_eq!(stopped(claim.server_id.clone()).await, ids);
    assert!(stopped("other-machine".into()).await.is_empty());
    store
        .set_agent_recovery_enabled("workspace-a", "agent-recovery", true)
        .await
        .unwrap();
    assert!(stopped(claim.server_id.clone()).await.is_empty());
}

#[tokio::test]
async fn machine_enrollment_is_single_use_and_binds_identity() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("workspace-a").await;
    assert!(store.authenticate_machine(&HeaderMap::new()).await.is_err());
    let enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .unwrap();
    assert_eq!(
        parse_machine_enrollment_key(&enrollment)
            .unwrap()
            .workspace_id,
        "workspace-a"
    );
    let claim = store.claim_machine_enrollment(&enrollment).await.unwrap();
    assert_eq!(claim.workspace_id, "workspace-a");
    assert!(store.claim_machine_enrollment(&enrollment).await.is_err());

    let mut headers = bearer(&claim.machine_token);
    let machine = store.authenticate_machine(&headers).await.unwrap();
    assert!(machine.allows_server("workspace-a", &claim.server_id));
    assert!(!machine.allows_server("workspace-b", &claim.server_id));
    assert!(!machine.allows_server("workspace-a", "srv_other"));

    let workload_credential = store
        .create_agent_credential("workspace-a", &claim.server_id, "agent-a")
        .await
        .unwrap();
    assert_eq!(
        store
            .active_agent_ids(
                "workspace-a",
                &claim.server_id,
                &["agent-a".to_string(), "agent-missing".to_string()],
            )
            .await
            .unwrap(),
        ["agent-a"]
    );
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    headers.insert(
        WORKLOAD_CREDENTIAL_HEADER,
        HeaderValue::from_str(&workload_credential).unwrap(),
    );
    let agent = store
        .authenticate_agent(&machine, &headers)
        .await
        .unwrap()
        .expect("Agent session");
    assert_eq!(agent.server_id, claim.server_id);
    let other_machine = MachineSession {
        server_id: Some("srv_other".to_string()),
        workspace_id: Some("workspace-a".to_string()),
    };
    assert!(store
        .authenticate_agent(&other_machine, &headers)
        .await
        .is_err());
    headers.insert(
        WORKLOAD_CREDENTIAL_HEADER,
        HeaderValue::from_static("wlc_invalid"),
    );
    assert!(store.authenticate_agent(&machine, &headers).await.is_err());

    store.delete_agent("workspace-a", "agent-a").await.unwrap();
    assert!(store
        .active_agent_ids("workspace-a", &claim.server_id, &["agent-a".to_string()])
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn re_enrollment_reuses_installation_identity_and_rotates_credentials() {
    let store = AuthStore::for_test("owner-password").await;
    let installation = "mid_0123456789abcdef0123456789abcdef";
    let first = enroll(&store, "workspace-a", Some(installation), None)
        .await
        .unwrap();
    let second = enroll(&store, "workspace-a", Some(installation), None)
        .await
        .unwrap();
    assert_eq!(first.server_id, second.server_id);
    assert!(!machine_ok(&store, &first.machine_token).await);
    assert!(machine_ok(&store, &second.machine_token).await);

    // A revoked legacy machine (no installation identity) is recovered by ID.
    let legacy = enroll(&store, "workspace-a", None, None).await.unwrap();
    sqlx::query("UPDATE machines SET revoked_at = $1 WHERE server_id = $2")
        .bind(Utc::now().to_rfc3339())
        .bind(&legacy.server_id)
        .execute(&store.pool)
        .await
        .unwrap();
    let recovered_installation = "mid_abcdef0123456789abcdef0123456789";
    let recovered = enroll(
        &store,
        "workspace-a",
        Some(recovered_installation),
        Some(&legacy.server_id),
    )
    .await
    .unwrap();
    assert_eq!(recovered.server_id, legacy.server_id);
    assert_eq!(
        machine_row(&store, &legacy.server_id).await.0,
        recovered_installation
    );
    assert!(!machine_ok(&store, &legacy.machine_token).await);
    assert!(machine_ok(&store, &recovered.machine_token).await);
}

#[tokio::test]
async fn deleted_machine_can_be_re_enrolled_from_a_new_installation_but_a_live_one_cannot() {
    // Issue #126: a service config still names the old server ID while the
    // installation identity was regenerated after the machine was deleted.
    let store = AuthStore::for_test("owner-password").await;
    let (old_install, new_install) = (
        "mid_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "mid_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    let conflict = |result: Result<MachineEnrollmentClaim, AuthFailure>| {
        result.expect_err("conflict").into_parts().1.code
    };
    let first = enroll(&store, "workspace-a", Some(old_install), None)
        .await
        .unwrap();
    assert_eq!(
        conflict(
            enroll(
                &store,
                "workspace-a",
                Some(new_install),
                Some(&first.server_id)
            )
            .await
        ),
        "machine_identity_conflict"
    );
    store
        .delete_machine("workspace-a", &first.server_id, &[])
        .await
        .unwrap();
    let second = enroll(
        &store,
        "workspace-a",
        Some(new_install),
        Some(&first.server_id),
    )
    .await
    .unwrap();
    assert_ne!(second.server_id, first.server_id);
    let old_row = machine_row(&store, &first.server_id).await;
    assert_eq!(old_row.0, old_install);
    assert!(old_row.1.is_some());
    assert!(!machine_ok(&store, &first.machine_token).await);
    assert!(machine_ok(&store, &second.machine_token).await);

    // The original installation reactivates its own row; the new machine stays.
    let reactivated = enroll(
        &store,
        "workspace-a",
        Some(old_install),
        Some(&first.server_id),
    )
    .await
    .unwrap();
    assert_eq!(reactivated.server_id, first.server_id);
    assert!(machine_ok(&store, &second.machine_token).await);

    // Another workspace never claims a server ID, deleted or not.
    store
        .delete_machine("workspace-a", &first.server_id, &[])
        .await
        .unwrap();
    assert_eq!(
        conflict(
            enroll(
                &store,
                "workspace-b",
                Some(new_install),
                Some(&first.server_id)
            )
            .await
        ),
        "machine_identity_conflict"
    );
}

#[tokio::test]
async fn concurrent_claims_for_a_deleted_machine_each_become_their_own_machine() {
    let store = AuthStore::for_test("owner-password").await;
    let first = enroll(
        &store,
        "workspace-a",
        Some("mid_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        None,
    )
    .await
    .unwrap();
    store
        .delete_machine("workspace-a", &first.server_id, &[])
        .await
        .unwrap();
    // Hold the row so both claims reach their locked eligibility read together.
    let mut holder = store.pool.begin().await.unwrap();
    sqlx::query("SELECT server_id FROM machines WHERE server_id = $1 FOR UPDATE")
        .bind(&first.server_id)
        .fetch_one(&mut *holder)
        .await
        .unwrap();
    let claims: Vec<_> = [
        "mid_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "mid_cccccccccccccccccccccccccccccccc",
    ]
    .into_iter()
    .map(|installation| {
        let store = store.clone();
        let server_id = first.server_id.clone();
        tokio::spawn(async move {
            enroll(&store, "workspace-a", Some(installation), Some(&server_id)).await
        })
    })
    .collect();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    holder.commit().await.unwrap();
    let mut minted = Vec::new();
    for claim in claims {
        let claim = claim.await.unwrap().expect("new machine minted");
        assert_ne!(claim.server_id, first.server_id);
        assert!(machine_ok(&store, &claim.machine_token).await);
        minted.push(claim.server_id);
    }
    assert_ne!(minted[0], minted[1]);
    assert!(machine_row(&store, &first.server_id).await.1.is_some());
}

#[tokio::test]
async fn re_enrolling_a_deleted_machine_never_revives_its_agents() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("workspace-a").await;
    let first = enroll(
        &store,
        "workspace-a",
        Some("mid_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        None,
    )
    .await
    .unwrap();
    let credential = store
        .create_agent_credential("workspace-a", &first.server_id, "ag_old")
        .await
        .unwrap();
    store
        .delete_machine("workspace-a", &first.server_id, &["ag_old".to_string()])
        .await
        .unwrap();
    let second = enroll(
        &store,
        "workspace-a",
        Some("mid_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        Some(&first.server_id),
    )
    .await
    .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("ag_old"));
    headers.insert(
        WORKLOAD_CREDENTIAL_HEADER,
        HeaderValue::from_str(&credential).unwrap(),
    );
    for server_id in [&first.server_id, &second.server_id] {
        let machine = MachineSession {
            server_id: Some(server_id.clone()),
            workspace_id: Some("workspace-a".into()),
        };
        assert!(store.authenticate_agent(&machine, &headers).await.is_err());
        assert!(store
            .active_agent_ids("workspace-a", server_id, &["ag_old".to_string()])
            .await
            .unwrap()
            .is_empty());
    }
    assert!(credential_hash(&store, "ag_old").await.1.is_some());
}

#[tokio::test]
async fn existing_machine_can_bind_an_installation_identity_for_migration() {
    let store = AuthStore::for_test("owner-password").await;
    let legacy = enroll(&store, "workspace-a", None, None).await.unwrap();
    let installation = "mid_abcdef0123456789abcdef0123456789";
    store
        .bind_machine_identity("workspace-a", &legacy.server_id, installation, "Migrated")
        .await
        .unwrap();
    let rebind = store
        .bind_machine_identity(
            "workspace-a",
            &legacy.server_id,
            "mid_11111111111111111111111111111111",
            "Other",
        )
        .await
        .expect_err("installation identity is immutable");
    assert_eq!(rebind.into_parts().1.code, "machine_identity_conflict");
    let reenrolled = enroll(&store, "workspace-a", Some(installation), None)
        .await
        .unwrap();
    assert_eq!(reenrolled.server_id, legacy.server_id);
}

fn security_test_snapshot() -> AgentServerSnapshot {
    let now = Utc::now();
    let build = |git_commit: &str| treer_protocol::BuildInfo {
        version: "0.1.2".to_string(),
        git_commit: git_commit.to_string(),
    };
    AgentServerSnapshot {
        server: ServerInfo {
            server_id: "server-a".to_string(),
            workspace_id: "workspace-a".to_string(),
            name: String::new(),
            hostname: "original-host".to_string(),
            root: "/workspace".to_string(),
            controller_build: build("controller-test"),
            host_build: build("host-test"),
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
            capabilities: vec![],
        },
        agents: vec![AgentInfo {
            path: None,
            run_generation: 0,
            agent_id: "agent-a".to_string(),
            workspace_id: "workspace-a".to_string(),
            server_id: "server-a".to_string(),
            kind: "codex".to_string(),
            name: "original-agent".to_string(),
            cwd: ".".to_string(),
            status: AgentStatus::Idle,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        }],
    }
}

async fn agent_name_rows(store: &AuthStore) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM agent_names")
        .fetch_one(&store.pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn controller_snapshot_cannot_cross_persisted_agent_identity_binding() {
    let store = AuthStore::for_test("test-password").await;
    store.seed_test_workspace("workspace-a").await;
    store.seed_test_workspace("workspace-b").await;
    for (workspace, server) in [("workspace-a", "server-b"), ("workspace-b", "server-a")] {
        // A credential bound elsewhere rejects the inventory before any name write.
        store
            .create_agent_credential(workspace, server, "agent-a")
            .await
            .unwrap();
        let mut snapshot = security_test_snapshot();
        assert_eq!(
            store
                .validate_agent_event(&snapshot.agents[0])
                .await
                .unwrap_err()
                .code(),
            "agent_identity_mismatch"
        );
        assert_eq!(
            store
                .apply_agent_names(&mut snapshot)
                .await
                .unwrap_err()
                .code(),
            "agent_identity_mismatch"
        );
        assert_eq!(agent_name_rows(&store).await, 0);
        sqlx::query("DELETE FROM agent_credentials WHERE agent_id='agent-a'")
            .execute(&store.pool)
            .await
            .unwrap();
        // A nested Agent claiming a foreign workspace/server is refused too.
        let mut snapshot = security_test_snapshot();
        snapshot.agents[0].workspace_id = workspace.into();
        snapshot.agents[0].server_id = server.into();
        assert!(store.apply_agent_names(&mut snapshot).await.is_err());
        assert_eq!(agent_name_rows(&store).await, 0);
    }
    store
        .set_agent_name("workspace-b", "agent-a", "legacy-victim")
        .await
        .unwrap();
    assert!(store
        .apply_agent_names(&mut security_test_snapshot())
        .await
        .is_err());
    sqlx::query("DELETE FROM agent_names")
        .execute(&store.pool)
        .await
        .unwrap();
    // Matching identities and credential-less legacy inventory remain supported.
    let mut snapshot = security_test_snapshot();
    store.apply_agent_names(&mut snapshot).await.unwrap();
    store
        .create_agent_credential("workspace-a", "server-a", "agent-a")
        .await
        .unwrap();
    store.apply_agent_names(&mut snapshot).await.unwrap();
    store
        .validate_agent_event(&snapshot.agents[0])
        .await
        .unwrap();
}

#[test]
fn machine_route_is_limited_to_credential_workspace() {
    let machine = MachineSession {
        server_id: Some("srv_test".to_string()),
        workspace_id: Some("team one".to_string()),
    };
    assert!(machine_workspace_matches(
        &machine,
        "/agent/workspaces/team%20one/agents"
    ));
    assert!(!machine_workspace_matches(
        &machine,
        "/agent/workspaces/other/agents"
    ));
    assert!(machine_workspace_matches(
        &machine,
        "/agent/machine/identity"
    ));
}

#[tokio::test]
async fn app_email_any_recipient_delivery_idempotency_rates_and_body_free_audit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
    let capture = captured.clone();
    let fake = Router::new().route(
        "/send",
        post(move |Json(body): Json<Value>| {
            let capture = capture.clone();
            async move {
                capture.lock().await.push(body);
                Json(json!({"success":true}))
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, fake).await });
    let mut store = AuthStore::for_test("password").await;
    store.seed_test_workspace("email").await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,email_verified,created_at) VALUES('u','member@example.test','M','unused',TRUE,'now'),('unverified','unverified@example.test','U','unused',FALSE,'now')")
        .execute(&store.pool).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_email','u','member','now'),('org_email','unverified','member','now')").execute(&store.pool).await.unwrap();
    let mut request = treer_protocol::SendAppEmailRequest {
        to: "outside@external.test".into(),
        subject: "Test".into(),
        text: "PRIVATE BODY <script>bad()</script>".into(),
        idempotency_key: "first".into(),
    };
    let error = store
        .send_app_email("email", "app-a", &request)
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error.error.code, "email_not_configured");
    store.email_sender = Some(CloudflareEmailSender {
        client: reqwest::Client::new(),
        endpoint: Url::parse(&format!("http://{address}/send")).unwrap(),
        api_token: "fake-token".into(),
        from: "fixed@example.test".into(),
    });
    let first = store
        .send_app_email("email", "app-a", &request)
        .await
        .unwrap();
    assert_eq!(first.status, "sent");
    let status = store
        .app_email_status("email", "app-a", "first")
        .await
        .unwrap();
    assert_eq!(
        (status.email_id.as_str(), status.status.as_str()),
        (first.email_id.as_str(), "sent")
    );
    for (app, key) in [("app-a", "unknown"), ("app-b", "first")] {
        let missing = store.app_email_status("email", app, key).await.unwrap_err();
        assert_eq!(missing.status, StatusCode::NOT_FOUND);
        assert_eq!(missing.error.code, "email_not_found");
    }
    sqlx::query("UPDATE app_email_sends SET status='sending' WHERE idempotency_key='first'")
        .execute(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .app_email_status("email", "app-a", "first")
            .await
            .unwrap()
            .status,
        "pending"
    );
    sqlx::query("UPDATE app_email_sends SET status='sent' WHERE idempotency_key='first'")
        .execute(&store.pool)
        .await
        .unwrap();
    assert_eq!(
        store
            .send_app_email("email", "app-a", &request)
            .await
            .unwrap()
            .email_id,
        first.email_id
    );
    assert_eq!(captured.lock().await.len(), 1);
    let mut conflict = request.clone();
    conflict.text = "different content".into();
    assert_eq!(
        store
            .send_app_email("email", "app-a", &conflict)
            .await
            .unwrap_err()
            .error
            .code,
        "email_idempotency_conflict"
    );
    assert_eq!(captured.lock().await.len(), 1);
    let body = captured.lock().await[0].clone();
    assert_eq!(body["to"], "outside@external.test");
    assert_eq!(body["from"], "fixed@example.test");
    assert!(body.get("reply_to").is_none());
    assert!(body["html"].as_str().unwrap().contains("&lt;script&gt;"));
    let audit: Value = sqlx::query_scalar("SELECT to_jsonb(a) FROM app_email_attempts a LIMIT 1")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    let audit = serde_json::to_string(&audit).unwrap();
    assert!(!audit.contains("PRIVATE"));
    assert!(!audit.contains("outside@external.test"));
    assert!(!audit.contains("fake-token"));
    for i in 0..4 {
        request.idempotency_key = format!("next-{i}");
        store
            .send_app_email("email", "app-a", &request)
            .await
            .unwrap();
    }
    request.idempotency_key = "limited".into();
    assert_eq!(
        store
            .send_app_email("email", "app-b", &request)
            .await
            .unwrap_err()
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    // An unverified member is also a legal recipient; membership/verification
    // must not decide outbound admission.
    request.to = "unverified@example.test".into();
    request.idempotency_key = "unverified".into();
    assert!(store
        .send_app_email("email", "app-a", &request)
        .await
        .is_ok());
    assert_eq!(
        captured.lock().await.last().unwrap()["to"],
        "unverified@example.test"
    );
    request.to = "malformed@@external.test".into();
    request.idempotency_key = "malformed".into();
    let attempts_before = captured.lock().await.len();
    assert_eq!(
        store
            .send_app_email("email", "app-a", &request)
            .await
            .unwrap_err()
            .error
            .code,
        "email_invalid"
    );
    assert_eq!(captured.lock().await.len(), attempts_before);
    for i in 0..14 {
        request.to = format!("p{i}@allowed.test");
        request.idempotency_key = format!("app-limit-{i}");
        store
            .send_app_email("email", "app-a", &request)
            .await
            .unwrap();
    }
    request.to = "last@allowed.test".into();
    request.idempotency_key = "app-limited".into();
    assert_eq!(
        store
            .send_app_email("email", "app-a", &request)
            .await
            .unwrap_err()
            .status,
        StatusCode::TOO_MANY_REQUESTS
    );
    request.subject = "injected\r\nBcc: other".into();
    assert_eq!(
        store
            .send_app_email("email", "app-a", &request)
            .await
            .unwrap_err()
            .error
            .code,
        "email_invalid"
    );
    server.abort();
}

#[tokio::test]
async fn app_email_provider_failure_is_sanitized_and_retryable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fake = Router::new().route(
        "/send",
        post(|| async {
            Json(json!({"success":false,"errors":[{"message":"SECRET provider response"}]}))
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, fake).await });
    let mut store = AuthStore::for_test("password").await;
    store.seed_test_workspace("email").await;
    store.email_sender = Some(CloudflareEmailSender {
        client: reqwest::Client::new(),
        endpoint: Url::parse(&format!("http://{address}/send")).unwrap(),
        api_token: "fake".into(),
        from: "fixed@example.test".into(),
    });
    let r = treer_protocol::SendAppEmailRequest {
        to: "allowed@example.test".into(),
        subject: "test".into(),
        text: "private".into(),
        idempotency_key: "k".into(),
    };
    for _ in 0..2 {
        let error = store.send_app_email("email", "app", &r).await.unwrap_err();
        assert_eq!(error.error.code, "email_delivery_failed");
        assert!(!error.error.message.contains("SECRET"));
    }
    assert_eq!(
        store
            .app_email_status("email", "app", "k")
            .await
            .unwrap()
            .status,
        "failed"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM app_email_attempts WHERE status='failed'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert_eq!(count, 2);
    server.abort();
}

#[tokio::test]
async fn sign_in_attempts_are_limited_per_account_and_administrator() {
    let store = AuthStore::for_test("owner-password").await;
    let limit = accounts::LOGIN_ATTEMPT_LIMIT;
    let login_code = |email: &'static str, password: &'static str| {
        let store = store.clone();
        async move {
            store
                .login(email, password)
                .await
                .unwrap_err()
                .code()
                .to_string()
        }
    };
    bootstrap_owner(&store, "victim@example.com", "Victim").await;
    bootstrap_owner(&store, "other@example.com", "Other").await;
    for _ in 0..limit {
        assert_eq!(
            login_code("victim@example.com", "wrong").await,
            "invalid_credentials"
        );
    }
    // The right password is refused once the budget is spent.
    assert_eq!(
        login_code("victim@example.com", "password123").await,
        "login_rate_limited"
    );
    for _ in 0..=limit {
        let _ = store.login("nobody@example.com", "x").await;
    }
    assert_eq!(
        login_code("nobody@example.com", "x").await,
        "login_rate_limited"
    );
    // Another account is unaffected, and success clears its budget.
    for _ in 0..3 {
        let _ = store.login("other@example.com", "wrong").await;
    }
    store
        .login("other@example.com", "password123")
        .await
        .unwrap();
    for _ in 0..limit {
        assert_eq!(
            login_code("other@example.com", "wrong").await,
            "invalid_credentials"
        );
    }
    for _ in 0..limit {
        assert_eq!(
            store.admin_login("guess").await.unwrap_err().code(),
            "invalid_admin_credentials"
        );
    }
    assert_eq!(
        store
            .admin_login("owner-password")
            .await
            .unwrap_err()
            .code(),
        "login_rate_limited"
    );
    // Window expiry restores service.
    sqlx::query("UPDATE login_attempts SET window_started_at='2000-01-01T00:00:00+00:00'")
        .execute(&store.pool)
        .await
        .unwrap();
    store.admin_login("owner-password").await.unwrap();
}

#[tokio::test]
async fn unicode_case_variants_of_an_email_share_one_login_budget() {
    let store = AuthStore::for_test("owner-password").await;
    for _ in 0..accounts::LOGIN_ATTEMPT_LIMIT {
        let _ = store.login("victim\u{e9}@example.test", "wrong").await;
    }
    assert_eq!(
        store
            .login("victim\u{c9}@example.test", "wrong")
            .await
            .unwrap_err()
            .code(),
        "login_rate_limited"
    );
}

#[tokio::test]
async fn concurrent_guesses_cannot_exceed_the_attempt_budget_and_reset_restores_service() {
    let store = AuthStore::for_test("owner-password").await;
    let tasks: Vec<_> = (0..30)
        .map(|_| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .admin_login("guess")
                    .await
                    .unwrap_err()
                    .code()
                    .to_string()
            })
        })
        .collect();
    let mut checked = 0;
    for task in tasks {
        match task.await.unwrap().as_str() {
            "invalid_admin_credentials" => checked += 1,
            "login_rate_limited" => {}
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!(checked, accounts::LOGIN_ATTEMPT_LIMIT);
    // Operator recovery (`treer-proxy --reset-login-attempts`) is a plain DELETE.
    sqlx::query("DELETE FROM login_attempts")
        .execute(&store.pool)
        .await
        .unwrap();
    store.admin_login("owner-password").await.unwrap();
}
