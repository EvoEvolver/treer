use super::*;
use crate::auth::AuthStore;
use treer_protocol::{
    CreateMachineServiceRequest, CreateServiceIngressRequest, CreateWorkloadFolderRequest,
    MachineServiceProtocol, ServiceIngressAccess, SetWorkloadPlacementRequest,
    UpdateMachineServiceRequest, UpdateServiceIngressRequest, UpdateWorkloadFolderRequest,
};

async fn events(store: &AuthStore) -> Vec<OrganizationAuditEvent> {
    list(&store.pool(), "org_audit", None, None, 100)
        .await
        .unwrap()
}

async fn fixture() -> AuthStore {
    let store = AuthStore::for_test("synthetic-password").await;
    store.seed_test_workspace("audit").await;
    store
}

fn caller(id: &str) -> MutationActor {
    MutationActor {
        kind: "agent",
        id: Some(id.into()),
    }
}

#[tokio::test]
async fn tree_mutations_are_atomic_and_noops_or_rejections_do_not_append() {
    let store = fixture().await;
    MUTATION_ACTOR
        .scope(caller("ag_caller"), async {
            let folder = store
                .create_workload_folder(
                    "audit",
                    CreateWorkloadFolderRequest {
                        name: "private-name".into(),
                        parent_id: None,
                    },
                )
                .await
                .unwrap();
            store
                .update_workload_folder(
                    "audit",
                    &folder.folder_id,
                    UpdateWorkloadFolderRequest {
                        name: Some(folder.name.clone()),
                        parent_id: None,
                    },
                )
                .await
                .unwrap();
            assert_eq!(events(&store).await.len(), 1);
            assert!(store
                .update_workload_folder(
                    "audit",
                    &folder.folder_id,
                    UpdateWorkloadFolderRequest {
                        name: None,
                        parent_id: Some(Some(folder.folder_id.clone()))
                    }
                )
                .await
                .is_err());
            assert_eq!(events(&store).await.len(), 1);
            store
                .update_workload_folder(
                    "audit",
                    &folder.folder_id,
                    UpdateWorkloadFolderRequest {
                        name: Some("renamed".into()),
                        parent_id: None,
                    },
                )
                .await
                .unwrap();
            store
                .create_named_agent_credential("audit", "srv_test", "ag_target", "target", None)
                .await
                .unwrap();
            for _ in 0..2 {
                store
                    .set_workload_placement(
                        "audit",
                        "ag_target",
                        SetWorkloadPlacementRequest {
                            folder_id: Some(folder.folder_id.clone()),
                            position: 100,
                        },
                    )
                    .await
                    .unwrap();
            }
            assert!(store
                .remove_workload_folder("audit", &folder.folder_id, true)
                .await
                .is_err());
            store
                .remove_workload_folder("audit", &folder.folder_id, false)
                .await
                .unwrap();
            let rows = events(&store).await;
            assert_eq!(rows.len(), 5);
            assert_eq!(
                rows.iter()
                    .filter(|e| e.action == "tree.placement.update")
                    .count(),
                1
            );
            assert!(rows
                .iter()
                .all(|e| e.actor_id.as_deref() == Some("ag_caller")
                    && e.payload == serde_json::json!({})
                    && e.resource_name.is_none()));
            assert!(!serde_json::to_string(&rows)
                .unwrap()
                .contains("private-name"));
        })
        .await;
    sqlx::query("ALTER TABLE organization_audit_events ADD CONSTRAINT reject_test_audit CHECK(action <> 'tree.folder.create') NOT VALID").execute(&store.pool()).await.unwrap();
    assert!(store
        .create_workload_folder(
            "audit",
            CreateWorkloadFolderRequest {
                name: "rolled-back".into(),
                parent_id: None
            }
        )
        .await
        .is_err());
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM workload_folders WHERE name='rolled-back')",
    )
    .fetch_one(&store.pool())
    .await
    .unwrap();
    assert!(!exists);
}

#[tokio::test]
async fn enrollment_audit_failure_rolls_back_consumption_and_replay_is_not_a_change() {
    let store = fixture().await;
    let key = store
        .create_machine_enrollment("audit", "usr_creator")
        .await
        .unwrap();
    sqlx::query("ALTER TABLE organization_audit_events ADD CONSTRAINT reject_test_audit CHECK(action <> 'machine.enroll')").execute(&store.pool()).await.unwrap();
    assert!(store.claim_machine_enrollment(&key).await.is_err());
    assert!(events(&store).await.is_empty());
    sqlx::query("ALTER TABLE organization_audit_events DROP CONSTRAINT reject_test_audit")
        .execute(&store.pool())
        .await
        .unwrap();
    store.claim_machine_enrollment(&key).await.unwrap();
    assert!(store.claim_machine_enrollment(&key).await.is_err());
    let rows = events(&store).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].action, "machine.enroll");
    assert_eq!(rows[0].actor_id.as_deref(), Some("usr_creator"));
    assert_eq!(rows[0].payload, serde_json::json!({}));
    assert!(!serde_json::to_string(&rows).unwrap().contains(&key));
}

#[tokio::test]
async fn reenrollment_is_a_real_rotation_and_app_compound_mutations_are_atomic() {
    let store = fixture().await;
    let mut server = None;
    for _ in 0..2 {
        let key = store
            .create_machine_enrollment("audit", "usr_creator")
            .await
            .unwrap();
        let claim = store
            .claim_machine_enrollment_for_installation(
                &key,
                Some("mid_0123456789abcdef0123456789abcdef"),
                None,
                None,
            )
            .await
            .unwrap();
        if let Some(previous) = &server {
            assert_eq!(previous, &claim.server_id);
        }
        server = Some(claim.server_id);
    }
    let rows = events(&store).await;
    assert_eq!(
        rows.iter().filter(|e| e.action == "machine.enroll").count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|e| e.action == "machine.reenroll")
            .count(),
        1
    );
    let app = store
        .create_app_deployment(
            "audit",
            "system",
            server.unwrap(),
            treer_protocol::CreateAppDeploymentRequest {
                folder_id: None,
                server_id: None,
                name: "private-app".into(),
                command: "synthetic-command".into(),
                args: vec!["synthetic-secret-argument".into()],
                cwd: ".".into(),
                port: 3000,
                hostname: String::new(),
                public: false,
            },
        )
        .await
        .unwrap();
    let rows = events(&store).await;
    for action in [
        "service.create",
        "virtual_host.create",
        "tree.placement.create",
    ] {
        assert_eq!(rows.iter().filter(|e| e.action == action).count(), 1);
    }
    sqlx::query("ALTER TABLE organization_audit_events ADD CONSTRAINT reject_test_audit CHECK(action <> 'service.delete') NOT VALID").execute(&store.pool()).await.unwrap();
    assert!(store
        .delete_app_deployment("audit", &app.app_id)
        .await
        .is_err());
    assert!(store
        .resolve_app_deployment("audit", &app.app_id)
        .await
        .is_ok());
    assert!(store
        .resolve_machine_service("audit", &app.service_id)
        .await
        .is_ok());
    sqlx::query("ALTER TABLE organization_audit_events DROP CONSTRAINT reject_test_audit")
        .execute(&store.pool())
        .await
        .unwrap();
    store
        .delete_app_deployment("audit", &app.app_id)
        .await
        .unwrap();
    let rows = events(&store).await;
    assert_eq!(
        rows.iter().filter(|e| e.action == "service.delete").count(),
        1
    );
    assert_eq!(
        rows.iter()
            .filter(|e| e.action == "tree.placement.delete")
            .count(),
        1
    );
    assert!(!serde_json::to_string(&rows)
        .unwrap()
        .contains("synthetic-secret-argument"));
}

fn service_request(name: &str) -> CreateMachineServiceRequest {
    CreateMachineServiceRequest {
        name: name.into(),
        server_id: "srv_test".into(),
        target_agent_id: None,
        primary: false,
        target_host: "127.0.0.1".into(),
        target_port: 3000,
        protocol: MachineServiceProtocol::Http,
    }
}

#[tokio::test]
async fn network_audits_only_changes_and_failure_does_not_make_committed_write_retryable() {
    let store = fixture().await;
    MUTATION_ACTOR
        .scope(caller("ag_network"), async {
            let service = store
                .create_machine_service(
                    "audit",
                    "ignored-untrusted-actor",
                    service_request("private-service"),
                )
                .await
                .unwrap();
            let host = store
                .create_virtual_network_host(
                    "audit",
                    "ignored",
                    treer_protocol::CreateVirtualNetworkHostRequest {
                        hostname: "private.example.test".into(),
                        service_id: service.service_id.clone(),
                    },
                )
                .await
                .unwrap();
            assert!(store
                .create_virtual_network_host(
                    "audit",
                    "ignored",
                    treer_protocol::CreateVirtualNetworkHostRequest {
                        hostname: host.hostname.clone(),
                        service_id: service.service_id.clone()
                    }
                )
                .await
                .is_err());
            store
                .delete_virtual_network_host("audit", &host.hostname)
                .await
                .unwrap();
            store
                .update_machine_service(
                    "audit",
                    &service.service_id,
                    "ignored",
                    UpdateMachineServiceRequest {
                        target_port: Some(3001),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            store
                .update_machine_service(
                    "audit",
                    &service.service_id,
                    "ignored",
                    UpdateMachineServiceRequest {
                        target_port: Some(3001),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let request = CreateServiceIngressRequest {
                service_id: service.service_id.clone(),
                slug: None,
                access: ServiceIngressAccess::Workspace,
            };
            let ingress = store
                .create_service_ingress("audit", "ignored", "apps.example.test", request.clone())
                .await
                .unwrap();
            store
                .create_service_ingress("audit", "ignored", "apps.example.test", request)
                .await
                .unwrap();
            for _ in 0..2 {
                store
                    .update_service_ingress(
                        "audit",
                        &ingress.ingress_id,
                        "ignored",
                        UpdateServiceIngressRequest {
                            access: None,
                            enabled: Some(false),
                        },
                    )
                    .await
                    .unwrap();
            }
            store
                .delete_service_ingress("audit", &ingress.ingress_id)
                .await
                .unwrap();
            assert!(store
                .delete_service_ingress("audit", &ingress.ingress_id)
                .await
                .is_err());
            store
                .delete_machine_service("audit", &service.service_id)
                .await
                .unwrap();
            let rows = events(&store).await;
            assert_eq!(rows.len(), 8);
            assert!(rows
                .iter()
                .all(|e| e.actor_id.as_deref() == Some("ag_network")
                    && e.payload == serde_json::json!({})));
            let encoded = serde_json::to_string(&rows).unwrap();
            assert!(!encoded.contains("private-service") && !encoded.contains("127.0.0.1"));
        })
        .await;
    sqlx::query("ALTER TABLE organization_audit_events ADD CONSTRAINT reject_test_audit CHECK(action <> 'service.create') NOT VALID").execute(&store.pool()).await.unwrap();
    let committed = store
        .create_machine_service("audit", "system", service_request("committed"))
        .await
        .unwrap();
    assert!(store
        .resolve_machine_service("audit", &committed.service_id)
        .await
        .is_ok());
    assert_eq!(events(&store).await.len(), 8);
}

#[tokio::test]
async fn new_events_retain_workspace_origin_after_deletion_without_guessing_history() {
    let store = fixture().await;
    mutation_best_effort(&store.pool(), "audit", "agent.prompt", "agent", "ag_target").await;
    sqlx::query("INSERT INTO organization_audit_events(event_id,organization_id,occurred_at,actor_kind,source,action,outcome,resource_kind,resource_id,payload) VALUES('aud_legacy','org_audit',$1,'system','test','legacy','succeeded','workspace','unknown','{}')").bind(Utc::now().to_rfc3339()).execute(&store.pool()).await.unwrap();
    sqlx::query("DELETE FROM workspaces WHERE workspace_id='audit'")
        .execute(&store.pool())
        .await
        .unwrap();
    let rows = events(&store).await;
    assert_eq!(
        rows.iter()
            .find(|e| e.action == "agent.prompt")
            .unwrap()
            .workspace_id
            .as_deref(),
        Some("audit")
    );
    assert!(rows
        .iter()
        .find(|e| e.action == "legacy")
        .unwrap()
        .workspace_id
        .is_none());
}

#[tokio::test]
async fn request_actor_is_future_scoped_and_background_does_not_inherit() {
    let store = fixture().await;
    let pool = store.pool();
    let a = MUTATION_ACTOR.scope(
        caller("ag_one"),
        mutation_best_effort(&pool, "audit", "service.create", "service", "svc_one"),
    );
    let b = MUTATION_ACTOR.scope(
        caller("ag_two"),
        mutation_best_effort(&pool, "audit", "service.create", "service", "svc_two"),
    );
    tokio::join!(a, b);
    MUTATION_ACTOR
        .scope(caller("ag_parent"), async {
            let pool = store.pool();
            tokio::spawn(async move {
                mutation_best_effort(
                    &pool,
                    "audit",
                    "service.create",
                    "service",
                    "svc_background",
                )
                .await
            })
            .await
            .unwrap();
        })
        .await;
    let rows = events(&store).await;
    for (resource, actor) in [
        ("svc_one", Some("ag_one")),
        ("svc_two", Some("ag_two")),
        ("svc_background", None),
    ] {
        assert_eq!(
            rows.iter()
                .find(|e| e.resource_id == resource)
                .unwrap()
                .actor_id
                .as_deref(),
            actor
        );
    }
}

#[test]
fn mutation_actions_drop_every_payload_field_even_previously_known_scalars() {
    for action in [
        "tree.folder.create",
        "tree.folder.update",
        "tree.folder.delete",
        "tree.placement.create",
        "tree.placement.update",
        "tree.placement.delete",
        "tree.workload.rename",
        "service.create",
        "service.update",
        "service.delete",
        "virtual_host.create",
        "virtual_host.delete",
        "ingress.http.create",
        "ingress.http.update",
        "ingress.http.delete",
        "ingress.tcp.publish",
        "ingress.tcp.unpublish",
        "machine.enroll",
        "machine.reenroll",
        "agent.prompt",
        "unknown",
    ] {
        assert_eq!(
            workspace_payload(
                action,
                &serde_json::json!({"body":"synthetic", "credential":"synthetic", "server_id":"srv_test", "exit_code":0})
            ),
            serde_json::json!({}),
            "{action}"
        );
    }
}
