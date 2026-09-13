use super::*;

#[test]
fn status_detector_prefers_blockers() {
    assert_eq!(
        detect_status("Working (esc to interrupt)\nAllow command?"),
        Some(AgentStatus::Blocked)
    );
}

#[test]
fn upload_names_reject_path_traversal() {
    assert!(validate_upload_file_name("notes.txt").is_ok());
    for name in [
        "",
        ".",
        "..",
        "../notes.txt",
        "dir/notes.txt",
        "dir\\notes.txt",
    ] {
        assert!(
            validate_upload_file_name(name).is_err(),
            "accepted {name:?}"
        );
    }
}

#[tokio::test]
async fn bounded_reader_drains_and_marks_truncation() {
    let input = &b"abcdefgh"[..];
    let (output, truncated) = read_bounded(input, 4).await.expect("read bytes");
    assert_eq!(output, b"abcd");
    assert!(truncated);
}

#[test]
fn status_detector_only_scans_recent_output() {
    let text = format!("Allow command?{}$", " ".repeat(STATUS_SCAN_LIMIT_BYTES));
    assert_eq!(detect_status(&text), Some(AgentStatus::Idle));
}

#[test]
fn output_trimming_uses_slack_and_preserves_utf8() {
    let mut below_slack = "x".repeat(OUTPUT_LIMIT_BYTES + OUTPUT_TRIM_SLACK_BYTES);
    trim_text(&mut below_slack);
    assert_eq!(
        below_slack.len(),
        OUTPUT_LIMIT_BYTES + OUTPUT_TRIM_SLACK_BYTES
    );

    let mut oversized = "é".repeat((OUTPUT_LIMIT_BYTES + OUTPUT_TRIM_SLACK_BYTES) / 2 + 1);
    trim_text(&mut oversized);
    assert_eq!(oversized.len(), OUTPUT_LIMIT_BYTES);
    assert_eq!(oversized.chars().count(), OUTPUT_LIMIT_BYTES / 2);
}

#[test]
fn workload_credentials_are_unique_and_compared_exactly() {
    let first = new_workload_credential();
    let second = new_workload_credential();
    assert!(first.starts_with("wlc_"));
    assert_eq!(first.len(), 68);
    assert_ne!(first, second);
    assert!(workload_credential_matches(&first, &first));
    assert!(!workload_credential_matches(&first, &second));
    assert!(!workload_credential_matches("", ""));
}

#[test]
fn host_metadata_preserves_the_workload_credential_for_controller_restarts() {
    let metadata = AgentMetadata {
        agent_id: "agent-a".to_string(),
        workspace_id: "workspace-a".to_string(),
        server_id: "server-a".to_string(),
        kind: "command".to_string(),
        name: "agent-a".to_string(),
        cwd: ".".to_string(),
        workload_credential: "wlc_secret".to_string(),
    };
    let encoded = serde_json::to_string(&metadata).expect("encode metadata");
    let restored: AgentMetadata = serde_json::from_str(&encoded).expect("restore metadata");
    assert_eq!(restored.workload_credential, "wlc_secret");
}

#[test]
fn prompt_text_uses_bracketed_paste_when_enabled() {
    assert_eq!(
        encode_prompt_text("hello", true),
        b"\x1b[200~hello\x1b[201~"
    );
    assert_eq!(encode_prompt_text("hello", false), b"hello");
}

#[test]
fn agent_commands_are_entered_in_an_interactive_shell() {
    let args = vec![
        "--model".to_string(),
        "gpt 5".to_string(),
        "it's".to_string(),
        String::new(),
    ];

    let launch = shell_agent_launch(
        "codex",
        Some("--dangerously-bypass-approvals-and-sandbox"),
        &args,
        false,
        Some("npm install -g @openai/codex"),
    );

    assert!(!launch.command.is_empty());
    assert_eq!(launch.args, ["-i"]);
    assert_eq!(
            launch.initial_writes,
            [HostWrite {
                data: b"if ! command -v 'codex' >/dev/null 2>&1; then echo 'treer: installing missing codex' >&2; npm install -g @openai/codex; fi; 'codex' '--dangerously-bypass-approvals-and-sandbox' '-c' 'check_for_upgrades_on_startup=false' '--model' 'gpt 5' 'it'\\''s' ''; exec 'codex' '--dangerously-bypass-approvals-and-sandbox' '-c' 'check_for_upgrades_on_startup=false' '--model' 'gpt 5' 'it'\\''s' ''\r".to_vec(),
                delay_ms: AGENT_COMMAND_DELAY.as_millis() as u64,
            }]
        );
}

#[test]
fn claude_launch_skips_permissions_and_confirms_workspace_trust() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "claude".to_string(),
        name: "claude-test".to_string(),
        cwd: ".".to_string(),
        args: vec!["--model".to_string(), "sonnet".to_string()],
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (_kind, launch) = resolve_launch(&request).expect("resolve claude launch");

    assert_eq!(launch.initial_writes.len(), 2);
    assert_eq!(
            launch.initial_writes[0].data,
            b"if ! command -v 'claude' >/dev/null 2>&1; then echo 'treer: installing missing claude' >&2; curl -fsSL https://claude.ai/install.sh | bash; fi; 'claude' '--dangerously-skip-permissions' '--model' 'sonnet'\r"
        );
    assert_eq!(launch.initial_writes[1].data, b"\r");
    assert_eq!(
        launch.initial_writes[1].delay_ms,
        CLAUDE_TRUST_CONFIRM_DELAY.as_millis() as u64
    );
}

#[test]
fn shell_commands_are_entered_after_interactive_shell_startup() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "shell".to_string(),
        name: "profile".to_string(),
        cwd: ".".to_string(),
        args: vec![
            "opencode".to_string(),
            "--model".to_string(),
            "provider/model name".to_string(),
        ],
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (_kind, launch) = resolve_launch(&request).expect("resolve shell command launch");

    assert!(!launch.command.is_empty());
    assert_eq!(launch.args, ["-i"]);
    assert_eq!(
        launch.initial_writes,
        [HostWrite {
            data: b"'opencode' '--model' 'provider/model name'\r".to_vec(),
            delay_ms: AGENT_COMMAND_DELAY.as_millis() as u64,
        }]
    );
}

#[test]
fn explicit_command_agents_still_spawn_directly() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "command".to_string(),
        name: "shell".to_string(),
        cwd: ".".to_string(),
        args: vec!["/bin/sh".to_string(), "-c".to_string(), "pwd".to_string()],
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (_kind, launch) = resolve_launch(&request).expect("resolve command launch");

    assert_eq!(launch.command, "/bin/sh");
    assert_eq!(launch.args, ["-c", "pwd"]);
    assert!(launch.initial_writes.is_empty());
}

#[test]
fn managed_apps_spawn_directly_and_publish_their_ui_port() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "app".to_string(),
        name: "docs".to_string(),
        cwd: ".".to_string(),
        args: vec![
            "python3".to_string(),
            "-m".to_string(),
            "http.server".to_string(),
            "8080".to_string(),
        ],
        cols: 120,
        rows: 36,
        publish_ports: vec![8080],
        recipe: None,
    };

    let (kind, launch) = resolve_launch(&request).expect("resolve App launch");

    assert_eq!(kind, "app");
    assert_eq!(launch.command, "python3");
    assert_eq!(launch.args, ["-m", "http.server", "8080"]);
    assert!(launch.initial_writes.is_empty());
    assert_eq!(launch.publish_ports, [8080]);
}

#[test]
fn empty_command_request_opens_an_unmodified_interactive_terminal() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "command".to_string(),
        name: "terminal".to_string(),
        cwd: ".".to_string(),
        args: Vec::new(),
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (_kind, launch) = resolve_launch(&request).expect("resolve terminal launch");

    assert!(!launch.command.is_empty());
    assert_eq!(launch.args, ["-i"]);
    assert!(launch.initial_writes.is_empty());
}

#[test]
fn auto_kind_selects_the_first_cli_on_the_search_path() {
    let root = std::env::temp_dir().join(format!("treer-auto-kind-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&root).expect("temp path");
    let cursor = root.join("cursor-agent");
    std::fs::write(&cursor, "#!/bin/sh\n").expect("write cursor stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cursor, std::fs::Permissions::from_mode(0o755))
            .expect("chmod cursor stub");
    }
    let request = CreateAgentRequest {
        server_id: None,
        kind: "auto".to_string(),
        name: "installer".to_string(),
        cwd: ".".to_string(),
        args: Vec::new(),
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };

    let (kind, launch) =
        resolve_launch_with_path(&request, &root.to_string_lossy()).expect("resolve auto");
    assert_eq!(kind, "cursor");
    let script = String::from_utf8(launch.initial_writes[0].data.clone()).expect("utf8");
    assert!(script.contains("'cursor-agent'"));
    assert!(!script.contains("npm install -g @openai/codex"));
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn auto_kind_falls_back_to_codex_when_nothing_is_on_path() {
    let request = CreateAgentRequest {
        server_id: None,
        kind: "auto".to_string(),
        name: "installer".to_string(),
        cwd: ".".to_string(),
        args: Vec::new(),
        cols: 120,
        rows: 36,
        publish_ports: Vec::new(),
        recipe: None,
    };
    let empty = std::env::temp_dir().join(format!("treer-empty-path-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&empty).expect("empty path");
    let (kind, launch) =
        resolve_launch_with_path(&request, &empty.to_string_lossy()).expect("resolve auto");
    assert_eq!(kind, "codex");
    let script = String::from_utf8(launch.initial_writes[0].data.clone()).expect("utf8");
    assert!(script.contains("npm install -g @openai/codex"));
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn agent_path_includes_user_local_and_fnm_dirs() {
    let path = join_agent_path(Some(std::path::Path::new("/opt/treer/bin/treer")));
    assert!(path.contains("/opt/treer/bin"));
    if let Some(home) = std::env::var_os("HOME") {
        let local = std::path::Path::new(&home).join(".local/bin");
        assert!(path.contains(&*local.to_string_lossy()));
    }
}

#[test]
fn agent_proxy_urls_carry_policy_identity() {
    let url = agent_network_proxy_url("socks5h://127.0.0.1:8791", "agent-a");
    let url = url::Url::parse(&url).expect("agent proxy URL");
    assert_eq!(url.username(), "agent-a");
    assert_eq!(url.password(), Some("treer"));
}

#[test]
fn transparent_networking_does_not_expose_the_host_loopback_proxy() {
    let env = network_environment("socks5h://agent-a:treer@127.0.0.1:8791".to_string(), true);

    assert_eq!(env.get("ALL_PROXY").map(String::as_str), Some(""));
    assert_eq!(env.get("all_proxy").map(String::as_str), Some(""));
    assert_eq!(env.get("HTTP_PROXY").map(String::as_str), Some(""));
    assert_eq!(env.get("http_proxy").map(String::as_str), Some(""));
    assert_eq!(env.get("HTTPS_PROXY").map(String::as_str), Some(""));
    assert_eq!(env.get("https_proxy").map(String::as_str), Some(""));
    assert_eq!(
        env.get("TREER_NETWORK_PROXY").map(String::as_str),
        Some("socks5h://agent-a:treer@127.0.0.1:8791")
    );
}

#[test]
fn compatibility_networking_exposes_the_socks_proxy() {
    let proxy = "socks5h://agent-a:treer@127.0.0.1:8791";
    let http_proxy = "http://agent-a:treer@127.0.0.1:8791";
    let env = network_environment(proxy.to_string(), false);

    assert_eq!(env.get("ALL_PROXY").map(String::as_str), Some(proxy));
    assert_eq!(env.get("all_proxy").map(String::as_str), Some(proxy));
    assert!(!env.contains_key("HTTP_PROXY"));
    assert!(!env.contains_key("http_proxy"));
    assert_eq!(env.get("HTTPS_PROXY").map(String::as_str), Some(http_proxy));
    assert_eq!(env.get("https_proxy").map(String::as_str), Some(http_proxy));
    assert_eq!(
        env.get("GIT_PROXY_COMMAND").map(String::as_str),
        Some("treer")
    );
    assert_eq!(
        env.get("TREER_GIT_PROXY_MODE").map(String::as_str),
        Some("1")
    );
    assert_eq!(
        env.get("NO_PROXY").map(String::as_str),
        Some("127.0.0.1,localhost,::1")
    );
    assert_eq!(
        env.get("no_proxy").map(String::as_str),
        Some("127.0.0.1,localhost,::1")
    );
    assert_eq!(
        env.get("TREER_NETWORK_PROXY").map(String::as_str),
        Some(proxy)
    );
}

#[test]
fn native_network_launch_gates_exec_without_reinterpreting_arguments() {
    let launch = native_network_launch(
        Some(std::path::Path::new(
            "/Applications/TreerNetwork.app/Contents/MacOS/TreerNetwork",
        )),
        "socks5h://agent-a:treer@127.0.0.1:8791",
        "agent-a",
        AgentLaunch {
            command: "/bin/zsh".into(),
            args: vec!["-c".into(), "printf '%s' '$literal ; space'".into()],
            initial_writes: Vec::new(),
            publish_ports: vec![8080],
        },
    );
    assert_eq!(
        launch.args,
        [
            "exec",
            "--agent-id",
            "agent-a",
            "--network-proxy",
            "socks5h://agent-a:treer@127.0.0.1:8791",
            "--",
            "/bin/zsh",
            "-c",
            "printf '%s' '$literal ; space'",
        ]
    );
    assert_eq!(launch.publish_ports, [8080]);
    assert!(launch.initial_writes.is_empty());
}

#[test]
fn transparent_sandbox_preserves_launch_and_initial_input() {
    let initial_writes = vec![HostWrite {
        data: b"codex\r".to_vec(),
        delay_ms: 500,
    }];
    let launch = sandbox_launch(
        Some(std::path::Path::new("/opt/treer-agent-server")),
        "socks5h://agent-a:treer@127.0.0.1:8791",
        "agent-a",
        AgentLaunch {
            command: "/bin/bash".to_string(),
            args: vec!["-i".to_string()],
            initial_writes: initial_writes.clone(),
            publish_ports: Vec::new(),
        },
    );

    assert_eq!(launch.command, "/opt/treer-agent-server");
    assert_eq!(
        launch.args,
        [
            "sandbox-exec",
            "--network-proxy",
            "socks5h://agent-a:treer@127.0.0.1:8791",
            "--service-socket",
            crate::network::agent_service_socket_path("agent-a")
                .display()
                .to_string()
                .as_str(),
            "--",
            "/bin/bash",
            "-i"
        ]
    );
    assert_eq!(launch.initial_writes, initial_writes);
}

#[test]
fn transparent_sandbox_publishes_requested_namespace_ports() {
    let launch = sandbox_launch(
        Some(std::path::Path::new("/opt/treer-agent-server")),
        "socks5h://127.0.0.1:8791",
        "agent-a",
        AgentLaunch {
            command: "/bin/bash".to_string(),
            args: vec!["-i".to_string()],
            initial_writes: Vec::new(),
            publish_ports: vec![4173],
        },
    );
    assert_eq!(
        launch.args,
        [
            "sandbox-exec",
            "--network-proxy",
            "socks5h://127.0.0.1:8791",
            "--service-socket",
            crate::network::agent_service_socket_path("agent-a")
                .display()
                .to_string()
                .as_str(),
            "--publish",
            "4173",
            "--",
            "/bin/bash",
            "-i"
        ]
    );
}

#[test]
fn virtual_host_snapshots_only_move_forward_on_one_connection() {
    let current = VirtualNetworkHostsSnapshot {
        workspace_id: "default".to_string(),
        revision: 8,
        hosts: Vec::new(),
    };
    assert!(!should_replace_virtual_hosts(Some(&current), &current));
    assert!(!should_replace_virtual_hosts(
        Some(&current),
        &VirtualNetworkHostsSnapshot {
            revision: 7,
            ..current.clone()
        }
    ));
    assert!(should_replace_virtual_hosts(
        Some(&current),
        &VirtualNetworkHostsSnapshot {
            revision: 9,
            ..current.clone()
        }
    ));
}
