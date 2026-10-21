//! Fake-Controller integration against a private PostgreSQL instance. No
//! installed files, native updater, machine upgrade or deployment is touched.
use super::*;
use axum::body::{to_bytes, Bytes};
use std::sync::atomic::{AtomicUsize, Ordering};
use treer_protocol::{
    controller_update as wire,
    controller_update_runtime::{DispatchBinding, Inspection},
    CommandResult, ProxyMessage,
};

const WS: &str = "default";
const MACHINE: &str = "machine-a";
fn session() -> CurrentSession {
    CurrentSession {
        token: "session-one".into(),
        user_id: "creator".into(),
        email: "creator@example.test".into(),
        preferred_name: "Creator".into(),
    }
}
fn identity(c: char) -> wire::Identity {
    wire::Identity {
        release_sha: c.to_string().repeat(40),
        artifact_source_sha: c.to_string().repeat(40),
        controller_sha256: c.to_string().repeat(64),
        cli_sha256: c.to_string().repeat(64),
    }
}
fn target() -> wire::Target {
    wire::Target {
        target_id: "release-b".into(),
        identity: identity('b'),
        platform: wire::Platform::LinuxX86_64,
        version_label: "1.2.0".into(),
    }
}
async fn seeded() -> AuthStore {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace(WS).await;
    sqlx::query("INSERT INTO users(id,email,preferred_name,password_hash,created_at) VALUES('creator','creator@example.test','Creator','fixture','now')").execute(&auth.pool()).await.unwrap();
    sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES($1,'creator','member','now')").bind(format!("org_{WS}")).execute(&auth.pool()).await.unwrap();
    let mut current = target();
    current.target_id = "release-a".into();
    current.identity = identity('a');
    for (seq, t) in [(1, current), (2, target())] {
        sqlx::query("INSERT INTO controller_update_catalog_entries(platform,revision,release_sequence,target) VALUES('linux-x86_64',2,$1,$2)").bind(seq as i64).bind(serde_json::to_value(t).unwrap()).execute(&auth.pool()).await.unwrap();
    }
    auth
}
async fn decoded<T: serde::de::DeserializeOwned>(response: Response) -> T {
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap()
}
async fn attach(state: &AppState) -> tokio::sync::mpsc::UnboundedReceiver<SocketFrame> {
    let server = state.resolve_server(WS, MACHINE).await.unwrap();
    let connection_id = Uuid::new_v4();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    state
        .register_server_instance_with_capabilities(
            server.clone(),
            connection_id,
            "fixture-controller".into(),
            vec![wire::CAPABILITY.into()],
            tx,
        )
        .await
        .unwrap();
    state
        .apply_snapshot(
            connection_id,
            treer_protocol::AgentServerSnapshot {
                server,
                agents: vec![],
            },
        )
        .await
        .unwrap();
    rx
}
fn accepted(b: &DispatchBinding) -> wire::Operation {
    wire::Operation {
        operation_id: b.operation_id.clone(),
        current: b.current.clone(),
        target: b.target.clone(),
        state: wire::State::Accepted,
        phase: None,
        observation: wire::Observation::WaitingForReconnect,
        observed_identity: None,
        restored_identity: None,
        health: wire::Health::Unknown,
        receipt_revision: None,
        created_at: b.created_at,
        updated_at: b.created_at,
        observed_at: None,
        finished_at: None,
        error_code: None,
    }
}
#[tokio::test]
async fn durable_prepare_confirm_discover_poll_and_no_repeat_dispatch() {
    let auth = seeded().await;
    let state = super::tests::state_with_managed_agent().await;
    let mut rx = attach(&state).await;
    let counter = Arc::new(AtomicUsize::new(0));
    let observed = counter.clone();
    let fake_state = state.clone();
    let fake_auth = auth.clone();
    tokio::spawn(async move {
        let mut binding = None;
        while let Some(SocketFrame::Text(text)) = rx.recv().await {
            let ProxyMessage::Command { envelope } = serde_json::from_str(&text).unwrap() else {
                continue;
            };
            let result = match envelope.command {
                AgentCommand::ControllerUpdateInspect {} => serde_json::to_value(Inspection {
                    platform: wire::Platform::LinuxX86_64,
                    current: identity('a'),
                    observed_at: Utc::now(),
                    native_ready: true,
                })
                .unwrap(),
                AgentCommand::ControllerUpdateDispatch { binding: b } => {
                    observed.fetch_add(1, Ordering::SeqCst);
                    // The acceptance/claim is committed before any frame exists.
                    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM controller_update_operations WHERE operation_id=$1 AND holds_slot").bind(&b.operation_id).fetch_one(&fake_auth.pool()).await.unwrap();
                    assert_eq!(count, 1);
                    let op = accepted(&b);
                    binding = Some(b);
                    serde_json::to_value(op).unwrap()
                }
                AgentCommand::ControllerUpdateOperation { operation_id } => {
                    let b = binding.as_ref().unwrap();
                    assert_eq!(operation_id, b.operation_id);
                    let mut op = accepted(b);
                    op.state = wire::State::Succeeded;
                    op.phase = Some(wire::Phase::Complete);
                    op.health = wire::Health::Healthy;
                    op.observation = wire::Observation::Available;
                    op.receipt_revision = Some(9);
                    op.observed_identity = Some(b.target.identity.clone());
                    op.updated_at = Utc::now();
                    op.observed_at = Some(op.updated_at);
                    op.finished_at = Some(op.updated_at);
                    serde_json::to_value(op).unwrap()
                }
                _ => panic!("update path must never dispatch Exec or a recipe"),
            };
            fake_state
                .complete_command(CommandResult::success(envelope.command_id, result))
                .await;
        }
    });
    let policy = PolicyEngine::allow_all();
    let creator = session();
    let p = controller_update_prepare(
        State(state.clone()),
        Extension(auth.clone()),
        Extension(policy.clone()),
        Extension(creator.clone()),
        Path((WS.into(), MACHINE.into())),
        Bytes::from_static(br#"{"target_id":"release-b"}"#),
    )
    .await
    .unwrap();
    let p: wire::Preview = decoded(p).await;
    p.validate().unwrap();
    let body = Bytes::from(
        serde_json::to_vec(&wire::ConfirmRequest {
            confirmation_token: p.confirmation_token,
        })
        .unwrap(),
    );
    let op: wire::Operation = decoded(
        controller_update_confirm(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(creator.clone()),
            Path((WS.into(), MACHINE.into())),
            body.clone(),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(op.operation_id, p.operation_id);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let _: wire::Operation = decoded(
        controller_update_confirm(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(creator.clone()),
            Path((WS.into(), MACHINE.into())),
            body,
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    let discovery: wire::Discovery = decoded(
        controller_update_operations(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(creator.clone()),
            Path((WS.into(), MACHINE.into())),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(discovery.operations.len(), 1);
    let done: wire::Operation = decoded(
        controller_update_operation(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(creator.clone()),
            Path((WS.into(), MACHINE.into(), op.operation_id.clone())),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(done.state, wire::State::Succeeded);
    done.validate().unwrap();
    // Projection remains readable after loss/replacement of the Controller.
    let fresh = super::tests::state_with_managed_agent().await;
    let recovered: wire::Discovery = decoded(
        controller_update_operations(
            State(fresh),
            Extension(auth),
            Extension(policy),
            Extension(creator),
            Path((WS.into(), MACHINE.into())),
        )
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(recovered.operations[0], done);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsupported_machine_keeps_readonly_discovery_and_refuses_prepare() {
    let auth = seeded().await;
    let state = super::tests::state_with_managed_agent().await;
    let policy = PolicyEngine::allow_all();
    let creator = session();
    let d: wire::Discovery = decoded(
        controller_update_operations(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(policy.clone()),
            Extension(creator.clone()),
            Path((WS.into(), MACHINE.into())),
        )
        .await
        .unwrap(),
    )
    .await;
    assert!(d.operations.is_empty());
    let result = controller_update_prepare(
        State(state),
        Extension(auth),
        Extension(policy),
        Extension(creator),
        Path((WS.into(), MACHINE.into())),
        Bytes::from_static(br#"{"target_id":"release-b"}"#),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn pending_update_is_refused_instead_of_replayed_to_replacement_controller() {
    let state = super::tests::state_with_managed_agent().await;
    let mut original = attach(&state).await;
    let sender = state.clone();
    let call = tokio::spawn(async move {
        sender
            .send_command(
                WS,
                MACHINE,
                AgentCommand::ControllerUpdateDispatch {
                    binding: DispatchBinding {
                        operation_id: "12345678-1234-4234-8234-123456789abc".into(),
                        workspace_id: WS.into(),
                        server_id: MACHINE.into(),
                        current: identity('a'),
                        target: target(),
                        catalog_revision: 2,
                        plan_digest: "a".repeat(64),
                        created_at: Utc::now(),
                    },
                },
            )
            .await
    });
    // Observe the first command, but do not complete it: the result is uncertain.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), original.recv())
            .await
            .unwrap()
            .unwrap();
        if let SocketFrame::Text(text) = frame {
            if matches!(
                serde_json::from_str::<ProxyMessage>(&text),
                Ok(ProxyMessage::Command { .. })
            ) {
                break;
            }
        }
    }
    let mut replacement = attach(&state).await;
    // The replacement's first snapshot invokes pending-command reconciliation.
    let result = tokio::time::timeout(Duration::from_secs(2), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert_eq!(result.code, "controller_update_dispatch_stale");
    while let Ok(frame) = replacement.try_recv() {
        if let SocketFrame::Text(text) = frame {
            assert!(!matches!(
                serde_json::from_str::<ProxyMessage>(&text),
                Ok(ProxyMessage::Command { .. })
            ));
        }
    }
}

#[tokio::test]
async fn every_update_endpoint_rechecks_machine_exec_before_any_controller_io() {
    let auth = seeded().await;
    let state = super::tests::state_with_managed_agent().await;
    let mut rx = attach(&state).await;
    while rx.try_recv().is_ok() {}
    let denied = PolicyEngine::new(
        crate::policy::PolicyDecision::Deny(crate::policy::PolicyDenial::new(
            "policy_denied",
            "denied by fixture",
        )),
        vec![],
    );
    macro_rules! call {
        ($handler:ident $(, $extra:expr)?) => {
            $handler(State(state.clone()), Extension(auth.clone()),
                Extension(denied.clone()), Extension(session()),
                Path((WS.into(), MACHINE.into())) $(, $extra)?).await.unwrap_err()
        };
    }
    for error in [
        call!(controller_update_catalog),
        call!(controller_update_operations),
        call!(
            controller_update_prepare,
            Bytes::from_static(br#"{"target_id":"release-b"}"#)
        ),
        call!(
            controller_update_confirm,
            Bytes::from_static(br#"{"confirmation_token":"00000000000000000000000000000000"}"#)
        ),
        controller_update_operation(
            State(state),
            Extension(auth),
            Extension(denied),
            Extension(session()),
            Path((WS.into(), MACHINE.into(), Uuid::new_v4().to_string())),
        )
        .await
        .unwrap_err(),
    ] {
        assert_eq!(error.status, StatusCode::FORBIDDEN);
    }
    assert!(rx.try_recv().is_err());
}
