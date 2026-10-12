use super::*;

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
                    command: ProfileCommand::Launch { target, machine, name, cwd }
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
                executable,
                args,
                ..
            }
        }) if machine == "builder"
            && name == "soul"
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
