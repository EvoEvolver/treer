use super::*;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::process::{Command, Stdio};
use tower::ServiceExt;
use treer_protocol::{
    CommandResult, CreateWorkloadFolderRequest, MachineServiceProtocol, PolicyEffect, PolicyMode,
    PolicyPrincipalKind, PolicyPrincipalRef, ProxyMessage, UpdateWorkloadFolderRequest,
    WorkspacePolicyDocument, POLICY_SCHEMA_VERSION,
};
use treer_proxy::policy_store::WorkspacePolicyStore;

#[tokio::test]
async fn agent_creation_ack_and_validated_inventory_close_reservation_cleanup_scope() {
    for confirmation_fails in [false, true] {
        let auth = AuthStore::for_test("test").await;
        auth.seed_test_workspace("default").await;
        if confirmation_fails {
            sqlx::raw_sql("CREATE FUNCTION refuse_confirmation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'private fixture detail'; END $$; CREATE TRIGGER refuse_confirmation BEFORE UPDATE ON agent_creation_reservations FOR EACH ROW EXECUTE FUNCTION refuse_confirmation()")
                .execute(&auth.pool()).await.unwrap();
        }
        let state = state_with_managed_agent().await;
        let server = state.resolve_server("default", "machine-a").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server_instance_with_capabilities(
                server,
                Uuid::new_v4(),
                "ack-fixture".into(),
                vec![treer_protocol::CAPABILITY_WORKLOAD_CREATE_PLACEMENT.into()],
                tx,
            )
            .await
            .unwrap();
        let waiting = tokio::spawn({
            let (state, auth) = (state.clone(), auth.clone());
            async move {
                let creator = CurrentSession {
                    token: "fixture".into(),
                    user_id: "creator".into(),
                    email: "creator@example.test".into(),
                    preferred_name: "Creator".into(),
                };
                execute_agent_create(&state,&auth,&PolicyEngine::allow_all(),Some(&creator),None,"default",
                    serde_json::from_value(json!({"server_id":"machine-a","kind":"command","name":"created","cwd":".","args":["fixture"]})).unwrap(),None).await
            }
        });
        let check = next_lifecycle_command(&mut rx).await;
        assert!(matches!(
            check.command,
            AgentCommand::CreationPlacementCheck {}
        ));
        state
            .complete_command(CommandResult::success(
                check.command_id,
                json!({"supported":true}),
            ))
            .await;
        let create = next_lifecycle_command(&mut rx).await;
        let AgentCommand::Create { agent_id, .. } = create.command else {
            panic!("Create")
        };
        state
            .complete_command(CommandResult::success(
                create.command_id,
                json!({"agent_id":agent_id}),
            ))
            .await;
        let result = waiting.await.unwrap();
        let confirmed: bool = sqlx::query_scalar(
            "SELECT confirmed_at IS NOT NULL FROM agent_creation_reservations WHERE agent_id=$1",
        )
        .bind(&agent_id)
        .fetch_one(&auth.pool())
        .await
        .unwrap();
        assert_eq!(confirmed, !confirmation_fails);
        if confirmation_fails {
            let error = result.unwrap_err().error;
            assert_eq!(error.code, "agent_creation_confirmation_failed");
            assert!(!error.message.contains("private fixture detail"));
            assert_eq!(
                error.details.unwrap().creation_state,
                treer_protocol::AgentCreationFailureState::ReservedOutcomeUnknown
            );
        } else {
            result.unwrap();
            assert!(auth
                .owned_agent_reservation("default", &agent_id, "creator")
                .await
                .is_err());
        }
    }
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    for (id, through_event) in [("ag_inventory", false), ("ag_event", true)] {
        let name = if through_event { "event" } else { "inventory" };
        auth.reserve_named_agent(
            "default",
            "machine-a",
            id,
            name,
            None,
            None,
            Some("creator"),
        )
        .await
        .unwrap();
        let mut agent = state.snapshot("default").await.unwrap().agents[0].clone();
        agent.agent_id = id.into();
        agent.name = name.into();
        agent.status = treer_protocol::AgentStatus::Exited;
        if through_event {
            auth.validate_agent_event(&agent).await.unwrap();
        } else {
            auth.apply_agent_names(&mut treer_protocol::AgentServerSnapshot {
                server: state.resolve_server("default", "machine-a").await.unwrap(),
                agents: vec![agent],
            })
            .await
            .unwrap();
        }
        assert!(
            auth.owned_agent_reservation("default", id, "creator")
                .await
                .is_err(),
            "even stopped, confirmed workload is not an unconfirmed reservation"
        );
        assert!(sqlx::query_scalar::<_, bool>(
            "SELECT revoked_at IS NULL FROM agent_credentials WHERE agent_id=$1"
        )
        .bind(id)
        .fetch_one(&auth.pool())
        .await
        .unwrap());
    }
}

#[tokio::test]
async fn reservation_cleanup_requires_owned_record_policy_and_durable_fence() {
    for outcome in [
        "success",
        "old",
        "offline",
        "observed",
        "unconfirmed",
        "audit_failure",
        "snapshot",
        "app_metadata",
    ] {
        let auth = AuthStore::for_test("test").await;
        auth.seed_test_workspace("default").await;
        auth.seed_test_workspace("other").await;
        sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('creator','creator@example.test','Creator','fixture','now'),('other','other@example.test','Other','fixture','now')")
            .execute(&auth.pool()).await.unwrap();
        for workspace in ["default", "other"] {
            for user in ["creator", "other"] {
                sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,$2,'member','now')")
                    .bind(format!("org_{workspace}")).bind(user).execute(&auth.pool()).await.unwrap();
            }
        }
        auth.reserve_named_agent(
            "default",
            "machine-a",
            "ag_candidate",
            "reserved-name",
            None,
            None,
            Some("creator"),
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO machine_services(service_id,workspace_id,name,server_id,target_agent_id,target_host,target_port,protocol,created_at,created_by,updated_at,updated_by) VALUES('svc_candidate','default','candidate','machine-a','ag_candidate','127.0.0.1',8080,'http','now','creator','now','creator')")
            .execute(&auth.pool()).await.unwrap();
        let state = state_with_managed_agent().await;
        let server = state.resolve_server("default", "machine-a").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let connection_id = Uuid::new_v4();
        state
            .register_server_instance_with_capabilities(
                server,
                connection_id,
                "cleanup-fixture".into(),
                if outcome == "old" {
                    vec![]
                } else {
                    vec![treer_protocol::CAPABILITY_AGENT_RESERVATION_FENCE.into()]
                },
                tx,
            )
            .await
            .unwrap();
        let call = |workspace: &str, user: &str, id: &str, policy: PolicyEngine| {
            let (state, auth) = (state.clone(), auth.clone());
            let (workspace, user, id) = (workspace.to_owned(), user.to_owned(), id.to_owned());
            async move {
                cleanup_agent_reservation(
                    State(state),
                    Extension(auth),
                    Extension(policy),
                    Extension(CurrentSession {
                        token: "fixture-session".into(),
                        user_id: user,
                        email: "fixture@example.test".into(),
                        preferred_name: "Fixture".into(),
                    }),
                    Path((workspace, id)),
                    axum::body::Bytes::new(),
                )
                .await
            }
        };
        for (workspace, user, id) in [
            ("default", "other", "ag_candidate"),
            ("other", "creator", "ag_candidate"),
            ("default", "creator", "ag_absent"),
        ] {
            let error = call(workspace, user, id, PolicyEngine::allow_all())
                .await
                .unwrap_err();
            assert_eq!(error.status, StatusCode::NOT_FOUND);
            assert_eq!(error.error.code, "agent_reservation_not_found");
            assert_eq!(error.error.message, "Agent reservation not available");
        }
        let denied = PolicyEngine::new(
            crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
                "policy_denied",
                "denied by fixture",
            )),
            vec![],
        );
        assert_eq!(
            call("default", "creator", "ag_candidate", denied)
                .await
                .unwrap_err()
                .status,
            StatusCode::FORBIDDEN
        );
        assert!(
            rx.try_recv().is_err(),
            "ownership/Policy denials must not contact Controller"
        );
        if outcome == "snapshot" {
            let mut agent = state.snapshot("default").await.unwrap().agents[0].clone();
            agent.agent_id = "ag_candidate".into();
            state.test_insert_agent(agent).await;
        }
        if outcome == "app_metadata" {
            let app = auth
                .create_app_deployment(
                    "default",
                    "creator",
                    "machine-a".into(),
                    CreateAppDeploymentRequest {
                        folder_id: None,
                        server_id: Some("machine-a".into()),
                        name: "candidate-app".into(),
                        command: "fixture".into(),
                        args: vec![],
                        cwd: ".".into(),
                        port: 8081,
                        hostname: String::new(),
                        public: false,
                    },
                )
                .await
                .unwrap();
            sqlx::query(
                "UPDATE app_deployments SET runtime_agent_id='ag_candidate' WHERE app_id=$1",
            )
            .bind(app.app_id)
            .execute(&auth.pool())
            .await
            .unwrap();
        }
        if outcome == "offline" {
            state
                .disconnect_server("default", "machine-a", connection_id)
                .await;
        }
        if outcome == "audit_failure" {
            sqlx::raw_sql("CREATE FUNCTION refuse_cleanup_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.action='agent.reservation.cleanup' THEN RAISE EXCEPTION 'fixture'; END IF; RETURN NEW; END $$; CREATE TRIGGER refuse_cleanup BEFORE INSERT ON organization_audit_events FOR EACH ROW EXECUTE FUNCTION refuse_cleanup_audit()")
                .execute(&auth.pool()).await.unwrap();
        }
        let responder = if !matches!(outcome, "old" | "offline" | "snapshot" | "app_metadata") {
            Some(tokio::spawn({
                let state = state.clone();
                async move {
                    let SocketFrame::Text(frame) = rx.recv().await.unwrap() else {
                        panic!("command frame")
                    };
                    let ProxyMessage::Command { envelope } = serde_json::from_str(&frame).unwrap()
                    else {
                        panic!("command")
                    };
                    assert!(
                        matches!(&envelope.command, AgentCommand::FenceAbsentAgentReservation { agent_id } if agent_id=="ag_candidate")
                    );
                    let result = if outcome == "observed" {
                        CommandResult::failure(
                            envelope.command_id,
                            ProtocolError::new("agent_reservation_observed", "fixture"),
                        )
                    } else {
                        CommandResult::success(
                            envelope.command_id,
                            json!({"fenced":outcome!="unconfirmed"}),
                        )
                    };
                    state.complete_command(result).await;
                }
            }))
        } else {
            None
        };
        let result = call(
            "default",
            "creator",
            "ag_candidate",
            PolicyEngine::allow_all(),
        )
        .await;
        if let Some(task) = responder {
            task.await.unwrap();
        }
        if outcome == "success" {
            assert_eq!(
                result.unwrap().0,
                json!({"agent_id":"ag_candidate","deleted":true,"already_deleted":false})
            );
            let repeat = call(
                "default",
                "creator",
                "ag_candidate",
                PolicyEngine::allow_all(),
            )
            .await
            .unwrap()
            .0;
            assert_eq!(repeat["already_deleted"], true);
            let audit: (String, serde_json::Value) = sqlx::query_as("SELECT actor_id,payload FROM organization_audit_events WHERE action='agent.reservation.cleanup'")
                .fetch_one(&auth.pool()).await.unwrap();
            assert_eq!(audit.0, "creator");
            assert_eq!(audit.1, json!({"server_id":"machine-a","deleted":true}));
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM machine_services WHERE service_id='svc_candidate'"
                )
                .fetch_one(&auth.pool())
                .await
                .unwrap(),
                0
            );
            assert!(sqlx::query_scalar::<_, bool>("SELECT revoked_at IS NOT NULL FROM agent_credentials WHERE agent_id='ag_candidate'").fetch_one(&auth.pool()).await.unwrap());
            auth.reserve_named_agent(
                "default",
                "machine-a",
                "ag_retry",
                "reserved-name",
                None,
                None,
                Some("creator"),
            )
            .await
            .unwrap();
        } else {
            let error = result.unwrap_err();
            if outcome == "audit_failure" {
                assert!(error.error.hint.as_ref().unwrap()[0].contains("retry this cleanup"));
                assert!(
                    error.error.hint.as_ref().unwrap()[0].contains("do not retry Agent creation")
                );
            }
            assert_eq!(
                error.status,
                match outcome {
                    "old" => StatusCode::NOT_IMPLEMENTED,
                    "offline" => StatusCode::SERVICE_UNAVAILABLE,
                    "audit_failure" => StatusCode::INTERNAL_SERVER_ERROR,
                    _ => StatusCode::CONFLICT,
                },
                "{outcome}"
            );
            let untouched: (bool, bool) = sqlx::query_as("SELECT revoked_at IS NULL,NOT EXISTS(SELECT 1 FROM deleted_agents WHERE agent_id='ag_candidate') FROM agent_credentials WHERE agent_id='ag_candidate'")
                .fetch_one(&auth.pool()).await.unwrap();
            assert_eq!(untouched, (true, true), "{outcome} must retain reservation");
            assert_eq!(
                sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM agent_names WHERE agent_id='ag_candidate'"
                )
                .fetch_one(&auth.pool())
                .await
                .unwrap(),
                1
            );
        }
    }
}

#[tokio::test]
async fn agent_create_pre_dispatch_denial_and_offline_preserve_their_errors() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let request = || CreateAgentRequest {
        folder_id: None,
        server_id: Some("machine-a".into()),
        kind: "command".into(),
        name: "not-reserved".into(),
        cwd: ".".into(),
        args: vec!["claude".into()],
        cols: 80,
        rows: 24,
        publish_ports: vec![],
        recipe: None,
    };
    let policy = PolicyEngine::new(
        crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
            "policy_denied",
            "blocked by fixture",
        )),
        vec![],
    );
    let subject = PolicySubject::Agent {
        server_id: "machine-a".into(),
        agent_id: "agent-a".into(),
    };
    let error = execute_agent_create(
        &state,
        &auth,
        &policy,
        None,
        Some(&subject),
        "default",
        request(),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "policy_denied");
    assert_eq!(error.error.message, "blocked by fixture");
    assert!(error.error.details.is_none());

    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let id = Uuid::new_v4();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    state.register_server(server, id, tx).await.unwrap();
    state.disconnect_server("default", "machine-a", id).await;
    let error = execute_agent_create(
        &state,
        &auth,
        &PolicyEngine::allow_all(),
        None,
        None,
        "default",
        request(),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "server_offline");
    assert!(error.error.details.is_none());
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_names WHERE name='not-reserved'")
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn agent_create_transport_failure_keeps_reservation_and_binds_error_scope() {
    use treer_protocol::AgentCreationFailureState;
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let server = state_with_managed_agent()
        .await
        .resolve_server("default", "machine-a")
        .await
        .unwrap();
    let state = AppState::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "test-controller".into(),
            vec![treer_protocol::CAPABILITY_WORKLOAD_CREATE_PLACEMENT.into()],
            tx,
        )
        .await
        .unwrap();
    let task = tokio::spawn({
        let state = state.clone();
        let auth = auth.clone();
        async move {
            let creator = CurrentSession {
                token: "fixture-session".into(),
                user_id: "human-creator".into(),
                email: "creator@example.test".into(),
                preferred_name: "Creator".into(),
            };
            execute_agent_create(
                &state,
                &auth,
                &PolicyEngine::allow_all(),
                Some(&creator),
                None,
                "default",
                CreateAgentRequest {
                    folder_id: None,
                    server_id: Some("machine-a".into()),
                    kind: "command".into(),
                    name: "transport-failure".into(),
                    cwd: ".".into(),
                    args: vec!["claude".into()],
                    cols: 80,
                    rows: 24,
                    publish_ports: vec![],
                    recipe: None,
                },
                None,
            )
            .await
        }
    });
    let check = next_lifecycle_command(&mut rx).await;
    assert!(matches!(
        check.command,
        AgentCommand::CreationPlacementCheck {}
    ));
    // Close the transport before acknowledging preflight. The subsequent Create
    // send deterministically fails, without relying on a sleep or timeout.
    drop(rx);
    state
        .complete_command(CommandResult::success(
            check.command_id,
            json!({"supported":true}),
        ))
        .await;
    let error = task.await.unwrap().unwrap_err().error;
    assert_eq!(error.code, "server_offline");
    let details = error.details.unwrap();
    assert_eq!(details.machine_id.as_deref(), Some("machine-a"));
    assert_eq!(
        details.creation_state,
        AgentCreationFailureState::ReservedOutcomeUnknown
    );
    assert!(details.command.is_none());
    let id = details.agent_id.unwrap();
    assert_eq!(
        auth.owned_agent_reservation("default", &id, "human-creator")
            .await
            .unwrap(),
        ("machine-a".into(), false)
    );
    assert!(auth
        .owned_agent_reservation("default", &id, "other-human")
        .await
        .is_err());
    let active: bool =
        sqlx::query_scalar("SELECT revoked_at IS NULL FROM agent_credentials WHERE agent_id=$1")
            .bind(id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert!(
        active,
        "transport codes alone cannot prove an unstarted workload"
    );
}

#[tokio::test]
async fn agent_create_missing_command_only_cleans_a_proven_unstarted_reservation() {
    use treer_protocol::{AgentCreationErrorDetails, AgentCreationFailureState};
    for (proof, mismatched_id, cleanup_fails, legacy) in [
        (true, false, false, false),
        (false, false, false, false),
        (false, false, false, true),
        (true, true, false, false),
        (true, false, true, false),
    ] {
        let auth = AuthStore::for_test("test").await;
        auth.seed_test_workspace("default").await;
        if cleanup_fails {
            sqlx::raw_sql("CREATE FUNCTION fail_cleanup() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic cleanup failure'; END $$; CREATE TRIGGER cleanup_failure BEFORE INSERT ON deleted_agents FOR EACH ROW EXECUTE FUNCTION fail_cleanup();")
                .execute(&auth.pool()).await.unwrap();
        }
        let server = state_with_managed_agent()
            .await
            .resolve_server("default", "machine-a")
            .await
            .unwrap();
        let state = AppState::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state
            .register_server_instance_with_capabilities(
                server,
                Uuid::new_v4(),
                "test-controller".into(),
                vec![treer_protocol::CAPABILITY_WORKLOAD_CREATE_PLACEMENT.into()],
                tx,
            )
            .await
            .unwrap();
        let task = tokio::spawn({
            let auth = auth.clone();
            let state = state.clone();
            async move {
                execute_agent_create(
                    &state,
                    &auth,
                    &PolicyEngine::allow_all(),
                    None,
                    None,
                    "default",
                    CreateAgentRequest {
                        folder_id: None,
                        server_id: Some("machine-a".into()),
                        kind: "command".into(),
                        name: "missing-provider".into(),
                        cwd: ".".into(),
                        args: vec!["claude".into()],
                        cols: 80,
                        rows: 24,
                        publish_ports: vec![],
                        recipe: None,
                    },
                    None,
                )
                .await
            }
        });
        let check = next_lifecycle_command(&mut rx).await;
        assert!(matches!(
            check.command,
            AgentCommand::CreationPlacementCheck {}
        ));
        state
            .complete_command(CommandResult::success(
                check.command_id,
                json!({"supported":true}),
            ))
            .await;
        let create = next_lifecycle_command(&mut rx).await;
        let AgentCommand::Create { agent_id, .. } = create.command else {
            panic!("expected create")
        };
        let mut failure = ProtocolError::new(
            if proof || legacy {
                "command_not_found"
            } else {
                "command_timeout"
            },
            "PATH /private/synthetic secret-argument",
        );
        if proof {
            failure.details = Some(Box::new(AgentCreationErrorDetails {
                command: Some("claude".into()),
                machine_id: Some("untrusted-target".into()),
                agent_id: Some(if mismatched_id {
                    "other-agent".into()
                } else {
                    agent_id.clone()
                }),
                creation_state: AgentCreationFailureState::NotStarted,
            }));
        }
        state
            .complete_command(CommandResult::failure(create.command_id, failure))
            .await;
        let error = task.await.unwrap().unwrap_err();
        let removed = proof && !mismatched_id && !cleanup_fails;
        let details = error.error.details.as_ref().unwrap();
        assert_eq!(details.machine_id.as_deref(), Some("machine-a"));
        assert_eq!(details.agent_id.as_deref(), Some(agent_id.as_str()));
        assert_eq!(
            details.creation_state,
            if removed {
                AgentCreationFailureState::IdentityRemoved
            } else if cleanup_fails {
                AgentCreationFailureState::CleanupFailed
            } else {
                AgentCreationFailureState::ReservedOutcomeUnknown
            }
        );
        let serialized = serde_json::to_string(&error.error).unwrap();
        for secret in [
            "PATH",
            "/private/synthetic",
            "secret-argument",
            "untrusted-target",
        ] {
            assert!(!serialized.contains(secret));
        }
        let revoked: bool = sqlx::query_scalar(
            "SELECT revoked_at IS NOT NULL FROM agent_credentials WHERE agent_id=$1",
        )
        .bind(&agent_id)
        .fetch_one(&auth.pool())
        .await
        .unwrap();
        assert_eq!(revoked, removed);
        let names: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_names WHERE agent_id=$1")
            .bind(&agent_id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
        assert_eq!(names, if removed { 0 } else { 1 });
        let visible_placement: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM workload_placements p JOIN agent_credentials c USING(agent_id) WHERE p.agent_id=$1 AND c.revoked_at IS NULL")
            .bind(&agent_id).fetch_one(&auth.pool()).await.unwrap();
        assert_eq!(visible_placement, if removed { 0 } else { 1 });
        let services: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM machine_services WHERE workspace_id='default' AND target_agent_id=$1")
            .bind(&agent_id).fetch_one(&auth.pool()).await.unwrap();
        assert_eq!(services, 0, "failed creation leaves no target services");
        let audit: serde_json::Value = sqlx::query_scalar("SELECT payload FROM organization_audit_events WHERE workspace_id='default' AND resource_id=$1 AND action='agent.create.failed'")
            .bind(&agent_id).fetch_one(&auth.pool()).await.unwrap();
        assert_eq!(audit["server_id"], "machine-a");
        assert_eq!(audit["code"], error.error.code);
        assert_eq!(
            audit["creation_state"],
            serde_json::to_value(details.creation_state).unwrap()
        );
        assert_eq!(
            audit.as_object().unwrap().len(),
            3,
            "audit contains only scoped metadata"
        );
        if removed {
            auth.create_named_agent_credential(
                "default",
                "machine-a",
                "ag_retry",
                "missing-provider",
                None,
            )
            .await
            .unwrap();
        }
        assert!(
            rx.try_recv().is_err(),
            "no retry/stop/install command is dispatched"
        );
    }
}

#[tokio::test]
async fn app_directory_and_resolution_do_not_enumerate_path_denied_agents() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('owner','owner@example.test',TRUE,'Owner','unused','test'),('other','other@example.test',TRUE,'Other','unused','test')").execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','owner','owner','test'),('org_default','other','member','test')").execute(&auth.pool()).await.unwrap();
    let app = auth
        .create_app_deployment(
            "default",
            "owner",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".into()),
                name: "Policy".into(),
                command: "python3".into(),
                args: vec![],
                cwd: ".".into(),
                port: 8888,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    auth.create_named_agent_credential("default", "machine-a", "agent-a", "agent-a", None)
        .await
        .unwrap();
    auth.create_named_agent_credential("default", "machine-a", "agent-b", "reviewer", None)
        .await
        .unwrap();
    let store = WorkspacePolicyStore::new(auth.pool());
    let doc=serde_json::from_value(json!({"schema_version":2,"defaults":{"agent.tree.read":"deny","agent.discover":"deny","agent.metadata.read":"deny","human.list":"deny"},"rules":[{"id":"one","priority":0,"effect":"allow","subjects":[{"kind":"human","id":"owner"}],"actions":["agent.discover","agent.metadata.read"],"resources":[{"kind":"agent","tree":{"path_eq":"/agent-a"}}]}]})).unwrap();
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            doc,
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let policy = PolicyEngine::durable(store);
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://proxy.example.test/").unwrap())
        .await
        .unwrap();
    let token = identity
        .issue_human_request("default", "owner", "Owner", "owner", &app.service_id)
        .unwrap()
        .access_token;
    let headers = HeaderMap::from_iter([(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    )]);
    let state = state_with_managed_agent().await;
    let Json(directory) = workspace_app_directory(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(identity.clone()),
        Path(app.service_id.clone()),
        headers.clone(),
    )
    .await
    .unwrap();
    let serialized = directory.to_string();
    assert!(serialized.contains("agent-a"));
    assert_eq!(
        directory["capabilities"][0],
        treer_protocol::APP_DIRECTORY_PATH_CAPABILITY
    );
    let caller = directory["principals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "agent-a")
        .unwrap();
    // Discovery/metadata allow the principal, but Tree read denial redacts path.
    assert!(caller["path"].is_null());
    assert!(serialized.contains("owner"));
    assert!(!serialized.contains("reviewer"));
    assert!(!serialized.contains("other"));
    let request: ResolveAppRecipientsRequest =
        serde_json::from_value(json!({"recipients":["reviewer"]})).unwrap();
    let error = resolve_workspace_app_recipients(
        State(state),
        Extension(auth),
        Extension(policy),
        Extension(identity),
        Path(app.service_id),
        headers,
        Json(request),
    )
    .await
    .unwrap_err();
    assert!(!error.error.message.contains("agent-b"));
}

#[tokio::test]
async fn app_directory_paths_follow_real_placement_and_folder_rename() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('owner','owner@example.test',TRUE,'Owner','unused','test')").execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','owner','owner','test')").execute(&auth.pool()).await.unwrap();
    let app = auth
        .create_app_deployment(
            "default",
            "owner",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".into()),
                name: "Issues".into(),
                command: "python3".into(),
                args: vec![],
                cwd: ".".into(),
                port: 8888,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    let folder = auth
        .create_workload_folder(
            "default",
            CreateWorkloadFolderRequest {
                name: "research".into(),
                parent_id: None,
            },
        )
        .await
        .unwrap();
    auth.create_named_agent_credential(
        "default",
        "machine-a",
        "agent-a",
        "reviewer",
        Some(&folder.folder_id),
    )
    .await
    .unwrap();
    auth.create_named_agent_credential("default", "machine-a", "agent-b", "reviewer", None)
        .await
        .unwrap();
    let state = state_with_managed_agent().await;
    state
        .rename_agent("default", "agent-a", "reviewer".into())
        .await
        .unwrap();
    let policy = PolicyEngine::durable(WorkspacePolicyStore::new(auth.pool()));
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://proxy.example.test/").unwrap())
        .await
        .unwrap();
    let token = identity
        .issue_human_request("default", "owner", "Owner", "owner", &app.service_id)
        .unwrap()
        .access_token;
    let headers = HeaderMap::from_iter([(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    )]);
    for expected in ["/research/reviewer", "/experiments/reviewer"] {
        let Json(directory) = workspace_app_directory(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(identity.clone()),
            Path(app.service_id.clone()),
            headers.clone(),
        )
        .await
        .unwrap();
        let principals = directory["principals"].as_array().unwrap();
        assert_eq!(
            principals.iter().find(|p| p["id"] == "agent-a").unwrap()["path"],
            expected
        );
        assert_eq!(
            principals.iter().find(|p| p["id"] == "agent-b").unwrap()["path"],
            "/reviewer"
        );
        assert!(principals.iter().find(|p| p["id"] == "owner").unwrap()["path"].is_null());
        auth.update_workload_folder(
            "default",
            &folder.folder_id,
            UpdateWorkloadFolderRequest {
                name: Some("experiments".into()),
                parent_id: None,
            },
        )
        .await
        .unwrap();
    }
    // A second Proxy reading the same store observes canonical renames even
    // while its machine inventory still contains the previous display name.
    auth.set_agent_name("default", "agent-a", "critic")
        .await
        .unwrap();
    let second_proxy = auth.clone();
    let principals = workspace_app_principals(&state, &second_proxy, "default")
        .await
        .unwrap();
    let renamed = principals.iter().find(|p| p.id == "agent-a").unwrap();
    assert_eq!(renamed.name, "critic");
    assert_eq!(renamed.path.as_deref(), Some("/experiments/critic"));
    auth.seed_test_workspace("foreign").await;
    auth.create_named_agent_credential("foreign", "machine-z", "foreign-agent", "secret", None)
        .await
        .unwrap();
    assert!(!auth
        .canonical_workload_paths("default")
        .await
        .unwrap()
        .contains_key("foreign-agent"));
    sqlx::query(
        "DELETE FROM workload_placements WHERE workspace_id='default' AND agent_id='agent-a'",
    )
    .execute(&auth.pool())
    .await
    .unwrap();
    let Json(directory) = workspace_app_directory(
        State(state),
        Extension(auth),
        Extension(policy),
        Extension(identity),
        Path(app.service_id),
        headers,
    )
    .await
    .unwrap();
    assert!(directory["principals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "agent-a")
        .unwrap()["path"]
        .is_null());
}

#[tokio::test]
async fn prompt_queue_policy_denies_send_and_listing_before_dispatch() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([
                    (ACTION_AGENT_PROMPT.into(), PolicyEffect::Deny),
                    (
                        crate::policy::ACTION_AGENT_QUEUE_READ.into(),
                        PolicyEffect::Deny,
                    ),
                ]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let policy = PolicyEngine::durable(store);
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    let machine = Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }));
    let error = prompt_agent(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        machine.clone(),
        headers.clone(),
        Path(("default".into(), "reviewer".into())),
        Json(treer_protocol::SubmitAgentPromptRequest {
            text: "task".into(),
            delivery: treer_protocol::PromptDelivery::Queue,
            notify: None,
            require_receipt: false,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "policy_denied");
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM organization_audit_events WHERE action='agent.prompt'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(audit_count, 0);
    let error = read_prompt_queue(
        State(state),
        Extension(auth),
        Extension(policy),
        machine,
        headers,
        Path(("default".into(), "reviewer".into())),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "policy_denied");
}

#[tokio::test]
async fn prompt_queue_attests_sender_and_checks_cancel_ownership() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "queue-test".into(),
            vec![treer_protocol::CAPABILITY_AGENT_PROMPT_DELIVERY.into()],
            tx,
        )
        .await
        .unwrap();
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([
                    (
                        crate::policy::ACTION_AGENT_QUEUE_READ.into(),
                        PolicyEffect::Deny,
                    ),
                    (
                        crate::policy::ACTION_AGENT_QUEUE_CANCEL.into(),
                        PolicyEffect::Deny,
                    ),
                ]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let policy = PolicyEngine::durable(store);
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    let machine = Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }));
    let pending = tokio::spawn({
        let (state, auth, policy, headers, machine) = (
            state.clone(),
            auth.clone(),
            policy.clone(),
            headers.clone(),
            machine.clone(),
        );
        async move {
            crate::audit::MUTATION_ACTOR
                .scope(
                    crate::audit::MutationActor {
                        kind: "agent",
                        id: Some("agent-a".into()),
                    },
                    prompt_agent(
                        State(state),
                        Extension(auth),
                        Extension(policy),
                        machine,
                        headers,
                        Path(("default".into(), "reviewer".into())),
                        Json(treer_protocol::SubmitAgentPromptRequest {
                            text: "task".into(),
                            delivery: treer_protocol::PromptDelivery::Queue,
                            notify: Some(treer_protocol::PromptNotify::default()),
                            require_receipt: true,
                        }),
                    ),
                )
                .await
        }
    });
    let envelope = next_lifecycle_command(&mut rx).await;
    match envelope.command {
        AgentCommand::SubmitPrompt {
            agent_id,
            sender: Some(sender),
            request,
        } => {
            assert_eq!(agent_id, "agent-b");
            assert_eq!(sender.agent_id, "agent-a");
            assert_eq!(sender.path, "/agent-a");
            assert_eq!(sender.name, "agent-a");
            assert_eq!(request.notify.unwrap().to.as_deref(), Some("agent-a"));
            assert!(request.require_receipt);
        }
        command => panic!("unexpected command: {command:?}"),
    }
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            json!({"submission":{"id":"sub-1","disposition":"queued","position":1}}),
        ))
        .await;
    assert!(pending.await.unwrap().is_ok());
    // Accepted prompts commit their audit to the durable outbox first. Drain
    // independently of the request scope and still require its original actor.
    assert_eq!(
        crate::audit::maintenance::drain_batch(&auth.pool())
            .await
            .unwrap(),
        1
    );
    let audit_rows = crate::audit::list(&auth.pool(), "org_default", None, None, 100)
        .await
        .unwrap();
    let prompts: Vec<_> = audit_rows
        .iter()
        .filter(|e| e.action == "agent.prompt")
        .collect();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].actor_id.as_deref(), Some("agent-a"));
    assert_eq!(prompts[0].resource_id, "agent-b");
    assert_eq!(prompts[0].payload, json!({}));
    assert!(prompts[0].resource_name.is_none());
    for sender in ["agent-b", "agent-a"] {
        let pending = tokio::spawn({
            let (state, auth, policy, headers, machine) = (
                state.clone(),
                auth.clone(),
                policy.clone(),
                headers.clone(),
                machine.clone(),
            );
            async move {
                cancel_prompt_submission(
                    State(state),
                    Extension(auth),
                    Extension(policy),
                    machine,
                    headers,
                    Path(("default".into(), "reviewer".into(), "sub-1".into())),
                )
                .await
            }
        });
        let read = next_lifecycle_command(&mut rx).await;
        assert!(matches!(read.command, AgentCommand::SubmissionRead { .. }));
        state
            .complete_command(CommandResult::success(
                read.command_id,
                json!({"submission":{"id":"sub-1","sender":{"agent_id":sender}}}),
            ))
            .await;
        if sender == "agent-b" {
            assert_eq!(
                pending.await.unwrap().unwrap_err().error.code,
                "policy_denied"
            );
            assert!(rx.try_recv().is_err());
        } else {
            let cancel = next_lifecycle_command(&mut rx).await;
            assert!(matches!(cancel.command, AgentCommand::QueueCancel { .. }));
            state
                .complete_command(CommandResult::success(
                    cancel.command_id,
                    json!({"submission":{"id":"sub-1","state":"interrupted"}}),
                ))
                .await;
            assert_eq!(
                pending.await.unwrap().unwrap().0["submission"]["state"],
                "interrupted"
            );
        }
    }
}

#[tokio::test]
async fn failed_prompt_dispatch_does_not_append_an_acceptance_audit() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let result = prompt_agent(
        State(state),
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Some(Extension(MachineSession {
            server_id: Some("machine-a".into()),
            workspace_id: Some("default".into()),
        })),
        HeaderMap::from_iter([(
            AGENT_ID_HEADER.parse().unwrap(),
            HeaderValue::from_static("agent-a"),
        )]),
        Path(("default".into(), "reviewer".into())),
        Json(treer_protocol::SubmitAgentPromptRequest {
            text: "synthetic-private-body".into(),
            delivery: treer_protocol::PromptDelivery::Queue,
            notify: None,
            require_receipt: false,
        }),
    )
    .await;
    assert!(result.is_err());
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM organization_audit_events WHERE action='agent.prompt'",
    )
    .fetch_one(&auth.pool())
    .await
    .unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
async fn prompt_queue_notifications_require_explicit_opt_in_even_for_interrupted_receipts() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "notify-test".into(),
            vec![treer_protocol::CAPABILITY_AGENT_PROMPT_DELIVERY.into()],
            tx,
        )
        .await
        .unwrap();
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([(
                    crate::policy::ACTION_AGENT_QUEUE_READ.into(),
                    PolicyEffect::Deny,
                )]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let policy = PolicyEngine::durable(store);
    let headers = HeaderMap::from_iter([(
        AGENT_ID_HEADER.parse().unwrap(),
        HeaderValue::from_static("agent-a"),
    )]);
    let machine = Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }));
    for delivery in ["queue", "steer"] {
        for notify in [
            None,
            Some(Value::Null),
            Some(json!({"on":["interrupted"]})),
            Some(json!({"on":["done"],"to":"agent-b"})),
        ] {
            for require_receipt in [false, true] {
                let mut body =
                    json!({"text":"task", "delivery":delivery, "require_receipt":require_receipt});
                if let Some(notify) = &notify {
                    body["notify"] = notify.clone();
                }
                let request = serde_json::from_value(body).unwrap();
                let pending = tokio::spawn(prompt_agent(
                    State(state.clone()),
                    Extension(auth.clone()),
                    Extension(policy.clone()),
                    machine.clone(),
                    headers.clone(),
                    Path(("default".into(), "reviewer".into())),
                    Json(request),
                ));
                let envelope = next_lifecycle_command(&mut rx).await;
                let AgentCommand::SubmitPrompt {
                    agent_id,
                    request,
                    sender: Some(sender),
                } = envelope.command
                else {
                    panic!("expected attested prompt")
                };
                assert_eq!(agent_id, "agent-b");
                assert_eq!(sender.agent_id, "agent-a");
                assert_eq!(sender.path, "/agent-a");
                assert_eq!(request.require_receipt, require_receipt);
                assert_eq!(serde_json::to_value(request.delivery).unwrap(), delivery);
                if let Some(explicit) = notify.as_ref().filter(|v| !v.is_null()) {
                    let actual = request.notify.unwrap();
                    assert_eq!(serde_json::to_value(actual.on).unwrap(), explicit["on"]);
                    assert_eq!(
                        actual.to.as_deref(),
                        Some(explicit["to"].as_str().unwrap_or("agent-a"))
                    );
                } else {
                    assert!(request.notify.is_none(), "omitted/null notify must stay absent for {delivery}, receipt={require_receipt}");
                }
                state.complete_command(CommandResult::success(envelope.command_id,
                    json!({"submission":{"id":"sub-opt-in","disposition":"queued","position":1}}))).await;
                assert!(pending.await.unwrap().is_ok());
                // A sender can poll its own interrupted receipt without queue.read
                // permission or any completion-notification subscription.
                let pending = tokio::spawn(read_prompt_submission(
                    State(state.clone()),
                    Extension(auth.clone()),
                    Extension(policy.clone()),
                    machine.clone(),
                    headers.clone(),
                    Path(("default".into(), "reviewer".into(), "sub-opt-in".into())),
                ));
                let read = next_lifecycle_command(&mut rx).await;
                assert!(matches!(read.command, AgentCommand::SubmissionRead { .. }));
                state.complete_command(CommandResult::success(read.command_id,
                    json!({"submission":{"id":"sub-opt-in","sender":{"agent_id":"agent-a"},"state":"interrupted","turn_id":"turn-opt-in"}}))).await;
                assert_eq!(
                    pending.await.unwrap().unwrap().0["submission"]["state"],
                    "interrupted"
                );
                assert!(rx.try_recv().is_err());
            }
        }
    }
}

#[test]
fn original_startup_stays_self_only() {
    let source = PolicySubject::Agent {
        server_id: "machine-a".into(),
        agent_id: "agent-a".into(),
    };
    assert!(require_self_agent(&source, "agent-a").is_ok());
    assert!(require_self_agent(&source, "agent-b").is_err());
    assert!(require_self_agent(
        &PolicySubject::Human {
            user_id: "owner".into()
        },
        "agent-a"
    )
    .is_err());
}

#[tokio::test]
async fn admin_startup_cross_agent_policy_denies_before_controller_dispatch() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([(
                    ACTION_AGENT_STARTUP_MANAGE.into(),
                    PolicyEffect::Deny,
                )]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let policy = PolicyEngine::durable(store);
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    let machine = Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }));
    let error = set_admin_agent_startup(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        None,
        machine.clone(),
        headers.clone(),
        Path(("default".into(), "reviewer".into())),
        Json(ReplaceAgentStartupRequest {
            cwd: None,
            command: "sh".into(),
            args: vec![],
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
    let error = get_admin_agent_startup(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        None,
        machine.clone(),
        headers.clone(),
        Path(("default".into(), "agent-b".into())),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
    let error = restart_agent_with_startup(
        State(state),
        Extension(auth),
        Extension(policy),
        None,
        machine,
        headers,
        Path(("default".into(), "agent-b".into())),
        Json(ReplaceAgentStartupRequest {
            cwd: None,
            command: "sh".into(),
            args: vec![],
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_startup_human_and_operator_routes_dispatch_canonical_id_and_reject_apps() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    let mut agent = app_workload_agent(&app);
    agent.agent_id = "agent-existing".into();
    agent.kind = "command".into();
    agent.name = "recover-me".into();
    state.test_insert_agent(agent.clone()).await;
    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let (tx, new_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "startup-controller".into(),
            vec![treer_protocol::CAPABILITY_AGENT_STARTUP_ADMIN.into()],
            tx,
        )
        .await
        .unwrap();
    // Replacement may enqueue fencing/close frames on the old connection.
    while let Ok(frame) = rx.try_recv() {
        if let SocketFrame::Text(text) = frame {
            assert!(!matches!(
                serde_json::from_str::<ProxyMessage>(&text).unwrap(),
                ProxyMessage::Command { .. }
            ));
        }
    }
    rx = new_rx;
    // Browser session requests carry no MachineSession; existing workspace
    // middleware authenticates the human before invoking this handler.
    let held_guard = auth
        .workload_lifecycle_guard("default", &agent.agent_id)
        .await
        .unwrap();
    let task = tokio::spawn({
        let state = state.clone();
        let auth = auth.clone();
        async move {
            get_admin_agent_startup(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::allow_all()),
                Some(Extension(CurrentSession {
                    token: "test".into(),
                    user_id: "owner".into(),
                    email: "owner@example.com".into(),
                    preferred_name: "Owner".into(),
                })),
                None,
                HeaderMap::new(),
                Path(("default".into(), "recover-me".into())),
            )
            .await
        }
    });
    let envelope = next_lifecycle_command(&mut rx).await;
    assert!(
        matches!(envelope.command, AgentCommand::StartupAdminGet { agent_id } if agent_id == agent.agent_id)
    );
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            json!({"command":"sh","args":[],"cwd":"."}),
        ))
        .await;
    assert!(task.await.unwrap().is_ok());
    drop(held_guard); // Startup reads must not wait for the lifecycle mutation lock.
    let task = tokio::spawn({
        let state = state.clone();
        let auth = auth.clone();
        async move {
            set_admin_agent_startup(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::allow_all()),
                None,
                Some(Extension(MachineSession {
                    server_id: Some("machine-a".into()),
                    workspace_id: Some("default".into()),
                })),
                HeaderMap::new(),
                Path(("default".into(), "recover-me".into())),
                Json(ReplaceAgentStartupRequest {
                    cwd: None,
                    command: "replacement".into(),
                    args: vec!["literal arg".into()],
                }),
            )
            .await
        }
    });
    let envelope = next_lifecycle_command(&mut rx).await;
    assert!(
        matches!(envelope.command, AgentCommand::StartupAdminSet { agent_id, request } if agent_id == agent.agent_id && request.command == "replacement" && request.cwd.is_none())
    );
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            json!({"command":"replacement"}),
        ))
        .await;
    assert!(task.await.unwrap().is_ok());
    let credential = auth
        .create_agent_credential("default", "machine-a", &agent.agent_id)
        .await
        .unwrap();
    auth.set_agent_recovery_enabled("default", &agent.agent_id, false)
        .await
        .unwrap();
    let before_hash: String =
        sqlx::query_scalar("SELECT secret_hash FROM agent_credentials WHERE agent_id=$1")
            .bind(&agent.agent_id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    let mut source = agent.clone();
    source.agent_id = "agent-source".into();
    source.name = "caller".into();
    state.test_insert_agent(source).await;
    let task = tokio::spawn({
        let state = state.clone();
        let auth = auth.clone();
        async move {
            let mut headers = HeaderMap::new();
            headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-source"));
            restart_agent_with_startup(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::allow_all()),
                None,
                Some(Extension(MachineSession {
                    server_id: Some("machine-a".into()),
                    workspace_id: Some("default".into()),
                })),
                headers,
                Path(("default".into(), "recover-me".into())),
                Json(ReplaceAgentStartupRequest {
                    cwd: Some("repo".into()),
                    command: "new-command".into(),
                    args: vec!["literal arg".into()],
                }),
            )
            .await
        }
    });
    let envelope = next_lifecycle_command(&mut rx).await;
    assert!(
        matches!(envelope.command, AgentCommand::RestartWithStartup { agent_id, request } if agent_id == agent.agent_id && request.command == "new-command" && request.cwd.as_deref() == Some("repo"))
    );
    state
        .complete_command(CommandResult::success(
            envelope.command_id,
            json!({"agent_id":agent.agent_id,"run_generation":2}),
        ))
        .await;
    let result = task.await.unwrap().unwrap().0;
    assert_eq!(result["agent_id"], agent.agent_id);
    assert_eq!(result["run_generation"], 2);
    let after_hash: String =
        sqlx::query_scalar("SELECT secret_hash FROM agent_credentials WHERE agent_id=$1")
            .bind(&agent.agent_id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert_eq!(before_hash, after_hash);
    assert!(auth
        .stopped_agent_recovery_ids(
            "default",
            "machine-a",
            std::slice::from_ref(&agent.agent_id)
        )
        .await
        .unwrap()
        .is_empty());
    assert!(!result.to_string().contains(&credential));
    let error = get_admin_agent_startup(
        State(state),
        Extension(auth),
        Extension(PolicyEngine::allow_all()),
        None,
        None,
        HeaderMap::new(),
        Path(("default".into(), app.app_id)),
    )
    .await
    .unwrap_err();
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(rx.try_recv().is_err());
}

pub(super) async fn state_with_managed_agent() -> AppState {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
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
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let agent = treer_protocol::AgentInfo {
        path: None,
        run_generation: 0,
        agent_id: "agent-a".to_string(),
        workspace_id: "default".to_string(),
        server_id: "machine-a".to_string(),
        kind: "command".to_string(),
        name: "agent-a".to_string(),
        cwd: ".".to_string(),
        status: treer_protocol::AgentStatus::Idle,
        pid: None,
        started_at: now,
        updated_at: now,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    };
    let mut recipient = agent.clone();
    recipient.agent_id = "agent-b".to_string();
    recipient.name = "reviewer".to_string();
    let connection_id = Uuid::new_v4();
    let (outgoing, _incoming) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, outgoing)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![agent, recipient],
            },
        )
        .await
        .expect("apply agent snapshot");
    state
}

#[tokio::test]
async fn machine_file_transfer_policy_denies_both_directions_before_dispatch() {
    use treer_protocol::{MachineFileTransferCall, MachineFileTransferRequest as Transfer};
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([
                    (ACTION_MACHINE_FILE_WRITE.into(), PolicyEffect::Deny),
                    (
                        crate::policy::ACTION_MACHINE_FILE_READ.into(),
                        PolicyEffect::Deny,
                    ),
                ]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let policy = PolicyEngine::durable(store);
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    let machine = Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }));
    for request in [
        Transfer::GetBegin {
            path: "result".into(),
            limit: 64,
        },
        Transfer::PutBegin {
            path: "result".into(),
            overwrite: false,
            mode: 0o600,
            size: Some(3),
            limit: 64,
        },
    ] {
        let error = machine_file_transfer(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            None,
            machine.clone(),
            headers.clone(),
            Path(("default".into(), "machine-a".into())),
            Json(MachineFileTransferCall {
                path: "result".into(),
                request,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.error.code, "policy_denied");
    }
}

#[tokio::test]
async fn machine_file_transfer_attests_actor_audits_only_metadata_and_rejects_old_controller() {
    use treer_protocol::{MachineFileTransferCall, MachineFileTransferRequest as Transfer};
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "file-test".into(),
            vec![treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER.into()],
            tx,
        )
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    let machine = Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }));
    let task = tokio::spawn({
        let (state, auth, headers, machine) = (
            state.clone(),
            auth.clone(),
            headers.clone(),
            machine.clone(),
        );
        async move {
            machine_file_transfer(
                State(state),
                Extension(auth),
                Extension(PolicyEngine::allow_all()),
                None,
                machine,
                headers,
                Path(("default".into(), "machine-a".into())),
                Json(MachineFileTransferCall {
                    path: "result".into(),
                    request: Transfer::PutFinish {
                        id: "fixture".into(),
                        size: 3,
                        sha256: "a".repeat(64),
                    },
                }),
            )
            .await
        }
    });
    let envelope = next_lifecycle_command(&mut rx).await;
    let AgentCommand::FileTransfer {
        actor,
        authorized_path,
        request,
    } = &envelope.command
    else {
        panic!("incorrect transfer command")
    };
    assert_eq!(actor.workspace_id, "default");
    assert_eq!(actor.kind, "agent");
    assert_eq!(actor.id, "agent-a");
    assert_eq!(authorized_path, "result");
    assert!(matches!(request, Transfer::PutFinish { .. }));
    state.complete_command(CommandResult::success(envelope.command_id,json!({"path":"result","size":3,"sha256":"a".repeat(64),"untrusted_content":"never audit these bytes"}))).await;
    let _ = task.await.unwrap().unwrap();
    let payloads: Vec<Value> = sqlx::query_scalar("SELECT payload FROM organization_audit_events WHERE action LIKE 'machine.file.%' ORDER BY sequence").fetch_all(&auth.pool()).await.unwrap();
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[0]["path"], "result");
    assert_eq!(payloads[1]["sha256"], "a".repeat(64));
    assert!(!serde_json::to_string(&payloads)
        .unwrap()
        .contains("never audit"));
    let old = state_with_managed_agent().await;
    let (tx, _old_receiver) = tokio::sync::mpsc::unbounded_channel();
    old.register_server_instance_with_capabilities(
        old.resolve_server("default", "machine-a").await.unwrap(),
        Uuid::new_v4(),
        "old-file-test".into(),
        vec![],
        tx,
    )
    .await
    .unwrap();
    let error = machine_file_transfer_info(
        State(old),
        machine,
        headers,
        Path(("default".into(), "machine-a".into())),
    )
    .await
    .unwrap_err();
    assert_eq!(error.error.code, "file_transfer_unsupported");
}

#[tokio::test]
async fn machine_file_transfer_durable_intent_failure_prevents_reads_and_commits() {
    use treer_protocol::{MachineFileTransferCall, MachineFileTransferRequest as Transfer};
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("default").await;
    let state = state_with_managed_agent().await;
    let server = state.resolve_server("default", "machine-a").await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "audit-file-test".into(),
            vec![treer_protocol::CAPABILITY_MACHINE_FILE_TRANSFER.into()],
            tx,
        )
        .await
        .unwrap();
    sqlx::query("ALTER TABLE organization_audit_events RENAME TO unavailable_audit_events")
        .execute(&auth.pool())
        .await
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, HeaderValue::from_static("agent-a"));
    let machine = Some(Extension(MachineSession {
        server_id: Some("machine-a".into()),
        workspace_id: Some("default".into()),
    }));
    for request in [
        Transfer::GetBegin {
            path: "result".into(),
            limit: 64,
        },
        Transfer::PutBegin {
            path: "result".into(),
            overwrite: false,
            mode: 0o600,
            size: None,
            limit: 64,
        },
        Transfer::PutFinish {
            id: "fixture".into(),
            size: 0,
            sha256: "a".repeat(64),
        },
    ] {
        let error = machine_file_transfer(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(PolicyEngine::allow_all()),
            None,
            machine.clone(),
            headers.clone(),
            Path(("default".into(), "machine-a".into())),
            Json(MachineFileTransferCall {
                path: "result".into(),
                request,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.error.code, "audit_unavailable");
        assert!(rx.try_recv().is_err());
    }
}

fn workload_tree_agent_router(state: AppState, auth: AuthStore, policy: PolicyEngine) -> Router {
    Router::new()
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree",
            get(get_workload_tree),
        )
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree/folders",
            post(create_workload_folder),
        )
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree/folders/{folder_id}",
            patch(update_workload_folder).delete(delete_workload_folder),
        )
        .route(
            "/agent/workspaces/{workspace_id}/workload-tree/placements/{agent_id}",
            axum::routing::put(set_workload_placement),
        )
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            auth::require_machine,
        ))
        .layer(Extension(auth))
        .layer(Extension(policy))
        .with_state(state)
}

async fn tree_request(
    router: &Router,
    headers: &HeaderMap,
    method: Method,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    request.headers_mut().extend(headers.clone());
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn workload_tree_agent_auth_and_directory_operations() {
    let auth = AuthStore::for_test("password").await;
    auth.seed_test_workspace("default").await;
    auth.seed_test_workspace("other").await;
    let enrollment = auth
        .create_machine_enrollment("default", "admin")
        .await
        .unwrap();
    let machine = auth.claim_machine_enrollment(&enrollment).await.unwrap();
    let credential = auth
        .create_agent_credential("default", &machine.server_id, "agent-a")
        .await
        .unwrap();
    auth.create_agent_credential("other", "foreign-machine", "foreign-agent")
        .await
        .unwrap();
    let state = state_with_managed_agent().await;
    let mut agent = state.resolve_agent("default", "agent-a").await.unwrap();
    agent.server_id = machine.server_id.clone();
    state.test_insert_agent(agent).await;
    let router = workload_tree_agent_router(state.clone(), auth.clone(), PolicyEngine::allow_all());
    let base = "/agent/workspaces/default/workload-tree";
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        format!("Bearer {}", machine.machine_token).parse().unwrap(),
    );
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().unwrap());
    // Partial/spoofed workload identities fail before the handler.
    assert_eq!(
        tree_request(&router, &headers, Method::GET, base, json!(null))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    headers.insert(
        treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
        "wrong".parse().unwrap(),
    );
    assert_eq!(
        tree_request(&router, &headers, Method::GET, base, json!(null))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    headers.insert(
        treer_protocol::WORKLOAD_CREDENTIAL_HEADER,
        credential.parse().unwrap(),
    );
    assert_eq!(
        tree_request(
            &router,
            &headers,
            Method::GET,
            "/agent/workspaces/other/workload-tree",
            json!(null)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );

    let (status, folder) = tree_request(
        &router,
        &headers,
        Method::POST,
        &format!("{base}/folders"),
        json!({"name":"research","actor_id":"spoofed-owner","actor_kind":"user"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = folder["folder"]["folder_id"].as_str().unwrap();
    let audit_rows = crate::audit::list(&auth.pool(), "org_default", None, None, 100)
        .await
        .unwrap();
    let event = audit_rows
        .iter()
        .find(|e| e.action == "tree.folder.create")
        .unwrap();
    assert_eq!(event.actor_kind, "agent");
    assert_eq!(event.actor_id.as_deref(), Some("agent-a"));
    assert_eq!(event.resource_id, id);
    assert_eq!(event.payload, json!({}));
    let duplicate = tree_request(
        &router,
        &headers,
        Method::POST,
        &format!("{base}/folders"),
        json!({"name":"research"}),
    )
    .await;
    assert_eq!(duplicate.1["error"]["code"], "folder_exists");
    assert_eq!(
        tree_request(
            &router,
            &headers,
            Method::POST,
            &format!("{base}/folders"),
            json!({"name":"bad/path"})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let moved = tree_request(
        &router,
        &headers,
        Method::PUT,
        &format!("{base}/placements/agent-a"),
        json!({"folder_id":id,"position":99}),
    )
    .await;
    assert_eq!(moved.0, StatusCode::OK);
    assert_eq!(moved.1["placement"]["agent_id"], "agent-a");
    assert_eq!(
        tree_request(
            &router,
            &headers,
            Method::PUT,
            &format!("{base}/placements/foreign-agent"),
            json!({"folder_id":id,"position":0})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let not_empty = tree_request(
        &router,
        &headers,
        Method::DELETE,
        &format!("{base}/folders/{id}?require_empty=true"),
        json!(null),
    )
    .await;
    assert_eq!(not_empty.1["error"]["code"], "folder_not_empty");
    assert_eq!(
        tree_request(
            &router,
            &headers,
            Method::PATCH,
            &format!("{base}/folders/{id}"),
            json!({"name":"science","parent_id":null})
        )
        .await
        .0,
        StatusCode::OK
    );
    let snapshot = tree_request(&router, &headers, Method::GET, base, json!(null))
        .await
        .1;
    assert_eq!(snapshot["folders"][0]["name"], "science");
    assert_eq!(snapshot["placements"][0]["folder_id"], id);

    // A different Proxy/router reads the same durable tree.
    let reloaded =
        workload_tree_agent_router(state.clone(), auth.clone(), PolicyEngine::allow_all());
    assert_eq!(
        tree_request(&reloaded, &headers, Method::GET, base, json!(null))
            .await
            .1,
        snapshot
    );
    assert_eq!(
        tree_request(
            &router,
            &headers,
            Method::PUT,
            &format!("{base}/placements/agent-a"),
            json!({"folder_id":null,"position":0})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        tree_request(
            &router,
            &headers,
            Method::DELETE,
            &format!("{base}/folders/{id}?require_empty=true"),
            json!(null)
        )
        .await
        .0,
        StatusCode::OK
    );

    // Tree authorization is independent of workload control or discovery.
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([
                    ("agent.tree.read".into(), PolicyEffect::Deny),
                    ("agent.tree.manage".into(), PolicyEffect::Deny),
                ]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let denied = workload_tree_agent_router(state, auth, PolicyEngine::durable(store));
    assert_eq!(
        tree_request(&denied, &headers, Method::GET, base, json!(null))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        tree_request(
            &denied,
            &headers,
            Method::POST,
            &format!("{base}/folders"),
            json!({"name":"denied"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn public_workspace_snapshots_include_app_workloads() {
    let state = state_with_managed_agent().await;
    let mut app = state.resolve_agent("default", "agent-a").await.unwrap();
    app.agent_id = "app_stable".into();
    app.kind = "app".into();
    app.name = "Docs".into();
    state.test_insert_agent(app).await;
    let snapshot = state.snapshot("default").await.unwrap();
    assert_eq!(snapshot.agents.len(), 3);
    assert!(snapshot
        .agents
        .iter()
        .any(|agent| agent.agent_id == "app_stable"));
}

async fn app_lifecycle_fixture() -> (
    AuthStore,
    AppState,
    AppDeployment,
    tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) {
    app_lifecycle_fixture_with_update(false).await
}

async fn app_lifecycle_fixture_with_update(
    update: bool,
) -> (
    AuthStore,
    AppState,
    AppDeployment,
    tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let app = auth
        .create_app_deployment(
            "default",
            "owner",
            "machine-a".into(),
            CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine-a".into()),
                name: "Docs".into(),
                command: "python3".into(),
                args: vec!["-m".into(), "http.server".into()],
                cwd: ".".into(),
                port: 8080,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(app.runtime_agent_id.as_deref(), Some(app.app_id.as_str()));
    let server = state_with_managed_agent()
        .await
        .resolve_server("default", "machine-a")
        .await
        .unwrap();
    let state = AppState::new();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server,
            Uuid::new_v4(),
            "test-controller".into(),
            if update {
                vec![
                    treer_protocol::CAPABILITY_AGENT_LIFECYCLE.into(),
                    treer_protocol::CAPABILITY_APP_UPDATE.into(),
                ]
            } else {
                vec![treer_protocol::CAPABILITY_AGENT_LIFECYCLE.into()]
            },
            tx,
        )
        .await
        .unwrap();
    (auth, state, app, rx)
}

#[tokio::test]
async fn app_update_serializes_revisions_and_restart_on_the_same_lifecycle_lock() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture_with_update(true).await;
    let update = || {
        tokio::spawn({
            let auth = auth.clone();
            let state = state.clone();
            let app = app.clone();
            async move { invoke_app_update(auth, state, &app, PolicyEngine::allow_all()).await }
        })
    };
    let first = update();
    let preflight = next_lifecycle_command(&mut rx).await;
    let second = update();
    let restart = tokio::spawn({
        let auth = auth.clone();
        let state = state.clone();
        let agent = app_workload_agent(&app);
        async move {
            apply_workload_action(
                &state,
                &auth,
                "default",
                &agent,
                WorkloadAction::Restart,
                "owner",
            )
            .await
        }
    });
    assert!(matches!(
        preflight.command,
        AgentCommand::AppStartupCheck { .. }
    ));
    state
        .complete_command(CommandResult::success(preflight.command_id, json!({})))
        .await;
    let apply = next_lifecycle_command(&mut rx).await;
    assert!(matches!(
        apply.command,
        AgentCommand::AppStartupUpdate { revision: 1, .. }
    ));
    assert!(
        rx.try_recv().is_err(),
        "other lifecycle actions wait until this update is acknowledged"
    );
    state
        .complete_command(CommandResult::success(apply.command_id, json!({})))
        .await;
    let _ = first.await.unwrap().unwrap();
    let mut seen_revision = 1;
    for _ in 0..2 {
        let next = next_lifecycle_command(&mut rx).await;
        match next.command {
            AgentCommand::AppStartupCheck { .. } => {
                state
                    .complete_command(CommandResult::success(next.command_id, json!({})))
                    .await;
                let apply = next_lifecycle_command(&mut rx).await;
                assert!(matches!(
                    apply.command,
                    AgentCommand::AppStartupUpdate { revision: 2, .. }
                ));
                seen_revision = 2;
                state
                    .complete_command(CommandResult::success(apply.command_id, json!({})))
                    .await;
            }
            AgentCommand::AppEnsureLifecycle {
                start_missing: false,
                revision,
                ..
            } => {
                assert_eq!(revision, seen_revision);
                state
                    .complete_command(CommandResult::success(next.command_id, json!({})))
                    .await;
                let restart = next_lifecycle_command(&mut rx).await;
                assert!(matches!(restart.command, AgentCommand::Restart { .. }));
                state
                    .complete_command(CommandResult::success(restart.command_id, json!({})))
                    .await;
            }
            other => panic!("unexpected command {other:?}"),
        }
    }
    let _ = second.await.unwrap().unwrap();
    restart.await.unwrap().unwrap();
    assert_eq!(
        auth.resolve_app_deployment("default", &app.app_id)
            .await
            .unwrap()
            .startup_revision,
        2
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn app_update_restore_validation_returns_current_spec_only_for_valid_machine_identity() {
    let (auth, _state, app, _rx) = app_lifecycle_fixture_with_update(true).await;
    auth.rotate_agent_credential("default", "machine-a", &app.app_id)
        .await
        .unwrap();
    let request = app_update_patch().apply(&app).unwrap();
    let guard = auth
        .workload_lifecycle_guard("default", &app.app_id)
        .await
        .unwrap();
    auth.update_app_startup("default", &app.app_id, &request, "owner")
        .await
        .unwrap();
    guard.commit().await.unwrap();
    let validate = |machine: &str| {
        agents::validate_agent_startup(
            Extension(auth.clone()),
            Extension(MachineSession {
                server_id: Some(machine.into()),
                workspace_id: Some("default".into()),
            }),
            Path(("default".into(), "machine-a".into())),
            Json(treer_protocol::ValidateAgentStartupRequest {
                agent_ids: vec![app.app_id.clone()],
            }),
        )
    };
    let response = validate("machine-a").await.unwrap().0;
    assert_eq!(response.app_startups.len(), 1);
    assert_eq!(response.app_startups[0].revision, 1);
    assert_eq!(response.app_startups[0].request, request);
    assert!(validate("another-machine").await.is_err());
    sqlx::query("UPDATE agent_credentials SET revoked_at=$1 WHERE agent_id=$2")
        .bind(Utc::now().to_rfc3339())
        .bind(&app.app_id)
        .execute(&auth.pool())
        .await
        .unwrap();
    let revoked = validate("machine-a").await.unwrap().0;
    assert!(
        revoked.app_startups.is_empty(),
        "revoked identities must not be restarted by spec reconciliation"
    );
    assert!(revoked.active_agent_ids.is_empty());
    assert!(
        revoked.deleted_agent_ids.is_empty(),
        "revocation alone is not a deletion confirmation"
    );
    auth.delete_agent("default", &app.app_id).await.unwrap();
    let deleted = agents::validate_agent_startup(
        Extension(auth.clone()),
        Extension(MachineSession {
            server_id: Some("machine-a".into()),
            workspace_id: Some("default".into()),
        }),
        Path(("default".into(), "machine-a".into())),
        Json(treer_protocol::ValidateAgentStartupRequest {
            agent_ids: vec![app.app_id.clone(), "ag_unrelated".into()],
        }),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(deleted.deleted_agent_ids, vec![app.app_id.clone()]);
    assert!(deleted.active_agent_ids.is_empty());
}

fn app_update_patch() -> treer_protocol::UpdateAppDeploymentRequest {
    treer_protocol::UpdateAppDeploymentRequest {
        cwd: Some("releases/v2".into()),
        command: Some("node".into()),
        args: Some(vec!["entry.mjs".into()]),
    }
}

async fn invoke_app_update(
    auth: AuthStore,
    state: AppState,
    app: &AppDeployment,
    policy: PolicyEngine,
) -> Result<Json<Value>, ApiFailure> {
    apps::update_app_deployment(
        State(state),
        Extension(auth),
        Extension(test_ingress_config()),
        Extension(policy),
        None,
        Some(Extension(MachineSession {
            server_id: Some(app.server_id.clone()),
            workspace_id: Some(app.workspace_id.clone()),
        })),
        HeaderMap::new(),
        Path((app.workspace_id.clone(), app.app_id.clone())),
        Json(app_update_patch()),
    )
    .await
}

#[tokio::test]
async fn app_update_old_controller_rejects_before_persisting() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    let error = invoke_app_update(auth.clone(), state.clone(), &app, PolicyEngine::allow_all())
        .await
        .unwrap_err();
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "unsupported_command");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("app.update.v1"));
    assert!(rx.try_recv().is_err());
    assert_eq!(
        auth.resolve_app_deployment("default", &app.app_id)
            .await
            .unwrap(),
        app
    );
    assert_eq!(state.snapshot("default").await.unwrap().servers.len(), 1);
}

#[tokio::test]
async fn app_update_persists_before_apply_preserving_identity_routing_and_credentials() {
    for fail_apply in [false, true] {
        let (auth, state, app, mut rx) = app_lifecycle_fixture_with_update(true).await;
        let ingress = auth
            .ensure_app_ingress(
                &app,
                "owner",
                "apps.treer.ai",
                Some(ServiceIngressAccess::Public),
            )
            .await
            .unwrap();
        let hosts_before = auth.list_virtual_network_hosts("default").await.unwrap();
        let mut origin_before = app.clone();
        hydrate_app_public_url(&auth, &test_ingress_config(), &mut origin_before)
            .await
            .unwrap();
        assert!(origin_before.public_url.is_some());
        let credential = auth
            .rotate_agent_credential("default", "machine-a", &app.app_id)
            .await
            .unwrap();
        let credential_before: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(c) FROM agent_credentials c WHERE agent_id=$1")
                .bind(&app.app_id)
                .fetch_one(&auth.pool())
                .await
                .unwrap();
        let task = tokio::spawn({
            let auth = auth.clone();
            let state = state.clone();
            let app = app.clone();
            async move { invoke_app_update(auth, state, &app, PolicyEngine::allow_all()).await }
        });
        let preflight = next_lifecycle_command(&mut rx).await;
        assert!(matches!(
            preflight.command,
            AgentCommand::AppStartupCheck { .. }
        ));
        assert_eq!(
            auth.resolve_app_deployment("default", &app.app_id)
                .await
                .unwrap()
                .startup_revision,
            0
        );
        state
            .complete_command(CommandResult::success(
                preflight.command_id,
                json!({"valid":true}),
            ))
            .await;
        let apply = next_lifecycle_command(&mut rx).await;
        assert!(
            matches!(&apply.command, AgentCommand::AppStartupUpdate { agent_id, revision: 1, request }
            if agent_id == &app.app_id && request.command == "node" && request.cwd.as_deref() == Some("releases/v2"))
        );
        let saved = auth
            .resolve_app_deployment("default", &app.app_id)
            .await
            .unwrap();
        assert_eq!(saved.command, "node");
        assert_eq!(saved.args, ["entry.mjs"]);
        assert_eq!(saved.cwd, "releases/v2");
        assert_eq!(saved.service_id, app.service_id);
        assert_eq!(saved.hostname, app.hostname);
        assert_eq!(saved.runtime_agent_id, app.runtime_agent_id);
        assert_eq!(saved.desired_state, app.desired_state);
        if fail_apply {
            state
                .complete_command(CommandResult::failure(
                    apply.command_id,
                    ProtocolError::new("spawn_failed", "synthetic failure"),
                ))
                .await;
        } else {
            state
                .complete_command(CommandResult::success(apply.command_id, json!({})))
                .await;
        }
        assert_eq!(task.await.unwrap().is_err(), fail_apply);
        let after = auth
            .resolve_app_deployment("default", &app.app_id)
            .await
            .unwrap();
        assert_eq!(after.startup_revision, 1);
        assert_eq!(
            after.command, "node",
            "failed spawn keeps new specification for repair"
        );
        assert_eq!(after.last_error.is_some(), fail_apply);
        assert_eq!(
            auth.list_virtual_network_hosts("default").await.unwrap(),
            hosts_before
        );
        assert_eq!(
            auth.list_service_ingresses("default").await.unwrap(),
            vec![ingress]
        );
        let mut origin_after = after.clone();
        hydrate_app_public_url(&auth, &test_ingress_config(), &mut origin_after)
            .await
            .unwrap();
        assert_eq!(origin_after.public_url, origin_before.public_url);
        assert_eq!(origin_after.access, Some(ServiceIngressAccess::Public));
        let credential_after: serde_json::Value =
            sqlx::query_scalar("SELECT to_jsonb(c) FROM agent_credentials c WHERE agent_id=$1")
                .bind(&app.app_id)
                .fetch_one(&auth.pool())
                .await
                .unwrap();
        assert_eq!(credential_before, credential_after);
        assert!(!serde_json::to_string(&after).unwrap().contains(&credential));
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn app_update_policy_denies_before_preflight_or_persistence() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture_with_update(true).await;
    let store = WorkspacePolicyStore::new(auth.pool());
    for (index, denied) in [
        ACTION_AGENT_STARTUP_MANAGE,
        ACTION_AGENT_STOP,
        ACTION_AGENT_CREATE,
    ]
    .into_iter()
    .enumerate()
    {
        store
            .replace(
                "default",
                index as u64,
                PolicyMode::Enforce,
                WorkspacePolicyDocument {
                    schema_version: POLICY_SCHEMA_VERSION,
                    defaults: BTreeMap::from([(denied.to_string(), PolicyEffect::Deny)]),
                    groups: BTreeMap::new(),
                    rules: vec![],
                },
                PolicyPrincipalRef {
                    kind: PolicyPrincipalKind::Human,
                    id: "owner".into(),
                },
            )
            .await
            .unwrap();
        let error = invoke_app_update(
            auth.clone(),
            state.clone(),
            &app,
            PolicyEngine::durable(store.clone()),
        )
        .await
        .unwrap_err();
        assert_eq!(error.into_response().status(), StatusCode::FORBIDDEN);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            auth.resolve_app_deployment("default", &app.app_id)
                .await
                .unwrap()
                .startup_revision,
            0
        );
    }
}

pub(super) async fn next_lifecycle_command(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<SocketFrame>,
) -> treer_protocol::CommandEnvelope {
    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timely lifecycle command")
        .expect("connected Controller");
    let SocketFrame::Text(text) = frame else {
        panic!("expected command frame")
    };
    let ProxyMessage::Command { envelope } = serde_json::from_str(&text).unwrap() else {
        panic!("expected command")
    };
    envelope
}

#[tokio::test]
async fn app_lifecycle_bootstrap_preserves_identity_and_serializes_declarations() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    let start = || {
        tokio::spawn(synchronize_app_lifecycles_for_server(
            state.clone(),
            auth.clone(),
            "default".into(),
            "machine-a".into(),
        ))
    };
    let first = start();
    let second = start();
    for _ in 0..2 {
        let envelope = next_lifecycle_command(&mut rx).await;
        let AgentCommand::EnsureLifecycle {
            agent_id,
            request,
            enabled,
            ..
        } = &envelope.command
        else {
            panic!("expected lifecycle declaration")
        };
        assert_eq!(agent_id, &app.app_id);
        assert_eq!(request.kind, "app");
        assert_eq!(request.args, vec!["python3", "-m", "http.server"]);
        assert!(*enabled);
        assert!(
            rx.try_recv().is_err(),
            "same workload declaration waits for prior acknowledgement"
        );
        state
            .complete_command(CommandResult::success(envelope.command_id, json!({})))
            .await;
    }
    first.await.unwrap();
    second.await.unwrap();
    let persisted = auth
        .resolve_app_deployment("default", &app.app_id)
        .await
        .unwrap();
    assert_eq!(persisted.runtime_agent_id, app.runtime_agent_id);
    assert_eq!(persisted.restart_count, 0);
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn app_lifecycle_stopped_legacy_identity_is_preserved_and_not_deleted() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    // Existing installations retain the identity that their local startup state owns.
    sqlx::query("UPDATE app_deployments SET runtime_agent_id='appw_legacy' WHERE app_id=$1")
        .bind(&app.app_id)
        .execute(&auth.pool())
        .await
        .unwrap();
    auth.set_app_desired_state("default", &app.app_id, AppDesiredState::Stopped, "owner")
        .await
        .unwrap();
    let mut runtime = state_with_managed_agent()
        .await
        .resolve_agent("default", "agent-a")
        .await
        .unwrap();
    runtime.agent_id = "appw_legacy".into();
    runtime.kind = "app".into();
    runtime.pid = Some(42);
    state.test_insert_agent(runtime).await;
    let task = tokio::spawn(synchronize_app_lifecycles_for_server(
        state.clone(),
        auth.clone(),
        "default".into(),
        "machine-a".into(),
    ));
    let declaration = next_lifecycle_command(&mut rx).await;
    assert!(
        matches!(&declaration.command,AgentCommand::EnsureLifecycle {agent_id,enabled:false,..} if agent_id == "appw_legacy")
    );
    state
        .complete_command(CommandResult::success(declaration.command_id, json!({})))
        .await;
    let stop = next_lifecycle_command(&mut rx).await;
    assert!(matches!(&stop.command,AgentCommand::Stop {agent_id} if agent_id == "appw_legacy"));
    state
        .complete_command(CommandResult::success(stop.command_id, json!({})))
        .await;
    task.await.unwrap();
    assert!(
        state.resolve_agent("default", "appw_legacy").await.is_ok(),
        "Stop retains workload identity"
    );
    let stopped = auth
        .resolve_app_deployment("default", &app.app_id)
        .await
        .unwrap();
    assert_eq!(stopped.runtime_agent_id.as_deref(), Some("appw_legacy"));
    assert_eq!(stopped.desired_state, AppDesiredState::Stopped);
}

#[tokio::test]
async fn app_lifecycle_stop_persists_intent_without_revoking_credential() {
    for stop_error in [
        None,
        Some("agent_not_found"),
        Some("process_not_running"),
        Some("process_not_found"),
    ] {
        let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
        auth.rotate_agent_credential("default", "machine-a", &app.app_id)
            .await
            .unwrap();
        let agent = app_workload_agent(&app);
        state.test_insert_agent(agent.clone()).await;
        let task = tokio::spawn({
            let state = state.clone();
            let auth = auth.clone();
            async move {
                apply_workload_action(
                    &state,
                    &auth,
                    "default",
                    &agent,
                    WorkloadAction::Stop,
                    "owner",
                )
                .await
            }
        });
        let stop = next_lifecycle_command(&mut rx).await;
        assert!(matches!(&stop.command,AgentCommand::Stop {agent_id} if agent_id == &app.app_id));
        let result = match stop_error {
            Some(code) => CommandResult::failure(
                stop.command_id,
                ProtocolError::new(code, "target process is already absent"),
            ),
            None => CommandResult::success(stop.command_id, json!({})),
        };
        state.complete_command(result).await;
        task.await.unwrap().unwrap();
        let intent: (bool, Option<String>) = sqlx::query_as(
            "SELECT recovery_enabled,revoked_at FROM agent_credentials WHERE agent_id=$1",
        )
        .bind(&app.app_id)
        .fetch_one(&auth.pool())
        .await
        .unwrap();
        assert_eq!(intent, (false, None));
        let stopped = auth
            .resolve_app_deployment("default", &app.app_id)
            .await
            .unwrap();
        assert_eq!(stopped.desired_state, AppDesiredState::Stopped);
        assert_eq!(
            stopped.runtime_agent_id.as_deref(),
            Some(app.app_id.as_str())
        );
        assert!(state.resolve_agent("default", &app.app_id).await.is_ok());
    }
}

#[tokio::test]
async fn app_lifecycle_missing_restart_declares_without_spawning_then_restarts_once() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    let agent = app_workload_agent(&app);
    assert!(state.resolve_agent("default", &app.app_id).await.is_err());
    let task = tokio::spawn({
        let state = state.clone();
        let auth = auth.clone();
        async move {
            apply_workload_action(
                &state,
                &auth,
                "default",
                &agent,
                WorkloadAction::Restart,
                "owner",
            )
            .await
        }
    });
    let declaration = next_lifecycle_command(&mut rx).await;
    assert!(
        matches!(&declaration.command,AgentCommand::EnsureLifecycle {agent_id,enabled:true,start_missing:false,..} if agent_id == &app.app_id)
    );
    assert!(rx.try_recv().is_err());
    state
        .complete_command(CommandResult::success(declaration.command_id, json!({})))
        .await;
    let restart = next_lifecycle_command(&mut rx).await;
    assert!(matches!(&restart.command,AgentCommand::Restart {agent_id} if agent_id == &app.app_id));
    state
        .complete_command(CommandResult::success(restart.command_id, json!({})))
        .await;
    task.await.unwrap().unwrap();
    assert!(
        rx.try_recv().is_err(),
        "restart must never emit Create or a second Restart"
    );
    let persisted = auth
        .resolve_app_deployment("default", &app.app_id)
        .await
        .unwrap();
    assert_eq!(
        persisted.runtime_agent_id.as_deref(),
        Some(app.app_id.as_str())
    );
    assert_eq!(persisted.restart_count, 0);
}

#[tokio::test]
async fn app_lifecycle_delete_stops_even_a_cached_terminal_workload_before_revocation() {
    assert_app_deletion_after_stop(None).await;
}

#[tokio::test]
async fn app_lifecycle_delete_accepts_already_exited_or_missing_processes() {
    for code in [
        "agent_not_found",
        "process_not_running",
        "process_not_found",
    ] {
        assert_app_deletion_after_stop(Some(code)).await;
    }
}

async fn assert_app_deletion_after_stop(stop_error: Option<&str>) {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    auth.rotate_agent_credential("default", "machine-a", &app.app_id)
        .await
        .unwrap();
    let terminal = app_workload_agent(&app);
    assert!(terminal.status.is_terminal());
    state.test_insert_agent(terminal).await;
    let task = tokio::spawn({
        let state = state.clone();
        let auth = auth.clone();
        let app = app.clone();
        async move { remove_app_workload(&state, &auth, &app).await }
    });
    let stop = next_lifecycle_command(&mut rx).await;
    assert!(matches!(&stop.command,AgentCommand::Stop {agent_id} if agent_id == &app.app_id));
    let revoked: Option<String> =
        sqlx::query_scalar("SELECT revoked_at FROM agent_credentials WHERE agent_id=$1")
            .bind(&app.app_id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert!(
        revoked.is_none(),
        "Controller must confirm Stop before identity revocation"
    );
    assert!(auth
        .resolve_app_deployment("default", &app.app_id)
        .await
        .is_ok());
    let result = match stop_error {
        Some(code) => CommandResult::failure(
            stop.command_id,
            ProtocolError::new(code, "target process is already absent"),
        ),
        None => CommandResult::success(stop.command_id, json!({})),
    };
    state.complete_command(result).await;
    task.await.unwrap().unwrap();
    let revoked: Option<String> =
        sqlx::query_scalar("SELECT revoked_at FROM agent_credentials WHERE agent_id=$1")
            .bind(&app.app_id)
            .fetch_one(&auth.pool())
            .await
            .unwrap();
    assert!(revoked.is_some());
    assert!(auth
        .resolve_app_deployment("default", &app.app_id)
        .await
        .is_err());
    assert!(state.resolve_agent("default", &app.app_id).await.is_err());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn app_lifecycle_delete_preserves_identity_when_stop_state_is_unknown() {
    for code in ["host_error", "terminal_error", "permission_denied"] {
        let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
        auth.rotate_agent_credential("default", "machine-a", &app.app_id)
            .await
            .unwrap();
        state.test_insert_agent(app_workload_agent(&app)).await;
        let task = tokio::spawn({
            let state = state.clone();
            let auth = auth.clone();
            let app = app.clone();
            async move { remove_app_workload(&state, &auth, &app).await }
        });
        let stop = next_lifecycle_command(&mut rx).await;
        state
            .complete_command(CommandResult::failure(
                stop.command_id,
                ProtocolError::new(code, "cannot confirm process stopped"),
            ))
            .await;
        assert!(task.await.unwrap().is_err(), "must propagate {code}");
        let revoked: Option<String> =
            sqlx::query_scalar("SELECT revoked_at FROM agent_credentials WHERE agent_id=$1")
                .bind(&app.app_id)
                .fetch_one(&auth.pool())
                .await
                .unwrap();
        assert!(revoked.is_none(), "must preserve credential on {code}");
        assert!(auth
            .resolve_app_deployment("default", &app.app_id)
            .await
            .is_ok());
        assert!(state.resolve_agent("default", &app.app_id).await.is_ok());
        assert!(rx.try_recv().is_err());
    }
}

#[tokio::test]
async fn app_lifecycle_start_and_restart_enforce_each_policy_action_before_dispatch() {
    let (auth, state, app, mut rx) = app_lifecycle_fixture().await;
    let store = WorkspacePolicyStore::new(auth.pool());
    for (index, (restart, denied)) in [
        (false, ACTION_AGENT_CREATE),
        (true, ACTION_AGENT_STOP),
        (true, ACTION_AGENT_CREATE),
    ]
    .into_iter()
    .enumerate()
    {
        let document = WorkspacePolicyDocument {
            schema_version: POLICY_SCHEMA_VERSION,
            defaults: BTreeMap::from([
                (
                    ACTION_AGENT_STOP.to_string(),
                    if denied == ACTION_AGENT_STOP {
                        PolicyEffect::Deny
                    } else {
                        PolicyEffect::Allow
                    },
                ),
                (
                    ACTION_AGENT_CREATE.to_string(),
                    if denied == ACTION_AGENT_CREATE {
                        PolicyEffect::Deny
                    } else {
                        PolicyEffect::Allow
                    },
                ),
            ]),
            groups: BTreeMap::new(),
            rules: Vec::new(),
        };
        store
            .replace(
                "default",
                index as u64,
                PolicyMode::Enforce,
                document,
                PolicyPrincipalRef {
                    kind: PolicyPrincipalKind::Human,
                    id: "owner".into(),
                },
            )
            .await
            .unwrap();
        let machine = Some(Extension(MachineSession {
            server_id: Some("machine-a".into()),
            workspace_id: Some("default".into()),
        }));
        let result = if restart {
            workloads::restart_agent(
                State(state.clone()),
                Extension(auth.clone()),
                Extension(PolicyEngine::durable(store.clone())),
                None,
                machine,
                HeaderMap::new(),
                Path(("default".into(), app.app_id.clone())),
            )
            .await
        } else {
            workloads::start_agent(
                State(state.clone()),
                Extension(auth.clone()),
                Extension(PolicyEngine::durable(store.clone())),
                None,
                machine,
                HeaderMap::new(),
                Path(("default".into(), app.app_id.clone())),
            )
            .await
        };
        let response = result.expect_err("denied lifecycle action").into_response();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "denied {denied}");
        assert!(
            rx.try_recv().is_err(),
            "authorization must precede all Controller commands"
        );
    }
}

fn test_config() -> BootstrapConfig {
    BootstrapConfig::new(
        Url::parse("https://treer.example/").expect("valid URL"),
        PathBuf::from("dist"),
        Url::parse("https://github.example/releases/latest/download").expect("valid release URL"),
    )
}

fn test_browser_access() -> BrowserAccess {
    BrowserAccess::new(
        &Url::parse("https://app.treer.ai/").expect("app URL"),
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .expect("browser access")
}

#[test]
fn browser_tunnels_accept_app_and_proxy_origins_only() {
    let access = test_browser_access();
    for origin in ["https://app.treer.ai", "https://proxy.treer.ai"] {
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, HeaderValue::from_static(origin));
        access
            .validate_tunnel_if_present(&headers)
            .expect("trusted tunnel origin");
    }
    let mut denied = HeaderMap::new();
    denied.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://attacker.example"),
    );
    assert!(access.validate_tunnel_if_present(&denied).is_err());
}

pub(super) fn test_ingress_config() -> IngressConfig {
    IngressConfig::new(
        Some(Url::parse("https://apps.treer.ai/").expect("ingress URL")),
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
        &Url::parse("https://app.treer.ai/").expect("app URL"),
    )
    .expect("ingress config")
}

#[test]
fn app_public_url_reports_the_managed_ingress_access() {
    let config = test_ingress_config();
    let now = Utc::now();
    let mut app = AppDeployment {
        startup_revision: 0,
        app_id: "app_1234".to_string(),
        workspace_id: "default".to_string(),
        name: "Public Docs".to_string(),
        server_id: "machine-a".to_string(),
        command: "python3".to_string(),
        args: vec![],
        cwd: ".".to_string(),
        port: 8080,
        hostname: String::new(),
        service_id: "svc_docs".to_string(),
        public_url: None,
        access: None,
        desired_state: AppDesiredState::Running,
        runtime_agent_id: None,
        restart_count: 0,
        status: AppDeploymentStatus::Pending,
        pid: None,
        exit_code: None,
        last_error: None,
        created_at: now,
        created_by: "agent-a".to_string(),
        updated_at: now,
        updated_by: "agent-a".to_string(),
    };
    let hostname = crate::auth::platform_service_hostname(
        &app.service_id,
        config.base_domain().expect("base domain"),
    )
    .expect("managed hostname");
    let ingress = ServiceIngress {
        ingress_id: "ing_docs".to_string(),
        workspace_id: "default".to_string(),
        service_id: app.service_id.clone(),
        hostname,
        access: ServiceIngressAccess::Public,
        enabled: true,
        created_at: now,
        created_by: "agent-a".to_string(),
        updated_at: now,
        updated_by: "agent-a".to_string(),
        limits: Default::default(),
    };

    attach_app_public_url(&config, &[ingress], &mut app);

    assert_eq!(app.access, Some(ServiceIngressAccess::Public));
    assert!(app
        .public_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://svc-")));
}

async fn admin_router(updater: crate::updater::UpdaterClient) -> Router {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        updater,
        crate::voice::VoiceServices::disabled(),
    )
}

async fn admin_cookie(app: Router) -> (Router, HeaderValue) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/login")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"password":"admin-password"}"#))
                .expect("login request"),
        )
        .await
        .expect("login response");
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .expect("admin cookie")
        .to_str()
        .expect("cookie text");
    let token = set_cookie.split(';').next().expect("cookie pair");
    (app, HeaderValue::from_str(token).expect("cookie header"))
}

#[tokio::test]
async fn registered_cookie_routes_reject_browser_writes_before_handlers() {
    super::route_catalog::take_paths();
    let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
    let paths = super::route_catalog::take_paths();
    assert!(paths.len() > 100, "enumerate the actual complete router");
    // Prevent a future raw registration from silently escaping this inventory.
    for source in [
        include_str!("routes.rs"),
        include_str!("../admin.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap(),
    ] {
        assert!(
            !source.contains(".route("),
            "use registered_route for Core routes"
        );
    }
    for (path, origin_class) in paths {
        // Explicit exceptions, with the authentication boundary that replaces
        // browser-cookie authority. Unknown new routes must be classified.
        let exception = if path.starts_with("/agent/") {
            Some("machine/Agent credentials; no browser session selection")
        } else if path == "/api/machines/enroll" {
            Some("single-use enrollment credential")
        } else if path.starts_with("/.treer/identity/")
            || path.starts_with("/.treer/apps/identity/")
        {
            Some("verifies an explicit signed token, never a browser session")
        } else if path == "/api/apps/oauth/authorize" || path == "/.treer/ingress/authorize" {
            Some("top-level navigation; fixed ingress, audience-bound single-use expiring code")
        } else if path == "/api/apps/oauth/token" {
            Some("single-use authorization code plus S256 verifier")
        } else if path.starts_with("/api/apps/") {
            Some("service-audience App identity or attested Provider runtime; no cookie fallback")
        } else if path.ends_with("/callback") && path.starts_with("/api/auth/oauth/") {
            Some("external OAuth callback: single-use state and browser cookie binding")
        } else if matches!(
            path.as_str(),
            "/install.sh" | "/install.ps1" | "/api/health" | "/.well-known/jwks.json"
        ) || path.starts_with("/artifacts/")
        {
            Some("public read-only resource")
        } else {
            None
        };
        if exception.is_some() {
            continue;
        }
        assert!(
            path.starts_with("/api/workspaces")
                || path.starts_with("/api/organizations")
                || path.starts_with("/api/auth/")
                || path.starts_with("/api/admin/")
                || path.starts_with("/api/invitations/"),
            "unclassified route {path}"
        );
        let concrete = path
            .split('/')
            .map(|segment| {
                if segment.starts_with('{') {
                    "fixture"
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&concrete)
                    .header(header::ORIGIN, "https://proxy.treer.example")
                    .header(header::COOKIE, "treer_session=synthetic-user")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        match origin_class {
            crate::auth::CookieOriginClass::Embedded => assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path}: Proxy origin must reach authentication, not CSRF denial"
            ),
            crate::auth::CookieOriginClass::Console => assert!(
                matches!(
                    response.status(),
                    StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
                ),
                "{path}: Proxy origin must not grant Core management authority"
            ),
        }
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            for origin in [None, Some("null"), Some("https://evil.example.test")] {
                let mut request = Request::builder()
                    .method(method)
                    .uri(&concrete)
                    .header(
                        header::COOKIE,
                        "treer_session=synthetic-user; treer_admin_session=synthetic-admin",
                    )
                    .header(header::CONTENT_TYPE, "text/plain");
                if let Some(origin) = origin {
                    request = request.header(header::ORIGIN, origin);
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::from("untrusted-body")).unwrap())
                    .await
                    .unwrap();
                assert!(
                    matches!(
                        response.status(),
                        StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
                    ),
                    "{method} {path}: {}",
                    response.status()
                );
            }
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(&concrete)
                        .header(header::ORIGIN, "https://app.treer.example")
                        .header("sec-fetch-site", "cross-site")
                        .header(header::CONTENT_TYPE, "text/plain")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert!(
                matches!(
                    response.status(),
                    StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
                ),
                "cross-site {method} {path}: {}",
                response.status()
            );
        }
    }
}

async fn spawn_updater_sidecar() -> Url {
    let app = Router::new()
        .route(
            "/v1/status",
            get(|| async {
                Json(serde_json::json!({"channel":"stable","services":[],"job":null}))
            }),
        )
        .route(
            "/v1/apply",
            post(|| async {
                (
                    StatusCode::ACCEPTED,
                    Json(serde_json::json!({
                        "channel": "stable",
                        "job": {"id": "job1", "state": "running", "error": null}
                    })),
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind sidecar");
    let addr = listener.local_addr().expect("sidecar address");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("sidecar server");
    });
    tokio::task::yield_now().await;
    Url::parse(&format!("http://{addr}/")).expect("sidecar URL")
}

#[test]
fn ingress_config_matches_one_wildcard_label_and_builds_urls() {
    let config = test_ingress_config();
    assert!(config.matches_hostname("demo.apps.treer.ai"));
    assert!(!config.matches_hostname("apps.treer.ai"));
    assert!(!config.matches_hostname("nested.demo.apps.treer.ai"));
    assert_eq!(
        config
            .url_for_hostname("demo.apps.treer.ai")
            .expect("ingress URL")
            .as_str(),
        "https://demo.apps.treer.ai/"
    );
}

#[tokio::test]
async fn trailing_slash_browser_tunnel_route_is_registered() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    for uri in [
        "/api/workspaces/default/virtual-hosts/self.test/proxy/",
        "/api/workspaces/default/apps/app-1/ui/",
        "/api/workspaces/default/apps/app-1/ui/app.js",
        "/api/workspaces/default/agents/agent-1/interface/ui",
        "/api/workspaces/default/agents/agent-1/interface/ui/",
        "/api/workspaces/default/agents/agent-1/interface/ui/ws",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("x-treer-interface-source", "controller-api")
                    .header("x-treer-agent-id", "agent-1")
                    .header("x-treer-interface-instance", "forged")
                    .body(Body::empty())
                    .expect("tunnel request"),
            )
            .await
            .expect("route response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/json",
            "{uri}"
        );
        let body = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        let error: ApiError =
            serde_json::from_slice(&body).expect("JSON error, never launcher HTML");
        assert_eq!(error.error.code, "authentication_required", "{uri}");
    }
}

#[tokio::test]
async fn workspace_deletion_route_requires_auth_and_deletes_the_workspace() {
    let auth = AuthStore::for_test("admin-password").await;
    let (invite, _) = auth
        .create_personal_invitation()
        .await
        .expect("personal invitation");
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/register")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "invite": invite,
                        "email": "owner@example.com",
                        "preferred_name": "Owner",
                        "password": "password123",
                    }))
                    .expect("register body"),
                ))
                .expect("register request"),
        )
        .await
        .expect("register response");
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = session_cookie(&response);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/organizations")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("organization list request"),
        )
        .await
        .expect("organization list response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("organization list body");
    let payload: Value = serde_json::from_slice(&bytes).expect("organization list JSON");
    let organization_id = payload["organizations"]
        .as_array()
        .expect("organization array")
        .first()
        .expect("personal organization")
        .get("organization_id")
        .expect("organization id")
        .as_str()
        .expect("organization id text")
        .to_string();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/workspaces")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::COOKIE, &cookie)
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "organization_id": organization_id,
                        "name": "Deletable",
                    }))
                    .expect("create workspace body"),
                ))
                .expect("create workspace request"),
        )
        .await
        .expect("create workspace response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("create workspace body");
    let payload: Value = serde_json::from_slice(&bytes).expect("create workspace JSON");
    let workspace_id = payload["workspace"]["workspace_id"]
        .as_str()
        .expect("workspace id")
        .to_string();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header(header::ORIGIN, "https://app.treer.example")
                .body(Body::empty())
                .expect("unauthenticated delete"),
        )
        .await
        .expect("unauthenticated response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("authenticated delete"),
        )
        .await
        .expect("delete response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("delete body");
    let payload: Value = serde_json::from_slice(&bytes).expect("delete JSON");
    assert_eq!(payload["workspace_id"], workspace_id);
    assert_eq!(payload["name"], "Deletable");
    assert_eq!(payload["organization_id"], organization_id);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/workspaces?organization_id={organization_id}"))
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .expect("workspace list request"),
        )
        .await
        .expect("list response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("list body");
    let payload: Value = serde_json::from_slice(&bytes).expect("list JSON");
    assert!(payload["workspaces"]
        .as_array()
        .expect("workspace array")
        .is_empty());

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri(format!("/api/workspaces/{workspace_id}"))
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, "https://app.treer.example")
                .body(Body::empty())
                .expect("repeat delete"),
        )
        .await
        .expect("repeat delete response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

fn session_cookie(response: &axum::response::Response) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("treer_session="))
        .map(|value| value.split(';').next().expect("cookie pair").to_string())
        .expect("session cookie")
}

#[tokio::test]
async fn rollout_gates_core_message_traffic() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let enrollment = auth
        .create_machine_enrollment("default", "test")
        .await
        .expect("create rollout test enrollment");
    let machine = auth
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim rollout test machine");
    let authorization = HeaderValue::from_str(&format!("Bearer {}", machine.machine_token))
        .expect("machine authorization header");
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::new(false),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );

    for path in [
        "/agent/workspaces/default/messages",
        "/api/apps/svc_mail/messages",
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(header::AUTHORIZATION, authorization.clone())
                    .body(Body::empty())
                    .expect("gated request"),
            )
            .await
            .expect("gated route response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read rollout error");
        let error: ApiError = serde_json::from_slice(&body).expect("decode rollout error");
        assert_eq!(error.error.code, "core_messages_disabled");
    }
}

#[tokio::test]
async fn machine_routes_expose_managed_apps_but_deny_direct_network_publication() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let enrollment = auth
        .create_machine_enrollment("default", "test")
        .await
        .expect("create App route enrollment");
    let machine = auth
        .claim_machine_enrollment(&enrollment)
        .await
        .expect("claim App route machine");
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/agent/workspaces/default/apps")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", machine.machine_token),
                )
                .body(Body::empty())
                .expect("App list request"),
        )
        .await
        .expect("App list response");
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read App list response");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("decode App list"),
        json!({ "apps": [] })
    );

    let response = app.clone().oneshot(Request::builder().method(Method::POST)
        .uri("/agent/workspaces/default/services")
        .header(header::AUTHORIZATION, format!("Bearer {}",machine.machine_token))
        .header(header::CONTENT_TYPE,"application/json")
        .body(Body::from(serde_json::to_vec(&json!({"name":"preview","server_id":machine.server_id,"target_port":3000,"protocol":"http"})).unwrap())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let data = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&data).unwrap();
    let id = value["service"]["service_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/agent/workspaces/default/services")
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", machine.machine_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"name":"wrong","server_id":"another-machine","target_port":3000}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!(
                    "/agent/workspaces/default/services/{id}/public-tcp"
                ))
                .header(
                    header::AUTHORIZATION,
                    format!("Bearer {}", machine.machine_token),
                )
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"access":"public"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let data = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let error: ApiError = serde_json::from_slice(&data).unwrap();
    assert_eq!(error.error.code, "public_tcp_not_configured");
}

#[tokio::test]
async fn workspace_ingress_redirects_humans_to_proxy_authorization() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "private app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    let ingress = auth
        .create_service_ingress(
            "default",
            "test-user",
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id,
                slug: None,
                access: ServiceIngressAccess::Workspace,
            },
        )
        .await
        .expect("create ingress");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let request = Request::builder()
        .uri("/dashboard?tab=active")
        .header(header::HOST, &ingress.hostname)
        .body(Body::empty())
        .expect("ingress request");
    let response = proxy_service_ingress(
        State(AppState::new()),
        Extension(auth),
        Extension(test_ingress_config()),
        Extension(identity),
        request,
    )
    .await
    .expect("authorization redirect");
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    let location = response
        .headers()
        .get(header::LOCATION)
        .expect("redirect location")
        .to_str()
        .expect("location text");
    assert!(location.starts_with("https://proxy.treer.ai/.treer/ingress/authorize?"));
    assert!(location.contains("hostname=svc-"));
    assert!(location.contains("return_path=%2Fdashboard%3Ftab%3Dactive"));
}

#[test]
fn browser_access_accepts_only_the_configured_origin_when_present() {
    let browser = test_browser_access();
    assert!(browser.validate_if_present(&HeaderMap::new()).is_ok());

    let mut allowed = HeaderMap::new();
    allowed.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://app.treer.ai"),
    );
    assert!(browser.validate_if_present(&allowed).is_ok());

    let mut denied = HeaderMap::new();
    denied.insert(
        header::ORIGIN,
        HeaderValue::from_static("https://other.treer.ai"),
    );
    let error = browser
        .validate_if_present(&denied)
        .expect_err("other browser origin must be denied");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.error.code, "browser_origin_denied");
}

#[tokio::test]
async fn cors_preflight_allows_the_configured_app_with_credentials() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/api/auth/login")
                .header(header::ORIGIN, "https://app.treer.ai")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                .header(
                    header::ACCESS_CONTROL_REQUEST_HEADERS,
                    "content-type,x-treer-console",
                )
                .body(Body::empty())
                .expect("preflight request"),
        )
        .await
        .expect("preflight response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&HeaderValue::from_static("https://app.treer.ai"))
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
        Some(&HeaderValue::from_static("true"))
    );
    if let Some(allow_headers) = response.headers().get(header::ACCESS_CONTROL_ALLOW_HEADERS) {
        assert!(allow_headers.to_str().unwrap().contains("x-treer-console"));
        assert!(
            !allow_headers
                .to_str()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("x-treer-client"),
            "native client header must not be CORS-allowed"
        );
    }
}

#[tokio::test]
async fn cors_headers_are_present_on_authenticated_route_errors() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/auth/me")
                .header(header::ORIGIN, "https://app.treer.ai")
                .body(Body::empty())
                .expect("authenticated request"),
        )
        .await
        .expect("authenticated response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some(&HeaderValue::from_static("https://app.treer.ai"))
    );
    assert_eq!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_CREDENTIALS),
        Some(&HeaderValue::from_static("true"))
    );
}

#[tokio::test]
async fn admin_update_requires_an_admin_session() {
    let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .body(Body::empty())
                .expect("update request"),
        )
        .await
        .expect("update response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn voice_asr_status_requires_authentication() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/voice/asr")
                .body(Body::empty())
                .expect("voice asr status request"),
        )
        .await
        .expect("voice asr status response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn voice_command_status_requires_authentication() {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let app = router(
        AppState::new(),
        test_config(),
        auth,
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/voice/command")
                .body(Body::empty())
                .expect("voice command status request"),
        )
        .await
        .expect("voice command status response");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

async fn voice_member_token(auth: &AuthStore, app: Router, workspace_id: &str) -> (Router, String) {
    let (invite, _) = auth
        .create_personal_invitation()
        .await
        .expect("personal invitation");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/register")
                .header("X-Treer-Client", "mobile")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "email": "voice@example.com",
                        "preferred_name": "Voice",
                        "password": "password123",
                        "invite": invite,
                    }))
                    .expect("register body"),
                ))
                .expect("register request"),
        )
        .await
        .expect("register response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("register body bytes"),
    )
    .expect("register json");
    let token = body["token"].as_str().expect("native token").to_string();
    let user_id = body["user_id"].as_str().expect("user id");
    sqlx::query(
        "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             VALUES($1, $2, 'owner', $3)",
    )
    .bind(format!("org_{workspace_id}"))
    .bind(user_id)
    .bind(Utc::now().to_rfc3339())
    .execute(&auth.pool())
    .await
    .expect("workspace membership");
    (app, token)
}

#[tokio::test]
async fn voice_command_is_unavailable_until_configured() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let state = AppState::new();
    state.ensure_workspace("default", "default").await;
    let app = router(
        state,
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let (app, token) = voice_member_token(&auth, app, "default").await;
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/voice/command")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("status request"),
        )
        .await
        .expect("status response");
    assert_eq!(status.status(), StatusCode::OK);
    let status_body: Value = serde_json::from_slice(
        &axum::body::to_bytes(status.into_body(), usize::MAX)
            .await
            .expect("status body"),
    )
    .expect("status json");
    assert_eq!(status_body["enabled"], false);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/workspaces/default/voice/command")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"text":"看一下 mac 上的 reviewer"}"#))
                .expect("command request"),
        )
        .await
        .expect("command response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error: ApiError = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("error body"),
    )
    .expect("error json");
    assert_eq!(error.error.code, "voice_llm_unavailable");
}

#[tokio::test]
async fn voice_command_turns_asr_text_into_agent_prompt() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let state = AppState::new();
    state.ensure_workspace("default", "lab").await;
    let now = Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "srv_mac".to_string(),
        workspace_id: "default".to_string(),
        name: "mac".to_string(),
        hostname: "MacBook-Pro.local".to_string(),
        root: "/tmp".to_string(),
        controller_build: treer_protocol::BuildInfo {
            version: "test".to_string(),
            git_commit: "test".to_string(),
        },
        host_build: treer_protocol::BuildInfo {
            version: "test".to_string(),
            git_commit: "test".to_string(),
        },
        supervision: None,
        labels: Default::default(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let agent = treer_protocol::AgentInfo {
        path: None,
        run_generation: 0,
        agent_id: "ag_reviewer".to_string(),
        workspace_id: "default".to_string(),
        server_id: "srv_mac".to_string(),
        kind: "codex".to_string(),
        name: "reviewer".to_string(),
        cwd: ".".to_string(),
        status: treer_protocol::AgentStatus::Idle,
        pid: None,
        started_at: now,
        updated_at: now,
        exited_at: None,
        exit_code: None,
        output_revision: 0,
        interface: None,
    };
    let connection_id = Uuid::new_v4();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server.clone(), connection_id, tx)
        .await
        .expect("register server");
    state
        .apply_snapshot(
            connection_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![agent],
            },
        )
        .await
        .expect("snapshot");
    let responder = state.clone();
    tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            let SocketFrame::Text(encoded) = frame else {
                continue;
            };
            let ProxyMessage::Command { envelope } =
                serde_json::from_str(&encoded).expect("command")
            else {
                continue;
            };
            responder
                .complete_command(CommandResult::success(
                    envelope.command_id,
                    json!({"ok": true}),
                ))
                .await;
        }
    });
    let upstream = crate::voice_llm::spawn_scripted_upstream(vec![
            json!({
                "id": "resp_1",
                "output": [{
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "treer",
                    "arguments": "{\"argv\":[\"agent\",\"prompt\",\"--machine\",\"mac\",\"reviewer\",\"给这个仓库写测试\"]}"
                }]
            }),
            json!({
                "id": "resp_2",
                "output": [{
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type":"output_text","text":"已经把写测试的任务发给 Mac 上的 reviewer 了。"}]
                }]
            }),
        ])
        .await;
    let llm = crate::voice_llm::VoiceLlmConfig::for_test(
        upstream.as_str(),
        crate::voice_llm::WireApi::Responses,
        "sk-test",
        "gpt-5.6-luna",
    );
    let app = router(
        state,
        test_config(),
        auth.clone(),
        PolicyEngine::allow_all(),
        identity,
        test_browser_access(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::with_llm(llm),
    );
    let (app, token) = voice_member_token(&auth, app, "default").await;
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/workspaces/default/voice/command")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"text":"请看一下 mac 这个设备上运行的 reviewer agent，让它进行写测试"}"#,
                ))
                .expect("command request"),
        )
        .await
        .expect("command response");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("command body"),
    )
    .expect("command json");
    assert!(body["reply"].as_str().expect("reply").contains("reviewer"));
    assert_eq!(body["tools"][0]["ok"], true);
    assert_eq!(body["tools"][0]["argv"][1], "prompt");
}

#[tokio::test]
async fn admin_update_is_unconfigured_without_a_sidecar() {
    let app = admin_router(crate::updater::UpdaterClient::disabled()).await;
    let (app, cookie) = admin_cookie(app).await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .expect("update request"),
        )
        .await
        .expect("update response");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    let error: ApiError = serde_json::from_slice(&body).expect("decode");
    assert_eq!(error.error.code, "updater_unconfigured");
}

#[tokio::test]
async fn admin_update_forwards_to_the_sidecar() {
    let sidecar = spawn_updater_sidecar().await;
    let updater =
        crate::updater::UpdaterClient::new(sidecar, "secret".to_string()).expect("client");
    let app = admin_router(updater).await;
    let (app, cookie) = admin_cookie(app).await;
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/admin/update")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .expect("status request"),
        )
        .await
        .expect("status response");
    assert_eq!(status.status(), StatusCode::OK);
    let apply = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/update")
                .header(header::ORIGIN, "https://app.treer.example")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .expect("apply request"),
        )
        .await
        .expect("apply response");
    assert_eq!(apply.status(), StatusCode::ACCEPTED);
}

#[test]
fn bootstrap_command_keeps_enrollment_tokens_out_of_the_url() {
    let config = test_config();
    let url = install_script_url(&config.public_url);
    assert_eq!(url.as_str(), "https://treer.example/install.sh");
    assert!(url.query().is_none());
}

#[tokio::test]
async fn legacy_enrollment_requests_without_identity_remain_supported() {
    let auth = AuthStore::for_test("admin-password").await;
    let enrollment = auth
        .create_machine_enrollment("default", "admin")
        .await
        .expect("create enrollment");
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {enrollment}")).expect("enrollment authorization"),
    );
    let response = enroll_machine(State(AppState::new()), Extension(auth), headers, None)
        .await
        .unwrap_or_else(|error| panic!("legacy enrollment: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn bootstrap_separates_public_installation_from_workspace_connection() {
    let config = test_config();
    let key = "enr_v1_64656661756c74_abc.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let (install, connect) = bootstrap_commands(&config.public_url, key);
    assert_eq!(
        install,
        "curl -fsSL 'https://treer.example/install.sh' | sh"
    );
    assert!(!install.contains("enr_"));
    assert!(!install.contains("connect"));
    assert_eq!(
        connect,
        format!("treer-agent-server connect --key '{key}' --proxy 'https://treer.example/'")
    );
    assert!(!connect.contains("install.sh"));
}

#[test]
fn windows_bootstrap_creates_an_explicit_user_root_before_connect() {
    let (install, connect) = windows_bootstrap_commands(
        &Url::parse("https://treer.example/").unwrap(),
        "enrollment'test",
    );
    assert!(!install.contains("enrollment"));
    assert!(connect.contains("IsNullOrWhiteSpace($env:LOCALAPPDATA)"));
    assert!(connect.contains("Join-Path $env:LOCALAPPDATA 'Treer\\workspace'"));
    assert!(
        connect.contains("New-Item -ItemType Directory -Force -Path $treerRoot -ErrorAction Stop")
    );
    assert!(connect.contains("connect --root \"$treerRoot\" --key 'enrollment''test'"));
    assert!(connect.find("New-Item").unwrap() < connect.find(" connect ").unwrap());
    assert!(!connect.contains("relocate-root"));
    assert!(!connect.contains("System32"));
}

#[test]
fn installer_is_posix_shell_and_keeps_enrollment_separate() {
    let config = test_config();
    let script = render_install_script(&config.public_url);
    assert!(script.starts_with("#!/bin/sh\nset -eu\n"));
    assert!(script.contains("platform=linux-aarch64"));
    assert!(!script.contains("unshare"));
    assert!(!script.contains("transparent agent networking"));
    assert!(script.contains("persistent proxy and agent host"));
    assert!(script.contains("container, or other sandbox"));
    assert!(script.contains(".local/libexec/treer"));
    assert!(script.contains("treer-agent-host"));
    assert!(script
        .contains("ln -sf \"$server_dir/treer-agent-server\" \"$install_dir/treer-agent-server\""));
    assert!(script.contains("https://treer.example/artifacts"));
    assert!(!script.contains("service --workspace"));
    assert!(!script.contains("machine_token"));
    assert!(!script.contains("TREER_MACHINE_TOKEN"));
    assert!(!script.contains("TREER_ENROLLMENT_KEY"));
    assert!(!script.contains("systemctl"));
    assert!(!script.contains("launchctl"));
    assert!(!script.contains("nohup"));
}

#[cfg(unix)]
#[test]
fn rendered_installer_has_valid_shell_syntax() {
    let config = test_config();
    let script = render_install_script(&config.public_url);
    let mut child = Command::new("sh")
        .arg("-n")
        .stdin(Stdio::piped())
        .spawn()
        .expect("start shell parser");
    child
        .stdin
        .take()
        .expect("shell stdin")
        .write_all(script.as_bytes())
        .expect("write installer");
    assert!(child.wait().expect("wait for shell parser").success());
}

#[test]
fn artifact_paths_reject_directory_traversal() {
    assert!(valid_artifact_component("linux-aarch64"));
    assert!(!valid_artifact_component("../linux-aarch64"));
    assert!(!valid_artifact_component("linux/aarch64"));
}

#[test]
fn release_artifact_names_match_tagged_assets() {
    let config = test_config();
    let url = release_artifact_url(&config, "darwin-aarch64", "treer-agent-host")
        .unwrap_or_else(|_| panic!("release artifact URL"));
    assert_eq!(
        url.as_str(),
        "https://github.example/releases/latest/download/treer-agent-host-darwin-aarch64"
    );
}

#[tokio::test]
async fn missing_local_artifacts_redirect_to_the_release() {
    let mut config = test_config();
    config.artifacts_dir = std::env::temp_dir().join(format!(
        "treer-missing-artifacts-{}",
        Uuid::new_v4().simple()
    ));
    let response = download_artifact(
        Extension(config),
        Path(("darwin-aarch64".to_string(), "treer".to_string())),
    )
    .await
    .unwrap_or_else(|_| panic!("missing artifact should redirect"));
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        response
            .headers()
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some("https://github.example/releases/latest/download/treer-darwin-aarch64")
    );
}

#[test]
fn display_names_are_trimmed_and_validated() {
    assert_eq!(
        normalize_display_name("  build machine  ".to_string()).expect("valid name"),
        "build machine"
    );
    assert!(normalize_display_name("  ".to_string()).is_err());
    assert!(normalize_display_name("bad\nname".to_string()).is_err());
    assert!(normalize_display_name("x".repeat(81)).is_err());
}

#[test]
fn launch_profiles_become_interactive_shell_requests() {
    let timestamp = "2026-08-20T00:00:00Z".parse().expect("valid timestamp");
    let profile = AgentLaunchProfile {
        profile_id: "alp_review".to_string(),
        workspace_id: "default".to_string(),
        name: "Reviewer".to_string(),
        description: String::new(),
        cwd: "packages/api".to_string(),
        command: "codex".to_string(),
        args: vec![
            "review".to_string(),
            "--base".to_string(),
            "main".to_string(),
        ],
        created_at: timestamp,
        created_by: "user".to_string(),
        updated_at: timestamp,
        updated_by: "user".to_string(),
    };
    let request = agent_request_from_launch_profile(
        &profile,
        LaunchAgentProfileRequest {
            folder_id: None,
            server_id: Some("machine-a".to_string()),
            agent_name: None,
            cwd: Some("reviews/42".to_string()),
            cols: 100,
            rows: 30,
        },
    )
    .expect("build create request");
    assert_eq!(request.server_id.as_deref(), Some("machine-a"));
    assert_eq!(request.kind, "shell");
    assert_eq!(request.name, "Reviewer");
    assert_eq!(request.cwd, "reviews/42");
    assert_eq!(request.args, ["codex", "review", "--base", "main"]);
    assert_eq!((request.cols, request.rows), (100, 30));

    let request = agent_request_from_launch_profile(
        &profile,
        LaunchAgentProfileRequest {
            folder_id: None,
            server_id: None,
            agent_name: None,
            cwd: None,
            cols: 120,
            rows: 36,
        },
    )
    .expect("build request with profile cwd");
    assert_eq!(request.cwd, "packages/api");
}

#[test]
fn machine_principals_cannot_target_another_machine() {
    let subject = PolicySubject::Machine {
        server_id: "machine-a".to_string(),
    };
    assert!(require_machine_target(Some(&subject), "machine-a").is_ok());
    let error = require_machine_target(Some(&subject), "machine-b")
        .expect_err("cross-machine operation must require Agent identity");
    assert_eq!(error.error.code, "agent_identity_required");
}

#[test]
fn same_machine_agents_can_probe_sibling_services() {
    let timestamp = "2026-08-20T00:00:00Z".parse().expect("valid timestamp");
    let service = MachineService {
        service_id: "svc_ui".to_string(),
        workspace_id: "default".to_string(),
        name: "codex-ui".to_string(),
        server_id: "machine-a".to_string(),
        target_agent_id: Some("ag_ui".to_string()),
        primary: false,
        target_host: "127.0.0.1".to_string(),
        target_port: 4173,
        protocol: MachineServiceProtocol::Http,
        created_at: timestamp,
        created_by: "agent:ag_ui".to_string(),
        updated_at: timestamp,
        updated_by: "agent:ag_ui".to_string(),
    };
    let installer = PolicySubject::Agent {
        server_id: "machine-a".to_string(),
        agent_id: "ag_installer".to_string(),
    };
    let other_machine = PolicySubject::Agent {
        server_id: "machine-b".to_string(),
        agent_id: "ag_other".to_string(),
    };
    assert!(require_agent_can_probe_service(&installer, &service).is_ok());
    assert_eq!(
        require_agent_can_probe_service(&other_machine, &service)
            .expect_err("cross-machine probe")
            .error
            .code,
        "service_not_owned"
    );
}

#[tokio::test]
async fn agent_policy_subject_is_bound_to_authenticated_machine() {
    let state = state_with_managed_agent().await;
    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let missing = agent_policy_subject(&state, &machine, &HeaderMap::new(), "default")
        .await
        .expect_err("agent identity is required");
    assert_eq!(missing.status, StatusCode::BAD_REQUEST);
    assert_eq!(missing.error.code, "invalid_agent_identity");

    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
    let subject = agent_policy_subject(&state, &machine, &headers, "default")
        .await
        .unwrap_or_else(|error| panic!("matching subject: {}", error.error.message));
    assert_eq!(
        subject,
        PolicySubject::Agent {
            server_id: "machine-a".to_string(),
            agent_id: "agent-a".to_string(),
        }
    );

    let error = agent_policy_subject(
        &state,
        &MachineSession {
            server_id: Some("machine-b".to_string()),
            workspace_id: Some("default".to_string()),
        },
        &headers,
        "default",
    )
    .await
    .expect_err("foreign machine must not claim the agent");
    assert_eq!(error.status, StatusCode::FORBIDDEN);
    assert_eq!(error.error.code, "policy_subject_mismatch");
}

#[tokio::test]
async fn agent_identity_tokens_use_the_canonical_service_audience() {
    let state = state_with_managed_agent().await;
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test",
            CreateMachineServiceRequest {
                name: "api".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create service");
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://treer.example/").expect("public URL"),
    )
    .await
    .expect("identity issuer");
    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));

    let response = agent_issue_identity_token(
        State(state.clone()),
        Extension(WorkloadIdentityApi {
            auth: auth.clone(),
            policy: PolicyEngine::allow_all(),
            issuer: identity.clone(),
        }),
        Extension(machine),
        headers,
        Path("default".to_string()),
        Json(WorkloadIdentityTokenRequest {
            audience: "api".to_string(),
        }),
    )
    .await
    .unwrap_or_else(|error| panic!("issue identity token: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read token response");
    let token: treer_protocol::WorkloadIdentityTokenResponse =
        serde_json::from_slice(&body).expect("decode token response");
    assert_eq!(token.audience, service.service_id);
    let verified = identity.verify(&token.access_token, &service.service_id);
    assert!(verified.active);
    let claims = verified.claims.expect("verified claims");
    assert_eq!(claims.sub, "agent-a");
    assert_eq!(claims.machine_id, "machine-a");

    let mut app_headers = HeaderMap::new();
    app_headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token))
            .expect("App authorization header"),
    );
    let (workspace_id, subject, principal) =
        app_message_identity(&state, &auth, &identity, &app_headers, &service.service_id)
            .await
            .expect("authenticate Agent App Message identity");
    assert_eq!(workspace_id, "default");
    assert_eq!(principal.id, "agent-a");
    assert!(matches!(
        subject,
        PolicySubject::Agent { server_id, agent_id }
            if server_id == "machine-a" && agent_id == "agent-a"
    ));
}

#[test]
fn app_recipients_share_one_agent_and_human_namespace() {
    let principals = vec![
        AppPrincipal {
            kind: AppPrincipalKind::Agent,
            id: "agent-reviewer".to_string(),
            name: "reviewer".to_string(),
            path: None,
            role: None,
        },
        AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: "user-owner".to_string(),
            name: "Owner".to_string(),
            path: None,
            role: Some("owner".to_string()),
        },
        AppPrincipal {
            kind: AppPrincipalKind::Human,
            id: "user-reviewer".to_string(),
            name: "reviewer".to_string(),
            path: None,
            role: Some("member".to_string()),
        },
    ];
    let owner = resolve_app_principal(&principals, "Owner").expect("unique human name");
    assert_eq!(owner.kind, AppPrincipalKind::Human);
    assert_eq!(owner.id, "user-owner");
    let stable = resolve_app_principal(&principals, "agent-reviewer").expect("stable Agent ID");
    assert_eq!(stable.kind, AppPrincipalKind::Agent);
    let ambiguous =
        resolve_app_principal(&principals, "reviewer").expect_err("ambiguous display name");
    assert_eq!(ambiguous.code, "recipient_ambiguous");
}

#[tokio::test]
async fn message_api_pins_durable_policy_and_hides_recipient_resolution_details() {
    let state = state_with_managed_agent().await;
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO users(id, email, email_verified, preferred_name, password_hash, created_at) \
             VALUES('user-reviewer', 'reviewer@example.test', TRUE, 'reviewer', 'unused', $1)",
    )
    .bind(&now)
    .execute(&auth.pool())
    .await
    .expect("seed duplicate-name human");
    sqlx::query(
        "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
             VALUES('org_default', 'user-reviewer', 'member', $1)",
    )
    .bind(&now)
    .execute(&auth.pool())
    .await
    .expect("seed duplicate-name membership");

    let messages = MessageStore::open(auth.pool())
        .await
        .expect("message store");
    let policy_store = WorkspacePolicyStore::new(auth.pool());
    let deny_send = WorkspacePolicyDocument {
        schema_version: POLICY_SCHEMA_VERSION,
        defaults: BTreeMap::from([(ACTION_MESSAGE_SEND.to_string(), PolicyEffect::Deny)]),
        groups: BTreeMap::new(),
        rules: Vec::new(),
    };
    let actor = PolicyPrincipalRef {
        kind: PolicyPrincipalKind::Human,
        id: "policy-owner".to_string(),
    };
    let monitor = policy_store
        .replace(
            "default",
            0,
            PolicyMode::Monitor,
            deny_send.clone(),
            actor.clone(),
        )
        .await
        .expect("install monitor policy");
    assert_eq!(monitor.revision, 1);

    let machine = MachineSession {
        server_id: Some("machine-a".to_string()),
        workspace_id: Some("default".to_string()),
    };
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
    let secret_body = "api-policy-body-must-not-enter-metadata";
    let request = |recipient: &str, key: &str, body: &str| SendMessageRequest {
        recipients: vec![recipient.to_string()],
        context_ids: Vec::new(),
        body: body.to_string(),
        expires_at: None,
        idempotency_key: Some(key.to_string()),
        correlation_id: Some("cor_api_policy".to_string()),
        trace_id: Some("trace_api_policy".to_string()),
        external_source: None,
    };

    let sent = send_core_message(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(PolicyEngine::durable(policy_store.clone())),
        Extension(messages.clone()),
        Extension(machine.clone()),
        Path("default".to_string()),
        headers.clone(),
        Json(request("agent-b", "api-monitor-send", secret_body)),
    )
    .await
    .expect("monitor policy must observe rather than deny");

    let envelope: Value = sqlx::query_scalar(
        "SELECT envelope FROM core_message_outbox \
             WHERE workspace_id = 'default' AND action = 'message.created' \
             ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&auth.pool())
    .await
    .expect("load Message outbox envelope");
    assert_eq!(envelope["resource"]["id"], sent.0.message.message_id);
    assert_eq!(envelope["workspace_revision"], monitor.revision);
    assert!(!envelope.to_string().contains(secret_body));

    let audit_payloads: Vec<String> =
        sqlx::query_scalar("SELECT payload::text FROM organization_audit_events")
            .fetch_all(&auth.pool())
            .await
            .expect("load audit payloads");
    assert!(
        audit_payloads
            .iter()
            .all(|payload| !payload.contains(secret_body)),
        "Message bodies must not enter audit payloads"
    );

    let nonexistent = send_core_message(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Extension(messages.clone()),
        Extension(machine.clone()),
        Path("default".to_string()),
        headers.clone(),
        Json(request("missing-recipient", "api-missing", "missing body")),
    )
    .await
    .expect_err("nonexistent recipient must be hidden");
    let duplicate = send_core_message(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(PolicyEngine::allow_all()),
        Extension(messages.clone()),
        Extension(machine.clone()),
        Path("default".to_string()),
        headers.clone(),
        Json(request("reviewer", "api-duplicate", "duplicate body")),
    )
    .await
    .expect_err("duplicate-name recipient must be hidden");

    let enforce = policy_store
        .replace(
            "default",
            monitor.revision,
            PolicyMode::Enforce,
            deny_send,
            actor,
        )
        .await
        .expect("enforce policy");
    assert_eq!(enforce.revision, 2);
    let hidden = send_core_message(
        State(state),
        Extension(auth.clone()),
        Extension(PolicyEngine::durable(policy_store)),
        Extension(messages),
        Extension(machine),
        Path("default".to_string()),
        headers,
        Json(request("agent-b", "api-hidden", "hidden body")),
    )
    .await
    .expect_err("enforced policy must hide the recipient");

    assert_eq!(nonexistent.error.code, "agent_not_in_folder");
    assert_eq!(nonexistent.error.hint, Some(vec![]));
    for error in [&duplicate, &hidden] {
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        assert_eq!(error.error.code, "message_recipient_unavailable");
        assert_eq!(
            error.error.message,
            "a recipient does not exist or is not available to this sender"
        );
    }
    let stored_messages: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM core_messages WHERE workspace_id = 'default'")
            .fetch_one(&auth.pool())
            .await
            .expect("count stored Messages");
    assert_eq!(stored_messages, 1, "all denied sends must be atomic");
}

#[tokio::test]
async fn browser_tunnel_rejects_tcp_services_before_opening_a_stream() {
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "database".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 5432,
                protocol: treer_protocol::MachineServiceProtocol::Tcp,
            },
        )
        .await
        .expect("create machine service");
    auth.create_virtual_network_host(
        "default",
        "test-user",
        CreateVirtualNetworkHostRequest {
            hostname: "database.internal".to_string(),
            service_id: service.service_id,
        },
    )
    .await
    .expect("create virtual host");

    let error = proxy_virtual_network_host(
        AppState::new(),
        auth,
        "default".to_string(),
        "database.internal".to_string(),
        String::new(),
        Request::new(Body::empty()),
    )
    .await
    .expect_err("TCP services must not enter the HTTP tunnel");
    assert_eq!(error.status, StatusCode::BAD_REQUEST);
    assert_eq!(error.error.code, "service_protocol_mismatch");
}

#[tokio::test]
async fn browser_tunnel_forwards_http_without_leaking_gateway_credentials() {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
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
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");

    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create machine service");
    auth.create_virtual_network_host(
        "default",
        "test-user",
        CreateVirtualNetworkHostRequest {
            hostname: "app.internal".to_string(),
            service_id: service.service_id,
        },
    )
    .await
    .expect("create virtual host");

    let controller_state = state.clone();
    let controller = tokio::spawn(async move {
        let open = match server_rx.recv().await.expect("network open") {
            SocketFrame::Binary(encoded) => {
                treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
            }
            _ => panic!("expected network open"),
        };
        assert_eq!(open.kind, treer_protocol::NetworkBinaryKind::Open);
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("open stream");

        let request = loop {
            let frame = match server_rx.recv().await.expect("HTTP request frame") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                }
                _ => continue,
            };
            if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                break String::from_utf8(frame.payload).expect("HTTP request text");
            }
        };
        assert!(request.starts_with("GET /status?full=1 HTTP/1.1\r\n"));
        assert!(request.contains("host: app.internal\r\n"));
        assert!(!request.to_ascii_lowercase().contains("cookie:"));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));

        controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\nSet-Cookie: internal=secret\r\n\r\nhello".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("close response");
    });

    let request = Request::builder()
        .uri("/ignored?full=1")
        .header(header::COOKIE, "treer_session=secret")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .expect("browser request");
    let response = proxy_virtual_network_host(
        state,
        auth,
        "default".to_string(),
        "app.internal".to_string(),
        "status".to_string(),
        request,
    )
    .await
    .unwrap_or_else(|error| panic!("tunnel request: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("read tunnel response");
    assert_eq!(&body[..], b"hello");
    controller.await.expect("join controller");
}

#[tokio::test]
async fn public_ingress_preserves_application_auth_and_strips_treer_headers() {
    let state = AppState::new();
    let now = chrono::Utc::now();
    let server = treer_protocol::ServerInfo {
        server_id: "machine-a".to_string(),
        workspace_id: "default".to_string(),
        name: "machine-a".to_string(),
        hostname: "machine-a".to_string(),
        root: "/tmp".to_string(),
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
        status: treer_protocol::ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };
    let connection_id = Uuid::new_v4();
    let (server_tx, mut server_rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server(server, connection_id, server_tx)
        .await
        .expect("register controller");

    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    let service = auth
        .create_machine_service(
            "default",
            "test-user",
            CreateMachineServiceRequest {
                name: "public app".to_string(),
                server_id: "machine-a".to_string(),
                target_agent_id: None,
                primary: false,
                target_host: "127.0.0.1".to_string(),
                target_port: 8080,
                protocol: treer_protocol::MachineServiceProtocol::Http,
            },
        )
        .await
        .expect("create machine service");
    let ingress = auth
        .create_service_ingress(
            "default",
            "test-user",
            "apps.treer.ai",
            CreateServiceIngressRequest {
                service_id: service.service_id,
                slug: None,
                access: ServiceIngressAccess::Public,
            },
        )
        .await
        .expect("create ingress");

    let controller_state = state.clone();
    let controller = tokio::spawn(async move {
        let open = match server_rx.recv().await.expect("network open") {
            SocketFrame::Binary(encoded) => {
                treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode open")
            }
            _ => panic!("expected network open"),
        };
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::Opened,
                    stream_id: open.stream_id.clone(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("open stream");
        let request = loop {
            let frame = match server_rx.recv().await.expect("HTTP request frame") {
                SocketFrame::Binary(encoded) => {
                    treer_protocol::NetworkBinaryFrame::decode(&encoded).expect("decode data")
                }
                _ => continue,
            };
            if frame.kind == treer_protocol::NetworkBinaryKind::Data {
                break String::from_utf8(frame.payload).expect("HTTP request text");
            }
        };
        let lower = request.to_ascii_lowercase();
        assert!(request.starts_with("GET /api/items?limit=2 HTTP/1.1\r\n"));
        assert!(lower.contains("authorization: bearer application-token\r\n"));
        assert!(lower.contains("cookie: app_session=visible\r\n"));
        assert!(!lower.contains("treer_ingress"));
        assert!(!lower.contains("x-treer-spoofed"));
        assert!(lower.contains("host: svc-"));
        assert!(lower.contains(".apps.treer.ai\r\n"));
        controller_state
                .relay_network_frame(
                    "default",
                    "machine-a",
                    connection_id,
                    treer_protocol::NetworkBinaryFrame {
                        kind: treer_protocol::NetworkBinaryKind::Data,
                        stream_id: open.stream_id.clone(),
                        payload: b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\nSet-Cookie: app_session=updated; Path=/\r\n\r\nok".to_vec(),
                    },
                )
                .await
                .expect("send HTTP response");
        controller_state
            .relay_network_frame(
                "default",
                "machine-a",
                connection_id,
                treer_protocol::NetworkBinaryFrame {
                    kind: treer_protocol::NetworkBinaryKind::HalfClose,
                    stream_id: open.stream_id,
                    payload: Vec::new(),
                },
            )
            .await
            .expect("close response");
    });
    let identity = IdentityIssuer::load(
        &auth,
        &Url::parse("https://proxy.treer.ai/").expect("proxy URL"),
    )
    .await
    .expect("identity issuer");
    let request = Request::builder()
        .uri("/api/items?limit=2")
        .header(header::HOST, &ingress.hostname)
        .header(header::AUTHORIZATION, "Bearer application-token")
        .header(
            header::COOKIE,
            "__Host-treer_ingress=private; app_session=visible",
        )
        .header("x-treer-spoofed", "false")
        .body(Body::empty())
        .expect("ingress request");
    let response = proxy_service_ingress(
        State(state),
        Extension(auth),
        Extension(test_ingress_config()),
        Extension(identity),
        request,
    )
    .await
    .unwrap_or_else(|error| panic!("ingress request: {}", error.error.message));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::SET_COOKIE),
        Some(&HeaderValue::from_static("app_session=updated; Path=/"))
    );
    controller.await.expect("join controller");
}

#[tokio::test]
async fn app_email_requires_owned_managed_app_identity_and_policy() {
    let (auth, _state, app, _rx) = app_lifecycle_fixture().await;
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://treer.example/").unwrap())
        .await
        .unwrap();
    let token = identity
        .issue(
            "default",
            "machine-a",
            app.runtime_agent_id.as_deref().unwrap(),
            &app.service_id,
        )
        .unwrap();
    let request = treer_protocol::SendAppEmailRequest {
        to: "person@example.test".into(),
        subject: "test".into(),
        text: "private".into(),
        idempotency_key: "k".into(),
    };
    let mut headers = HeaderMap::new();
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::allow_all()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::UNAUTHORIZED
    );
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token)).unwrap(),
    );
    let policy_store = WorkspacePolicyStore::new(auth.pool());
    policy_store
        .replace(
            "default",
            0,
            PolicyMode::Enforce,
            WorkspacePolicyDocument {
                schema_version: POLICY_SCHEMA_VERSION,
                defaults: BTreeMap::from([("email.send".into(), PolicyEffect::Deny)]),
                groups: BTreeMap::new(),
                rules: vec![],
            },
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::durable(policy_store)),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::FORBIDDEN
    );
    let wrong = identity
        .issue("default", "machine-a", "ordinary-agent", &app.service_id)
        .unwrap();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", wrong.access_token)).unwrap(),
    );
    assert_eq!(
        send_app_email(
            Extension(auth.clone()),
            Extension(identity.clone()),
            Extension(PolicyEngine::allow_all()),
            Path(app.service_id.clone()),
            headers.clone(),
            Json(request.clone())
        )
        .await
        .unwrap_err()
        .error
        .code,
        "email_app_required"
    );
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", token.access_token)).unwrap(),
    );
    assert_eq!(
        send_app_email(
            Extension(auth),
            Extension(identity),
            Extension(PolicyEngine::allow_all()),
            Path("svc_wrong".into()),
            headers,
            Json(request)
        )
        .await
        .unwrap_err()
        .status,
        StatusCode::UNAUTHORIZED
    );
}

/// Keys a streamed `AgentInfo` may carry (#141): the member-visible flat
/// inventory. `path` is tolerated only as null; folders, placements and pins
/// never appear on the stream.
const STREAMED_AGENT_KEYS: [&str; 17] = [
    "path",
    "agent_id",
    "workspace_id",
    "server_id",
    "kind",
    "name",
    "cwd",
    "status",
    "pid",
    "run_generation",
    "started_at",
    "updated_at",
    "exited_at",
    "exit_code",
    "output_revision",
    "interface",
    "interface_port",
];
const STREAMED_SERVER_KEYS: [&str; 13] = [
    "server_id",
    "workspace_id",
    "name",
    "hostname",
    "root",
    "controller_build",
    "host_build",
    "supervision",
    "labels",
    "available_agents",
    "status",
    "connected_at",
    "last_seen_at",
];
const STREAM_DENIED_KEYS: [&str; 7] = [
    "folder",
    "folder_id",
    "folders",
    "placement",
    "placements",
    "pins",
    "pinned_by",
];

fn assert_no_denied_keys(value: &Value, context: &str) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                assert!(
                    !STREAM_DENIED_KEYS.contains(&key.as_str()),
                    "{context}: Tree data key {key} on the stream"
                );
                if key == "path" {
                    assert!(inner.is_null(), "{context}: a path reached the stream");
                }
                assert_no_denied_keys(inner, context);
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| assert_no_denied_keys(item, context)),
        _ => {}
    }
}

fn assert_streamed_agent(agent: &Value, context: &str) {
    let keys = agent
        .as_object()
        .unwrap_or_else(|| panic!("{context}: agent object"));
    for key in keys.keys() {
        assert!(
            STREAMED_AGENT_KEYS.contains(&key.as_str()),
            "{context}: unexpected AgentInfo field {key} on the stream; extend the documented inventory or redact it"
        );
    }
    assert_no_denied_keys(agent, context);
}

/// Every workspace event kind carries only the documented member-visible
/// inventory: flat agent facts (including cwd and pid), machine facts, IDs.
pub(super) fn assert_stream_event_shape(event: &WorkspaceEvent) {
    let context = format!("event {}", event.event);
    assert_no_denied_keys(&event.data, &context);
    match event.event.as_str() {
        "workspace.snapshot" => {
            let mut keys: Vec<&str> = event
                .data
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["agents", "revision", "servers", "workspace"]);
            for agent in event.data["agents"].as_array().unwrap() {
                assert_streamed_agent(agent, &context);
            }
            for server in event.data["servers"].as_array().unwrap() {
                for key in server.as_object().unwrap().keys() {
                    assert!(
                        STREAMED_SERVER_KEYS.contains(&key.as_str()),
                        "{context}: server field {key}"
                    );
                }
            }
        }
        kind if kind.starts_with("agent.") => assert_streamed_agent(&event.data, &context),
        kind if kind.starts_with("server.") => {
            for key in event.data.as_object().unwrap().keys() {
                assert!(
                    STREAMED_SERVER_KEYS.contains(&key.as_str()),
                    "{context}: server field {key}"
                );
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn workspace_event_payloads_carry_only_the_flat_inventory() {
    let state = state_with_managed_agent().await;
    let mut events = state.subscribe();
    let snapshot = state.snapshot("default").await.unwrap();
    assert_stream_event_shape(&WorkspaceEvent {
        revision: snapshot.revision,
        workspace_id: "default".into(),
        event: "workspace.snapshot".into(),
        data: serde_json::to_value(&snapshot).unwrap(),
    });
    assert!(snapshot.agents.iter().all(|agent| agent.path.is_none()));
    state
        .rename_agent("default", "agent-a", "renamed".into())
        .await
        .unwrap();
    state
        .rename_server("default", "machine-a", "machine-renamed".into())
        .await
        .unwrap();
    state
        .publish_workspace_signal("default", "workspace.pins.changed", |revision| {
            json!({"revision": revision, "kind": "agent", "id": "agent-a", "action": "pin"})
        })
        .await
        .unwrap();
    let mut kinds = Vec::new();
    for _ in 0..3 {
        let event = events.recv().await.unwrap();
        assert_stream_event_shape(&event);
        kinds.push(event.event);
    }
    assert_eq!(
        kinds,
        ["agent.renamed", "server.renamed", "workspace.pins.changed"]
    );
}

async fn connect_event_stream(
    addr: std::net::SocketAddr,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    let request = Request::builder()
        .uri(format!("ws://{addr}/api/workspaces/default/events"))
        .header(header::HOST, addr.to_string())
        .header(header::ORIGIN, "https://app.treer.example")
        .header(header::COOKIE, format!("treer_session={token}"))
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(
            header::SEC_WEBSOCKET_KEY,
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (socket, response) = tokio_tungstenite::client_async(request, stream)
        .await
        .expect("websocket upgrade");
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    socket
}

async fn next_stream_message(
    socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
) -> tokio_tungstenite::tungstenite::Message {
    tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("timely stream message")
        .expect("open stream")
        .expect("stream frame")
}

fn stream_event(message: &tokio_tungstenite::tungstenite::Message) -> WorkspaceEvent {
    let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
        panic!("expected a text event, got {message:?}");
    };
    serde_json::from_str(text).expect("workspace event")
}

fn close_code(message: &tokio_tungstenite::tungstenite::Message) -> u16 {
    let tokio_tungstenite::tungstenite::Message::Close(Some(frame)) = message else {
        panic!("expected a close frame, got {message:?}");
    };
    assert!(frame.reason.is_empty(), "close reason must stay empty");
    u16::from(frame.code)
}

/// #141: a subscriber keeps receiving only while its session and workspace
/// membership hold. Revocation takes effect before the next delivery after
/// the re-check window, or on the idle tick, with fixed close codes and no
/// detail. Tree read never gates the flat inventory, so a Tree-read-denied
/// member receives agent events while GET /workload-tree answers 403.
#[tokio::test]
async fn workspace_event_stream_closes_on_revoked_membership_and_ended_session() {
    super::events::set_stream_revalidation_for_test(
        Duration::from_millis(200),
        Duration::from_millis(400),
    );
    let auth = AuthStore::for_test("admin-password").await;
    auth.seed_test_workspace("default").await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('member','member@example.test','Member','fixture','now'),('other','other@example.test','Other','fixture','now')")
        .execute(&auth.pool()).await.unwrap();
    for user in ["member", "other"] {
        sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default',$1,'member','now')")
            .bind(user).execute(&auth.pool()).await.unwrap();
        sqlx::query("INSERT INTO sessions(token,user_id,created_at,expires_at) VALUES($1,$2,'2026-01-01T00:00:00Z','2999-01-01T00:00:00Z')")
            .bind(format!("tok-{user}")).bind(user).execute(&auth.pool()).await.unwrap();
    }
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let identity = IdentityIssuer::load(&auth, &Url::parse("https://treer.example/").unwrap())
        .await
        .unwrap();
    let state = state_with_managed_agent().await;
    // Policy denies everything, including agent.tree.read: the flat inventory
    // is membership-based and still flows; the Tree route refuses.
    let denied = PolicyEngine::new(
        crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
            "policy_denied",
            "denied by fixture",
        )),
        vec![],
    );
    let app = router(
        state.clone(),
        test_config(),
        auth.clone(),
        denied,
        identity,
        // The cookie CSRF check and the browser-origin check must agree on
        // the Console origin, which the test auth store sets to app.treer.example.
        BrowserAccess::new(
            &Url::parse("https://app.treer.example/").unwrap(),
            &Url::parse("https://proxy.treer.example/").unwrap(),
        )
        .unwrap(),
        test_ingress_config(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    );
    let tree = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/workspaces/default/workload-tree")
                .header(header::COOKIE, "treer_session=tok-member")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(tree.status(), StatusCode::FORBIDDEN);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // A member sees the flat inventory and live agent events, no Tree data.
    let mut member = connect_event_stream(addr, "tok-member").await;
    let initial = stream_event(&next_stream_message(&mut member).await);
    assert_eq!(initial.event, "workspace.snapshot");
    assert_eq!(initial.workspace_id, "default");
    assert_stream_event_shape(&initial);
    assert_eq!(initial.data["agents"].as_array().unwrap().len(), 2);
    state
        .rename_agent("default", "agent-a", "renamed".into())
        .await
        .unwrap();
    let renamed = stream_event(&next_stream_message(&mut member).await);
    assert_eq!(renamed.event, "agent.renamed");
    assert_eq!(renamed.data["name"], "renamed");
    assert_stream_event_shape(&renamed);

    // Membership revoked: the next delivery after the window is a 4403 close
    // and the event itself never reaches the former member.
    sqlx::query("DELETE FROM organization_members WHERE user_id='member'")
        .execute(&auth.pool())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(250)).await;
    state
        .publish_workspace_signal("default", "workspace.pins.changed", |revision| {
            json!({"revision": revision, "kind": "agent", "id": "agent-a", "action": "pin"})
        })
        .await
        .unwrap();
    assert_eq!(close_code(&next_stream_message(&mut member).await), 4403);
    assert!(
        tokio::time::timeout(Duration::from_secs(2), member.next())
            .await
            .map(|frame| frame.is_none() || frame.is_some_and(|f| f.is_err()))
            .unwrap_or(false),
        "nothing follows the close frame"
    );

    // An ended session is noticed on the idle tick even without events.
    let mut other = connect_event_stream(addr, "tok-other").await;
    assert_eq!(
        stream_event(&next_stream_message(&mut other).await).event,
        "workspace.snapshot"
    );
    sqlx::query("DELETE FROM sessions WHERE token='tok-other'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert_eq!(close_code(&next_stream_message(&mut other).await), 4401);

    // A fresh connection with the ended session is refused at the upgrade.
    let request = Request::builder()
        .uri(format!("ws://{addr}/api/workspaces/default/events"))
        .header(header::HOST, addr.to_string())
        .header(header::ORIGIN, "https://app.treer.example")
        .header(header::COOKIE, "treer_session=tok-other")
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(
            header::SEC_WEBSOCKET_KEY,
            tokio_tungstenite::tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let refused = tokio_tungstenite::client_async(request, stream).await;
    assert!(
        matches!(refused, Err(tokio_tungstenite::tungstenite::Error::Http(ref response)) if response.status() == StatusCode::UNAUTHORIZED),
        "{refused:?}"
    );
}
