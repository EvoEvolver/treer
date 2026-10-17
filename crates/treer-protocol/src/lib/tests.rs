#[test]
fn primary_service_models_accept_legacy_records_and_round_trip() {
    let legacy = serde_json::json!({
        "name": "api", "server_id": "machine-a", "target_port": 4180,
        "protocol": "http"
    });
    let mut request: super::CreateMachineServiceRequest = serde_json::from_value(legacy).unwrap();
    assert!(!request.primary);
    request.primary = true;
    request.target_agent_id = Some("agent-a".into());
    assert_eq!(
        serde_json::from_value::<super::CreateMachineServiceRequest>(
            serde_json::to_value(&request).unwrap()
        )
        .unwrap(),
        request
    );
    let record = serde_json::json!({
        "service_id": "svc_a", "workspace_id": "workspace-a", "name": "api",
        "server_id": "machine-a", "target_agent_id": "agent-a",
        "target_host": "127.0.0.1", "target_port": 4180, "protocol": "http",
        "created_at": "2026-10-11T00:00:00Z", "created_by": "agent-a",
        "updated_at": "2026-10-11T00:00:00Z", "updated_by": "agent-a"
    });
    let mut service: super::MachineService = serde_json::from_value(record).unwrap();
    assert!(!service.primary);
    service.primary = true;
    assert_eq!(
        serde_json::from_value::<super::MachineService>(serde_json::to_value(&service).unwrap())
            .unwrap(),
        service
    );
    let update: super::UpdateMachineServiceRequest =
        serde_json::from_value(serde_json::json!({"primary": false})).unwrap();
    assert_eq!(update.primary, Some(false));
}

#[test]
fn lifecycle_declaration_defaults_to_starting_missing_for_older_senders() {
    let command: super::AgentCommand = serde_json::from_value(serde_json::json!({
            "action": "ensure_lifecycle", "agent_id": "app", "workload_credential": null,
            "enabled": true, "request": {"kind":"app", "name":"app", "cwd":".", "args":["/bin/true"], "cols":80,"rows":24}
        })).unwrap();
    assert!(matches!(
        command,
        super::AgentCommand::EnsureLifecycle {
            start_missing: true,
            ..
        }
    ));
    assert_eq!(
        command.required_capability(),
        Some(super::CAPABILITY_AGENT_LIFECYCLE)
    );
}
use super::*;

#[test]
fn app_update_commands_round_trip_and_require_capability() {
    let request = ReplaceAgentStartupRequest {
        cwd: Some("releases/v2".into()),
        command: "node".into(),
        args: vec!["entry.mjs".into()],
    };
    for command in [
        serde_json::from_value(serde_json::json!({
            "action":"app_ensure_lifecycle", "agent_id":"app_stable", "workload_credential":null,
            "enabled":true, "start_missing":false, "revision":7,
            "request":{"kind":"app", "name":"stable", "cwd":".", "args":["node","entry.mjs"]}
        }))
        .unwrap(),
        AgentCommand::AppStartupCheck {
            agent_id: "app_stable".into(),
            request: request.clone(),
        },
        AgentCommand::AppStartupUpdate {
            agent_id: "app_stable".into(),
            request,
            revision: 7,
        },
    ] {
        assert_eq!(command.required_capability(), Some(CAPABILITY_APP_UPDATE));
        assert_eq!(
            serde_json::from_value::<AgentCommand>(serde_json::to_value(&command).unwrap())
                .unwrap(),
            command
        );
    }
    assert!(serde_json::from_value::<UpdateAppDeploymentRequest>(
        serde_json::json!({"env":{"SECRET":"not-accepted"}})
    )
    .is_err());
    let legacy: ValidateAgentStartupResponse =
        serde_json::from_value(serde_json::json!({"active_agent_ids":[],"stopped_agent_ids":[]}))
            .unwrap();
    assert!(legacy.app_startups.is_empty());
}

#[test]
fn admin_startup_commands_round_trip_and_require_a_new_capability() {
    let request = ReplaceAgentStartupRequest {
        cwd: None,
        command: "/bin/sh".into(),
        args: vec!["-c".into(), "literal $HOME ; quotes '".into()],
    };
    for command in [
        AgentCommand::StartupAdminGet {
            agent_id: "target".into(),
        },
        AgentCommand::StartupAdminSet {
            agent_id: "target".into(),
            request: request.clone(),
        },
        AgentCommand::RestartWithStartup {
            agent_id: "target".into(),
            request,
        },
    ] {
        assert_eq!(
            command.required_capability(),
            Some(CAPABILITY_AGENT_STARTUP_ADMIN)
        );
        let wire = serde_json::to_value(&command).unwrap();
        assert!(!wire.to_string().contains("credential"));
        assert_eq!(
            serde_json::from_value::<AgentCommand>(wire).unwrap(),
            command
        );
    }
    assert!(serde_json::from_value::<ReplaceAgentStartupRequest>(
        serde_json::json!({"command":"sh","workload_credential":"secret"})
    )
    .is_err());
    assert_eq!(
        AgentCommand::StartupGet {
            agent_id: "self".into()
        }
        .required_capability(),
        Some(CAPABILITY_AGENT_STARTUP)
    );
}

#[test]
fn startup_replacement_rejects_secret_literals_and_malformed_argv() {
    let valid = ReplaceAgentStartupRequest {
        cwd: None,
        command: "sh".into(),
        args: vec!["-lc".into(), "exec agent --key-file .private/key".into()],
    };
    assert!(valid.validate().is_ok());
    for prefix in ["wlc_", "opc_"] {
        let secret = format!("{prefix}{}", "a".repeat(64));
        assert!(ReplaceAgentStartupRequest {
            args: vec![format!("--key={secret}")],
            ..valid.clone()
        }
        .validate()
        .is_err());
        assert!(ReplaceAgentStartupRequest {
            cwd: Some(secret),
            ..valid.clone()
        }
        .validate()
        .is_err());
    }
    assert!(ReplaceAgentStartupRequest {
        command: " ".into(),
        ..valid.clone()
    }
    .validate()
    .is_err());
    assert!(ReplaceAgentStartupRequest {
        args: vec!["a\0b".into()],
        ..valid.clone()
    }
    .validate()
    .is_err());
    assert!(ReplaceAgentStartupRequest {
        cwd: Some("a\0b".into()),
        ..valid
    }
    .validate()
    .is_err());
}

#[test]
fn managed_app_public_access_is_explicit_and_backward_compatible() {
    let legacy: CreateAppDeploymentRequest = serde_json::from_value(serde_json::json!({
        "name": "docs",
        "command": "python3",
        "args": ["-m", "http.server", "8080"],
        "cwd": ".",
        "port": 8080,
        "hostname": "docs.internal"
    }))
    .expect("deserialize legacy App request");
    assert!(!legacy.public);
    assert!(legacy.folder_id.is_none());
    assert_eq!(
        serde_json::to_value(&legacy)
            .expect("serialize private App request")
            .get("public"),
        None
    );

    let public: CreateAppDeploymentRequest = serde_json::from_value(serde_json::json!({
        "name": "docs",
        "command": "python3",
        "port": 8080,
        "hostname": "docs.internal",
        "public": true
    }))
    .expect("deserialize public App request");
    assert!(public.public);
}

#[test]
fn creation_placement_round_trips_inherited_folder_and_explicit_root() {
    for folder in [None, Some("/"), Some("folder_team")] {
        let value =
            serde_json::json!({"name":"mail","command":"/bin/true","port":8888,"folder_id":folder});
        let request: CreateAppDeploymentRequest = serde_json::from_value(value).unwrap();
        assert_eq!(request.folder_id.as_deref(), folder);
        assert_eq!(
            serde_json::from_value::<CreateAppDeploymentRequest>(
                serde_json::to_value(&request).unwrap()
            )
            .unwrap(),
            request
        );
    }
    let check = AgentCommand::CreationPlacementCheck {};
    assert_eq!(
        check.required_capability(),
        Some(CAPABILITY_WORKLOAD_CREATE_PLACEMENT)
    );
    assert_eq!(
        serde_json::from_value::<AgentCommand>(serde_json::to_value(&check).unwrap()).unwrap(),
        check
    );
}

#[test]
fn launch_profile_request_round_trips_optional_cwd_override() {
    let request = LaunchAgentProfileRequest {
        folder_id: None,
        server_id: Some("srv_1".to_string()),
        agent_name: Some("reviewer".to_string()),
        cwd: Some("packages/api".to_string()),
        cols: 100,
        rows: 30,
    };
    let encoded = serde_json::to_value(&request).expect("serialize launch request");
    assert_eq!(encoded["cwd"], "packages/api");
    assert_eq!(
        serde_json::from_value::<LaunchAgentProfileRequest>(encoded)
            .expect("deserialize launch request"),
        request
    );

    let legacy = serde_json::from_value::<LaunchAgentProfileRequest>(serde_json::json!({
        "server_id": "srv_1",
        "cols": 120,
        "rows": 36
    }))
    .expect("deserialize request without cwd");
    assert_eq!(legacy.cwd, None);
}

#[test]
fn machine_supervision_is_optional_for_older_controllers() {
    let now = Utc::now();
    let server = ServerInfo {
        server_id: "srv_1".to_string(),
        workspace_id: "default".to_string(),
        name: "builder".to_string(),
        hostname: "builder".to_string(),
        root: "/workspace".to_string(),
        controller_build: BuildInfo {
            version: "0.1.0".to_string(),
            git_commit: "abc123".to_string(),
        },
        host_build: BuildInfo {
            version: "0.1.0".to_string(),
            git_commit: "abc123".to_string(),
        },
        supervision: Some(MachineSupervision {
            mode: MachineSupervisionMode::Foreground,
            fallback_reason: Some("user bus unavailable".to_string()),
        }),
        labels: BTreeMap::new(),
        available_agents: None,
        status: ServerStatus::Online,
        connected_at: now,
        last_seen_at: now,
    };

    let encoded = serde_json::to_value(&server).expect("serialize server info");
    assert_eq!(encoded["supervision"]["mode"], "foreground");
    assert_eq!(
        encoded["supervision"]["fallback_reason"],
        "user bus unavailable"
    );

    let mut legacy = encoded;
    legacy
        .as_object_mut()
        .expect("server info object")
        .remove("supervision");
    let decoded: ServerInfo =
        serde_json::from_value(legacy).expect("deserialize older server info");
    assert_eq!(decoded.supervision, None);
}

#[test]
fn nohup_supervision_round_trips() {
    let supervision = MachineSupervision {
        mode: MachineSupervisionMode::Nohup,
        fallback_reason: None,
    };
    let encoded = serde_json::to_value(&supervision).expect("serialize nohup supervision");
    assert_eq!(encoded["mode"], "nohup");
    assert_eq!(
        serde_json::from_value::<MachineSupervision>(encoded)
            .expect("deserialize nohup supervision"),
        supervision
    );
}

#[test]
fn command_wire_shape_is_stable() {
    let message = ProxyMessage::Command {
        envelope: CommandEnvelope {
            command_id: "cmd_1".to_string(),
            workspace_id: "default".to_string(),
            command: AgentCommand::Stop {
                agent_id: "ag_1".to_string(),
            },
        },
    };

    let json = serde_json::to_value(message).expect("serialize command");
    assert_eq!(json["type"], "command");
    assert_eq!(json["envelope"]["command"]["action"], "stop");
}

#[test]
fn commands_added_after_protocol_v4_require_advertised_capabilities() {
    assert_eq!(
        AgentCommand::Abort {
            agent_id: "ag_1".to_string()
        }
        .required_capability(),
        Some(CAPABILITY_AGENT_ABORT)
    );
    assert_eq!(
        AgentCommand::StartupGet {
            agent_id: "ag_1".to_string()
        }
        .required_capability(),
        Some(CAPABILITY_AGENT_STARTUP)
    );
    assert_eq!(
        AgentCommand::FetchServiceHttp {
            policy_schema_version: None,
            port: 8080,
            path: "/v1/manifest".to_string(),
            max_bytes: 4096,
            timeout_ms: 3_000,
        }
        .required_capability(),
        Some(CAPABILITY_SERVICE_HTTP_FETCH)
    );
    assert_eq!(
        AgentCommand::Stop {
            agent_id: "ag_1".to_string()
        }
        .required_capability(),
        None
    );
}

#[test]
fn policy_provider_v1_wire_shape_round_trips() {
    let bundle = PolicyProviderBundle {
        protocol: POLICY_PROVIDER_PROTOCOL_V1.to_string(),
        workspace_id: "ws_1".to_string(),
        revision: 12,
        mode: PolicyMode::Enforce,
        document: WorkspacePolicyDocument {
            schema_version: POLICY_SCHEMA_VERSION,
            defaults: BTreeMap::from([("agent.prompt".to_string(), PolicyEffect::Deny)]),
            groups: BTreeMap::new(),
            rules: Vec::new(),
        },
        generated_at: Utc::now(),
    };
    let value = serde_json::to_value(&bundle).expect("serialize provider bundle");
    assert_eq!(value["protocol"], POLICY_PROVIDER_PROTOCOL_V1);
    assert_eq!(value["revision"], 12);
    assert_eq!(
        serde_json::from_value::<PolicyProviderBundle>(value).expect("deserialize provider bundle"),
        bundle
    );
    let install = InstallDefaultPolicyAppRequest {
        server_id: "server_1".to_string(),
    };
    assert_eq!(
        serde_json::to_value(&install).expect("serialize default Policy App install"),
        serde_json::json!({"server_id": "server_1"})
    );
    assert_eq!(
        serde_json::from_value::<InstallDefaultPolicyAppRequest>(serde_json::json!({
            "server_id": "server_1"
        }))
        .expect("deserialize default Policy App install"),
        install
    );
}

#[test]
fn legacy_registration_response_defaults_to_no_capabilities() {
    let message: ProxyMessage = serde_json::from_value(serde_json::json!({
        "type": "registered",
        "protocol": PROTOCOL_VERSION,
        "workspace_revision": 7
    }))
    .expect("decode legacy registration response");
    assert!(matches!(
        message,
        ProxyMessage::Registered {
            protocol: PROTOCOL_VERSION,
            capabilities,
            workspace_revision: 7
        } if capabilities.is_empty()
    ));
}

#[test]
fn abort_command_wire_shape_is_stable() {
    let message = ProxyMessage::Command {
        envelope: CommandEnvelope {
            command_id: "cmd_abort".to_string(),
            workspace_id: "default".to_string(),
            command: AgentCommand::Abort {
                agent_id: "ag_1".to_string(),
            },
        },
    };
    let json = serde_json::to_value(message).expect("serialize abort command");
    assert_eq!(json["envelope"]["command"]["action"], "abort");
    assert_eq!(json["envelope"]["command"]["agent_id"], "ag_1");
}

#[test]
fn virtual_host_snapshot_wire_shape_carries_revision() {
    let message = ProxyMessage::VirtualNetworkHosts {
        snapshot: VirtualNetworkHostsSnapshot {
            workspace_id: "default".to_string(),
            revision: 42,
            hosts: Vec::new(),
        },
    };

    assert_eq!(
        serde_json::to_value(message).expect("serialize virtual hosts"),
        serde_json::json!({
            "type": "virtual_network_hosts",
            "snapshot": {
                "workspace_id": "default",
                "revision": 42,
                "hosts": []
            }
        })
    );
}

#[test]
fn machine_shutdown_wire_shape_is_stable() {
    let message = ProxyMessage::Command {
        envelope: CommandEnvelope {
            command_id: "cmd_shutdown".to_string(),
            workspace_id: "default".to_string(),
            command: AgentCommand::ShutdownMachine,
        },
    };

    let json = serde_json::to_value(message).expect("serialize command");
    assert_eq!(json["envelope"]["command"]["action"], "shutdown_machine");
}

#[test]
fn network_probe_wire_shape_is_stable() {
    let message = ProxyMessage::Command {
        envelope: CommandEnvelope {
            command_id: "cmd_probe".to_string(),
            workspace_id: "default".to_string(),
            command: AgentCommand::ProbeNetwork {
                host: "127.0.0.1".to_string(),
                port: 8080,
                timeout_ms: 3_000,
                target_agent_id: None,
            },
        },
    };
    let json = serde_json::to_value(message).expect("serialize probe command");
    assert_eq!(json["envelope"]["command"]["action"], "probe_network");
    assert_eq!(json["envelope"]["command"]["port"], 8080);
}

#[test]
fn machine_service_requests_use_service_ids_for_aliases() {
    let request = CreateVirtualNetworkHostRequest {
        hostname: "api.internal".to_string(),
        service_id: "svc_api".to_string(),
    };
    assert_eq!(
        serde_json::to_value(request).expect("serialize virtual host request"),
        serde_json::json!({
            "hostname": "api.internal",
            "service_id": "svc_api"
        })
    );
}

#[test]
fn workload_identity_requests_have_stable_wire_shapes() {
    let token = WorkloadIdentityTokenRequest {
        audience: "api".to_string(),
    };
    assert_eq!(
        serde_json::to_value(token).expect("serialize token request"),
        serde_json::json!({ "audience": "api" })
    );
    let inactive = WorkloadIdentityVerifyResponse {
        active: false,
        claims: None,
    };
    assert_eq!(
        serde_json::to_value(inactive).expect("serialize verification"),
        serde_json::json!({ "active": false })
    );
}

#[test]
fn domain_event_envelope_has_a_stable_wire_shape() {
    let event = DomainEventEnvelope {
        event_id: "evt_123".to_string(),
        schema_version: DOMAIN_EVENT_SCHEMA_VERSION,
        organization_id: Some("org_1".to_string()),
        workspace_id: "ws_1".to_string(),
        actor: DomainEventActor {
            kind: "agent".to_string(),
            id: Some("agent_1".to_string()),
        },
        action: "service.updated".to_string(),
        resource: DomainEventResource {
            kind: "service".to_string(),
            id: "svc_1".to_string(),
        },
        occurred_at: "2026-08-18T21:00:00Z".parse().expect("timestamp"),
        trace_id: Some("trace_1".to_string()),
        causation_id: Some("evt_122".to_string()),
        correlation_id: Some("task_1".to_string()),
        workspace_revision: Some(9),
        payload: serde_json::json!({"hostname": "api.internal"}),
    };

    assert_eq!(
        serde_json::to_value(event).expect("serialize domain event"),
        serde_json::json!({
            "event_id": "evt_123",
            "schema_version": 1,
            "organization_id": "org_1",
            "workspace_id": "ws_1",
            "actor": {"kind": "agent", "id": "agent_1"},
            "action": "service.updated",
            "resource": {"kind": "service", "id": "svc_1"},
            "occurred_at": "2026-08-18T21:00:00Z",
            "trace_id": "trace_1",
            "causation_id": "evt_122",
            "correlation_id": "task_1",
            "workspace_revision": 9,
            "payload": {"hostname": "api.internal"}
        })
    );
}

#[test]
fn terminal_input_wire_shape_is_stable() {
    let message = TerminalClientMessage::Resize {
        cols: 140,
        rows: 48,
    };
    assert_eq!(
        serde_json::to_value(message).expect("serialize"),
        serde_json::json!({ "type": "resize", "cols": 140, "rows": 48 })
    );

    assert_eq!(
        serde_json::to_value(TerminalClientMessage::Ack { bytes: 32_768 })
            .expect("serialize acknowledgement"),
        serde_json::json!({ "type": "ack", "bytes": 32768 })
    );
}

#[test]
fn terminal_attach_cursor_is_optional_on_the_wire() {
    let parsed: ProxyMessage = serde_json::from_value(serde_json::json!({
        "type": "terminal_attach",
        "session_id": "term_1",
        "agent_id": "agent_1",
        "cols": 80,
        "rows": 24
    }))
    .expect("legacy attach without cursor");
    match parsed {
        ProxyMessage::TerminalAttach { cursor, .. } => assert_eq!(cursor, None),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn terminal_ready_carries_stream_replay_metadata() {
    let message = TerminalServerMessage::Ready {
        session_id: "term_1".to_string(),
        stream_epoch: Some("stream_a".to_string()),
        revision: Some(9),
        gap: true,
        replay_chunks: Some(3),
    };
    assert_eq!(
        serde_json::to_value(&message).expect("serialize"),
        serde_json::json!({
            "type": "ready",
            "session_id": "term_1",
            "stream_epoch": "stream_a",
            "revision": 9,
            "gap": true,
            "replay_chunks": 3
        })
    );
    let parsed: TerminalServerMessage = serde_json::from_value(serde_json::json!({
        "type": "ready",
        "session_id": "term_1"
    }))
    .expect("legacy ready");
    assert_eq!(
        parsed,
        TerminalServerMessage::Ready {
            session_id: "term_1".to_string(),
            stream_epoch: None,
            revision: None,
            gap: false,
            replay_chunks: None,
        }
    );
}

#[test]
fn terminal_cursor_is_a_text_control_frame() {
    let message = TerminalServerMessage::Cursor {
        stream_epoch: "stream_a".to_string(),
        revision: 11,
    };
    assert_eq!(
        serde_json::to_value(&message).expect("serialize"),
        serde_json::json!({
            "type": "cursor",
            "stream_epoch": "stream_a",
            "revision": 11
        })
    );
}

#[test]
fn controller_terminal_ready_is_additive_json() {
    let parsed: AgentServerMessage = serde_json::from_value(serde_json::json!({
        "type": "terminal_ready",
        "session_id": "term_1",
        "stream_epoch": "stream_a",
        "revision": 4
    }))
    .expect("terminal ready without gap");
    assert_eq!(
        parsed,
        AgentServerMessage::TerminalReady {
            session_id: "term_1".to_string(),
            stream_epoch: "stream_a".to_string(),
            revision: 4,
            gap: false,
        }
    );
}

#[test]
fn terminal_binary_frame_round_trips_raw_bytes() {
    let frame = TerminalBinaryFrame {
        kind: TerminalBinaryKind::Output,
        session_id: "term_abc".to_string(),
        revision: 42,
        payload: vec![0, 1, 2, 0xff],
    };
    let encoded = frame.encode().expect("encode terminal frame");
    assert_eq!(TerminalBinaryFrame::decode(&encoded), Ok(frame));
}

#[test]
fn terminal_binary_frame_rejects_unknown_versions() {
    let frame = TerminalBinaryFrame {
        kind: TerminalBinaryKind::Input,
        session_id: "term_abc".to_string(),
        revision: 0,
        payload: b"hello".to_vec(),
    };
    let mut encoded = frame.encode().expect("encode terminal frame");
    encoded[0] = TERMINAL_BINARY_VERSION.saturating_add(1);
    let error = TerminalBinaryFrame::decode(&encoded).expect_err("version must fail");
    assert_eq!(error.code, "terminal_binary_version_mismatch");
}

#[test]
fn network_binary_frame_round_trips_raw_bytes() {
    let frame = NetworkBinaryFrame {
        kind: NetworkBinaryKind::Data,
        stream_id: "net_123".to_string(),
        payload: vec![0, 255, 13, 10, 42],
    };
    let encoded = frame.encode().expect("encode network frame");
    assert!(NetworkBinaryFrame::is_network_frame(&encoded));
    assert_eq!(
        NetworkBinaryFrame::decode(&encoded).expect("decode network frame"),
        frame
    );
}

#[test]
fn network_direct_target_round_trips() {
    let target = NetworkDirectTarget {
        report_usage: false,
        usage_ticket: None,
        host: "example.com".to_string(),
        port: 443,
    };
    let frame = NetworkBinaryFrame {
        kind: NetworkBinaryKind::Direct,
        stream_id: "net_direct".to_string(),
        payload: serde_json::to_vec(&target).expect("encode direct target"),
    };

    let decoded = NetworkBinaryFrame::decode(&frame.encode().expect("encode direct frame"))
        .expect("decode direct frame");
    assert_eq!(decoded.kind, NetworkBinaryKind::Direct);
    assert_eq!(
        serde_json::from_slice::<NetworkDirectTarget>(&decoded.payload)
            .expect("decode direct target"),
        target
    );
}

#[test]
fn network_datagram_and_usage_ack_keep_explicit_wire_kinds_and_legacy_defaults() {
    for (kind, byte) in [
        (NetworkBinaryKind::OpenDatagram, 9),
        (NetworkBinaryKind::UsageAck, 10),
    ] {
        let frame = NetworkBinaryFrame {
            kind,
            stream_id: "stream".into(),
            payload: vec![],
        };
        let wire = frame.encode().unwrap();
        assert_eq!(wire[4], byte);
        assert_eq!(NetworkBinaryFrame::decode(&wire).unwrap(), frame);
    }
    let target: NetworkDirectTarget =
        serde_json::from_str(r#"{"host":"example.test","port":443}"#).unwrap();
    assert!(!target.report_usage);
    assert!(target.usage_ticket.is_none());
    let open: NetworkOpenRequest =
        serde_json::from_str(r#"{"destination":"example.test","host":"example.test","port":443}"#)
            .unwrap();
    assert!(!open.track_lifetime);
    assert!(!open.durable_usage);
}

#[test]
fn machine_enrollment_keys_embed_the_workspace() {
    let secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let key = format_machine_enrollment_key("team one/研发", "abc123", secret)
        .expect("format enrollment key");
    assert!(!key.contains("team one"));
    assert_eq!(
        parse_machine_enrollment_key(&key).expect("parse enrollment key"),
        MachineEnrollmentKey {
            identifier: key.split_once('.').expect("key separator").0.to_string(),
            workspace_id: "team one/研发".to_string(),
            enrollment_id: "abc123".to_string(),
            secret: secret.to_string(),
        }
    );
}

#[test]
fn malformed_machine_enrollment_keys_are_rejected() {
    assert!(parse_machine_enrollment_key("enr_old.secret").is_err());
    assert!(parse_machine_enrollment_key("enr_v1_zz_id.0123").is_err());
}

#[test]
fn workspace_policy_document_has_a_stable_json_shape() {
    let document = WorkspacePolicyDocument {
        schema_version: POLICY_SCHEMA_VERSION,
        defaults: BTreeMap::from([("agent.prompt".to_string(), PolicyEffect::Deny)]),
        groups: BTreeMap::new(),
        rules: vec![WorkspacePolicyRule {
            bindings: Default::default(),
            conditions: Vec::new(),
            id: "self-read".to_string(),
            priority: 100,
            effect: PolicyEffect::Allow,
            subjects: vec![PolicySubjectSelector {
                tree: None,
                kind: Some(PolicyPrincipalKind::Agent),
                id: None,
                machine_id: None,
                group: None,
                is_self: true,
            }],
            actions: vec!["agent.metadata.read".to_string()],
            resources: vec![PolicyResourceSelector {
                tree: None,
                kind: Some("agent".to_string()),
                id: None,
                principal_group: None,
            }],
        }],
    };
    let value = serde_json::to_value(&document).expect("serialize policy document");
    assert_eq!(
        value,
        serde_json::json!({
            "schema_version": 1,
            "defaults": {"agent.prompt": "deny"},
            "groups": {},
            "rules": [{
                "id": "self-read",
                "priority": 100,
                "effect": "allow",
                "subjects": [{"kind": "agent", "self": true}],
                "actions": ["agent.metadata.read"],
                "resources": [{"kind": "agent"}]
            }]
        })
    );
    assert_eq!(
        serde_json::from_value::<WorkspacePolicyDocument>(value)
            .expect("deserialize policy document"),
        document
    );
    assert!(
        serde_json::from_value::<WorkspacePolicyDocument>(serde_json::json!({
            "schema_version": 1,
            "unexpected": true
        }))
        .is_err()
    );
}

#[test]
fn core_message_wire_shape_round_trips_ordered_contexts() {
    let message = CoreMessage {
        schema_version: MESSAGE_SCHEMA_VERSION,
        message_id: "msg_1".to_string(),
        workspace_id: "ws_1".to_string(),
        sender: MessagePrincipal {
            path: None,
            kind: MessagePrincipalKind::Agent,
            id: "agent_1".to_string(),
            name: "builder".to_string(),
            role: None,
        },
        recipients: vec![MessagePrincipal {
            path: None,
            kind: MessagePrincipalKind::Human,
            id: "user_1".to_string(),
            name: "Owner".to_string(),
            role: Some("owner".to_string()),
        }],
        context_ids: vec!["msg_parent_a".to_string(), "msg_parent_b".to_string()],
        body: "ready\nwith details".to_string(),
        created_at: "2026-08-21T12:00:00Z".parse().expect("timestamp"),
        expires_at: None,
        correlation_id: Some("task_1".to_string()),
        trace_id: None,
        external_source: Some(MessageExternalSource {
            channel: "telegram".to_string(),
            account_id: Some("bot_7".to_string()),
            conversation_id: "-10042:9".to_string(),
            message_id: "100".to_string(),
            metadata: BTreeMap::from([("user_id".to_string(), "1234".to_string())]),
        }),
    };

    let encoded = serde_json::to_value(&message).expect("serialize message");
    assert_eq!(encoded["schema_version"], MESSAGE_SCHEMA_VERSION);
    assert_eq!(
        encoded["context_ids"],
        serde_json::json!(["msg_parent_a", "msg_parent_b"])
    );
    assert_eq!(
        serde_json::from_value::<CoreMessage>(encoded).expect("deserialize message"),
        message
    );
}

#[test]
fn message_requests_reject_unknown_fields() {
    assert!(
        serde_json::from_value::<SendMessageRequest>(serde_json::json!({
            "recipients": ["agent_1"],
            "body": "hello",
            "unexpected": true
        }))
        .is_err()
    );
    assert_eq!(
        serde_json::from_value::<ReceiveMessagesRequest>(serde_json::json!({}))
            .expect("default receive request"),
        ReceiveMessagesRequest {
            limit: 50,
            wait_milliseconds: 0,
        }
    );
}

#[test]
fn agent_interface_command_and_transcript_round_trip() {
    let command = AgentCommand::Transcript {
        agent_id: "agent-1".to_string(),
        cursor: Some("42".to_string()),
        limit: Some(1),
    };
    let encoded = serde_json::to_value(&command).expect("serialize interface command");
    assert_eq!(encoded["action"], "transcript");
    assert_eq!(
        serde_json::from_value::<AgentCommand>(encoded).expect("deserialize interface command"),
        command
    );

    let descriptor = AgentInterfaceDescriptor {
        origin_url: None,
        protocol: AGENT_INTERFACE_PROTOCOL_V1.to_string(),
        instance_id: "pi-one".to_string(),
        port: 4180,
        capabilities: vec!["prompt.submit".to_string(), "transcript.read".to_string()],
        ui_path: Some("/".to_string()),
        registered_at: "2026-08-24T12:00:00Z".parse().expect("timestamp"),
    };
    assert!(descriptor.supports("prompt.submit"));
    assert!(!descriptor.supports("abort"));
    let encoded = serde_json::to_value(&descriptor).expect("serialize descriptor");
    assert_eq!(
        serde_json::from_value::<AgentInterfaceDescriptor>(encoded)
            .expect("deserialize descriptor"),
        descriptor
    );

    let transcript = AgentTranscriptResponse {
        agent_id: "agent-1".to_string(),
        interface_instance_id: "pi-one".to_string(),
        page: Some(0),
        page_count: Some(2),
        next_page: Some(1),
        cursor: Some("0".to_string()),
        next_cursor: Some("1".to_string()),
        entries: vec![AgentTranscriptEntry {
            id: "entry-1".to_string(),
            kind: "message".to_string(),
            role: Some("user".to_string()),
            content: serde_json::json!("hello"),
            created_at: Some("2026-08-24T12:00:00Z".to_string()),
        }],
    };
    let encoded = serde_json::to_value(&transcript).expect("serialize transcript page");
    assert_eq!(encoded["page"], 0);
    assert_eq!(encoded["page_count"], 2);
    assert_eq!(encoded["next_page"], 1);
    assert_eq!(
        serde_json::from_value::<AgentTranscriptResponse>(encoded)
            .expect("deserialize transcript page"),
        transcript
    );
}

#[test]
fn machine_exec_and_upload_commands_round_trip() {
    let exec = AgentCommand::Exec {
        request: MachineExecRequest {
            cwd: "repo".to_string(),
            command: "git".to_string(),
            args: vec!["status".to_string(), "--short".to_string()],
            timeout_ms: 5_000,
        },
    };
    let encoded = serde_json::to_value(&exec).expect("serialize machine exec");
    assert_eq!(encoded["action"], "exec");
    assert_eq!(
        serde_json::from_value::<AgentCommand>(encoded).expect("deserialize machine exec"),
        exec
    );

    let upload = AgentCommand::UploadChunk {
        upload_id: "upl_test".to_string(),
        content_base64: "aGVsbG8=".to_string(),
    };
    let encoded = serde_json::to_value(&upload).expect("serialize upload chunk");
    assert_eq!(encoded["action"], "upload_chunk");
    assert_eq!(
        serde_json::from_value::<AgentCommand>(encoded).expect("deserialize upload chunk"),
        upload
    );

    let startup = AgentCommand::StartupSet {
        agent_id: "agent-one".to_string(),
        request: SetAgentStartupRequest {
            cwd: "repo".to_string(),
            command: "sh".to_string(),
            args: vec!["-lc".to_string(), "exec ./agent".to_string()],
            publish_ports: vec![4180],
        },
    };
    let encoded = serde_json::to_value(&startup).expect("serialize Agent startup");
    assert_eq!(encoded["action"], "startup_set");
    assert_eq!(
        serde_json::from_value::<AgentCommand>(encoded).expect("deserialize Agent startup"),
        startup
    );
}

#[test]
fn treer_header_names_fold_underscores_and_case() {
    for name in [
        "x-treer-agent-id",
        "X-Treer-Agent-Id",
        "x_treer_agent_id",
        "X_TREER_IDENTITY_TOKEN",
        "x-treer_mixed",
    ] {
        assert!(is_treer_header_name(name), "{name}");
    }
    for name in [
        "x-treerx-agent",
        "x-trees-agent",
        "authorization",
        "x-treer",
        "",
    ] {
        assert!(!is_treer_header_name(name), "{name}");
    }
}
