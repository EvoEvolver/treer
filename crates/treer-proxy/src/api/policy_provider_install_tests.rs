//! Provision only synthetic local capabilities, using an isolated DB and fake
//! Controller. No live machine, installed binary or real credential is used.
#[cfg(unix)]
mod unix {
    use super::super::*;
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use tracing::instrument::WithSubscriber;
    use treer_protocol::{CommandResult, ProxyMessage};

    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[derive(Clone)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);
    impl Write for LogBuffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn default_policy_install_keeps_local_token_out_of_proxy_boundaries() {
        let root = std::env::temp_dir().join(format!("treer-policy-install-{}", Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let fixture = Fixture(std::fs::canonicalize(root).unwrap());
        let auth = AuthStore::for_test("synthetic-password").await;
        auth.seed_test_workspace("default").await;
        sqlx::query("INSERT INTO users(id,email,email_verified,preferred_name,password_hash,created_at) VALUES('owner','owner@example.test',TRUE,'Owner','unused','test')").execute(&auth.pool()).await.unwrap();
        sqlx::query("INSERT INTO organization_members(organization_id,user_id,role,joined_at) VALUES('org_default','owner','owner','test')").execute(&auth.pool()).await.unwrap();
        let state = super::super::super::tests::state_with_managed_agent().await;
        let server = state.live_server("machine-a").await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let connection = Uuid::new_v4();
        state
            .register_server_instance_with_capabilities(
                server,
                connection,
                "isolated-install".into(),
                treer_protocol::CONTROLLER_CAPABILITIES
                    .iter()
                    .map(|s| s.to_string()),
                tx,
            )
            .await
            .unwrap();
        let controller_state = state.clone();
        let controller_auth = auth.clone();
        let local_root = fixture.0.clone();
        let observed = Arc::new(Mutex::new(Vec::<String>::new()));
        let traffic = observed.clone();
        let controller = tokio::spawn(async move {
            let mut uploads = BTreeMap::<String, (String, Vec<u8>)>::new();
            while let Some(frame) = rx.recv().await {
                let SocketFrame::Text(encoded) = frame else {
                    continue;
                };
                let Ok(ProxyMessage::Command { envelope }) = serde_json::from_str(&encoded) else {
                    continue;
                };
                traffic.lock().unwrap().push(encoded);
                let value = match envelope.command {
                    AgentCommand::CreationPlacementCheck {} => json!({"supported":true}),
                    AgentCommand::UploadBegin {
                        upload_id,
                        directory,
                        file_name,
                        ..
                    } => {
                        assert!(directory.is_empty());
                        assert!(!file_name.contains("admin-token"));
                        uploads.insert(upload_id, (file_name, vec![]));
                        json!({})
                    }
                    AgentCommand::UploadChunk {
                        upload_id,
                        content_base64,
                        ..
                    } => {
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(content_base64)
                            .unwrap();
                        uploads.get_mut(&upload_id).unwrap().1.extend(bytes);
                        json!({})
                    }
                    AgentCommand::UploadCommit { upload_id } => {
                        let (name, bytes) = uploads.remove(&upload_id).unwrap();
                        traffic
                            .lock()
                            .unwrap()
                            .push(String::from_utf8(bytes.clone()).unwrap());
                        std::fs::write(local_root.join(&name), &bytes).unwrap();
                        json!({"server_id":"machine-a","path":name,"bytes_written":bytes.len()})
                    }
                    AgentCommand::EnsureLifecycle {
                        agent_id, request, ..
                    } => {
                        let initialize = request
                            .args
                            .iter()
                            .any(|arg| arg == "--initialize-admin-token-file");
                        // Execute the uploaded App's provisioner on the fake
                        // target, not in the Proxy. App main/argv is separately
                        // exercised by the App's closed-environment tests.
                        let output = std::process::Command::new("python3")
                            .args(["-c", "import runpy,sys; app=runpy.run_path(sys.argv[1]); app['admin_token_source'](initialize=sys.argv[2]=='yes')"])
                            .arg(local_root.join(DEFAULT_POLICY_APP_SCRIPT))
                            .arg(if initialize { "yes" } else { "no" })
                            .env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default())
                            .current_dir(&local_root).output().unwrap();
                        assert!(output.status.success());
                        assert!(output.stdout.is_empty() && output.stderr.is_empty());
                        let app = controller_auth
                            .resolve_app_deployment_by_runtime("default", &agent_id)
                            .await
                            .unwrap()
                            .unwrap();
                        serde_json::to_value(app_workload_agent(&app)).unwrap()
                    }
                    AgentCommand::Restart { agent_id } => {
                        let app = controller_auth
                            .resolve_app_deployment_by_runtime("default", &agent_id)
                            .await
                            .unwrap()
                            .unwrap();
                        serde_json::to_value(app_workload_agent(&app)).unwrap()
                    }
                    AgentCommand::FetchServiceHttp { path, .. } if path == "/v1/manifest" => {
                        json!({"protocol":POLICY_PROVIDER_PROTOCOL_V1,"capabilities":["policy.bundle.v1"],"provider_name":"Treer Policy"})
                    }
                    AgentCommand::FetchServiceHttp { .. } => {
                        json!({"protocol":POLICY_PROVIDER_PROTOCOL_V1,"workspace_id":"default","revision":1,"mode":"monitor","document":{"schema_version":1,"defaults":{},"groups":{},"rules":[]},"generated_at":Utc::now()})
                    }
                    other => panic!("unexpected isolated Controller command: {other:?}"),
                };
                traffic.lock().unwrap().push(value.to_string());
                controller_state
                    .complete_command_from(
                        "default",
                        "machine-a",
                        connection,
                        CommandResult::success(envelope.command_id, value),
                    )
                    .await
                    .unwrap();
            }
        });
        let logs = LogBuffer(Arc::new(Mutex::new(vec![])));
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let subscriber = tracing::Dispatch::new(subscriber);
        let Json(response) = install_default_policy_app(
            State(state.clone()),
            Extension(auth.clone()),
            Extension(super::super::super::tests::test_ingress_config()),
            Extension(PolicyEngine::allow_all()),
            Extension(PolicyProviderStore::new(auth.pool())),
            Extension(CurrentSession {
                token: "synthetic-session".into(),
                user_id: "owner".into(),
                email: "owner@example.test".into(),
                preferred_name: "Owner".into(),
            }),
            Path("default".into()),
            Json(InstallDefaultPolicyAppRequest {
                server_id: "machine-a".into(),
            }),
        )
        .with_subscriber(subscriber.clone())
        .await
        .unwrap();
        // Bundled installs must include the exact client advertised by the App,
        // and its installed asset lookup must resolve without a source checkout.
        let client = fixture.0.join("treer-policy-client.py");
        assert_eq!(
            std::fs::read(&client).unwrap(),
            include_bytes!("../../../../apps/policy/treer-policy")
        );
        let asset_check = std::process::Command::new("python3")
            .args(["-c", "import runpy,sys; app=runpy.run_path(sys.argv[1]); assert app['ASSETS']['/treer-policy'][1].is_file()"])
            .arg(fixture.0.join(DEFAULT_POLICY_APP_SCRIPT))
            .env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(&fixture.0).output().unwrap();
        assert!(asset_check.status.success());
        assert!(asset_check.stdout.is_empty() && asset_check.stderr.is_empty());
        let client_help = std::process::Command::new("python3")
            .arg(client)
            .arg("--help")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .current_dir(&fixture.0)
            .output()
            .unwrap();
        assert!(client_help.status.success() && client_help.stderr.is_empty());
        assert!(String::from_utf8(client_help.stdout)
            .unwrap()
            .contains("submit"));
        let token = std::fs::read_to_string(fixture.0.join(".treer-policy-admin-token")).unwrap();
        let token = token.trim();
        assert!(token.len() >= 43);
        let mut reinstall_responses = vec![];
        // Reinstall preserves capabilities and identity. Legacy startup args
        // are deliberately not rewritten by uploading newer assets.
        for legacy in [false, true] {
            let mut expected_args = default_policy_app_args(8787);
            if legacy {
                expected_args.retain(|arg| arg != "--initialize-admin-token-file");
                sqlx::query("UPDATE app_deployments SET args=$1 WHERE app_id=$2")
                    .bind(json!(expected_args))
                    .bind(response["app"]["app_id"].as_str().unwrap())
                    .execute(&auth.pool())
                    .await
                    .unwrap();
            }
            PolicyProviderStore::new(auth.pool())
                .clear("default")
                .await
                .unwrap();
            let Json(reinstalled) = install_default_policy_app(
                State(state.clone()),
                Extension(auth.clone()),
                Extension(super::super::super::tests::test_ingress_config()),
                Extension(PolicyEngine::allow_all()),
                Extension(PolicyProviderStore::new(auth.pool())),
                Extension(CurrentSession {
                    token: "synthetic-session".into(),
                    user_id: "owner".into(),
                    email: "owner@example.test".into(),
                    preferred_name: "Owner".into(),
                }),
                Path("default".into()),
                Json(InstallDefaultPolicyAppRequest {
                    server_id: "machine-a".into(),
                }),
            )
            .with_subscriber(subscriber.clone())
            .await
            .unwrap();
            for field in ["app_id", "service_id", "runtime_agent_id", "public_url"] {
                assert_eq!(reinstalled["app"][field], response["app"][field]);
            }
            assert_eq!(reinstalled["app"]["args"], json!(expected_args));
            assert_eq!(
                std::fs::read_to_string(fixture.0.join(".treer-policy-admin-token"))
                    .unwrap()
                    .trim(),
                token
            );
            reinstall_responses.push(reinstalled.to_string());
        }
        controller.abort();
        let _ = controller.await;
        let encoded_token = base64::engine::general_purpose::STANDARD.encode(token);
        let audit: Vec<String> = sqlx::query_scalar("SELECT row_to_json(e)::text FROM organization_audit_events e WHERE workspace_id='default'").fetch_all(&auth.pool()).await.unwrap();
        assert!(audit
            .iter()
            .any(|entry| entry.contains("machine.file.uploaded")));
        assert!(audit
            .iter()
            .any(|entry| entry.contains("policy_provider.configured")));
        let mut boundaries = observed.lock().unwrap().clone();
        boundaries.extend(audit);
        boundaries.extend(reinstall_responses);
        boundaries.push(response.to_string());
        boundaries.push(String::from_utf8(logs.0.lock().unwrap().clone()).unwrap());
        for boundary in boundaries {
            assert!(!boundary.contains(token));
            assert!(!boundary.contains(&encoded_token));
        }
        assert_eq!(
            response["app"]["args"],
            json!(default_policy_app_args(8787))
        );
        assert!(response.get("admin_token").is_none());
    }
}
