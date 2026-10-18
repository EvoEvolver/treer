use super::*;
use serde_json::json;
use treer_protocol::{
    CreateWorkloadFolderRequest, UpdateWorkloadFolderRequest, WorkspacePolicyDocument,
};

#[tokio::test]
async fn invalid_provider_contract_never_falls_back_to_stale_or_fail_open() {
    let auth = AuthStore::for_test("test").await;
    auth.seed_test_workspace("contract").await;
    let app = auth
        .create_app_deployment(
            "contract",
            "owner",
            "machine".into(),
            treer_protocol::CreateAppDeploymentRequest {
                folder_id: None,
                server_id: Some("machine".into()),
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
    let state = AppState::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let connection = uuid::Uuid::new_v4();
    let build = treer_protocol::BuildInfo {
        version: "test".into(),
        git_commit: "test".into(),
    };
    state
        .register_server_instance_with_capabilities(
            treer_protocol::ServerInfo {
                server_id: "machine".into(),
                workspace_id: "contract".into(),
                name: "machine".into(),
                hostname: "machine".into(),
                root: "/tmp".into(),
                controller_build: build.clone(),
                host_build: build,
                supervision: None,
                labels: Default::default(),
                available_agents: None,
                status: treer_protocol::ServerStatus::Online,
                connected_at: chrono::Utc::now(),
                last_seen_at: chrono::Utc::now(),
                capabilities: vec![],
            },
            connection,
            "synthetic".into(),
            vec![
                treer_protocol::CAPABILITY_SERVICE_HTTP_FETCH.into(),
                treer_protocol::CAPABILITY_POLICY_PATH_RULES.into(),
            ],
            tx,
        )
        .await
        .unwrap();
    for mode in [
        PolicyProviderFailureMode::FailOpen,
        PolicyProviderFailureMode::FailClosed,
    ] {
        for has_stale in [false, true] {
            for malformed in ["unknown_condition", "invalid_path", "unknown_schema"] {
                let store = PolicyProviderStore::new(auth.pool());
                store
                    .set(
                        "contract",
                        &app.app_id,
                        &app.service_id,
                        mode,
                        300,
                        0,
                        "owner",
                    )
                    .await
                    .unwrap();
                let evaluator = Arc::new(ProviderPolicyEvaluator::new(
                    store,
                    auth.clone(),
                    state.clone(),
                ));
                if has_stale {
                    evaluator.cache.write().await.insert(
                        "contract".into(),
                        CachedProviderPolicy {
                            app_id: app.app_id.clone(),
                            checked_at: Instant::now() - POLICY_CACHE_TTL,
                            loaded_at: Some(Instant::now()),
                            policy: Some(Arc::new(CompiledWorkspacePolicy::allow(1))),
                            last_error: None,
                        },
                    );
                }
                let task = tokio::spawn({
                    let evaluator = evaluator.clone();
                    async move { evaluator.compiled("contract").await }
                });
                for step in 0..2 {
                    let frame = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                        .await
                        .unwrap()
                        .unwrap();
                    let crate::state::SocketFrame::Text(wire) = frame else {
                        panic!("expected command")
                    };
                    let treer_protocol::ProxyMessage::Command { envelope } =
                        serde_json::from_str(&wire).unwrap()
                    else {
                        panic!("expected command")
                    };
                    let mut doc = serde_json::to_value(document()).unwrap();
                    match malformed {
                        "unknown_condition" => {
                            doc["rules"][0]["conditions"][0]["op"] = json!("future-condition")
                        }
                        "invalid_path" => {
                            doc["rules"][0]["subjects"][0]["tree"]["path_glob"] = json!("//team/*")
                        }
                        _ => doc["schema_version"] = json!(99),
                    }
                    let value = if step == 0 {
                        json!({"protocol":POLICY_PROVIDER_PROTOCOL_V1,"provider_name":"Synthetic","capabilities":["policy.bundle.v2"]})
                    } else {
                        json!({"protocol":POLICY_PROVIDER_PROTOCOL_V1,"workspace_id":"contract","revision":2,"mode":"enforce","generated_at":chrono::Utc::now(),"document":doc})
                    };
                    state
                        .complete_command_from(
                            "contract",
                            "machine",
                            connection,
                            treer_protocol::CommandResult::success(envelope.command_id, value),
                        )
                        .await
                        .unwrap();
                }
                assert_eq!(
                    task.await.unwrap().unwrap_err().code,
                    "invalid_policy_contract",
                    "{mode:?}/{has_stale}/{malformed}"
                );
                assert!(evaluator.cache.read().await.get("contract").is_none());
            }
        }
    }
}

fn document() -> WorkspacePolicyDocument {
    serde_json::from_value(json!({"schema_version":2,"defaults":{"agent.prompt":"deny"},"rules":[
        {"id":"team","priority":100,"effect":"allow","subjects":[{"kind":"agent","tree":{"path_glob":"/team/*"}}],"actions":["agent.prompt"],"resources":[{"kind":"agent","tree":{"path_prefix":{"path":"/team","include_self":false}}}],"bindings":{"home":{"ref":"subject.tree.folder"}},"conditions":[{"op":"resource_below_folder","folder":{"var":"home"}}]}
    ]})).unwrap()
}

#[test]
fn action_index_shares_large_rules_and_deduplicates_repeated_actions() {
    for repeated in [false, true] {
        let mut doc = document();
        let rule = &mut doc.rules[0];
        rule.actions = (0..5000)
            .map(|i| {
                if repeated {
                    "agent.prompt".into()
                } else {
                    format!("action-{i}")
                }
            })
            .collect();
        rule.conditions = serde_json::from_value(json!(vec![
            json!({"op":"resource_below_folder","folder":{"path":format!("/{}","x".repeat(4000))}});
            32
        ]))
        .unwrap();
        treer_proxy::policy_store::validate_document(&doc).unwrap();
        let compiled = CompiledWorkspacePolicy::compile(WorkspacePolicy {
            workspace_id: "w".into(),
            revision: 1,
            mode: PolicyMode::Monitor,
            document: doc,
            updated_at: chrono::Utc::now(),
            updated_by: PolicyPrincipalRef {
                kind: PolicyPrincipalKind::Human,
                id: "owner".into(),
            },
        });
        let first = &compiled.rules_by_action.values().next().unwrap()[0];
        assert_eq!(
            compiled.rules_by_action.len(),
            if repeated { 1 } else { 5000 }
        );
        for rules in compiled.rules_by_action.values() {
            assert_eq!(rules.len(), 1);
            assert!(Arc::ptr_eq(first, &rules[0]));
        }
    }
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
