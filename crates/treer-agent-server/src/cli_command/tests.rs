use super::*;

#[test]
fn prompt_delivery_and_queue_commands_parse() {
    for flags in [
        vec![],
        vec!["--queue"],
        vec!["--steer"],
        vec!["--notify", "--wait", "--timeout", "1000"],
    ] {
        let mut argv = vec!["treer", "agent", "prompt", "/reviewer", "task"];
        argv.extend(flags);
        assert!(Args::try_parse_from(argv).is_ok());
    }
    assert!(Args::try_parse_from([
        "treer", "agent", "prompt", "target", "task", "--queue", "--steer"
    ])
    .is_err());
    let defaults = Args::try_parse_from(["treer", "agent", "prompt", "target", "task"]).unwrap();
    assert!(matches!(
        defaults.command,
        Some(Command::Agent {
            command: AgentCommand::Prompt {
                steer: false,
                notify: false,
                ..
            }
        })
    ));
    assert!(Args::try_parse_from(["treer", "agent", "queue", "ls", "/reviewer"]).is_ok());
    assert!(
        Args::try_parse_from(["treer", "agent", "queue", "cancel", "/reviewer", "sub-1"]).is_ok()
    );
}

#[tokio::test]
async fn prompt_wait_follows_its_receipt_instead_of_unrelated_idle_state() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let polls = Arc::new(AtomicUsize::new(0));
    let seen = polls.clone();
    let app = axum::Router::new()
        .route("/api/agents/{agent_id}/prompt", axum::routing::post(|axum::Json(body): axum::Json<Value>| async move {
            assert_eq!(body["delivery"], "queue");
            assert_eq!(body["require_receipt"], true);
            axum::Json(json!({"submission":{"id":"sub-ours","disposition":"queued","position":2}}))
        }))
        .route("/api/agents/{agent_id}/submissions/{id}", axum::routing::get(move |axum::extract::Path((agent_id,id)): axum::extract::Path<(String,String)>| {
            let seen = seen.clone();
            async move {
                assert_eq!(id, "sub-ours");
                assert_eq!(agent_id, "ag_target");
                let state = if seen.fetch_add(1, Ordering::SeqCst) == 0 { "running" } else { "done" };
                axum::Json(json!({"submission":{"id":id,"sender":null,"created_at":"2026-10-12T00:00:00Z","position":null,"text_preview":"task","state":state,"turn_id":"turn-ours","assistant_preview":"done","error":null}}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ApiClient {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        base: Url::parse(&format!("http://{address}/")).unwrap(),
        source_agent_id: None,
        workload_credential: None,
        operator_credential: None,
    };
    let result = prompt_and_maybe_wait(
        &client,
        "ag_target",
        "task".into(),
        false,
        false,
        PromptWaitArgs {
            wait: true,
            until: vec![],
            timeout: Some(5000),
        },
    )
    .await
    .unwrap();
    assert_eq!(result["completion"]["id"], "sub-ours");
    assert_eq!(result["completion"]["turn_id"], "turn-ours");
    assert_eq!(polls.load(Ordering::SeqCst), 2);
    server.abort();
}

#[test]
fn admin_startup_and_restart_argv_parse_without_shell_interpretation() {
    let set = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "startup",
        "set",
        "reviewer",
        "--cwd",
        "repo",
        "--",
        "codex",
        "--no-daemon",
        "literal $() ' spaces",
    ])
    .unwrap();
    assert!(
        matches!(set.command, Some(Command::Agent { command: AgentCommand::Admin { command: AgentAdminCommand::Startup { command: AgentAdminStartupCommand::Set { target, cwd: Some(cwd), command } } } }) if target == "reviewer" && cwd == "repo" && command == ["codex", "--no-daemon", "literal $() ' spaces"])
    );
    for target in ["self", "reviewer", "ag_stable"] {
        Args::try_parse_from(["treer", "agent", "admin", "startup", "show", target]).unwrap();
    }
    let restart = Args::try_parse_from([
        "treer",
        "agent",
        "restart",
        "self",
        "--",
        "sh",
        "-lc",
        "echo hello",
    ])
    .unwrap();
    assert!(
        matches!(restart.command, Some(Command::Agent { command: AgentCommand::Restart { cwd: None, command, .. } }) if command == ["sh", "-lc", "echo hello"])
    );
    assert!(
        Args::try_parse_from(["treer", "agent", "restart", "reviewer", "--cwd", "repo"]).is_err()
    );
    assert!(
        Args::try_parse_from(["treer", "agent", "admin", "startup", "set", "reviewer"]).is_err()
    );
    let original = Args::try_parse_from(["treer", "agent", "startup", "set", "--", "sh"]).unwrap();
    assert!(
        matches!(original.command, Some(Command::Agent { command: AgentCommand::Startup { command: AgentStartupCommand::Set { cwd, command, .. } } }) if cwd == "." && command == ["sh"])
    );
}

#[test]
fn key_names_encode_to_terminal_sequences() {
    assert_eq!(
        encode_keys(&["ctrl-c".into(), "enter".into(), "up".into()]).expect("encode"),
        b"\x03\r\x1b[A"
    );
}

#[test]
fn agent_ls_replaces_list_with_a_clear_migration_error() {
    let parsed = Args::try_parse_from(["treer", "agent", "ls"]).unwrap();
    assert!(
        matches!(parsed.command, Some(Command::Agent { command: AgentCommand::Ls { path, recursive: false } }) if path == ".")
    );
    assert!(Args::try_parse_from(["treer", "agent", "ls", "../sibling", "-R"]).is_ok());
    assert!(Args::try_parse_from(["treer", "agent", "list"]).is_err());
    assert!(
        removed_command(&["agent".into(), "list".into(), "--machine".into()])
            .unwrap()
            .contains("treer agent ls -R /")
    );
    assert!(removed_command(&["agent".into(), "ls".into()]).is_none());
}

#[test]
fn path_targets_are_percent_encoded() {
    assert_eq!(path_segment("review agent"), "review%20agent");
}

#[test]
fn discovery_marks_the_current_agent_and_machine() {
    let snapshot: WorkspaceSnapshot = serde_json::from_value(json!({
        "revision": 1,
        "workspace": {
            "workspace_id": "workspace-a",
            "name": "Workspace A",
            "created_at": "2026-08-17T00:00:00Z"
        },
        "servers": [{
            "server_id": "server-a",
            "workspace_id": "workspace-a",
            "name": "builder",
            "hostname": "build-host",
            "root": "/workspace",
            "controller_build": {
                "version": "0.1.2",
                "git_commit": "controller-test"
            },
            "host_build": {
                "version": "0.1.2",
                "git_commit": "host-test"
            },
            "labels": {},
            "available_agents": ["claude"],
            "status": "online",
            "connected_at": "2026-08-17T00:00:00Z",
            "last_seen_at": "2026-08-17T00:00:00Z"
        }],
        "agents": [{
            "agent_id": "agent-a",
            "workspace_id": "workspace-a",
            "server_id": "server-a",
            "kind": "codex",
            "name": "reviewer",
            "cwd": ".",
            "status": "idle",
            "started_at": "2026-08-17T00:00:00Z",
            "updated_at": "2026-08-17T00:00:00Z",
            "output_revision": 0
        }]
    }))
    .expect("valid workspace snapshot");

    let value = discovery_value(snapshot.clone(), Some("agent-a"), Some("server-a"))
        .expect("resolve current identity");
    assert_eq!(value["self"]["agent"]["name"], "reviewer");
    assert_eq!(value["self"]["machine"]["name"], "builder");
    assert_eq!(
        discovery_value(snapshot.clone(), None, None).expect("unmanaged discovery")["self"],
        Value::Null
    );
    assert!(resolve_self(&snapshot, "agent-a", "server-other").is_err());
}

#[test]
fn skill_flags_work_without_a_subcommand() {
    for flag in ["--skill", "--skills"] {
        let args = Args::try_parse_from(["treer", flag]).expect("skill flag should parse");
        assert_eq!(args.skill.as_deref(), Some("treer"));
        assert!(args.command.is_none());
    }
    let install =
        Args::try_parse_from(["treer", "--skill", "install"]).expect("install skill should parse");
    assert_eq!(install.skill.as_deref(), Some("install"));
    let macos = Args::try_parse_from(["treer", "--skill", "macos-container"])
        .expect("macos-container skill should parse");
    assert_eq!(macos.skill.as_deref(), Some("macos-container"));
    assert!(SKILL.starts_with("---\nname: treer\n"));
    assert!(INSTALL_SKILL.starts_with("---\nname: treer-install\n"));
    assert!(MACOS_CONTAINER_SKILL.starts_with("---\nname: treer-macos-container\n"));
    assert!(!SKILL.contains("TODO"));
    assert!(!INSTALL_SKILL.contains("TODO"));
    assert!(!MACOS_CONTAINER_SKILL.contains("TODO"));
}

#[test]
fn attach_commands_parse() {
    let nested = Args::try_parse_from(["treer", "agent", "attach", "reviewer"])
        .expect("agent attach should parse");
    assert!(matches!(
        nested.command,
        Some(Command::Agent {
            command: AgentCommand::Attach { target }
        }) if target == "reviewer"
    ));
    assert!(Args::try_parse_from(["treer", "attach", "reviewer"]).is_err());
}

#[test]
fn machine_delete_command_parses() {
    let args = Args::try_parse_from(["treer", "machine", "delete", "srv_test"])
        .expect("machine delete should parse");
    assert!(matches!(
        args.command,
        Some(Command::Machine {
            command: MachineCommand::Delete { target }
        }) if target == "srv_test"
    ));
}

#[test]
fn machine_file_transfer_commands_parse_and_preserve_local_transport_paths() {
    let argv = [
        "machine",
        "put",
        "--machine",
        "build machine",
        "-",
        "research/result",
        "--force",
        "--mode",
        "640",
    ]
    .map(str::to_owned);
    let action = session_action(&argv, None).unwrap().unwrap();
    assert!(
        matches!(action,treer_protocol::cli::CliOutput::FileTransfer {path,local,remote,download:false,overwrite:true,mode:0o640} if path=="/api/machines/build%20machine/file-transfer" && local=="-" && remote=="research/result")
    );
    let args = Args::try_parse_from([
        "treer",
        "machine",
        "get",
        "--machine",
        "self",
        "report",
        "-",
    ])
    .unwrap();
    assert!(
        matches!(args.command,Some(Command::Machine {command:MachineCommand::Get {local,remote,force:false,mode:0o600,..}}) if local=="-" && remote=="report")
    );
    for mode in ["888", "4755", "-1"] {
        assert!(Args::try_parse_from([
            "treer",
            "machine",
            "put",
            "--machine",
            "self",
            "input",
            "output",
            "--mode",
            mode
        ])
        .is_err());
    }
    assert!(Args::try_parse_from(["treer", "machine", "put", "input", "output"]).is_err());
}

#[test]
fn machine_exec_command_preserves_argv() {
    let args = Args::try_parse_from([
        "treer",
        "exec",
        "--machine",
        "builder",
        "--cwd",
        "repo",
        "--timeout",
        "1500",
        "--",
        "sh",
        "-lc",
        "printf 'ok'",
    ])
    .expect("machine exec should parse");
    assert!(matches!(
        args.command,
        Some(Command::Exec {
            machine,
            cwd,
            timeout: 1500,
            command,
        }) if machine == "builder"
            && cwd == "repo"
            && command == ["sh", "-lc", "printf 'ok'"]
    ));
}

#[test]
fn launch_profile_commands_parse_structured_arguments() {
    let create = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "profile",
        "create",
        "reviewer",
        "--description",
        "Review changes",
        "--cwd",
        "/workspace",
        "codex",
        "--",
        "review",
        "--base",
        "main",
    ])
    .expect("profile create should parse");
    assert!(matches!(
        create.command,
        Some(Command::Agent {
            command: AgentCommand::Admin {
                command: AgentAdminCommand::Profile {
                    command: ProfileCommand::Create {
                        name,
                        executable,
                        args,
                        ..
                    }
                }
            }
        }) if name == "reviewer" && executable == "codex" && args == ["review", "--base", "main"]
    ));

    let update = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "profile",
        "update",
        "reviewer",
        "--arg",
        "--quiet",
        "--arg=check",
    ])
    .expect("profile update should parse flags as argument values");
    assert!(matches!(
        update.command,
        Some(Command::Agent {
            command: AgentCommand::Admin {
                command: AgentAdminCommand::Profile {
                    command: ProfileCommand::Update { args, .. }
                }
            }
        }) if args == ["--quiet", "check"]
    ));

    let launch = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "profile",
        "launch",
        "reviewer",
        "--machine",
        "builder",
        "--name",
        "review-42",
        "--cwd",
        "packages/api",
    ])
    .expect("profile launch should parse");
    assert!(matches!(
        launch.command,
        Some(Command::Agent {
            command: AgentCommand::Admin {
                command: AgentAdminCommand::Profile {
                    command: ProfileCommand::Launch { target, machine, name, cwd, .. }
                }
            }
        }) if target == "reviewer"
            && machine.as_deref() == Some("builder")
            && name.as_deref() == Some("review-42")
            && cwd.as_deref() == Some("packages/api")
    ));
    assert!(Args::try_parse_from(["treer", "profile", "list"]).is_err());
}

#[test]
fn network_publication_commands_are_not_exposed() {
    for args in [
        vec!["treer", "network", "service", "list"],
        vec![
            "treer", "network", "service", "create", "api", "--agent", "self", "--port", "8080",
        ],
        vec!["treer", "network", "host", "list"],
        vec!["treer", "network", "host", "create", "api.internal", "api"],
        vec!["treer", "network", "publish", "list"],
    ] {
        assert!(
            Args::try_parse_from(args).is_err(),
            "Agent CLI must not expose service publication commands"
        );
    }
}

#[test]
fn network_connect_command_parses() {
    let args = Args::try_parse_from(["treer", "network", "connect", "git.internal", "9418"])
        .expect("network connect should parse");
    assert!(matches!(
        args.command,
        Some(Command::Network {
            command: NetworkCommand::Connect { host, port }
        }) if host == "git.internal" && port == 9418
    ));
}

#[test]
fn agent_interface_commands_parse() {
    let register = Args::try_parse_from([
        "treer",
        "interface",
        "register",
        "--port",
        "4180",
        "--instance-id",
        "pi-one",
        "--capability",
        "prompt.submit",
        "--capability",
        "transcript.read",
        "--ui-path",
        "/",
    ])
    .expect("Agent Interface registration should parse");
    assert!(matches!(
        register.command,
        Some(Command::Interface {
            command: InterfaceCommand::Register {
                port: 4180,
                instance_id,
                capabilities,
                ui_path: Some(ui_path),
            }
        }) if instance_id == "pi-one"
            && capabilities == ["prompt.submit", "transcript.read"]
            && ui_path == "/"
    ));

    let transcript = Args::try_parse_from([
        "treer",
        "agent",
        "transcript",
        "reviewer",
        "--page",
        "2",
        "--cursor",
        "10",
        "--limit",
        "25",
    ])
    .expect("Agent transcript should parse");
    assert!(matches!(
        transcript.command,
        Some(Command::Agent {
            command: AgentCommand::Transcript {
                target,
                page: Some(2),
                cursor: Some(cursor),
                limit: 25,
            }
        }) if target == "reviewer" && cursor == "10"
    ));
}

#[test]
fn identity_token_command_parses() {
    let args = Args::try_parse_from(["treer", "token", "create", "api"])
        .expect("token create should parse");
    assert!(matches!(
        args.command,
        Some(Command::Token {
            command: TokenCommand::Create {
                audience,
                json: false,
            }
        }) if audience == "api"
    ));
}

#[test]
fn member_directory_command_parses() {
    let members =
        Args::try_parse_from(["treer", "member", "list"]).expect("member list should parse");
    assert!(matches!(
        members.command,
        Some(Command::Member {
            command: MemberCommand::List
        })
    ));
}

#[test]
fn message_commands_parse_machine_readable_inputs() {
    let send = Args::try_parse_from([
        "treer",
        "message",
        "send",
        "--to",
        "agent-a",
        "--to",
        "user-b",
        "--context",
        "msg-parent",
        "--idempotency-key",
        "telegram:update:42",
        "--body-file",
        "-",
    ])
    .expect("message send should parse");
    assert!(matches!(
        send.command,
        Some(Command::Message {
            command: MessageCommand::Send {
                recipients,
                context_ids,
                idempotency_key: Some(key),
                body_file: Some(path),
                ..
            }
        }) if recipients == ["agent-a", "user-b"]
            && context_ids == ["msg-parent"]
            && key == "telegram:update:42"
            && path == Path::new("-")
    ));

    let ack = Args::try_parse_from([
        "treer",
        "message",
        "ack",
        "dlv-a",
        "dlv-b",
        "--operation-id",
        "ack-42",
    ])
    .expect("message ack should parse");
    assert!(matches!(
        ack.command,
        Some(Command::Message {
            command: MessageCommand::Ack {
                delivery_ids,
                operation_id: Some(operation_id),
            }
        }) if delivery_ids == ["dlv-a", "dlv-b"] && operation_id == "ack-42"
    ));
}

#[test]
fn agent_admin_commands_parse() {
    for action in ["start", "stop", "restart"] {
        Args::try_parse_from(["treer", "agent", action, "workload"])
            .expect("unified lifecycle parses");
        Args::try_parse_from(["treer", "agent", "admin", action, "workload"])
            .expect("admin compatibility parses");
    }
    let create = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "create",
        "--machine",
        "builder",
        "--kind",
        "command",
        "--name",
        "shell",
        "--",
        "/bin/sh",
    ])
    .expect("agent admin create should parse");
    assert!(matches!(
        create.command,
        Some(Command::Agent {
            command: AgentCommand::Admin {
                command: AgentAdminCommand::Create {
                    machine: Some(machine),
                    name,
                    ..
                }
            }
        }) if machine == "builder" && name == "shell"
    ));

    let delete = Args::try_parse_from(["treer", "agent", "admin", "delete", "reviewer"])
        .expect("agent admin delete should parse");
    assert!(matches!(
        delete.command,
        Some(Command::Agent {
            command: AgentCommand::Admin {
                command: AgentAdminCommand::Delete { target }
            }
        }) if target == "reviewer"
    ));
    assert!(Args::try_parse_from(["treer", "agent", "delete", "reviewer"]).is_err());

    let publish = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "create",
        "--machine",
        "builder",
        "--kind",
        "command",
        "--name",
        "codex-ui",
        "--publish",
        "4173",
        "--",
        "/opt/codex-agent-ui/scripts/treer-agent.sh",
    ])
    .expect("agent admin create --publish should parse");
    assert!(matches!(
        publish.command,
        Some(Command::Agent {
            command: AgentCommand::Admin {
                command: AgentAdminCommand::Create {
                    publish_ports,
                    ..
                }
            }
        }) if publish_ports == vec![4173]
    ));

    let recipe = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "create",
        "--machine",
        "builder",
        "--kind",
        "auto",
        "--name",
        "installer",
        "--recipe",
        "https://github.com/example/recipe.git",
    ])
    .expect("agent admin create --recipe should parse");
    assert!(matches!(
        recipe.command,
        Some(Command::Agent {
            command: AgentCommand::Admin {
                command: AgentAdminCommand::Create {
                    recipe: Some(url),
                    ..
                }
            }
        }) if url == "https://github.com/example/recipe.git"
    ));
}

#[test]
fn agent_creation_accepts_tree_folder_separately_from_working_directory() {
    let parsed = Args::try_parse_from([
        "treer",
        "agent",
        "admin",
        "create",
        "--kind",
        "command",
        "--name",
        "child",
        "--folder",
        "../reviews",
        "--cwd",
        "repo",
        "--",
        "/bin/sh",
    ])
    .unwrap();
    assert!(matches!(parsed.command,
        Some(Command::Agent { command: AgentCommand::Admin {
            command: AgentAdminCommand::Create { folder: Some(folder), cwd, .. }
        } }) if folder == "../reviews" && cwd == "repo"));
    let parsed = Args::try_parse_from([
        "treer", "agent", "admin", "profile", "launch", "reviewer", "--folder", "/reviews",
    ])
    .unwrap();
    assert!(matches!(parsed.command,
        Some(Command::Agent { command: AgentCommand::Admin {
            command: AgentAdminCommand::Profile { command: ProfileCommand::Launch { folder: Some(folder), cwd: None, .. } }
        } }) if folder == "/reviews"));
}

#[test]
fn agent_startup_command_preserves_argv() {
    let args = Args::try_parse_from([
        "treer",
        "agent",
        "startup",
        "set",
        "--cwd",
        "repo",
        "--publish",
        "4180",
        "--",
        "sh",
        "-lc",
        "exec ./agent --serve",
    ])
    .expect("Agent startup command should parse");
    assert!(matches!(
        args.command,
        Some(Command::Agent {
            command: AgentCommand::Startup {
                command: AgentStartupCommand::Set {
                    cwd,
                    publish_ports,
                    command,
                }
            }
        }) if cwd == "repo"
            && publish_ports == [4180]
            && command == ["sh", "-lc", "exec ./agent --serve"]
    ));
}

#[test]
fn managed_app_commands_parse() {
    let create = Args::try_parse_from([
        "treer",
        "app",
        "create",
        "--machine",
        "builder",
        "--name",
        "soul",
        "--port",
        "9420",
        "--public",
        "--folder",
        "/team",
        "python3",
        "--",
        "apps/soul/soul.py",
    ])
    .expect("App create should parse");
    assert!(matches!(
        create.command,
        Some(Command::App {
            command: AppCommand::Create {
                machine: Some(machine),
                name,
                port: 9420,
                public: true,
                folder: Some(folder),
                executable,
                args,
                ..
            }
        }) if machine == "builder"
            && name == "soul"
            && folder == "/team"
            && executable == "python3"
            && args == ["apps/soul/soul.py"]
    ));

    let restart = Args::try_parse_from(["treer", "app", "restart", "soul"])
        .expect("App restart should parse");
    assert!(matches!(
        restart.command,
        Some(Command::App {
            command: AppCommand::Restart { target }
        }) if target == "soul"
    ));
}

#[test]
fn agent_and_app_help_explain_paths_and_sibling_scope() {
    use clap::CommandFactory;
    let mut cli = Args::command();
    let help = cli
        .find_subcommand_mut("agent")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(help.contains("../team/agent"));
    assert!(help.contains("siblings only"));
    assert!(help.contains("agent_not_in_folder"));
    let help = cli
        .find_subcommand_mut("app")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(help.contains("Bare names never fall back"));
    for action in ["send", "reply"] {
        let help = cli
            .find_subcommand_mut("message")
            .unwrap()
            .find_subcommand_mut(action)
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(help.contains("Agent tree path"));
        assert!(help.contains("sibling Agent name"));
    }

    for target in [
        "/team/reviewer",
        "../team/reviewer",
        "./reviewer",
        "sub/reviewer",
        "ag_stable",
        "app_stable",
        "self",
        ".",
    ] {
        for verb in [
            "show",
            "read",
            "start",
            "stop",
            "restart",
            "wait",
            "transcript",
        ] {
            assert!(Args::try_parse_from(["treer", "agent", verb, target]).is_ok());
        }
        assert!(Args::try_parse_from([
            "treer", "message", "send", "--to", target, "--body", "hello"
        ])
        .is_ok());
    }
}
