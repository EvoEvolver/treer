use treer_protocol::{
    agent_install_command_name, AgentCreationErrorDetails, AgentCreationFailureState, ProtocolError,
};

#[test]
fn creation_details_are_optional_for_existing_errors_and_round_trip_new_errors() {
    let legacy = serde_json::json!({"code":"spawn_failed","message":"failed"});
    let error: ProtocolError = serde_json::from_value(legacy.clone()).unwrap();
    assert!(error.details.is_none());
    assert_eq!(serde_json::to_value(error).unwrap(), legacy);
    let mut error = ProtocolError::new("command_not_found", "Required executable is not installed");
    error.details = Some(Box::new(AgentCreationErrorDetails {
        command: Some("claude".into()),
        machine_id: Some("srv_test".into()),
        agent_id: Some("ag_test".into()),
        creation_state: AgentCreationFailureState::IdentityRemoved,
    }));
    let bytes = serde_json::to_vec(&error).unwrap();
    assert_eq!(
        serde_json::from_slice::<ProtocolError>(&bytes).unwrap(),
        error
    );
    assert_eq!(
        serde_json::to_value(error).unwrap()["details"]["creation_state"],
        "identity_removed"
    );
}

#[test]
fn install_identifiers_cannot_be_paths_shell_commands_or_unbounded_diagnostics() {
    for allowed in [
        "claude",
        "codex",
        "grok",
        "opencode",
        "claude.exe",
        "unknown-tool",
    ] {
        assert_eq!(
            agent_install_command_name(allowed).as_deref(),
            Some(allowed)
        );
    }
    for rejected in [
        "",
        ".",
        "..",
        "tool.",
        "/private/bin/claude",
        "C:\\secret\\claude.exe",
        "claude --help",
        "claude;evil",
        "$(evil)",
        "tool\n",
        "é",
        &"a".repeat(129),
    ] {
        assert!(
            agent_install_command_name(rejected).is_none(),
            "{rejected:?}"
        );
    }
}
