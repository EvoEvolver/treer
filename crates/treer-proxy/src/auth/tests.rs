
use super::*;
use axum::extract::Form;
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::{oneshot, Mutex};
use treer_protocol::{AgentInfo, AgentStatus, CreateVirtualNetworkHostRequest, ServerStatus};

type EmailCapture = Arc<Mutex<Option<oneshot::Sender<(HeaderMap, Value)>>>>;

#[test]
fn managed_app_ingress_hostnames_are_stable_and_dns_safe() {
    assert_eq!(
        managed_app_ingress_hostname("Soul Archive", "app_abcdef1234567890", "apps.treer.test")
            .expect("managed App hostname"),
        "soul-archive-abcdef123456.apps.treer.test"
    );
    assert_eq!(
        managed_app_ingress_hostname("灵魂", "app_1234567890abcdef", "apps.treer.test")
            .expect("fallback App hostname"),
        "app-1234567890ab.apps.treer.test"
    );
    assert!(managed_app_ingress_hostname("Soul", "app_---", "apps.treer.test").is_err());
}

async fn capture_email(
    State(capture): State<EmailCapture>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    if let Some(sender) = capture.lock().await.take() {
        let _ = sender.send((headers, body));
    }
    Json(json!({ "success": true }))
}

async fn oauth_token(Form(form): Form<HashMap<String, String>>) -> Json<Value> {
    assert_eq!(form.get("client_id").map(String::as_str), Some("client-id"));
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
            .and_then(|value| value.to_str().ok()),
        Some("Bearer provider-access-token")
    );
}

async fn github_user(headers: HeaderMap) -> Json<Value> {
    assert_oauth_bearer(&headers);
    Json(json!({ "id": 12345, "login": "octocat", "name": "Octo Cat" }))
}

async fn github_emails(headers: HeaderMap) -> Json<Value> {
    assert_oauth_bearer(&headers);
    Json(json!([
        { "email": "unverified@example.com", "primary": true, "verified": false },
        { "email": "octo@example.com", "primary": false, "verified": true }
    ]))
}

async fn google_user(headers: HeaderMap) -> Json<Value> {
    assert_oauth_bearer(&headers);
    Json(json!({
        "sub": "google-subject",
        "email": "google@example.com",
        "email_verified": true,
        "name": "Google User"
    }))
}

async fn bootstrap_owner(store: &AuthStore, email: &str, name: &str) -> CurrentSession {
    let (invite, _) = store
        .create_personal_invitation()
        .await
        .expect("personal invitation");
    store
        .register(Some(&invite), email, name, "password123")
        .await
        .expect("owner registration")
}

#[tokio::test]
async fn app_oauth_codes_require_pkce_and_are_single_use() {
    let store = AuthStore::for_test("admin-password").await;
    store.seed_test_workspace("app-oauth").await;
    let owner = bootstrap_owner(&store, "ada@example.com", "Ada").await;
    sqlx::query(
        "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             VALUES($1, $2, 'owner', $3)",
    )
    .bind("org_app-oauth")
    .bind(&owner.user_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&store.pool)
    .await
    .expect("add app workspace member");
    sqlx::query(
        "INSERT INTO machine_services(\
             service_id, workspace_id, server_id, name, target_host, target_port, protocol, \
             created_at, created_by, updated_at, updated_by\
             ) VALUES($1, $2, $3, $4, $5, $6, $7, $8, $9, $8, $9)",
    )
    .bind("service-mail")
    .bind("app-oauth")
    .bind("machine-a")
    .bind("Mail")
    .bind("127.0.0.1")
    .bind(8788_i64)
    .bind("http")
    .bind(Utc::now().to_rfc3339())
    .bind("user-a")
    .execute(&store.pool)
    .await
    .expect("insert app service");
    let verifier = "v".repeat(64);
    let code = store
        .create_app_oauth_code(
            &AppOAuthGrant {
                workspace_id: "app-oauth".to_string(),
                service_id: "service-mail".to_string(),
                user_id: owner.user_id.clone(),
                preferred_name: "Ada".to_string(),
                role: "owner".to_string(),
            },
            "https://mail.example/api/auth/callback",
            &pkce_challenge(&verifier),
        )
        .await
        .expect("create OAuth code");
    assert!(store
        .consume_app_oauth_code(
            &code,
            "service-mail",
            "https://mail.example/api/auth/callback",
            &"x".repeat(64),
        )
        .await
        .is_err());
    let grant = store
        .consume_app_oauth_code(
            &code,
            "service-mail",
            "https://mail.example/api/auth/callback",
            &verifier,
        )
        .await
        .expect("consume OAuth code");
    assert_eq!(grant.user_id, owner.user_id);
    assert!(store
        .consume_app_oauth_code(
            &code,
            "service-mail",
            "https://mail.example/api/auth/callback",
            &verifier,
        )
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
                server_id: Some("machine-a".to_string()),
                name: "Soul".to_string(),
                command: "python3".to_string(),
                args: vec!["apps/soul/soul.py".to_string()],
                cwd: ".".to_string(),
                port: 9420,
                hostname: "soul.internal".to_string(),
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
        .resolve_virtual_network_host("apps", "soul.internal")
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
    assert_eq!(ingress.service_id, app.service_id);
    assert_eq!(ingress.access, ServiceIngressAccess::Workspace);
    assert!(ingress.hostname.starts_with("soul-"));
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
                server_id: Some("machine-a".to_string()),
                name: "Public Docs".to_string(),
                command: "python3".to_string(),
                args: vec!["-m".to_string(), "http.server".to_string()],
                cwd: ".".to_string(),
                port: 8080,
                hostname: "public-docs.internal".to_string(),
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

    let first = store
        .claim_app_runtime("apps", &app.app_id, None, "appw_first", "reconciler")
        .await
        .expect("claim first runtime")
        .expect("first runtime claim");
    assert_eq!(first.restart_count, 0);
    let second = store
        .claim_app_runtime(
            "apps",
            &app.app_id,
            Some("appw_first"),
            "appw_second",
            "reconciler",
        )
        .await
        .expect("replace runtime")
        .expect("replacement runtime claim");
    assert_eq!(second.restart_count, 1);
    assert_eq!(second.service_id, app.service_id);
    assert_eq!(second.hostname, "soul.internal");

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
        .resolve_virtual_network_host("apps", "soul.internal")
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
async fn agent_launch_profiles_support_crud_validation_and_audit() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("profiles").await;

    let created = store
        .create_agent_launch_profile(
            "profiles",
            ProfileMutationActor {
                kind: "agent",
                id: Some("agent-owner"),
                label: "agent:agent-owner",
            },
            CreateAgentLaunchProfileRequest {
                name: "Reviewer".to_string(),
                description: "Review the current change".to_string(),
                cwd: ".".to_string(),
                command: "codex".to_string(),
                args: vec!["--dangerously-bypass-approvals-and-sandbox".to_string()],
            },
        )
        .await
        .expect("create launch profile");
    assert!(created.profile_id.starts_with("alp_"));
    assert_eq!(created.created_by, "agent:agent-owner");
    assert_eq!(
        store
            .resolve_agent_launch_profile("profiles", "reviewer")
            .await
            .expect("resolve launch profile by name"),
        created
    );
    store.seed_test_workspace("other-workspace").await;
    let cross_workspace = store
        .resolve_agent_launch_profile("other-workspace", &created.profile_id)
        .await
        .expect_err("profiles cannot be resolved across workspaces");
    assert_eq!(cross_workspace.error.code, "launch_profile_not_found");

    let duplicate = store
        .create_agent_launch_profile(
            "profiles",
            ProfileMutationActor {
                kind: "agent",
                id: Some("agent-owner"),
                label: "agent:agent-owner",
            },
            CreateAgentLaunchProfileRequest {
                name: "REVIEWER".to_string(),
                description: String::new(),
                cwd: String::new(),
                command: "claude".to_string(),
                args: Vec::new(),
            },
        )
        .await
        .expect_err("profile names are unique within a workspace");
    assert_eq!(duplicate.error.code, "launch_profile_exists");

    let updated = store
        .update_agent_launch_profile(
            "profiles",
            &created.profile_id,
            ProfileMutationActor {
                kind: "user",
                id: Some("user-owner"),
                label: "user-owner",
            },
            UpdateAgentLaunchProfileRequest {
                name: Some("Code reviewer".to_string()),
                args: Some(vec![
                    "review".to_string(),
                    "--base".to_string(),
                    "main".to_string(),
                ]),
                ..UpdateAgentLaunchProfileRequest::default()
            },
        )
        .await
        .expect("update launch profile");
    assert_eq!(updated.name, "Code reviewer");
    assert_eq!(updated.args, ["review", "--base", "main"]);
    assert_eq!(updated.updated_by, "user-owner");

    let invalid = store
        .update_agent_launch_profile(
            "profiles",
            &created.profile_id,
            ProfileMutationActor {
                kind: "user",
                id: Some("user-owner"),
                label: "user-owner",
            },
            UpdateAgentLaunchProfileRequest {
                args: Some(vec!["bad\0argument".to_string()]),
                ..UpdateAgentLaunchProfileRequest::default()
            },
        )
        .await
        .expect_err("NUL bytes are rejected");
    assert_eq!(invalid.error.code, "invalid_launch_profile");

    store
        .delete_agent_launch_profile(
            "profiles",
            &created.profile_id,
            ProfileMutationActor {
                kind: "user",
                id: Some("user-owner"),
                label: "user-owner",
            },
        )
        .await
        .expect("delete launch profile");
    assert!(store
        .list_agent_launch_profiles("profiles")
        .await
        .expect("list launch profiles")
        .is_empty());

    let actions = sqlx::query_scalar::<_, String>(
        "SELECT action FROM organization_audit_events WHERE workspace_id = $1 ORDER BY sequence",
    )
    .bind("profiles")
    .fetch_all(&store.pool)
    .await
    .expect("list audit actions");
    assert_eq!(
        actions,
        [
            "launch_profile.created",
            "launch_profile.updated",
            "launch_profile.deleted"
        ]
    );
}

#[tokio::test]
async fn schema_initialization_is_idempotent() {
    let store = AuthStore::for_test("owner-password").await;
    store
        .initialize_schema()
        .await
        .expect("repeat schema initialization");
    assert_eq!(
        store.all_workspaces().await.expect("load workspaces").len(),
        0
    );
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
    let admin = store
        .admin_login("owner-password")
        .await
        .expect("admin login");
    assert!(store.admin_session(&admin.token).await.unwrap().is_some());
    let (invite, url) = store
        .create_personal_invitation()
        .await
        .expect("invitation");
    assert!(url.as_str().contains(&invite));
    assert!(url
        .as_str()
        .starts_with("https://app.treer.example/?invite="));

    let registered = store
        .register(Some(&invite), "Alice@Example.com", "Alice", "password123")
        .await
        .expect("registration");
    assert_eq!(registered.email, "alice@example.com");
    assert_eq!(registered.preferred_name, "Alice");
    let organizations = store
        .list_organizations(&registered.user_id)
        .await
        .expect("personal organization");
    assert_eq!(organizations.len(), 1);
    assert_eq!(organizations[0].name, "Alice Personal");
    assert_eq!(organizations[0].role, "owner");
    assert!(store
        .register(Some(&invite), "bob@example.com", "Bob", "password123")
        .await
        .is_err());

    let login = store
        .login("ALICE@EXAMPLE.COM", "password123")
        .await
        .expect("case-insensitive login");
    assert_eq!(login.email, "alice@example.com");
    let updated = store
        .update_profile(&login.user_id, "alicia@example.com", "Alicia")
        .await
        .expect("update profile");
    assert_eq!(updated.preferred_name, "Alicia");
    assert!(store
        .login("alice@example.com", "password123")
        .await
        .is_err());
    assert!(store
        .login("alicia@example.com", "password123")
        .await
        .is_ok());
}

#[tokio::test]
async fn successful_registration_sends_a_welcome_email() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind email API");
    let address = listener.local_addr().expect("email API address");
    let (capture_tx, capture_rx) = oneshot::channel();
    let capture = Arc::new(Mutex::new(Some(capture_tx)));
    let app = Router::new()
        .route("/send", post(capture_email))
        .with_state(capture);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let mut store = AuthStore::for_test("owner-password").await;
    store.email_sender = Some(CloudflareEmailSender {
        client: reqwest::Client::new(),
        endpoint: Url::parse(&format!("http://{address}/send")).expect("email endpoint"),
        api_token: "cloudflare-test-token".into(),
        from: "service@treer.ai".into(),
    });
    let (invite, _) = store
        .create_personal_invitation()
        .await
        .expect("invitation");
    store
        .register(Some(&invite), "Alice@Example.com", "Alice", "password123")
        .await
        .expect("registration");

    let (headers, body) = tokio::time::timeout(StdDuration::from_secs(2), capture_rx)
        .await
        .expect("welcome email timeout")
        .expect("captured welcome email");
    assert_eq!(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer cloudflare-test-token")
    );
    assert_eq!(body["to"], "alice@example.com");
    assert_eq!(body["from"], "service@treer.ai");
    assert_eq!(body["subject"], "Welcome to Treer");
    assert!(body["text"].as_str().expect("text body").contains("Alice"));
    assert!(body["html"].as_str().expect("HTML body").contains("Alice"));
    server.abort();
}

#[tokio::test]
async fn oauth_provider_exchange_uses_verified_provider_profiles() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind OAuth provider");
    let address = listener.local_addr().expect("OAuth provider address");
    let base = format!("http://{address}");
    let app = Router::new()
        .route("/token", post(oauth_token))
        .route("/github/user", get(github_user))
        .route("/github/emails", get(github_emails))
        .route("/google/user", get(google_user));
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let github = OAuthProviderConfig::new(
        "client-id".to_string(),
        "client-secret".to_string(),
        &format!("{base}/authorize"),
        &format!("{base}/token"),
        &format!("{base}/github/user"),
        Some(&format!("{base}/github/emails")),
    )
    .expect("GitHub OAuth config");
    let google = OAuthProviderConfig::new(
        "client-id".to_string(),
        "client-secret".to_string(),
        &format!("{base}/authorize"),
        &format!("{base}/token"),
        &format!("{base}/google/user"),
        None,
    )
    .expect("Google OAuth config");
    let mut store = AuthStore::for_test("owner-password").await;
    store.oauth = Arc::new(OAuthConfig::new(Some(github), Some(google), true));

    let github = store
        .exchange_oauth_code("github", "oauth-code")
        .await
        .expect("GitHub profile");
    assert_eq!(github.subject, "12345");
    assert_eq!(github.email, "octo@example.com");
    assert_eq!(github.preferred_name, "Octo Cat");
    let google = store
        .exchange_oauth_code("google", "oauth-code")
        .await
        .expect("Google profile");
    assert_eq!(google.subject, "google-subject");
    assert_eq!(google.email, "google@example.com");
    assert_eq!(google.preferred_name, "Google User");
    server.abort();
}

#[tokio::test]
async fn oauth_state_is_provider_scoped_and_single_use() {
    let github = OAuthProviderConfig::github("client-id".to_string(), "client-secret".to_string())
        .expect("GitHub OAuth config");
    let mut store = AuthStore::for_test("owner-password").await;
    store.oauth = Arc::new(OAuthConfig::new(Some(github), None, true));
    let authorization = store
        .oauth_authorization_url("github", Some("invite-token"))
        .await
        .expect("authorization URL");
    assert_eq!(authorization.host_str(), Some("github.com"));
    assert!(authorization.query_pairs().any(|(key, value)| {
        key == "redirect_uri"
            && value == "https://proxy.treer.example/api/auth/oauth/github/callback"
    }));
    let state = authorization
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("OAuth state");
    assert!(store.consume_oauth_state("google", &state).await.is_err());
    assert_eq!(
        store
            .consume_oauth_state("github", &state)
            .await
            .expect("consume state")
            .as_deref(),
        Some("invite-token")
    );
    assert!(store.consume_oauth_state("github", &state).await.is_err());
}

#[tokio::test]
async fn oauth_merges_verified_email_and_keeps_stable_provider_identity() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let merged = store
        .complete_oauth_login(
            OAuthProfile {
                provider: "github",
                subject: "github-123".to_string(),
                email: "OWNER@example.com".to_string(),
                preferred_name: "Provider Name".to_string(),
            },
            None,
        )
        .await
        .expect("merge by verified email");
    assert_eq!(merged.user_id, owner.user_id);
    assert_eq!(merged.preferred_name, "Owner");
    assert!(store
        .session(&owner.token)
        .await
        .expect("old session")
        .is_none());
    assert!(store
        .login("owner@example.com", "password123")
        .await
        .is_err());

    let stable = store
        .complete_oauth_login(
            OAuthProfile {
                provider: "github",
                subject: "github-123".to_string(),
                email: "changed@example.com".to_string(),
                preferred_name: "Changed Provider Name".to_string(),
            },
            None,
        )
        .await
        .expect("login by stable provider identity");
    assert_eq!(stable.user_id, owner.user_id);
    assert_eq!(stable.email, "owner@example.com");
    let identity_user: String = sqlx::query_scalar(
        "SELECT user_id FROM oauth_identities WHERE provider = 'github' AND subject = $1",
    )
    .bind("github-123")
    .fetch_one(&store.pool)
    .await
    .expect("linked identity");
    assert_eq!(identity_user, owner.user_id);
}

#[tokio::test]
async fn invitation_switch_controls_new_password_and_oauth_accounts() {
    let mut required = AuthStore::for_test("owner-password").await;
    assert!(required
        .complete_oauth_login(
            OAuthProfile {
                provider: "google",
                subject: "new-google-user".to_string(),
                email: "new@example.com".to_string(),
                preferred_name: "New User".to_string(),
            },
            None,
        )
        .await
        .is_err());

    required.oauth = Arc::new(OAuthConfig::new(None, None, false));
    let oauth_user = required
        .complete_oauth_login(
            OAuthProfile {
                provider: "google",
                subject: "new-google-user".to_string(),
                email: "new@example.com".to_string(),
                preferred_name: "New User".to_string(),
            },
            None,
        )
        .await
        .expect("OAuth registration without invite");
    assert_eq!(
        required
            .list_organizations(&oauth_user.user_id)
            .await
            .expect("OAuth personal organization")[0]
            .name,
        "New User Personal"
    );
    let password_user = required
        .register(None, "password@example.com", "Password User", "password123")
        .await
        .expect("password registration without invite");
    assert_eq!(
        required
            .list_organizations(&password_user.user_id)
            .await
            .expect("password personal organization")[0]
            .name,
        "Password User Personal"
    );
}

#[tokio::test]
async fn password_reset_is_single_use_rate_limited_and_revokes_sessions() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let pending = store
        .create_password_reset("OWNER@example.com")
        .await
        .expect("create password reset")
        .expect("known user reset");
    assert!(pending
        .url
        .as_str()
        .starts_with("https://app.treer.example/?reset="));
    assert!(store
        .create_password_reset("owner@example.com")
        .await
        .expect("rate limit reset")
        .is_none());
    assert!(store
        .create_password_reset("missing@example.com")
        .await
        .expect("unknown email")
        .is_none());

    let token = pending
        .url
        .query_pairs()
        .find_map(|(key, value)| (key == "reset").then(|| value.into_owned()))
        .expect("reset token in URL");
    let stored_hash = sqlx::query_scalar::<_, String>(
        "SELECT secret_hash FROM password_reset_tokens WHERE token_id = $1",
    )
    .bind(&pending.token_id)
    .fetch_one(&store.pool)
    .await
    .expect("stored reset hash");
    assert!(!stored_hash.contains(&token));

    store
        .reset_password(&token, "new-password-123")
        .await
        .expect("reset password");
    assert!(store
        .session(&owner.token)
        .await
        .expect("read old session")
        .is_none());
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

    let pending = store
        .create_password_reset("owner@example.com")
        .await
        .expect("create second password reset")
        .expect("second reset");
    let token = pending
        .url
        .query_pairs()
        .find_map(|(key, value)| (key == "reset").then(|| value.into_owned()))
        .expect("second reset token in URL");
    store
        .update_profile(&owner.user_id, "renamed@example.com", "Owner")
        .await
        .expect("change account email");
    assert!(store
        .reset_password(&token, "newer-password")
        .await
        .is_err());
}

#[tokio::test]
async fn cloudflare_password_reset_email_uses_structured_send_api() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind email API");
    let address = listener.local_addr().expect("email API address");
    let (capture_tx, capture_rx) = oneshot::channel();
    let capture = Arc::new(Mutex::new(Some(capture_tx)));
    let app = Router::new()
        .route("/send", post(capture_email))
        .with_state(capture);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let sender = CloudflareEmailSender {
        client: reqwest::Client::new(),
        endpoint: Url::parse(&format!("http://{address}/send")).expect("email endpoint"),
        api_token: "cloudflare-test-token".into(),
        from: "service@treer.ai".into(),
    };
    let reset_url = Url::parse(
        "https://app.treer.example/?reset=pwd_0123456789abcdef0123456789abcdef.secret&source=test",
    )
    .expect("reset URL");
    sender
        .send_password_reset("owner@example.com", &reset_url)
        .await
        .expect("send reset email");
    let (headers, body) = capture_rx.await.expect("captured email");
    assert_eq!(
        headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer cloudflare-test-token")
    );
    assert_eq!(body["to"], "owner@example.com");
    assert_eq!(body["from"], "service@treer.ai");
    assert_eq!(body["subject"], "Reset your Treer password");
    assert!(body["text"]
        .as_str()
        .expect("text body")
        .contains(reset_url.as_str()));
    assert!(body["html"]
        .as_str()
        .expect("HTML body")
        .contains("&amp;source=test"));
    server.abort();
}

#[tokio::test]
async fn new_workspaces_include_deletable_default_launch_profiles() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let organization = store
        .list_organizations(&owner.user_id)
        .await
        .expect("list organizations")
        .remove(0);
    store
        .create_workspace(
            &organization.organization_id,
            "defaults",
            "Defaults",
            &owner.user_id,
        )
        .await
        .expect("create workspace");

    let profiles = store
        .list_agent_launch_profiles("defaults")
        .await
        .expect("list default profiles");
    assert_eq!(
        profiles
            .iter()
            .map(|profile| (profile.name.as_str(), profile.command.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Claude", "claude"),
            ("Codex", "codex"),
            ("OpenCode", "opencode"),
            ("Pi", "pi"),
        ]
    );
    assert!(profiles.iter().all(|profile| {
        profile.cwd == "." && profile.args.is_empty() && profile.created_by == owner.user_id
    }));

    store
        .delete_agent_launch_profile(
            "defaults",
            "Codex",
            ProfileMutationActor {
                kind: "user",
                id: Some(&owner.user_id),
                label: &owner.preferred_name,
            },
        )
        .await
        .expect("default profile remains deletable");
    assert_eq!(
        store
            .list_agent_launch_profiles("defaults")
            .await
            .expect("list profiles after delete")
            .len(),
        3
    );
}

#[tokio::test]
async fn workspace_deletion_requires_a_manager_and_revokes_credentials() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let organization = store
        .create_organization(&owner.user_id, "Engineering")
        .await
        .expect("create organization");
    store
        .create_workspace(
            &organization.organization_id,
            "ws_deletable",
            "Deletable",
            &owner.user_id,
        )
        .await
        .expect("create workspace");
    let (invite, _) = store
        .create_invitation(&organization.organization_id, &owner.user_id)
        .await
        .expect("invite member");
    let member = store
        .register(Some(&invite), "member@example.com", "Member", "password123")
        .await
        .expect("register member");
    assert!(
        store
            .delete_workspace("ws_deletable", &member.user_id)
            .await
            .is_err(),
        "a plain member cannot delete a workspace"
    );

    let enrollment = store
        .create_machine_enrollment("ws_deletable", &owner.user_id)
        .await
        .expect("create enrollment");
    let machine = store
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim machine");
    sqlx::query(
            "INSERT INTO agent_credentials(agent_id, workspace_id, server_id, secret_hash, created_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind("agent-1")
        .bind("ws_deletable")
        .bind(&machine.server_id)
        .bind("agent-secret-hash")
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("insert agent credential");
    sqlx::query(
        "INSERT INTO machine_names(server_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4)",
    )
    .bind(&machine.server_id)
    .bind("ws_deletable")
    .bind("machine name")
    .bind(Utc::now().to_rfc3339())
    .execute(&store.pool)
    .await
    .expect("insert machine name");
    sqlx::query(
        "INSERT INTO agent_names(agent_id, workspace_id, name, updated_at) \
             VALUES($1, $2, $3, $4)",
    )
    .bind("agent-1")
    .bind("ws_deletable")
    .bind("agent name")
    .bind(Utc::now().to_rfc3339())
    .execute(&store.pool)
    .await
    .expect("insert agent name");
    store
        .create_agent_launch_profile(
            "ws_deletable",
            ProfileMutationActor {
                kind: "user",
                id: Some(&owner.user_id),
                label: &owner.preferred_name,
            },
            CreateAgentLaunchProfileRequest {
                name: "Custom".to_string(),
                description: "".to_string(),
                cwd: ".".to_string(),
                command: "bash".to_string(),
                args: vec![],
            },
        )
        .await
        .expect("create launch profile");

    sqlx::query(
        "INSERT INTO machine_traffic_hourly(\
             workspace_id, window_start, source_server_id, destination_server_id, \
             payload_bytes, payload_frames, updated_at) VALUES($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind("ws_deletable")
    .bind(Utc::now().timestamp())
    .bind(&machine.server_id)
    .bind("peer-machine")
    .bind(128_i64)
    .bind(1_i64)
    .bind(Utc::now().to_rfc3339())
    .execute(&store.pool)
    .await
    .expect("insert traffic history");
    sqlx::query(
            "INSERT INTO traffic_usage_hourly(\
             workspace_id, window_start, traffic_class, source_type, source_id, \
             destination_type, destination_id, payload_bytes, payload_frames, \
             billable_bytes, meter_version, updated_at) \
             VALUES($1, $2, 'service_ingress', 'client', 'browser', 'machine', $3, $4, $5, $4, 1, $6)",
        )
        .bind("ws_deletable")
        .bind(Utc::now().timestamp())
        .bind(&machine.server_id)
        .bind(256_i64)
        .bind(2_i64)
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool)
        .await
        .expect("insert usage history");
    crate::message_store::MessageStore::open(store.pool())
        .await
        .expect("initialize message store");
    sqlx::query(
        "INSERT INTO core_messages(\
             message_id, workspace_id, sender_kind, sender_id, sender_name, body, created_at) \
             VALUES($1, $2, 'agent', $3, $4, $5, $6)",
    )
    .bind("msg-history")
    .bind("ws_deletable")
    .bind("agent-1")
    .bind("Agent One")
    .bind("retained history")
    .bind(Utc::now().to_rfc3339())
    .execute(&store.pool)
    .await
    .expect("insert message history");
    let unused_enrollment = store
        .create_machine_enrollment("ws_deletable", &owner.user_id)
        .await
        .expect("create unused enrollment");

    let blocked = store
        .delete_workspace("ws_deletable", &owner.user_id)
        .await
        .expect_err("an active machine must block workspace deletion");
    let (status, error) = blocked.into_parts();
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error.code, "workspace_has_machines");

    store
        .delete_machine("ws_deletable", &machine.server_id, &["agent-1".to_string()])
        .await
        .expect("delete machine first");

    let deleted = store
        .delete_workspace("ws_deletable", &owner.user_id)
        .await
        .expect("delete workspace");
    assert_eq!(deleted.workspace_id, "ws_deletable");
    assert_eq!(deleted.organization_id, organization.organization_id);
    assert_eq!(deleted.name, "Deletable");
    assert_eq!(deleted.machine_count, 0);
    assert_eq!(deleted.agent_count, 1);
    assert_eq!(deleted.app_count, 0);

    assert!(store
        .list_workspaces(&organization.organization_id, &owner.user_id)
        .await
        .expect("list workspaces")
        .is_empty());
    let retained = sqlx::query(
            "SELECT deleted_at, deleted_by, \
             (SELECT COUNT(*) FROM machines WHERE workspace_id = $1) AS machines, \
             (SELECT COUNT(*) FROM agent_credentials WHERE workspace_id = $1) AS agents, \
             (SELECT COUNT(*) FROM agent_launch_profiles WHERE workspace_id = $1) AS profiles, \
             (SELECT COUNT(*) FROM machine_traffic_hourly WHERE workspace_id = $1) AS legacy_traffic, \
             (SELECT COUNT(*) FROM traffic_usage_hourly WHERE workspace_id = $1) AS traffic, \
             (SELECT COUNT(*) FROM core_messages WHERE workspace_id = $1) AS messages \
             FROM workspaces WHERE workspace_id = $1",
        )
        .bind("ws_deletable")
        .fetch_one(&store.pool)
        .await
        .expect("load retained workspace history");
    assert!(retained.get::<Option<String>, _>("deleted_at").is_some());
    assert_eq!(
        retained.get::<Option<String>, _>("deleted_by"),
        Some(owner.user_id.clone())
    );
    assert_eq!(retained.get::<i64, _>("machines"), 1);
    assert_eq!(retained.get::<i64, _>("agents"), 1);
    assert_eq!(retained.get::<i64, _>("profiles"), 5);
    assert_eq!(retained.get::<i64, _>("legacy_traffic"), 1);
    assert_eq!(retained.get::<i64, _>("traffic"), 1);
    assert_eq!(retained.get::<i64, _>("messages"), 1);
    assert!(store
        .claim_machine_enrollment(&unused_enrollment)
        .await
        .is_err());
    assert!(
        store
            .delete_workspace("ws_deletable", &owner.user_id)
            .await
            .is_err(),
        "a deleted workspace cannot be deleted again"
    );
    let audit_events = store
        .list_audit_events(
            &organization.organization_id,
            &owner.user_id,
            None,
            None,
            100,
        )
        .await
        .expect("owner audit events");
    let deletion = audit_events
        .iter()
        .find(|event| event.action == "workspace.deleted")
        .expect("workspace.deleted audit event");
    assert_eq!(deletion.resource_id, "ws_deletable");
    assert_eq!(deletion.payload["machine_count"], 0);
    assert_eq!(deletion.payload["agent_count"], 1);
}

#[tokio::test]
async fn organization_roles_control_members_and_share_workspaces() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let organization = store
        .create_organization(&owner.user_id, "Engineering")
        .await
        .expect("create organization");
    let renamed = store
        .rename_organization(&organization.organization_id, &owner.user_id, "Product")
        .await
        .expect("rename organization");
    assert_eq!(renamed.name, "Product");
    store
        .create_workspace(
            &organization.organization_id,
            "ws_engineering",
            "Engineering",
            &owner.user_id,
        )
        .await
        .expect("create workspace");
    let (alice_invite, _) = store
        .create_invitation(&organization.organization_id, &owner.user_id)
        .await
        .expect("invite alice");
    let alice = store
        .register(
            Some(&alice_invite),
            "alice@example.com",
            "Alice",
            "password123",
        )
        .await
        .expect("register alice");
    let alice_organizations = store
        .list_organizations(&alice.user_id)
        .await
        .expect("alice organizations");
    assert_eq!(alice_organizations.len(), 1);
    assert_eq!(
        alice_organizations[0].organization_id,
        organization.organization_id
    );
    assert_ne!(alice_organizations[0].name, "Alice Personal");

    let workspaces = store
        .list_workspaces(&organization.organization_id, &alice.user_id)
        .await
        .expect("member workspaces");
    assert_eq!(workspaces[0].workspace_id, "ws_engineering");
    let renamed_workspace = store
        .rename_workspace("ws_engineering", &alice.user_id, "Platform")
        .await
        .expect("members may rename workspaces");
    assert_eq!(renamed_workspace.workspace_id, "ws_engineering");
    assert_eq!(renamed_workspace.name, "Platform");
    assert_eq!(
        store
            .list_workspaces(&organization.organization_id, &alice.user_id)
            .await
            .expect("renamed workspace remains visible")[0]
            .name,
        "Platform"
    );
    store
        .create_workspace(
            &organization.organization_id,
            "ws_product",
            "Product",
            &alice.user_id,
        )
        .await
        .expect("members may create workspaces");
    assert!(store
        .create_invitation(&organization.organization_id, &alice.user_id)
        .await
        .is_err());
    assert!(store
        .list_audit_events(
            &organization.organization_id,
            &alice.user_id,
            Some("ws_engineering"),
            None,
            100,
        )
        .await
        .is_err());

    store
        .update_member_role(
            &organization.organization_id,
            &owner.user_id,
            &alice.user_id,
            "admin",
        )
        .await
        .expect("promote alice");
    let (bob_invite, _) = store
        .create_invitation(&organization.organization_id, &alice.user_id)
        .await
        .expect("admin invite");
    let bob = store
        .register(Some(&bob_invite), "bob@example.com", "Bob", "password123")
        .await
        .expect("register bob");
    store
        .remove_member(&organization.organization_id, &alice.user_id, &bob.user_id)
        .await
        .expect("admin removes member");
    assert!(store
        .remove_member(
            &organization.organization_id,
            &alice.user_id,
            &owner.user_id
        )
        .await
        .is_err());
    let audit_events = store
        .list_audit_events(
            &organization.organization_id,
            &owner.user_id,
            Some("ws_engineering"),
            None,
            100,
        )
        .await
        .expect("owner audit events");
    assert!(audit_events
        .iter()
        .any(|event| event.action == "organization.renamed"));
    assert!(audit_events
        .iter()
        .any(|event| event.action == "workspace.created"));
    assert!(audit_events.iter().any(|event| {
        event.action == "workspace.renamed"
            && event.payload["old_name"] == "Engineering"
            && event.payload["new_name"] == "Platform"
    }));
    assert!(audit_events
        .iter()
        .any(|event| event.action == "member.role_updated"));
    assert!(!serde_json::to_string(&audit_events)
        .expect("serialize audit events")
        .contains(&alice_invite));
}

#[tokio::test]
async fn workspace_access_is_limited_to_organization_members() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let personal = store
        .list_organizations(&owner.user_id)
        .await
        .expect("owner organizations")
        .into_iter()
        .next()
        .expect("personal organization");
    let (invite, _) = store
        .create_invitation(&personal.organization_id, &owner.user_id)
        .await
        .expect("personal organization invite");
    let alice = store
        .register(Some(&invite), "alice@example.com", "Alice", "password123")
        .await
        .expect("register alice");
    let private = store
        .create_organization(&owner.user_id, "Private")
        .await
        .expect("create private organization");
    store
        .create_workspace(
            &private.organization_id,
            "ws_private",
            "Private",
            &owner.user_id,
        )
        .await
        .expect("create private workspace");

    let error = store
        .require_workspace_member("ws_private", &alice.user_id)
        .await
        .expect_err("cross-organization access must fail");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn restricted_workspaces_support_user_and_group_grants() {
    let store = AuthStore::for_test("owner-password").await;
    let owner = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    let organization = store
        .create_organization(&owner.user_id, "Engineering")
        .await
        .expect("create organization");
    store
        .create_workspace(
            &organization.organization_id,
            "ws_restricted",
            "Restricted",
            &owner.user_id,
        )
        .await
        .expect("create workspace");
    let (alice_invite, _) = store
        .create_invitation(&organization.organization_id, &owner.user_id)
        .await
        .expect("invite alice");
    let alice = store
        .register(
            Some(&alice_invite),
            "alice@example.com",
            "Alice",
            "password123",
        )
        .await
        .expect("register alice");
    let (bob_invite, _) = store
        .create_invitation(&organization.organization_id, &owner.user_id)
        .await
        .expect("invite bob");
    let bob = store
        .register(Some(&bob_invite), "bob@example.com", "Bob", "password123")
        .await
        .expect("register bob");

    store
        .update_workspace_access_mode("ws_restricted", &owner.user_id, "restricted")
        .await
        .expect("restrict workspace");
    assert!(store
        .list_workspaces(&organization.organization_id, &alice.user_id)
        .await
        .expect("alice workspace list")
        .is_empty());
    assert_eq!(
        store
            .require_workspace_member("ws_restricted", &alice.user_id)
            .await
            .expect_err("ungranted member must be denied")
            .status,
        StatusCode::FORBIDDEN
    );

    store
        .upsert_workspace_user_grant("ws_restricted", &owner.user_id, &alice.user_id, "member")
        .await
        .expect("grant alice");
    assert_eq!(
        store
            .workspace_member_role("ws_restricted", &alice.user_id)
            .await
            .expect("alice role"),
        "member"
    );
    assert!(store
        .delete_workspace("ws_restricted", &alice.user_id)
        .await
        .is_err());

    let group = store
        .create_organization_group(&organization.organization_id, &owner.user_id, "Reviewers")
        .await
        .expect("create group");
    store
        .set_organization_group_member(
            &organization.organization_id,
            &owner.user_id,
            &group.group_id,
            &bob.user_id,
            true,
        )
        .await
        .expect("add bob to group");
    store
        .upsert_workspace_group_grant("ws_restricted", &owner.user_id, &group.group_id, "member")
        .await
        .expect("grant group");
    assert_eq!(
        store
            .workspace_member_role("ws_restricted", &bob.user_id)
            .await
            .expect("bob group role"),
        "member"
    );

    store
        .upsert_workspace_user_grant("ws_restricted", &owner.user_id, &alice.user_id, "owner")
        .await
        .expect("promote alice");
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
    assert!(ingress.hostname.starts_with("issue-tracker-"));
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
        .create_ingress_auth_code(&ingress, &owner.user_id, "/issues?mine=1")
        .await
        .expect("create authorization code");
    let authorization = store
        .consume_ingress_auth_code(&ingress.hostname, &code)
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
        .consume_ingress_auth_code(&ingress.hostname, &code)
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
async fn logout_invalidates_the_session() {
    let store = AuthStore::for_test("owner-password").await;
    let session = bootstrap_owner(&store, "owner@example.com", "Owner").await;
    assert!(store
        .session(&session.token)
        .await
        .expect("session lookup")
        .is_some());
    store.logout(&session.token).await.expect("logout");
    assert!(store
        .session(&session.token)
        .await
        .expect("session lookup")
        .is_none());
}

#[tokio::test]
async fn bearer_authorization_authenticates_the_session() {
    let store = AuthStore::for_test("owner-password").await;
    let session = bootstrap_owner(&store, "bearer@example.com", "Bearer").await;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", session.token)).expect("bearer header"),
    );
    let authenticated = authenticate_request(&store, &headers)
        .await
        .expect("bearer session");
    assert_eq!(authenticated.user_id, session.user_id);
    assert_eq!(authenticated.email, "bearer@example.com");
}

#[test]
fn native_client_kind_is_exact() {
    let mut headers = HeaderMap::new();
    headers.insert(NATIVE_CLIENT_HEADER, HeaderValue::from_static("mobile_ios"));
    assert_eq!(native_client_kind(&headers), Some("mobile_ios"));
    headers.insert(NATIVE_CLIENT_HEADER, HeaderValue::from_static("browser"));
    assert_eq!(native_client_kind(&headers), None);
}

#[tokio::test]
async fn session_json_includes_token_only_for_native_clients() {
    let store = AuthStore::for_test("owner-password").await;
    let session = bootstrap_owner(&store, "ios@example.com", "iOS").await;
    let mut headers = HeaderMap::new();
    headers.insert(NATIVE_CLIENT_HEADER, HeaderValue::from_static("mobile_ios"));
    let native = session_response(&store, &session, &headers);
    let native_body = axum::body::to_bytes(native.into_body(), usize::MAX)
        .await
        .expect("native body");
    let native_json: Value = serde_json::from_slice(&native_body).expect("native session json");
    assert_eq!(native_json["token"], session.token);
    assert_eq!(native_json["user_id"], session.user_id);

    let browser = session_response(&store, &session, &HeaderMap::new());
    let browser_body = axum::body::to_bytes(browser.into_body(), usize::MAX)
        .await
        .expect("browser body");
    let browser_json: Value = serde_json::from_slice(&browser_body).expect("browser session json");
    assert!(browser_json.get("token").is_none());
    assert_eq!(browser_json["user_id"], session.user_id);
}

#[test]
fn cookie_parser_handles_multiple_values() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        HeaderValue::from_static("theme=dark; treer_session=abc123; other=value"),
    );
    assert_eq!(
        cookie_value(&headers, SESSION_COOKIE).as_deref(),
        Some("abc123")
    );
}

#[tokio::test]
async fn disabled_auth_injects_a_local_user() {
    let mut store = AuthStore::for_test("owner-password").await;
    store.disabled = true;
    let session = authenticate_request(&store, &HeaderMap::new())
        .await
        .expect("local session");
    assert_eq!(session.user_id, "local");
    assert_eq!(session.preferred_name, "Local user");
}

#[tokio::test]
async fn machine_enrollment_is_single_use_and_binds_identity() {
    let store = AuthStore::for_test("owner-password").await;
    store.seed_test_workspace("workspace-a").await;
    let enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create enrollment");
    assert_eq!(
        parse_machine_enrollment_key(&enrollment)
            .expect("parse enrollment")
            .workspace_id,
        "workspace-a"
    );
    let claim = store
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim enrollment");
    assert_eq!(claim.workspace_id, "workspace-a");
    assert!(claim.server_id.starts_with("srv_"));
    assert!(store.claim_machine_enrollment(&enrollment).await.is_err());

    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", claim.machine_token))
            .expect("authorization header"),
    );
    let machine = store
        .authenticate_machine(&headers)
        .await
        .expect("authenticate machine");
    assert!(machine.allows_server("workspace-a", &claim.server_id));
    assert!(!machine.allows_server("workspace-b", &claim.server_id));
    assert!(!machine.allows_server("workspace-a", "srv_other"));

    let workload_credential = store
        .create_agent_credential("workspace-a", &claim.server_id, "agent-a")
        .await
        .expect("create Agent credential");
    assert_eq!(
        store
            .active_agent_ids(
                "workspace-a",
                &claim.server_id,
                &["agent-a".to_string(), "agent-missing".to_string()],
            )
            .await
            .expect("validate startup identities"),
        ["agent-a"]
    );
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    headers.insert(
        WORKLOAD_CREDENTIAL_HEADER,
        HeaderValue::from_str(&workload_credential).expect("workload header"),
    );
    let agent = store
        .authenticate_agent(&machine, &headers)
        .await
        .expect("authenticate Agent")
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

    store
        .delete_agent("workspace-a", "agent-a")
        .await
        .expect("revoke Agent");
    assert!(store
        .active_agent_ids("workspace-a", &claim.server_id, &["agent-a".to_string()])
        .await
        .expect("validate revoked startup identity")
        .is_empty());
}

#[tokio::test]
async fn repeated_enrollment_reuses_installation_identity_and_rotates_credentials() {
    let store = AuthStore::for_test("owner-password").await;
    let installation_id = "mid_0123456789abcdef0123456789abcdef";
    let first_enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create first enrollment");
    let first = store
        .claim_machine_enrollment_for_installation(
            &first_enrollment,
            Some(installation_id),
            Some("Builder one"),
            None,
        )
        .await
        .expect("claim first enrollment");
    let second_enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create second enrollment");
    let second = store
        .claim_machine_enrollment_for_installation(
            &second_enrollment,
            Some(installation_id),
            Some("Builder two"),
            None,
        )
        .await
        .expect("claim second enrollment");

    assert_eq!(first.server_id, second.server_id);
    let machine_count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM machines WHERE workspace_id = $1 AND installation_id = $2",
    )
    .bind("workspace-a")
    .bind(installation_id)
    .fetch_one(&store.pool)
    .await
    .expect("count machines");
    assert_eq!(machine_count, 1);
    let stored_name = sqlx::query_scalar::<_, String>(
        "SELECT name FROM machine_names WHERE workspace_id = $1 AND server_id = $2",
    )
    .bind("workspace-a")
    .bind(&second.server_id)
    .fetch_one(&store.pool)
    .await
    .expect("load machine name");
    assert_eq!(stored_name, "Builder two");

    let mut old_headers = HeaderMap::new();
    old_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", first.machine_token))
            .expect("old authorization"),
    );
    assert!(store.authenticate_machine(&old_headers).await.is_err());
    let mut new_headers = HeaderMap::new();
    new_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", second.machine_token))
            .expect("new authorization"),
    );
    assert!(store.authenticate_machine(&new_headers).await.is_ok());
}

#[tokio::test]
async fn enrollment_recovers_a_revoked_legacy_machine_without_its_old_credential() {
    let store = AuthStore::for_test("owner-password").await;
    let first_enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create legacy enrollment");
    let legacy = store
        .claim_machine_enrollment(&first_enrollment)
        .await
        .expect("claim legacy enrollment");
    sqlx::query("UPDATE machines SET revoked_at = $1 WHERE server_id = $2")
        .bind(Utc::now().to_rfc3339())
        .bind(&legacy.server_id)
        .execute(&store.pool)
        .await
        .expect("revoke legacy machine");

    let installation_id = "mid_abcdef0123456789abcdef0123456789";
    let replacement_enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create replacement enrollment");
    let replacement = store
        .claim_machine_enrollment_for_installation(
            &replacement_enrollment,
            Some(installation_id),
            Some("Recovered builder"),
            Some(&legacy.server_id),
        )
        .await
        .expect("recover revoked machine");

    assert_eq!(replacement.server_id, legacy.server_id);
    let stored_installation = sqlx::query_scalar::<_, String>(
        "SELECT installation_id FROM machines WHERE server_id = $1",
    )
    .bind(&legacy.server_id)
    .fetch_one(&store.pool)
    .await
    .expect("load recovered installation identity");
    assert_eq!(stored_installation, installation_id);

    let mut old_headers = HeaderMap::new();
    old_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", legacy.machine_token))
            .expect("old authorization"),
    );
    assert!(store.authenticate_machine(&old_headers).await.is_err());
    let mut new_headers = HeaderMap::new();
    new_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", replacement.machine_token))
            .expect("new authorization"),
    );
    assert!(store.authenticate_machine(&new_headers).await.is_ok());
}

#[tokio::test]
async fn existing_machine_can_bind_an_installation_identity_for_migration() {
    let store = AuthStore::for_test("owner-password").await;
    let first_enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create legacy enrollment");
    let legacy = store
        .claim_machine_enrollment(&first_enrollment)
        .await
        .expect("claim legacy enrollment");
    let installation_id = "mid_abcdef0123456789abcdef0123456789";
    store
        .bind_machine_identity(
            "workspace-a",
            &legacy.server_id,
            installation_id,
            "Migrated builder",
        )
        .await
        .expect("bind installation identity");
    let replacement = store
        .bind_machine_identity(
            "workspace-a",
            &legacy.server_id,
            "mid_11111111111111111111111111111111",
            "Other builder",
        )
        .await
        .expect_err("installation identity is immutable");
    assert_eq!(replacement.into_parts().1.code, "machine_identity_conflict");

    let second_enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create replacement enrollment");
    let reenrolled = store
        .claim_machine_enrollment_for_installation(
            &second_enrollment,
            Some(installation_id),
            Some("Migrated builder"),
            None,
        )
        .await
        .expect("reenroll migrated machine");
    assert_eq!(reenrolled.server_id, legacy.server_id);
}

#[tokio::test]
async fn machine_authentication_rejects_missing_credentials() {
    let store = AuthStore::for_test("owner-password").await;
    assert!(store.authenticate_machine(&HeaderMap::new()).await.is_err());
}

#[tokio::test]
async fn deleting_machine_revokes_credential_and_cleans_names() {
    let store = AuthStore::for_test("owner-password").await;
    let enrollment = store
        .create_machine_enrollment("workspace-a", "admin")
        .await
        .expect("create enrollment");
    let claim = store
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim enrollment");
    store
        .set_machine_name("workspace-a", &claim.server_id, "builder")
        .await
        .expect("store machine name");
    store
        .set_agent_name("workspace-a", "agent-a", "reviewer")
        .await
        .expect("store agent name");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", claim.machine_token))
            .expect("authorization header"),
    );
    assert!(store
        .machine_is_active("workspace-a", &claim.server_id)
        .await
        .expect("check active machine"));
    assert_eq!(
        store.active_machine_count().await.expect("machine count"),
        1
    );

    store
        .delete_machine("workspace-a", &claim.server_id, &["agent-a".to_string()])
        .await
        .expect("delete machine");

    assert!(store.authenticate_machine(&headers).await.is_err());
    assert!(!store
        .machine_is_active("workspace-a", &claim.server_id)
        .await
        .expect("check revoked machine"));
    assert_eq!(
        store.active_machine_count().await.expect("machine count"),
        0
    );
    let machine_name =
        sqlx::query_scalar::<_, String>("SELECT name FROM machine_names WHERE server_id = $1")
            .bind(&claim.server_id)
            .fetch_optional(&store.pool)
            .await
            .expect("query machine name");
    let agent_name =
        sqlx::query_scalar::<_, String>("SELECT name FROM agent_names WHERE agent_id = $1")
            .bind("agent-a")
            .fetch_optional(&store.pool)
            .await
            .expect("query agent name");
    assert!(machine_name.is_none());
    assert!(agent_name.is_none());
}

#[tokio::test]
async fn persisted_names_are_applied_to_controller_snapshots() {
    let store = AuthStore::for_test("owner-password").await;
    store
        .set_machine_name("workspace-a", "server-a", "build-machine")
        .await
        .expect("store machine name");
    store
        .set_agent_name("workspace-a", "agent-a", "reviewer")
        .await
        .expect("store agent name");
    let now = Utc::now();
    let mut snapshot = AgentServerSnapshot {
        server: ServerInfo {
            server_id: "server-a".to_string(),
            workspace_id: "workspace-a".to_string(),
            name: String::new(),
            hostname: "original-host".to_string(),
            root: "/workspace".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller-test".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host-test".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        },
        agents: vec![AgentInfo {
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
    };

    store
        .apply_server_name(&mut snapshot.server)
        .await
        .expect("apply machine name");
    store
        .apply_agent_names(&mut snapshot)
        .await
        .expect("apply agent names");

    assert_eq!(snapshot.server.name, "build-machine");
    assert_eq!(snapshot.agents[0].name, "reviewer");

    store
        .delete_agent("workspace-a", "agent-a")
        .await
        .expect("persist deletion");
    snapshot.agents[0].name = "original-agent".to_string();
    let deleted = store
        .apply_agent_names(&mut snapshot)
        .await
        .expect("apply deletion");
    assert_eq!(deleted, ["agent-a"]);
    assert!(snapshot.agents.is_empty());
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
