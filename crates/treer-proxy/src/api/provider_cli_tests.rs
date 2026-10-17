//! Provider CLI route tests (#123 part 2) against isolated PostgreSQL and a
//! fake Controller connection. They prove the human-only scope, the
//! consume-then-dispatch ordering, uniform refusals, creator-only polling,
//! output-free audit rows and the never-replay rule.
use super::*;
use crate::auth::ProviderCliScope;
use treer_protocol::{
    CommandResult, ProviderCliAction, ProviderCliInstallAccepted, ProviderCliInstallRequest,
    ProviderCliObservation, ProviderCliObservedState, ProviderCliOperationReport,
    ProviderCliOperationStatus, ProviderCliPlanStep, ProviderCliProbe, ProviderCliRecipeRef,
    ProviderCliRecipeView, ProviderCliRecipesResponse, ProviderCliVersionRelation, ProxyMessage,
    CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL, CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL_V2,
};

const WORKSPACE: &str = "default";
const MACHINE: &str = "machine-a";
const TABLE_HASH: &str = "859a60731bd8f04707ddc8becfb7dc0cf405a24517385b849053154dbdeee10b";
const CODEX_HASH: &str = "e9e559641f371bb3c403245e97ad6cff629e410175dd9677e19226cc415f7c12";
const TARGET_VERSION: &str = "0.163.0";
/// The path only the creator's prepare/check response may carry.
const PREFIX_PATH: &str = "/opt/fixture-prefix/bin/codex";

/// Controller observation fixture: `version` makes it `installed`, `found`
/// without a version makes it unknown-but-found, neither makes it absent.
fn observation(version: Option<&str>, found: bool) -> ProviderCliObservation {
    let state = match (version, found) {
        (Some(_), _) => ProviderCliObservedState::Installed,
        (None, true) => ProviderCliObservedState::Unknown,
        (None, false) => ProviderCliObservedState::Absent,
    };
    ProviderCliObservation {
        state,
        current_version: version.map(str::to_owned),
        version_text: version.map(|v| format!("codex-cli {v}")),
        executable: Some(PREFIX_PATH.into()),
        found_in_prefix: found,
        probe: ProviderCliProbe {
            exit_code: version.map(|_| 0),
            timed_out: false,
            cleanup_confirmed: true,
            error_code: match (version, found) {
                (Some(_), _) => None,
                (None, true) => Some("provider_cli_version_unparsed".into()),
                (None, false) => Some("provider_cli_binary_not_found".into()),
            },
        },
    }
}

fn unobservable() -> ProviderCliObservation {
    ProviderCliObservation {
        state: ProviderCliObservedState::Unknown,
        current_version: None,
        version_text: None,
        executable: None,
        found_in_prefix: false,
        probe: ProviderCliProbe {
            exit_code: Some(1),
            timed_out: false,
            cleanup_confirmed: true,
            error_code: Some("provider_cli_prefix_unresolved".into()),
        },
    }
}

/// A v2 Controller's check response around an observation.
fn check_response(observation: &ProviderCliObservation) -> Value {
    let installed = observation.state == ProviderCliObservedState::Installed;
    json!({
        "installed": installed,
        "found_in_prefix": observation.found_in_prefix,
        "version": observation.version_text,
        "exit_code": observation.probe.exit_code,
        "timed_out": false,
        "stdout": observation.version_text.clone().map(|t| format!("{t}\n")).unwrap_or_default(),
        "stderr": "",
        "truncated": false,
        "error_code": observation.probe.error_code,
        "executable": observation.executable,
        "observation": observation,
    })
}

fn request(
    token: &str,
    acknowledged: Option<ProviderCliVersionRelation>,
) -> ProviderCliInstallRequest {
    ProviderCliInstallRequest {
        confirmation_token: token.into(),
        acknowledged_relation: acknowledged,
    }
}

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
                package: Some(treer_protocol::ProviderCliPackage {
                    name: "@openai/codex".into(),
                    version: TARGET_VERSION.into(),
                    integrity: "sha512-x".into(),
                    registry: "https://registry.npmjs.org/".into(),
                }),
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
    vec![
        CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL.into(),
        CAPABILITY_MACHINE_PROVIDER_CLI_INSTALL_V2.into(),
    ]
}

/// A Controller from before #134: installs without observing.
fn v1_only() -> Vec<String> {
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
        action: Some(ProviderCliAction::Install),
        version_relation: None,
        observation_hash: Some(observation(None, false).hash(CODEX_HASH, TARGET_VERSION)),
        observed_before: terminal
            .then(|| observation(None, false).summary(CODEX_HASH, TARGET_VERSION)),
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
            consent,
        } => {
            assert_eq!(recipe, &codex_ref());
            assert_eq!(table_hash, TABLE_HASH);
            assert_eq!(argv_hash, &expected_hash);
            assert_eq!(consent.action, ProviderCliAction::Install);
            assert_eq!(consent.version_relation, None);
            assert_eq!(
                consent.observation_hash,
                observation(None, false).hash(CODEX_HASH, TARGET_VERSION)
            );
            Ok(serde_json::to_value(ProviderCliInstallAccepted {
                operation_id: operation_id.clone(),
                status: ProviderCliOperationStatus::Running,
                argv_hash: argv_hash.clone(),
            })
            .unwrap())
        }
        AgentCommand::ProviderCliOperation { operation_id } => {
            // Commands so far: recipes, recipes+check (prepare), install,
            // then the first poll is still running; later polls finished.
            let status = if index < 5 {
                ProviderCliOperationStatus::Running
            } else {
                ProviderCliOperationStatus::Succeeded
            };
            Ok(serde_json::to_value(report(status, operation_id, &expected_hash)).unwrap())
        }
        AgentCommand::ProviderCliCheck { recipe } => {
            assert_eq!(recipe, &codex_ref());
            // Empty prefix at prepare time; installed once the job finished.
            Ok(check_response(&observation(
                (index >= 5).then_some(TARGET_VERSION),
                index >= 5,
            )))
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
    let token = prepared
        .confirmation_token
        .clone()
        .expect("install issues a token");
    assert!(token.starts_with("pcl_"));
    assert!(prepared.expires_at.is_some());
    assert_eq!(prepared.recipe.plan.as_deref(), Some(plan().as_slice()));
    assert_eq!(prepared.machine_id, MACHINE);
    assert_eq!(prepared.action, ProviderCliAction::Install);
    assert_eq!(prepared.version_relation, None);
    assert_eq!(prepared.target_version, TARGET_VERSION);
    assert_eq!(prepared.observation.state, ProviderCliObservedState::Absent);
    assert_eq!(
        prepared.observation.executable.as_deref(),
        Some(PREFIX_PATH),
        "the creator sees the path"
    );
    assert_eq!(
        prepared.observation_hash,
        observation(None, false).hash(CODEX_HASH, TARGET_VERSION)
    );
    let stored: (String, String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT token_hash, argv_hash, action, observation_hash, observed_state FROM provider_cli_install_tokens",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_ne!(stored.0, token, "only the hash is stored");
    assert_eq!(stored.1, argv_hash);
    assert_eq!(stored.2.as_deref(), Some("install"));
    assert_eq!(
        stored.3.as_deref(),
        Some(prepared.observation_hash.as_str())
    );
    assert_eq!(stored.4.as_deref(), Some("absent"));
    let prepare_audit = audit_actions(&auth, "provider_cli.prepare").await;
    assert_eq!(prepare_audit.len(), 1);
    assert!(prepare_audit[0].to_string().contains(CODEX_HASH));
    assert!(!prepare_audit[0].to_string().contains(&token[4..]));
    assert_eq!(prepare_audit[0]["action"], "install");
    assert_eq!(prepare_audit[0]["observed_state"], "absent");
    assert!(
        !prepare_audit[0].to_string().contains("fixture-prefix"),
        "audit carries no path"
    );

    let installed = provider_cli_install(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WORKSPACE.into(), MACHINE.into())),
        Json(request(&token, None)),
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
        Json(request(&token, None)),
    )
    .await
    .unwrap_err();
    assert_eq!(busy.status, StatusCode::CONFLICT);
    assert_eq!(busy.error.code, "provider_cli_machine_busy");
    let accepted = audit_actions(&auth, "provider_cli.install.accepted").await;
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["operation_id"], installed.operation_id);
    assert_eq!(accepted[0]["argv_hash"], argv_hash);
    assert_eq!(accepted[0]["action"], "install");
    assert_eq!(accepted[0]["observation_hash"], prepared.observation_hash);
    assert!(!accepted[0].to_string().contains("fixture-prefix"));

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
    assert_eq!(finished.action, Some(ProviderCliAction::Install));
    assert_eq!(
        finished.observed_state,
        Some(ProviderCliObservedState::Absent)
    );
    assert_eq!(
        finished.observation_hash.as_deref(),
        Some(prepared.observation_hash.as_str())
    );
    assert_eq!(
        finished.observed_before.as_ref().map(|o| o.state),
        Some(ProviderCliObservedState::Absent)
    );
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
        Json(request(&token, None)),
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
    assert!(
        !all_audit.contains("fixture-prefix"),
        "paths never enter audit"
    );
    assert!(finished_audit[0]["observed_before"]["state"] == "absent");
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
    assert_eq!(
        check.observation.as_ref().map(|o| o.state),
        Some(ProviderCliObservedState::Installed)
    );
    let check_audit = audit_actions(&auth, "provider_cli.check").await;
    assert_eq!(check_audit.len(), 1, "prepare's observation is not a check");
    assert_eq!(check_audit[0]["installed"], true);
    assert_eq!(check_audit[0]["observed_version"], TARGET_VERSION);
    assert!(check_audit[0].get("stdout").is_none());
    assert!(!check_audit[0].to_string().contains("fixture-prefix"));
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
        AgentCommand::ProviderCliCheck { .. } => Ok(check_response(&observation(None, false))),
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
                Json(request(&token, None)),
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
                .confirmation_token
                .unwrap(),
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
            .provider_cli_accept(&scope(workspace, server), &tokens[0], None)
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            "provider_cli_token_invalid",
            "{workspace}/{server}"
        );
    }
    assert_eq!(live_tokens().await, 4);

    // Ten prepares per minute per user and machine, by database time, counted
    // as attempts before any Controller command (refused recipes count too:
    // the changed and unsupported references above were attempts).
    let attempts = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_cli_prepare_attempts WHERE user_id='creator'",
        )
        .fetch_one(&auth.pool())
        .await
        .unwrap()
    };
    assert_eq!(
        attempts().await,
        6,
        "changed, unsupported, four tokens; the exhausted prepare was refused before counting"
    );
    sqlx::query("UPDATE provider_cli_install_tokens SET consumed_at=CURRENT_TIMESTAMP WHERE user_id='creator'")
        .execute(&auth.pool()).await.unwrap();
    for _ in 0..4 {
        sqlx::query("INSERT INTO provider_cli_prepare_attempts(workspace_id, server_id, user_id) VALUES($1,$2,'creator')")
            .bind(WORKSPACE).bind(MACHINE)
            .execute(&auth.pool()).await.unwrap();
    }
    let commands_before = seen.load(std::sync::atomic::Ordering::SeqCst);
    let error = prepare(creator.clone(), allow.clone(), codex_ref())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "provider_cli_rate_limited")
    );
    assert_eq!(
        seen.load(std::sync::atomic::Ordering::SeqCst),
        commands_before,
        "a rate-limited prepare sends the machine nothing"
    );
    sqlx::query("UPDATE provider_cli_install_tokens SET created_at = CURRENT_TIMESTAMP - INTERVAL '2 minutes', consumed_at = NULL WHERE user_id='creator'")
        .execute(&auth.pool()).await.unwrap();
    sqlx::query("DELETE FROM provider_cli_prepare_attempts")
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
    // A v1-only Controller (installs without observing): 409 for prepare and
    // install, before any Controller command and before consumption.
    let (v1_id, v1_rx) = connect(&state, v1_only()).await;
    drop(v1_rx);
    let commands_before = seen.load(std::sync::atomic::Ordering::SeqCst);
    let error = prepare(creator.clone(), allow.clone(), codex_ref())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_controller_outdated")
    );
    let error = install(creator.clone(), tokens[1].clone())
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_controller_outdated")
    );
    assert_eq!(
        seen.load(std::sync::atomic::Ordering::SeqCst),
        commands_before,
        "an outdated Controller receives no provider CLI command"
    );
    assert_eq!(live_tokens().await, 3);
    state.disconnect_server(WORKSPACE, MACHINE, v1_id).await;
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
    let envelope = super::tests::next_lifecycle_command(&mut rx).await;
    assert!(matches!(
        envelope.command,
        AgentCommand::ProviderCliCheck { .. }
    ));
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            check_response(&observation(None, false)),
        ))
        .await;
    let prepared = prepare.await.unwrap();

    let install = tokio::spawn({
        let (state, auth, policy, creator) =
            (state.clone(), auth.clone(), policy.clone(), creator.clone());
        let token = prepared.confirmation_token.clone().unwrap();
        async move {
            provider_cli_install(
                State(state),
                Extension(auth),
                Extension(policy),
                Extension(creator),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(request(&token, None)),
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
        AgentCommand::ProviderCliCheck { .. } => Ok(check_response(&observation(None, false))),
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
            .confirmation_token
            .unwrap(),
        );
    }
    let scope = ProviderCliScope {
        workspace_id: WORKSPACE,
        server_id: MACHINE,
        user_id: "creator",
        session_token: "session-one",
    };
    // Deterministic race: an uncommitted competing insert is invisible to the
    // busy check (READ COMMITTED) but blocks the unique index. PostgreSQL's
    // pg_blocking_pids proves the accept is waiting on that insert before the
    // competitor commits; then the accept loses on the index, its whole
    // transaction rolls back and the token stays usable.
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
            auth.provider_cli_accept(&scope, &token, None).await
        }
    });
    // Deterministic evidence: PostgreSQL reports the accept's backend blocked
    // on its INSERT by the competitor's backend, so the busy SELECT was passed
    // and the unique index is what decides.
    let competitor_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *competitor)
        .await
        .unwrap();
    // The test pool has two connections and both are busy (competitor, accept):
    // probe server state over a dedicated connection. pg_stat_activity is
    // cluster-wide, so the isolated schema does not matter here.
    let database_url = std::env::var("TREER_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://treer:treer@127.0.0.1:55432/treer_test".to_string());
    let mut probe = <sqlx::PgConnection as sqlx::Connection>::connect(&database_url)
        .await
        .unwrap();
    let blocked_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let blocked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)) AND query ILIKE 'INSERT INTO provider_cli_operations%'",
        )
        .bind(competitor_pid)
        .fetch_one(&mut probe)
        .await
        .unwrap();
        if blocked == 1 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < blocked_deadline,
            "the accept never blocked on the competitor's uncommitted insert"
        );
        assert!(
            !racing.is_finished(),
            "the accept must not finish before the competitor commits"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
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
    assert!(auth
        .provider_cli_accept(&scope, &tokens[0], None)
        .await
        .is_ok());
    assert_eq!(
        auth.provider_cli_accept(&scope, &tokens[1], None)
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
            sqlx::query("INSERT INTO provider_cli_install_tokens(token_hash, workspace_id, server_id, user_id, session_hash, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, action, observation_hash, observed_state, observed_in_prefix, expires_at) VALUES($1,$2,$3,'creator',$4,'codex-npm-posix',1,$5,$6,'c0ffee','h','install',$7,'absent',false, CURRENT_TIMESTAMP + INTERVAL '2 minutes')")
                .bind(crate::auth::sha256_hex(&plaintext)).bind(WORKSPACE).bind(MACHINE).bind(crate::auth::sha256_hex("session-one")).bind(CODEX_HASH).bind(TABLE_HASH).bind("0".repeat(64))
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
        .provider_cli_accept(&scope, &token("a").await, None)
        .await
        .unwrap();
    assert_eq!(accepted.status, ProviderCliOperationStatus::Queued);
    sqlx::query("UPDATE provider_cli_operations SET status='failed', finished_at=CURRENT_TIMESTAMP WHERE operation_id=$1")
        .bind(&accepted.operation_id).execute(&auth.pool()).await.unwrap();
    // Accept alone reconciles a stale row that nobody polled.
    stale("pco_silent", "26 hours").await;
    let accepted = auth
        .provider_cli_accept(&scope, &token("c").await, None)
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
        auth.provider_cli_accept(&scope, &token("d").await, None)
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

/// #134: the observed state decides the one action; `noop` issues no token,
/// a downgrade or unknown relation needs an explicit acknowledgement that
/// must match the bound relation, and an unobservable prefix refuses prepare.
#[tokio::test]
async fn provider_cli_prepare_binds_observed_state_and_requires_acknowledgement() {
    let auth = seeded_auth().await;
    let state = super::tests::state_with_managed_agent().await;
    let (_, rx) = connect(&state, capability()).await;
    // The fixture Controller reports whatever the test puts in this slot.
    let observed: Arc<std::sync::Mutex<ProviderCliObservation>> = Arc::new(std::sync::Mutex::new(
        observation(Some(TARGET_VERSION), true),
    ));
    let dispatched: Arc<std::sync::Mutex<Vec<treer_protocol::ProviderCliConsent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let argv_hash = treer_protocol::plan_argv_hash(&plan());
    controller(state.clone(), rx, {
        let (observed, dispatched) = (observed.clone(), dispatched.clone());
        move |command, _| match command {
            AgentCommand::ProviderCliRecipes {} => {
                Ok(serde_json::to_value(recipes_response()).unwrap())
            }
            AgentCommand::ProviderCliCheck { .. } => {
                Ok(check_response(&observed.lock().unwrap().clone()))
            }
            AgentCommand::ProviderCliInstall {
                operation_id,
                consent,
                argv_hash,
                ..
            } => {
                dispatched.lock().unwrap().push(consent.clone());
                Ok(serde_json::to_value(ProviderCliInstallAccepted {
                    operation_id: operation_id.clone(),
                    status: ProviderCliOperationStatus::Running,
                    argv_hash: argv_hash.clone(),
                })
                .unwrap())
            }
            other => panic!("unexpected command {other:?}"),
        }
    });
    let creator = session("creator", "session-one");
    let prepare = || {
        let (state, auth, creator) = (state.clone(), auth.clone(), creator.clone());
        async move {
            provider_cli_prepare(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::allow_all()),
                Extension(creator),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(codex_ref()),
            )
            .await
        }
    };
    let install = |token: String, acknowledged: Option<ProviderCliVersionRelation>| {
        let (state, auth, creator) = (state.clone(), auth.clone(), creator.clone());
        async move {
            provider_cli_install(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::allow_all()),
                Extension(creator),
                Path((WORKSPACE.into(), MACHINE.into())),
                Json(request(&token, acknowledged)),
            )
            .await
        }
    };
    let token_rows = || async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM provider_cli_install_tokens")
            .fetch_one(&auth.pool())
            .await
            .unwrap()
    };
    let live_tokens = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM provider_cli_install_tokens WHERE consumed_at IS NULL",
        )
        .fetch_one(&auth.pool())
        .await
        .unwrap()
    };

    // Same pinned version installed: noop, no token, no row, audited.
    let noop = prepare().await.unwrap().0;
    assert_eq!(noop.action, ProviderCliAction::Noop);
    assert_eq!(noop.confirmation_token, None);
    assert_eq!(noop.expires_at, None);
    assert_eq!(noop.version_relation, None);
    assert_eq!(
        noop.observation.current_version.as_deref(),
        Some(TARGET_VERSION)
    );
    assert_eq!(noop.observation.executable.as_deref(), Some(PREFIX_PATH));
    assert_eq!(token_rows().await, 0);
    let prepare_audit = audit_actions(&auth, "provider_cli.prepare").await;
    assert_eq!(prepare_audit.len(), 1);
    assert_eq!(prepare_audit[0]["action"], "noop");
    assert_eq!(prepare_audit[0]["observed_version"], TARGET_VERSION);
    assert!(!prepare_audit[0].to_string().contains("fixture-prefix"));

    // Unresolvable prefix: refused, no token.
    *observed.lock().unwrap() = unobservable();
    let error = prepare().await.unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_state_unobservable")
    );
    assert_eq!(token_rows().await, 0);
    assert_eq!(
        audit_actions(&auth, "provider_cli.prepare").await[1]["outcome"],
        "unobservable"
    );

    // Newer version installed: replace with a downgrade relation.
    *observed.lock().unwrap() = observation(Some("0.164.0"), true);
    let downgrade = prepare().await.unwrap().0;
    assert_eq!(downgrade.action, ProviderCliAction::Replace);
    assert_eq!(
        downgrade.version_relation,
        Some(ProviderCliVersionRelation::Downgrade)
    );
    let token = downgrade.confirmation_token.clone().unwrap();
    assert_eq!(
        downgrade.observation_hash,
        observation(Some("0.164.0"), true).hash(CODEX_HASH, TARGET_VERSION)
    );
    let stored: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT action, version_relation, observed_version FROM provider_cli_install_tokens",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(
        stored,
        (
            Some("replace".into()),
            Some("downgrade".into()),
            Some("0.164.0".into())
        )
    );
    // Missing or wrong acknowledgement: 400, token untouched, nothing dispatched.
    for (acknowledged, code) in [
        (None, "provider_cli_acknowledgement_required"),
        (
            Some(ProviderCliVersionRelation::Unknown),
            "provider_cli_acknowledgement_mismatch",
        ),
        (
            Some(ProviderCliVersionRelation::Upgrade),
            "provider_cli_acknowledgement_mismatch",
        ),
    ] {
        let error = install(token.clone(), acknowledged).await.unwrap_err();
        assert_eq!(
            (error.status, error.error.code.as_str()),
            (StatusCode::BAD_REQUEST, code)
        );
    }
    assert_eq!(live_tokens().await, 1);
    assert!(dispatched.lock().unwrap().is_empty());
    assert!(audit_actions(&auth, "provider_cli.install.accepted")
        .await
        .is_empty());
    // Exact acknowledgement: dispatched with the bound consent.
    let installed = install(token.clone(), Some(ProviderCliVersionRelation::Downgrade))
        .await
        .unwrap()
        .0;
    assert_eq!(installed.status, ProviderCliOperationStatus::Running);
    let consents = dispatched.lock().unwrap().clone();
    assert_eq!(consents.len(), 1);
    assert_eq!(consents[0].action, ProviderCliAction::Replace);
    assert_eq!(
        consents[0].version_relation,
        Some(ProviderCliVersionRelation::Downgrade)
    );
    assert_eq!(consents[0].observation_hash, downgrade.observation_hash);
    let accepted = audit_actions(&auth, "provider_cli.install.accepted").await;
    assert_eq!(accepted[0]["version_relation"], "downgrade");
    assert_eq!(accepted[0]["observed_version"], "0.164.0");
    assert!(!accepted[0].to_string().contains("fixture-prefix"));
    let row: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT action, version_relation, observed_state FROM provider_cli_operations WHERE operation_id=$1",
    )
    .bind(&installed.operation_id)
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            Some("replace".into()),
            Some("downgrade".into()),
            Some("installed".into())
        )
    );
    sqlx::query(
        "UPDATE provider_cli_operations SET status='failed', finished_at=CURRENT_TIMESTAMP",
    )
    .execute(&auth.pool())
    .await
    .unwrap();

    // Found but unreadable: replace with an unknown relation, acknowledged.
    *observed.lock().unwrap() = observation(None, true);
    let unknown = prepare().await.unwrap().0;
    assert_eq!(unknown.action, ProviderCliAction::Replace);
    assert_eq!(
        unknown.version_relation,
        Some(ProviderCliVersionRelation::Unknown)
    );
    assert_eq!(unknown.observation.current_version, None);
    let token = unknown.confirmation_token.clone().unwrap();
    assert_eq!(
        install(token.clone(), None).await.unwrap_err().error.code,
        "provider_cli_acknowledgement_required"
    );
    let _ = install(token, Some(ProviderCliVersionRelation::Unknown))
        .await
        .unwrap();
    sqlx::query(
        "UPDATE provider_cli_operations SET status='failed', finished_at=CURRENT_TIMESTAMP",
    )
    .execute(&auth.pool())
    .await
    .unwrap();

    // Older version installed: upgrade needs no acknowledgement and refuses one.
    *observed.lock().unwrap() = observation(Some("0.162.0"), true);
    let upgrade = prepare().await.unwrap().0;
    assert_eq!(
        upgrade.version_relation,
        Some(ProviderCliVersionRelation::Upgrade)
    );
    let token = upgrade.confirmation_token.clone().unwrap();
    assert_eq!(
        install(token.clone(), Some(ProviderCliVersionRelation::Upgrade))
            .await
            .unwrap_err()
            .error
            .code,
        "provider_cli_acknowledgement_mismatch"
    );
    let _ = install(token, None).await.unwrap();
    assert_eq!(dispatched.lock().unwrap().len(), 3);

    // A token row without consent (pre-#134 Proxy) is never accepted.
    sqlx::query(
        "UPDATE provider_cli_operations SET status='failed', finished_at=CURRENT_TIMESTAMP",
    )
    .execute(&auth.pool())
    .await
    .unwrap();
    let legacy = format!("pcl_{}", "e".repeat(64));
    sqlx::query("INSERT INTO provider_cli_install_tokens(token_hash, workspace_id, server_id, user_id, session_hash, recipe_id, recipe_version, recipe_hash, table_hash, controller_source_sha, argv_hash, expires_at) VALUES($1,$2,$3,'creator',$4,'codex-npm-posix',1,$5,$6,'c0ffee',$7, CURRENT_TIMESTAMP + INTERVAL '2 minutes')")
        .bind(crate::auth::sha256_hex(&legacy)).bind(WORKSPACE).bind(MACHINE).bind(crate::auth::sha256_hex("session-one")).bind(CODEX_HASH).bind(TABLE_HASH).bind(&argv_hash)
        .execute(&auth.pool()).await.unwrap();
    let error = install(legacy, None).await.unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::CONFLICT, "provider_cli_token_invalid")
    );
    assert_eq!(dispatched.lock().unwrap().len(), 3);

    // A malformed observation from the Controller fails closed: 502, no token,
    // nothing stored or audited from it.
    let token_rows_before = token_rows().await;
    let prepare_audits_before = audit_actions(&auth, "provider_cli.prepare").await.len();
    let mut malformed = observation(Some("0.164.0"), true);
    malformed.current_version = Some("latest".into());
    *observed.lock().unwrap() = malformed;
    let error = prepare().await.unwrap_err();
    assert_eq!(
        (error.status, error.error.code.as_str()),
        (StatusCode::BAD_GATEWAY, "provider_cli_controller_error")
    );
    let mut malformed = observation(Some("0.164.0"), true);
    malformed.executable = Some("relative/codex".into());
    *observed.lock().unwrap() = malformed;
    assert_eq!(
        prepare().await.unwrap_err().error.code,
        "provider_cli_controller_error"
    );
    let mut malformed = observation(None, true);
    malformed.version_text = Some("x\u{7}y".into());
    *observed.lock().unwrap() = malformed;
    assert_eq!(
        prepare().await.unwrap_err().error.code,
        "provider_cli_controller_error"
    );
    assert_eq!(token_rows().await, token_rows_before);
    assert_eq!(
        audit_actions(&auth, "provider_cli.prepare").await.len(),
        prepare_audits_before
    );
    let all_audit = sqlx::query_scalar::<_, String>(
        "SELECT payload::text FROM organization_audit_events WHERE action LIKE 'provider_cli.%'",
    )
    .fetch_all(&auth.pool())
    .await
    .unwrap()
    .join("\n");
    assert!(!all_audit.contains("latest") && !all_audit.contains("relative/codex"));

    // Every prepare, including noop, is an attempt: the eleventh per minute
    // is refused before the machine is asked anything.
    *observed.lock().unwrap() = observation(Some(TARGET_VERSION), true);
    let attempts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM provider_cli_prepare_attempts WHERE user_id='creator'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    let mut refused = None;
    for _ in attempts..=treer_protocol::PROVIDER_CLI_PREPARES_PER_MINUTE {
        match prepare().await {
            Ok(response) => assert_eq!(response.0.action, ProviderCliAction::Noop),
            Err(error) => {
                refused = Some(error);
                break;
            }
        }
    }
    let refused = refused.expect("the limit applies to noop prepares");
    assert_eq!(
        (refused.status, refused.error.code.as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "provider_cli_rate_limited")
    );
    assert_eq!(token_rows().await, token_rows_before);
}
