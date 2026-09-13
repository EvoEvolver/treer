
use super::*;

#[test]
fn source_agent_identity_is_required_and_validated() {
    let mut headers = HeaderMap::new();
    assert!(source_agent_id(&headers).is_err());
    headers.insert(AGENT_ID_HEADER, " agent-a ".parse().expect("agent header"));
    assert_eq!(
        source_agent_id(&headers).unwrap_or_else(|_| panic!("valid identity")),
        "agent-a"
    );
}

#[test]
fn workload_identity_requires_both_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
    assert!(workload_identity(&headers).is_err());
    headers.insert(
        WORKLOAD_CREDENTIAL_HEADER,
        "wlc_secret".parse().expect("credential header"),
    );
    assert_eq!(
        workload_identity(&headers).unwrap_or_else(|_| panic!("workload identity")),
        ("agent-a", "wlc_secret")
    );
}

#[test]
fn optional_workload_identity_distinguishes_operator_and_agent_requests() {
    let mut headers = HeaderMap::new();
    assert_eq!(
        optional_workload_identity(&headers).expect("operator request"),
        None
    );

    headers.insert(AGENT_ID_HEADER, "agent-a".parse().expect("agent header"));
    let missing_credential =
        optional_workload_identity(&headers).expect_err("partial Agent identity");
    assert_eq!(
        missing_credential.error.code,
        "workload_credential_required"
    );

    headers.insert(
        WORKLOAD_CREDENTIAL_HEADER,
        "wlc_secret".parse().expect("credential header"),
    );
    assert_eq!(
        optional_workload_identity(&headers).expect("managed Agent request"),
        Some(("agent-a", "wlc_secret"))
    );

    headers.remove(AGENT_ID_HEADER);
    let missing_agent =
        optional_workload_identity(&headers).expect_err("credential without Agent ID");
    assert_eq!(missing_agent.error.code, "agent_identity_required");
}

#[test]
fn operator_requests_require_the_private_controller_credential() {
    let mut headers = HeaderMap::new();
    assert!(!operator_credential_matches("opc_secret", &headers));
    headers.insert(
        OPERATOR_CREDENTIAL_HEADER,
        "opc_wrong".parse().expect("operator header"),
    );
    assert!(!operator_credential_matches("opc_secret", &headers));
    headers.insert(
        OPERATOR_CREDENTIAL_HEADER,
        "opc_secret".parse().expect("operator header"),
    );
    assert!(operator_credential_matches("opc_secret", &headers));
}
