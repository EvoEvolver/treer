//! Provider CLI route tests (#123 part 2) against isolated PostgreSQL and a
//! fake Controller connection. They prove the human-only scope, the
//! consume-then-dispatch ordering, uniform refusals, creator-only polling,
//! output-free audit rows and the never-replay rule.
use super::*;
use crate::auth::ProviderCliScope;
use treer_protocol::{
    CommandResult, ProviderCliInstallAccepted, ProviderCliInstallRequest,
    ProviderCliOperationReport, ProviderCliOperationStatus, ProviderCliPlanStep,
    ProviderCliRecipeRef, ProviderCliRecipeView, ProviderCliRecipesResponse, ProxyMessage,
    CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL,
};

const WORKSPACE: &str = "default";
const MACHINE: &str = "machine-a";
const TABLE_HASH: &str = "859a60731bd8f04707ddc8becfb7dc0cf405a24517385b849053154dbdeee10b";
const CODEX_HASH: &str = "e9e559641f371bb3c403245e97ad6cff629e410175dd9677e19226cc415f7c12";

fn codex_ref() -> ProviderCliRecipeRef {
    ProviderCliRecipeRef {
        recipe_id: "codex-npm-posix".into(),
        recipe_version: 1,
        recipe_hash: CODEX_HASH.into(),
    }
}

fn plan() -> Vec<ProviderCliPlanStep> {
    vec![
        ProviderCliPlanStep::Command {
            step: 1,
            purpose: "pack".into(),
            executable: "/opt/node/bin/npm".into(),
            args: vec!["pack".into(), "@openai/codex@0.163.0".into()],
        },
        ProviderCliPlanStep::Verify {
            step: 2,
            purpose: "integrity".into(),
            algorithm: "sha512".into(),
            expected: "sha512-x".into(),
        },
        ProviderCliPlanStep::Command {
            step: 3,
            purpose: "install".into(),
            executable: "/opt/node/bin/npm".into(),
            args: vec!["install".into(), "--global".into()],
        },
    ]
}

fn recipes_response() -> ProviderCliRecipesResponse {
    let reference = codex_ref();
    ProviderCliRecipesResponse {
        schema_version: 1,
        machine_id: MACHINE.into(),
        platform: "darwin-aarch64".into(),
        controller_source_sha: "c0ffee".into(),
        table_hash: TABLE_HASH.into(),
        recipes: vec![
            ProviderCliRecipeView {
                provider_id: "codex".into(),
                recipe_id: reference.recipe_id,
                recipe_version: 1,
                recipe_hash: reference.recipe_hash,
                platform: "darwin-aarch64".into(),
                binary_names: vec!["codex".into()],
                supported: true,
                official_docs: "https://github.com/openai/codex".into(),
                login_hint: "sign in yourself".into(),
                unsupported_reason: None,
                node_min: Some(22),
                lifecycle_scripts: Some(treer_protocol::ProviderCliLifecycleScripts {
                    enabled: false,
                    review: None,
                }),
                package: None,
                plan: Some(plan()),
            },
            ProviderCliRecipeView {
                provider_id: "claude".into(),
                recipe_id: "claude-native-npm".into(),
                recipe_version: 1,
                recipe_hash: "91ddd665a03dd3873e59c7358e9b21e55c8c4e7e32f4aa4557e891762c84dc0b"
                    .into(),
                platform: "darwin-aarch64".into(),
                binary_names: vec!["claude".into()],
                supported: false,
                official_docs: "https://code.claude.com/docs/en/setup".into(),
                login_hint: "sign in yourself".into(),
                unsupported_reason: Some("lifecycle_review_pending".into()),
                node_min: None,
                lifecycle_scripts: None,
                package: None,
                plan: None,
            },
        ],
    }
}

fn session(user: &str, token: &str) -> CurrentSession {
    CurrentSession {
        token: token.into(),
        user_id: user.into(),
        email: format!("{user}@example.test"),
        preferred_name: user.into(),
    }
}

async fn seeded_auth() -> AuthStore {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace(WORKSPACE).await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('creator','creator@example.test','Creator','fixture','now'),('other','other@example.test','Other','fixture','now'),('outsider','outsider@example.test','Outsider','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    for user in ["creator", "other"] {
        sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,$2,'member','now')")
            .bind(format!("org_{WORKSPACE}")).bind(user).execute(&auth.pool()).await.unwrap();
    }
    auth
}

/// Fake Controller: answers every command with `respond`, counting them.
fn controller(
    state: AppState,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
    respond: impl Fn(&AgentCommand, usize) -> Result<Value, ProtocolError> + Send + 'static,
) -> Arc<std::sync::atomic::AtomicUsize> {
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = seen.clone();
    tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Text(text) = frame else {
                continue;
            };
            let Ok(ProxyMessage::Command { envelope }) = serde_json::from_str(&text) else {
                continue;
            };
            let index = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let result = match respond(&envelope.command, index) {
                Ok(value) => CommandResult::success(envelope.command_id, value),
                Err(error) => CommandResult::failure(envelope.command_id, error),
            };
            state.complete_command(result).await;
        }
    });
    seen
}

async fn connect(
    state: &AppState,
    capabilities: Vec<String>,
) -> (Uuid, tokio::sync::mpsc::UnboundedReceiver<SocketFrame>) {
    let server = state.resolve_server(WORKSPACE, MACHINE).await.unwrap();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let connection_id = Uuid::new_v4();
    state
        .register_server_instance_with_capabilities(
            server,
            connection_id,
            format!("controller-{connection_id}"),
            capabilities,
            tx,
        )
        .await
        .unwrap();
    (connection_id, rx)
}

fn capability() -> Vec<String> {
    vec![CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL.into()]
}

async fn audit_actions(auth: &AuthStore, action: &str) -> Vec<Value> {
    sqlx::query_scalar::<_, Value>(
        "SELECT payload FROM organization_audit_events WHERE action=$1 ORDER BY occurred_at",
    )
    .bind(action)
    .fetch_all(&auth.pool())
    .await
    .unwrap()
}

fn report(
    status: ProviderCliOperationStatus,
    operation_id: &str,
    argv_hash: &str,
) -> ProviderCliOperationReport {
    let terminal = status.is_terminal();
    ProviderCliOperationReport {
        operation_id: operation_id.into(),
        recipe: codex_ref(),
        status,
        step: Some(if terminal { 5 } else { 3 }),
        elapsed_ms: 1200,
        exit_code: terminal.then_some(0),
        timed_out: false,
        stdout: if terminal {
            "codex-cli 0.163.0\n".into()
        } else {
            "secret progress".into()
        },
        stderr: String::new(),
        truncated: false,
        error_code: None,
        steps: terminal.then(Vec::new),
        plan: terminal.then(plan),
        argv_hash: argv_hash.into(),
    }
}

#[tokio::test]
async fn provider_cli_prepare_install_and_poll_round_trip() {
    let auth = seeded_auth().await;
    let state = super::tests::state_with_managed_agent().await;
    let (_, rx) = connect(&state, capability()).await;
    let argv_hash = treer_protocol::plan_argv_hash(&plan());
    let expected_hash = argv_hash.clone();
    let seen = controller(state.clone(), rx, move |command, index| match command {
        AgentCommand::ProviderCliRecipes {} => {
            Ok(serde_json::to_value(recipes_response()).unwrap())
        }
        AgentCommand::ProviderCliInstall {
            operation_id,
            recipe,
            table_hash,
            argv_hash,
        } => {
            assert_eq!(recipe, &codex_ref());
            assert_eq!(table_hash, TABLE_HASH);
            assert_eq!(argv_hash, &expected_hash);
            Ok(serde_json::to_value(ProviderCliInstallAccepted {
                operation_id: operation_id.clone(),
                status: ProviderCliOperationStatus::Running,
                argv_hash: argv_hash.clone(),
            })
            .unwrap())
        }
        AgentCommand::ProviderCliOperation { operation_id } => {
            // First poll: still running (no output). Later polls: finished.
            let status = if index < 4 {
                ProviderCliOperationStatus::Running
            } else {
                ProviderCliOperationStatus::Succeeded
            };
            Ok(serde_json::to_value(report(status, operation_id, &expected_hash)).unwrap())
        }
        AgentCommand::ProviderCliCheck { recipe } => {
            assert_eq!(recipe, &codex_ref());
            Ok(
                json!({"installed":true,"found_in_prefix":true,"version":"codex-cli 0.163.0","exit_code":0,"timed_out":false,"stdout":"codex-cli 0.163.0\n","stderr":"","truncated":false}),
            )
        }
        other => panic!("unexpected command {other:?}"),
    });
    let creator = session("creator", "session-one");
    let policy = PolicyEngine::allow_all();

    let recipes = provider_cli_recipes(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into())),
    )
    .await
    .unwrap();
    assert_eq!(recipes.0.table_hash, TABLE_HASH);
    assert_eq!(recipes.0.recipes.len(), 2);

    let prepared = provider_cli_prepare(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into())),
        Json(codex_ref()),
    )
    .await
    .unwrap()
    .0;
    assert!(prepared.confirmation_token.starts_with("pcl_"));
    assert_eq!(prepared.recipe.plan.as_deref(), Some(plan().as_slice()));
    assert_eq!(prepared.machine_id, MACHINE);
    let stored: (String, String) =
        sqlx::query_as("SELECT token_hash, argv_hash FROM provider_cli_install_tokens")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_ne!(
        stored.0, prepared.confirmation_token,
        "only the hash is stored"
    );
    assert_eq!(stored.1, argv_hash);
    let prepare_audit = audit_actions(&auth, "provider_cli.prepare").await;
    assert_eq!(prepare_audit.len(), 1);
    assert!(prepare_audit[0].to_string().contains(CODEX_HASH));
    assert!(!prepare_audit[0]
        .to_string()
        .contains(&prepared.confirmation_token[4..]));

    let installed = provider_cli_install(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into())),
        Json(ProviderCliInstallRequest {
            confirmation_token: prepared.confirmation_token.clone(),
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(installed.status, ProviderCliOperationStatus::Running);
    assert!(installed.operation_id.starts_with("pco_"));
    // While the job runs the machine is busy; afterwards the consumed token
    // is still refused: the same confirmation never installs twice.
    let busy = provider_cli_install(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into())),
        Json(ProviderCliInstallRequest {
            confirmation_token: prepared.confirmation_token.clone(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(busy.status, StatusCode::CONFLICT);
    assert_eq!(busy.error.code, "provider_cli_machine_busy");
    let accepted = audit_actions(&auth, "provider_cli.install.accepted").await;
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["operation_id"], installed.operation_id);
    assert_eq!(accepted[0]["argv_hash"], argv_hash);

    let poll = |user: CurrentSession, id: String| {
        let (state, auth, policy) = (state.clone(), auth.clone(), policy.clone());
        async move {
            provider_cli_operation(
                State(state),
                Extension(auth),
                Extension(policy),
                Extension(user),
                Path((WORKSPACE.into(), MACHINE.into(), id)),
            )
            .await
        }
    };
    // Running: status only, no output tail, poll hint present.
    let running = poll(creator.clone(), installed.operation_id.clone())
        .await
        .unwrap()
        .0;
    assert_eq!(running.status, ProviderCliOperationStatus::Running);
    assert_eq!(running.step, Some(3));
    assert!(
        running.stdout.is_empty(),
        "non-terminal responses expose no output"
    );
    assert_eq!(running.poll_after_ms, Some(1000));
    // Another user, even a member, gets the uniform 404.
    let foreign = poll(
        session("other", "session-other"),
        installed.operation_id.clone(),
    )
    .await
    .unwrap_err();
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(foreign.error.code, "provider_cli_operation_not_found");
    let unknown_id = poll(
        creator.clone(),
        "pco_00000000000000000000000000000000".into(),
    )
    .await
    .unwrap_err();
    assert_eq!(unknown_id.error.code, "provider_cli_operation_not_found");
    // Terminal from another session of the same creator: output arrives.
    let finished = poll(
        session("creator", "session-two"),
        installed.operation_id.clone(),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(finished.status, ProviderCliOperationStatus::Succeeded);
    assert_eq!(finished.exit_code, Some(0));
    assert!(finished.stdout.contains("codex-cli 0.163.0"));
    assert_eq!(finished.poll_after_ms, None);
    let again = poll(creator.clone(), installed.operation_id.clone())
        .await
        .unwrap()
        .0;
    assert_eq!(again.status, ProviderCliOperationStatus::Succeeded);
    let reused = provider_cli_install(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into())),
        Json(ProviderCliInstallRequest {
            confirmation_token: prepared.confirmation_token.clone(),
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (reused.status, reused.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_token_invalid")
    );
    let finished_audit = audit_actions(&auth, "provider_cli.install.finished").await;
    assert_eq!(finished_audit.len(), 1, "finished is audited once");
    assert_eq!(finished_audit[0]["status"], "succeeded");
    assert_eq!(finished_audit[0]["exit_code"], 0);
    let all_audit = sqlx::query_scalar::<_, String>(
        "SELECT payload::text FROM organization_audit_events WHERE action LIKE 'provider_cli.%'",
    )
    .fetch_all(&auth.pool())
    .await
    .unwrap()
    .join("\n");
    assert!(
        !all_audit.contains("codex-cli 0.163.0") && !all_audit.contains("secret progress"),
        "output never enters audit"
    );
    let pg_status: (String, Option<String>) = sqlx::query_as(
        "SELECT status, argv_hash FROM provider_cli_operations WHERE operation_id=$1",
    )
    .bind(&installed.operation_id)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(pg_status, ("succeeded".into(), Some(argv_hash.clone())));

    let check = provider_cli_check(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into())),
        Json(codex_ref()),
    )
    .await
    .unwrap()
    .0;
    assert!(check.installed && check.found_in_prefix);
    let check_audit = audit_actions(&auth, "provider_cli.check").await;
    assert_eq!(check_audit.len(), 1);
    assert_eq!(check_audit[0]["installed"], true);
    assert!(check_audit[0].get("stdout").is_none());
    assert!(seen.load(std::sync::atomic::Ordering::SeqCst) >= 6);
}

#[tokio::test]
async fn provider_cli_refuses_before_consuming_tokens_and_limits_prepares() {
    let auth = seeded_auth().await;
    let state = super::tests::state_with_managed_agent().await;
    let (_, rx) = connect(&state, capability()).await;
    let seen = controller(state.clone(), rx, |command, _| match command {
        AgentCommand::ProviderCliRecipes {} => {
            Ok(serde_json::to_value(recipes_response()).unwrap())
        }
        AgentCommand::ProviderCliInstall { .. } => Err(ProtocolError::new(
            "provider_cli_table_mismatch",
            "fixture table changed",
        )),
        other => panic!("unexpected command {other:?}"),
    });
    let creator = session("creator", "session-one");
    let allow = PolicyEngine::allow_all();
    let prepare = |user: CurrentSession, policy: PolicyEngine, reference: ProviderCliRecipeRef| {
        let (state, auth) = (state.clone(), auth.clone());
        async move {
            provider_cli_prepare(
                State(state),
                Extension(auth),
                Extension(policy),
                Extension(user),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(reference),
            )
            .await
        }
    };
    let install = |user: CurrentSession, token: String| {
        let (state, auth, policy) = (state.clone(), auth.clone(), allow.clone());
        async move {
            provider_cli_install(
                State(state),
                Extension(auth),
                Extension(policy),
                Extension(user),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(ProviderCliInstallRequest {
                    confirmation_token: token,
                }),
            )
            .await
        }
    };

    // Policy denial and non-membership stop before any Controller command.
    let denied = PolicyEngine::new(
        crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
            "policy_denied",
            "denied by fixture",
        )),
        vec![],
    );
    let error = prepare(creator.clone(), denied, codex_ref())
        .await
        .unwrap_err();
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    let error = prepare(session("outsider", "s"), allow.clone(), codex_ref())
        .await
        .unwrap_err();
    assert!(
        matches!(error.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{error:?}"
    );
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 0);

    // Malformed, changed and unsupported recipe references.
    let mut malformed = codex_ref();
    malformed.recipe_hash = "nope".into();
    let error = prepare(creator.clone(), allow.clone(), malformed)
        .await
        .unwrap_err();
    assert_eq!(error.error.code, "provider_cli_recipe_invalid");
    let mut changed = codex_ref();
    changed.recipe_hash = "0".repeat(64);
    let error = prepare(creator.clone(), allow.clone(), changed)
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_recipe_mismatch")
    );
    let error = prepare(
        creator.clone(),
        allow.clone(),
        ProviderCliRecipeRef {
            recipe_id: "claude-native-npm".into(),
            recipe_version: 1,
            recipe_hash: "91ddd665a03dd3873e59c7358e9b21e55c8c4e7e32f4aa4557e891762c84dc0b".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::BAD_REQUEST, "provider_cli_recipe_unsupported")
    );
    assert!(audit_actions(&auth, "provider_cli.prepare")
        .await
        .is_empty());

    // Four live tokens per user and machine; the fifth is refused.
    let mut tokens = Vec::new();
    for _ in 0..4 {
        tokens.push(
            prepare(creator.clone(), allow.clone(), codex_ref())
                .await
                .unwrap()
                .0
                .confirmation_token,
        );
    }
    let error = prepare(creator.clone(), allow.clone(), codex_ref())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "provider_cli_tokens_exhausted"
        )
    );
    // Another member has their own allowance.
    let _ = prepare(session("other", "o"), allow.clone(), codex_ref())
        .await
        .unwrap();

    let live_tokens = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM provider_cli_install_tokens WHERE consumed_at IS NULL AND user_id='creator'")
            .fetch_one(&auth.pool()).await.unwrap()
    };
    assert_eq!(live_tokens().await, 4);

    // Wrong session, other user, malformed token: one uniform 409, nothing consumed.
    for (user, token) in [
        (session("creator", "session-two"), tokens[0].clone()),
        (session("other", "o"), tokens[0].clone()),
        (creator.clone(), "pcl_not-a-token".to_owned()),
        (creator.clone(), format!("ses_{}", &tokens[0][4..])),
    ] {
        let error = install(user, token).await.unwrap_err();
        assert_eq!(
            (error.status, error.error.code.as_str()),
            (StatusCode::CONFLICT, "provider_cli_token_invalid")
        );
    }
    assert_eq!(live_tokens().await, 4);

    // Tokens are bound to workspace and machine at the store level too.
    let scope = |workspace: &'static str, server: &'static str| ProviderCliScope {
        workspace_id: workspace,
        server_id: server,
        user_id: "creator",
        session_token: "session-one",
    };
    for (workspace, server) in [("other", MACHINE), (WORKSPACE, "machine-b")] {
        let error = auth
            .provider_cli_accept(&scope(workspace, server), &tokens[0])
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            "provider_cli_token_invalid",
            "{workspace}/{server}"
        );
    }
    assert_eq!(live_tokens().await, 4);

    // Ten prepares per minute per user and machine, by database time.
    sqlx::query("UPDATE provider_cli_install_tokens SET consumed_at=CURRENT_TIMESTAMP WHERE user_id='creator'")
        .execute(&auth.pool()).await.unwrap();
    for n in 0..6 {
        sqlx::query("INSERT INTO provider_cli_install_tokens(token_hash, workspace_id, server_id, user_id, session_hash, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, expires_at, consumed_at) VALUES($1,$2,$3,'creator','s','codex-npm-posix',1,$4,$5,'c0ffee','h', CURRENT_TIMESTAMP + INTERVAL '2 minutes', CURRENT_TIMESTAMP)")
            .bind(format!("filler-{n}")).bind(WORKSPACE).bind(MACHINE).bind(CODEX_HASH).bind(TABLE_HASH)
            .execute(&auth.pool()).await.unwrap();
    }
    let error = prepare(creator.clone(), allow.clone(), codex_ref())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "provider_cli_rate_limited")
    );
    sqlx::query("UPDATE provider_cli_install_tokens SET created_at = CURRENT_TIMESTAMP - INTERVAL '2 minutes', consumed_at = NULL WHERE user_id='creator' AND token_hash NOT LIKE 'filler-%'")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("DELETE FROM provider_cli_install_tokens WHERE token_hash LIKE 'filler-%'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert_eq!(live_tokens().await, 4);

    // A busy machine is refused before consumption.
    sqlx::query("INSERT INTO provider_cli_operations(operation_id, workspace_id, server_id, created_by, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, status) VALUES('pco_busy', $1, $2, 'other', 'codex-npm-posix', 1, $3, $4, 'c0ffee', 'h', 'running')")
        .bind(WORKSPACE).bind(MACHINE).bind(CODEX_HASH).bind(TABLE_HASH)
        .execute(&auth.pool()).await.unwrap();
    let error = install(creator.clone(), tokens[0].clone())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_machine_busy")
    );
    assert_eq!(live_tokens().await, 4);
    sqlx::query("DELETE FROM provider_cli_operations WHERE operation_id='pco_busy'")
        .execute(&auth.pool())
        .await
        .unwrap();

    // The Controller refusing (table changed) fails the operation; the token
    // is consumed because the confirmation no longer describes this machine.
    let error = install(creator.clone(), tokens[0].clone())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_table_mismatch")
    );
    assert_eq!(live_tokens().await, 3);
    let failed: (String, Option<String>) = sqlx::query_as(
        "SELECT status, error_code FROM provider_cli_operations ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(
        failed,
        ("failed".into(), Some("provider_cli_table_mismatch".into()))
    );

    // An old Controller: 501 before consumption; offline: 503 before consumption.
    let (old_id, old_rx) = connect(&state, vec![]).await;
    drop(old_rx);
    let error = install(creator.clone(), tokens[1].clone())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::NOT_IMPLEMENTED, "provider_cli_unsupported")
    );
    state.disconnect_server(WORKSPACE, MACHINE, old_id).await;
    let error = install(creator.clone(), tokens[1].clone())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_cli_machine_offline"
        )
    );
    assert_eq!(live_tokens().await, 3);
    // Expired tokens are refused like consumed ones.
    sqlx::query("UPDATE provider_cli_install_tokens SET expires_at = CURRENT_TIMESTAMP - INTERVAL '1 second'")
        .execute(&auth.pool()).await.unwrap();
    let (_, rx) = connect(&state, capability()).await;
    controller(state.clone(), rx, |_, _| {
        panic!("expired token must not dispatch")
    });
    let error = install(creator.clone(), tokens[2].clone())
        .await
        .unwrap_err();
    assert_eq!(error.error.code, "provider_cli_token_invalid");
}

#[tokio::test]
async fn provider_cli_lost_dispatch_becomes_unknown_and_is_never_replayed() {
    let auth = seeded_auth().await;
    let state = super::tests::state_with_managed_agent().await;
    let (first_id, mut rx) = connect(&state, capability()).await;
    let creator = session("creator", "session-one");
    let policy = PolicyEngine::allow_all();
    // Recipes answered by hand so the install frame can be intercepted.
    let prepare = tokio::spawn({
        let (state, auth, policy, creator) =
            (state.clone(), auth.clone(), policy.clone(), creator.clone());
        async move {
            provider_cli_prepare(
                State(state),
                Extension(auth),
                Extension(policy),
                Extension(creator),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(codex_ref()),
            )
            .await
            .unwrap()
            .0
        }
    });
    let envelope = super::tests::next_lifecycle_command(&mut rx).await;
    assert!(matches!(
        envelope.command,
        AgentCommand::ProviderCliRecipes {}
    ));
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            serde_json::to_value(recipes_response()).unwrap(),
        ))
        .await;
    let prepared = prepare.await.unwrap();

    let install = tokio::spawn({
        let (state, auth, policy, creator) =
            (state.clone(), auth.clone(), policy.clone(), creator.clone());
        let token = prepared.confirmation_token.clone();
        async move {
            provider_cli_install(
                State(state),
                Extension(auth),
                Extension(policy),
                Extension(creator),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(ProviderCliInstallRequest {
                    confirmation_token: token,
                }),
            )
            .await
        }
    });
    // The install frame left the Proxy; the Controller then reconnects as a
    // new instance before answering. Nothing may be replayed to it.
    let envelope = super::tests::next_lifecycle_command(&mut rx).await;
    let AgentCommand::ProviderCliInstall { operation_id, .. } = &envelope.command else {
        panic!("expected install dispatch, got {:?}", envelope.command);
    };
    let operation_id = operation_id.clone();
    let (second_id, mut second_rx) = connect(&state, capability()).await;
    let server = state.resolve_server(WORKSPACE, MACHINE).await.unwrap();
    state
        .apply_snapshot(
            second_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![],
            },
        )
        .await
        .unwrap();
    assert_ne!(first_id, second_id);
    let outcome = install.await.unwrap().unwrap().0;
    assert_eq!(outcome.operation_id, operation_id);
    assert_eq!(outcome.status, ProviderCliOperationStatus::Unknown);
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, error_code FROM provider_cli_operations WHERE operation_id=$1",
    )
    .bind(&operation_id)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(row, ("unknown".into(), Some("dispatch_unconfirmed".into())));
    // Drain everything the replacement receives for a fixed period: no frame
    // may be an install dispatch.
    let drain_until = tokio::time::Instant::now() + Duration::from_millis(500);
    while let Ok(Some(frame)) = tokio::time::timeout_at(drain_until, second_rx.recv()).await {
        if let SocketFrame::Text(text) = frame {
            if let Ok(ProxyMessage::Command { envelope }) =
                serde_json::from_str::<ProxyMessage>(&text)
            {
                assert!(
                    !matches!(envelope.command, AgentCommand::ProviderCliInstall { .. }),
                    "install must never be replayed to a replacement Controller"
                );
            }
        }
    }
    let finished = audit_actions(&auth, "provider_cli.install.finished").await;
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0]["status"], "unknown");

    // The machine's own later observation resolves unknown; the Proxy records it.
    let argv_hash = treer_protocol::plan_argv_hash(&plan());
    controller(state.clone(), second_rx, move |command, _| match command {
        AgentCommand::ProviderCliOperation { operation_id } => Ok(serde_json::to_value(report(
            ProviderCliOperationStatus::Succeeded,
            operation_id,
            &argv_hash,
        ))
        .unwrap()),
        other => panic!("unexpected {other:?}"),
    });
    let resolved = provider_cli_operation(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into(), operation_id.clone())),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(resolved.status, ProviderCliOperationStatus::Succeeded);
    assert_eq!(
        audit_actions(&auth, "provider_cli.install.finished")
            .await
            .len(),
        2
    );
}

#[tokio::test]
async fn provider_cli_concurrent_accepts_consume_exactly_one_token() {
    let auth = seeded_auth().await;
    let state = super::tests::state_with_managed_agent().await;
    let (_, rx) = connect(&state, capability()).await;
    controller(state.clone(), rx, |command, _| match command {
        AgentCommand::ProviderCliRecipes {} => {
            Ok(serde_json::to_value(recipes_response()).unwrap())
        }
        other => panic!("unexpected command {other:?}"),
    });
    let creator = session("creator", "session-one");
    let mut tokens = Vec::new();
    for _ in 0..2 {
        tokens.push(
            provider_cli_prepare(
                State(state.clone()),
                Extension(auth.clone()),
                Extension(PolicyEngine::allow_all()),
                Extension(creator.clone()),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(codex_ref()),
            )
            .await
            .unwrap()
            .0
            .confirmation_token,
        );
    }
    let scope = ProviderCliScope {
        workspace_id: WORKSPACE,
        server_id: MACHINE,
        user_id: "creator",
        session_token: "session-one",
    };
    // Deterministic race: an uncommitted competing insert is invisible to the
    // busy check (READ COMMITTED) but blocks the unique index, so the accept
    // passes the busy branch, waits, then loses on the index after the
    // competitor commits; its whole transaction rolls back and the token stays.
    let mut competitor = auth.pool().begin().await.unwrap();
    sqlx::query("INSERT INTO provider_cli_operations(operation_id, workspace_id, server_id, created_by, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, status) VALUES('pco_competitor', $1, $2, 'other', 'codex-npm-posix', 1, $3, $4, 'c0ffee', 'h', 'queued')")
        .bind(WORKSPACE).bind(MACHINE).bind(CODEX_HASH).bind(TABLE_HASH)
        .execute(&mut *competitor).await.unwrap();
    let racing = tokio::spawn({
        let (auth, token) = (auth.clone(), tokens[0].clone());
        async move {
            let scope = ProviderCliScope {
                workspace_id: WORKSPACE,
                server_id: MACHINE,
                user_id: "creator",
                session_token: "session-one",
            };
            auth.provider_cli_accept(&scope, &token).await
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !racing.is_finished(),
        "the accept must be blocked on the unique index, not refused early"
    );
    competitor.commit().await.unwrap();
    let loser = racing.await.unwrap().unwrap_err();
    assert_eq!(loser.code(), "provider_cli_machine_busy");
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_cli_install_tokens WHERE consumed_at IS NULL",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(live, 2, "the losing token stays usable");
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_cli_operations WHERE status IN ('queued','running')",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(active, 1);
    // Without a competitor the same token is accepted once the slot is free.
    sqlx::query("DELETE FROM provider_cli_operations WHERE operation_id='pco_competitor'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert!(auth.provider_cli_accept(&scope, &tokens[0]).await.is_ok());
    assert_eq!(
        auth.provider_cli_accept(&scope, &tokens[1])
            .await
            .unwrap_err()
            .code(),
        "provider_cli_machine_busy"
    );
}

/// A silent Controller or an accepted-but-undispatched orphan is `unknown`
/// only past the grace period, measured from the last report (or dispatch, or
/// acceptance). Time is moved in PostgreSQL rather than by waiting.
#[tokio::test]
async fn provider_cli_silence_past_grace_is_unknown_and_releases_the_machine() {
    let auth = seeded_auth().await;
    let state = super::tests::state_with_managed_agent().await;
    let creator = session("creator", "session-one");
    let insert = |id: &'static str,
                  status: &'static str,
                  created: &'static str,
                  dispatched: Option<&'static str>,
                  reported: Option<&'static str>| {
        let auth = auth.clone();
        async move {
            sqlx::query(&format!("INSERT INTO provider_cli_operations(operation_id, workspace_id, server_id, created_by, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, status, created_at, dispatched_at, reported_at) VALUES($1,$2,$3,'creator','codex-npm-posix',1,$4,$5,'c0ffee','h',$6, CURRENT_TIMESTAMP - INTERVAL '{created}', {}, {})",
                dispatched.map(|d| format!("CURRENT_TIMESTAMP - INTERVAL '{d}'")).unwrap_or("NULL".into()),
                reported.map(|r| format!("CURRENT_TIMESTAMP - INTERVAL '{r}'")).unwrap_or("NULL".into())))
                .bind(id).bind(WORKSPACE).bind(MACHINE).bind(CODEX_HASH).bind(TABLE_HASH).bind(status)
                .execute(&auth.pool()).await.unwrap();
        }
    };
    let poll = |id: &'static str| {
        let (state, auth, creator) = (state.clone(), auth.clone(), creator.clone());
        async move {
            provider_cli_operation(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::allow_all()),
                Extension(creator),
                Path((WORKSPACE.into(), MACHINE.into(), id.into())),
            )
            .await
            .unwrap()
            .0
        }
    };
    let status_of = |id: &'static str| {
        let auth = auth.clone();
        async move {
            sqlx::query_as::<_, (String, Option<String>)>(
                "SELECT status, error_code FROM provider_cli_operations WHERE operation_id=$1",
            )
            .bind(id)
            .fetch_one(&auth.pool())
            .await
            .unwrap()
        }
    };
    // Controller offline throughout. One active row at a time (unique index).
    // ACK received eleven minutes ago, nothing since.
    insert(
        "pco_acked",
        "running",
        "12 minutes",
        Some("11 minutes"),
        Some("11 minutes"),
    )
    .await;
    let response = poll("pco_acked").await;
    assert_eq!(response.status, ProviderCliOperationStatus::Unknown);
    assert_eq!(
        response.error_code.as_deref(),
        Some("controller_unreachable")
    );
    assert_eq!(
        status_of("pco_acked").await,
        ("unknown".into(), Some("controller_unreachable".into()))
    );
    // One progress report nine minutes ago: still inside the grace.
    insert(
        "pco_recent",
        "running",
        "30 minutes",
        Some("29 minutes"),
        Some("9 minutes"),
    )
    .await;
    let response = poll("pco_recent").await;
    assert_eq!(response.status, ProviderCliOperationStatus::Running);
    assert_eq!(response.poll_after_ms, Some(1000));
    assert_eq!(status_of("pco_recent").await.0, "running");
    sqlx::query("UPDATE provider_cli_operations SET reported_at = CURRENT_TIMESTAMP - INTERVAL '11 minutes' WHERE operation_id='pco_recent'")
        .execute(&auth.pool()).await.unwrap();
    assert_eq!(
        poll("pco_recent").await.status,
        ProviderCliOperationStatus::Unknown
    );
    // Accepted but never dispatched (Proxy crashed before the frame): expires too.
    insert("pco_orphan", "queued", "11 minutes", None, None).await;
    let response = poll("pco_orphan").await;
    assert_eq!(response.status, ProviderCliOperationStatus::Unknown);
    assert_eq!(
        response.error_code.as_deref(),
        Some("controller_unreachable")
    );
    // A fresh orphan is not misjudged.
    insert("pco_fresh", "queued", "1 minute", None, None).await;
    assert_eq!(
        poll("pco_fresh").await.status,
        ProviderCliOperationStatus::Queued
    );
    // The machine slot is free again for every unknown row.
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_cli_operations WHERE status IN ('queued','running')",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(active, 1, "only the fresh queued row holds the slot");
    assert_eq!(
        audit_actions(&auth, "provider_cli.install.finished")
            .await
            .len(),
        3
    );
}

/// A stale active row older than the output retention must not hold the
/// machine slot forever: polling returns 410 for the output but first makes
/// the row unknown (audited once), and accepting a new token reconciles even
/// without any poll.
#[tokio::test]
async fn provider_cli_stale_rows_reconcile_past_retention_and_free_the_slot() {
    let auth = seeded_auth().await;
    let state = super::tests::state_with_managed_agent().await;
    let creator = session("creator", "session-one");
    let scope = ProviderCliScope {
        workspace_id: WORKSPACE,
        server_id: MACHINE,
        user_id: "creator",
        session_token: "session-one",
    };
    let stale = |id: &'static str, age: &'static str| {
        let auth = auth.clone();
        async move {
            sqlx::query(&format!("INSERT INTO provider_cli_operations(operation_id, workspace_id, server_id, created_by, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, status, created_at, dispatched_at, reported_at) VALUES($1,$2,$3,'creator','codex-npm-posix',1,$4,$5,'c0ffee','h','running', CURRENT_TIMESTAMP - INTERVAL '{age}', CURRENT_TIMESTAMP - INTERVAL '{age}', CURRENT_TIMESTAMP - INTERVAL '{age}')"))
                .bind(id).bind(WORKSPACE).bind(MACHINE).bind(CODEX_HASH).bind(TABLE_HASH)
                .execute(&auth.pool()).await.unwrap();
        }
    };
    let token = |name: &'static str| {
        let auth = auth.clone();
        async move {
            let plaintext = format!("pcl_{}", name.repeat(64 / name.len()));
            sqlx::query("INSERT INTO provider_cli_install_tokens(token_hash, workspace_id, server_id, user_id, session_hash, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, expires_at) VALUES($1,$2,$3,'creator',$4,'codex-npm-posix',1,$5,$6,'c0ffee','h', CURRENT_TIMESTAMP + INTERVAL '2 minutes')")
                .bind(crate::auth::sha256_hex(&plaintext)).bind(WORKSPACE).bind(MACHINE).bind(crate::auth::sha256_hex("session-one")).bind(CODEX_HASH).bind(TABLE_HASH)
                .execute(&auth.pool()).await.unwrap();
            plaintext
        }
    };
    // Controller offline, no poll for more than a day.
    stale("pco_old", "25 hours").await;
    let error = provider_cli_operation(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into(), "pco_old".into())),
    )
    .await
    .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::GONE, "provider_cli_output_expired")
    );
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, error_code FROM provider_cli_operations WHERE operation_id='pco_old'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(
        row,
        ("unknown".into(), Some("controller_unreachable".into()))
    );
    assert_eq!(
        audit_actions(&auth, "provider_cli.install.finished")
            .await
            .len(),
        1
    );
    let accepted = auth
        .provider_cli_accept(&scope, &token("a").await)
        .await
        .unwrap();
    assert_eq!(accepted.status, ProviderCliOperationStatus::Queued);
    sqlx::query("UPDATE provider_cli_operations SET status='failed', finished_at=CURRENT_TIMESTAMP WHERE operation_id=$1")
        .bind(&accepted.operation_id).execute(&auth.pool()).await.unwrap();
    // Accept alone reconciles a stale row that nobody polled.
    stale("pco_silent", "26 hours").await;
    let accepted = auth
        .provider_cli_accept(&scope, &token("c").await)
        .await
        .unwrap();
    assert_eq!(accepted.status, ProviderCliOperationStatus::Queued);
    let silent: (String, Option<String>) = sqlx::query_as(
        "SELECT status, error_code FROM provider_cli_operations WHERE operation_id='pco_silent'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(
        silent,
        ("unknown".into(), Some("controller_unreachable".into()))
    );
    assert_eq!(
        audit_actions(&auth, "provider_cli.install.finished")
            .await
            .len(),
        2,
        "each stale row is audited once"
    );
    let actors: Vec<(String, Option<String>, Value)> = sqlx::query_as("SELECT actor_kind, actor_id, payload FROM organization_audit_events WHERE action='provider_cli.install.finished' ORDER BY occurred_at")
        .fetch_all(&auth.pool()).await.unwrap();
    for (kind, id, payload) in &actors {
        assert_eq!(
            (kind.as_str(), id.as_deref()),
            ("system", None),
            "reconciliation is a control-plane event"
        );
        assert_eq!(payload["created_by"], "creator");
        assert_eq!(payload["reconciled_during_request_by"], "creator");
    }
    // Rows inside the grace are left alone by both paths.
    sqlx::query("UPDATE provider_cli_operations SET status='failed', finished_at=CURRENT_TIMESTAMP WHERE operation_id=$1")
        .bind(&accepted.operation_id).execute(&auth.pool()).await.unwrap();
    stale("pco_fresh", "5 minutes").await;
    assert_eq!(
        auth.provider_cli_accept(&scope, &token("d").await)
            .await
            .unwrap_err()
            .code(),
        "provider_cli_machine_busy"
    );
    assert_eq!(
        audit_actions(&auth, "provider_cli.install.finished")
            .await
            .len(),
        2
    );
}
