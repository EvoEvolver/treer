use super::*;
use serde_json::json;
use treer_protocol::{
    CreateWorkloadFolderRequest, UpdateWorkloadFolderRequest, WorkspacePolicyDocument,
};

fn document() -> WorkspacePolicyDocument {
    serde_json::from_value(json!({"schema_version":2,"defaults":{"agent.prompt":"deny"},"rules":[
        {"id":"team","priority":100,"effect":"allow","subjects":[{"kind":"agent","tree":{"path_glob":"/team/*"}}],"actions":["agent.prompt"],"resources":[{"kind":"agent","tree":{"path_prefix":{"path":"/team","include_self":false}}}],"bindings":{"home":{"ref":"subject.tree.folder"}},"conditions":[{"op":"resource_below_folder","folder":{"var":"home"}}]}
    ]})).unwrap()
}

#[tokio::test]
async fn current_paths_follow_rename_on_two_proxies_without_policy_refresh() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("paths").await;
    auth.seed_test_workspace("other").await;
    let folder = auth
        .create_workload_folder(
            "paths",
            CreateWorkloadFolderRequest {
                name: "team".into(),
                parent_id: None,
            },
        )
        .await
        .unwrap();
    for id in ["sender", "target"] {
        auth.create_named_agent_credential("paths", "machine", id, id, Some(&folder.folder_id))
            .await
            .unwrap();
    }
    let store = WorkspacePolicyStore::new(auth.pool());
    store
        .replace(
            "paths",
            0,
            PolicyMode::Enforce,
            document(),
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let engines = [
        PolicyEngine::durable(store.clone()),
        PolicyEngine::durable(store),
    ];
    let mut request = PolicyRequest::new(
        "paths",
        PolicySubject::Agent {
            agent_id: "sender".into(),
            server_id: "machine".into(),
        },
        "agent.prompt",
        PolicyResource::new("agent", "target"),
    );
    for engine in &engines {
        engine.authorize(&request).await.unwrap();
    }
    let before = engines[0].enrich(&request).await.unwrap();
    assert_eq!(before.resource_tree.as_ref().unwrap().path, "/team/target");
    auth.update_workload_folder(
        "paths",
        &folder.folder_id,
        UpdateWorkloadFolderRequest {
            name: Some("renamed".into()),
            parent_id: None,
        },
    )
    .await
    .unwrap();
    for engine in &engines {
        assert_eq!(
            engine.authorize(&request).await.unwrap_err().code,
            "policy_denied"
        );
    }
    assert_eq!(
        engines[0]
            .check_tree_revision(&before)
            .await
            .unwrap_err()
            .code,
        "policy_tree_changed"
    );
    // Caller-provided facts are replaced, even when plausible in the same workspace.
    request.subject_tree = before.subject_tree;
    assert_eq!(
        engines[1]
            .enrich(&request)
            .await
            .unwrap()
            .subject_tree
            .unwrap()
            .path,
        "/renamed/sender"
    );
    let other = crate::policy_tree::TreeSnapshot::load(&auth.pool(), "other")
        .await
        .unwrap();
    assert!(other.enrich(&request).resource_tree.is_none());
    let human = PolicyRequest::new(
        "paths",
        PolicySubject::Human {
            user_id: "owner".into(),
        },
        "agent.prompt",
        PolicyResource::new("agent", "target"),
    );
    assert!(engines[0]
        .enrich(&human)
        .await
        .unwrap()
        .subject_tree
        .is_none());
    let snapshot = crate::policy_tree::TreeSnapshot::load(&auth.pool(), "paths")
        .await
        .unwrap();
    sqlx::query("UPDATE workload_folders SET name=name WHERE workspace_id='paths'")
        .execute(&auth.pool())
        .await
        .unwrap();
    assert_eq!(
        crate::policy_tree::TreeSnapshot::load(&auth.pool(), "paths")
            .await
            .unwrap()
            .revision,
        snapshot.revision
    );
}

#[tokio::test]
async fn monitor_logs_are_bounded_redacted_and_include_batch_would_denies() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("monitor").await;
    let store = WorkspacePolicyStore::new(auth.pool());
    let doc =
        serde_json::from_value(json!({"schema_version":2,"defaults":{"agent.prompt":"deny"}}))
            .unwrap();
    store
        .replace(
            "monitor",
            0,
            PolicyMode::Monitor,
            doc,
            PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        )
        .await
        .unwrap();
    let engine = PolicyEngine::durable(store);
    let req = PolicyRequest::new(
        "monitor",
        PolicySubject::Human {
            user_id: "caller".into(),
        },
        "agent.prompt",
        PolicyResource::new("agent", "target").with_attribute("token", "secret-test-value"),
    );
    engine.authorize(&req).await.unwrap();
    engine
        .authorize_batch(&[req.clone(), req.clone()])
        .await
        .unwrap();
    let records = engine.decision_records("monitor").await;
    assert_eq!(records.len(), 3);
    assert!(!serde_json::to_string(&records)
        .unwrap()
        .contains("secret-test-value"));
    assert_eq!(records[0].explanation.computed_effect, PolicyEffect::Deny);
    assert_eq!(
        records[0].explanation.effective_decision,
        PolicyEffect::Allow
    );
    assert!(engine.decision_records("other").await.is_empty());
    let explanation = engine.explain_request(&req).await.unwrap();
    for _ in 0..2100 {
        engine.record(&req, &explanation).await;
    }
    assert_eq!(engine.records.lock().await.len(), 2048);
    assert_eq!(engine.decision_records("monitor").await.len(), 100);
}

#[test]
fn relations_and_bindings_do_not_treat_missing_context_as_root() {
    let tree = treer_protocol::WorkloadTree {
        folders: vec![treer_protocol::WorkloadFolder {
            folder_id: "f".into(),
            name: "team".into(),
            parent_id: None,
        }],
        placements: vec![],
    };
    // Obtain realistic context through the public wire model; ancestors are trusted Core facts.
    let ctx:treer_protocol::PolicyTreeContext=serde_json::from_value(json!({"node_id":"a","kind":"agent","name":"a","path":"/team/a","folder_id":"f","folder_path":"/team","ancestor_ids":["f"],"ancestor_paths":["/","/team"],"placed":true,"tree_revision":1})).unwrap();
    assert_eq!(tree.folder_path(Some("f")).unwrap(), ctx.folder_path);
    let mut req = PolicyRequest::new(
        "w",
        PolicySubject::Agent {
            agent_id: "a".into(),
            server_id: "s".into(),
        },
        "agent.prompt",
        PolicyResource::new("agent", "b"),
    );
    req.subject_tree = Some(ctx.clone());
    req.resource_tree = Some(ctx);
    let mut rule = document().rules.remove(0);
    assert!(path_match::conditions_match(&rule, &req));
    rule.conditions = serde_json::from_value(json!([{"op":"same_folder"}])).unwrap();
    assert!(path_match::conditions_match(&rule, &req));
    req.subject_tree = None;
    assert!(!path_match::conditions_match(&rule, &req));
    rule.conditions =
        serde_json::from_value(json!([{"op":"resource_below_folder","folder":{"path":"/tea"}}]))
            .unwrap();
    assert!(!path_match::conditions_match(&rule, &req));
    rule.conditions = serde_json::from_value(
        json!([{"op":"resource_below_folder","folder":{"id":"f"},"strict":true}]),
    )
    .unwrap();
    assert!(!path_match::conditions_match(&rule, &req));
}
