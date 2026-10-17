use std::io::Cursor;
use treer_message_endpoint::protocol::{
    proof, read_frame, verify_proof, write_frame, BindingKey, Request, Role, Session, Transcript,
    MAX_FRAME,
};

fn transcript() -> Transcript {
    Transcript {
        version: 1,
        endpoint_id: "ag_test".into(),
        generation: 1,
        hub_nonce: [7; 32],
        endpoint_nonce: [9; 32],
    }
}
fn sessions() -> (Session, Session) {
    let key = BindingKey::new([42; 32]);
    let t = transcript();
    (
        Session::authenticated(
            &key,
            Role::Endpoint,
            &t,
            &proof(&key, Role::Hub, &t).unwrap(),
        )
        .unwrap(),
        Session::authenticated(
            &key,
            Role::Hub,
            &t,
            &proof(&key, Role::Endpoint, &t).unwrap(),
        )
        .unwrap(),
    )
}

#[test]
fn proofs_bind_identity_generation_both_nonces_version_and_direction() {
    let key = BindingKey::new([42; 32]);
    let t = transcript();
    let p = proof(&key, Role::Endpoint, &t).unwrap();
    verify_proof(&key, Role::Endpoint, &t, &p).unwrap();
    assert!(verify_proof(&key, Role::Hub, &t, &p).is_err());
    assert!(verify_proof(&BindingKey::new([41; 32]), Role::Endpoint, &t, &p).is_err());
    for change in 0..5 {
        let mut changed = t.clone();
        match change {
            0 => changed.endpoint_id = "ag_other".into(),
            1 => changed.generation += 1,
            2 => changed.hub_nonce[0] ^= 1,
            3 => changed.endpoint_nonce[0] ^= 1,
            _ => changed.version += 1,
        }
        assert!(verify_proof(&key, Role::Endpoint, &changed, &p).is_err());
    }
    assert!(Session::authenticated(&key, Role::Hub, &t, &[0; 32]).is_err());
}

#[test]
fn directional_sessions_exchange_exact_bytes_and_reject_reflection() {
    let (mut endpoint, mut hub) = sessions();
    let submit = endpoint
        .seal(br#"{"op":"status","request_id":"1"}"#)
        .unwrap();
    assert_eq!(
        hub.open(&submit).unwrap(),
        br#"{"op":"status","request_id":"1"}"#
    );
    let response = hub.seal(br#"{"ok":true}"#).unwrap();
    assert_eq!(endpoint.open(&response).unwrap(), br#"{"ok":true}"#);
    let (mut endpoint, _) = sessions();
    let reflected = endpoint.seal(b"private").unwrap();
    assert!(endpoint.open(&reflected).is_err());
}

#[test]
fn replay_gap_and_tampering_poison_connection() {
    let (mut endpoint, mut hub) = sessions();
    let first = endpoint.seal(b"first").unwrap();
    let second = endpoint.seal(b"second").unwrap();
    assert!(hub.open(&second).is_err());
    assert!(hub.open(&first).is_err());

    let (mut endpoint, mut hub) = sessions();
    let frame = endpoint.seal(b"payload").unwrap();
    hub.open(&frame).unwrap();
    assert!(hub.open(&frame).is_err());

    for position in [0, 8, 46] {
        let (mut endpoint, mut hub) = sessions();
        let original = endpoint.seal(b"payload").unwrap();
        let mut corrupt = original.clone();
        corrupt[position] ^= 1;
        assert!(hub.open(&corrupt).is_err());
        assert!(hub.open(&original).is_err());
    }
}

#[test]
fn fresh_challenge_separates_reconnection_keys() {
    let key = BindingKey::new([42; 32]);
    let old = transcript();
    let mut new = old.clone();
    new.hub_nonce[0] ^= 1;
    let mut old_session = Session::authenticated(
        &key,
        Role::Endpoint,
        &old,
        &proof(&key, Role::Hub, &old).unwrap(),
    )
    .unwrap();
    let mut new_session = Session::authenticated(
        &key,
        Role::Hub,
        &new,
        &proof(&key, Role::Endpoint, &new).unwrap(),
    )
    .unwrap();
    assert!(new_session
        .open(&old_session.seal(b"stale").unwrap())
        .is_err());
}

#[test]
fn bounded_frames_reject_length_before_reading_payload() {
    for n in [0, MAX_FRAME as u32 + 1, u32::MAX] {
        let mut reader = Cursor::new(n.to_be_bytes());
        assert!(read_frame(&mut reader).is_err());
        assert_eq!(reader.position(), 4);
    }
    let mut wire = vec![];
    write_frame(&mut wire, b"valid").unwrap();
    assert_eq!(read_frame(&mut Cursor::new(&wire)).unwrap(), b"valid");
    wire.pop();
    assert!(read_frame(&mut Cursor::new(wire)).is_err());
    assert!(write_frame(&mut vec![], &[]).is_err());
    assert!(write_frame(&mut vec![], &vec![0; MAX_FRAME + 1]).is_err());
}

#[test]
fn strict_requests_reject_duplicate_fields_unknowns_overrides_and_trailing_data() {
    for bytes in [
        r#"{"op":"status","request_id":"a","request_id":"b"}"#,
        r#"{"op":"status","op":"pull","request_id":"a","wait_ms":0}"#,
        r#"{"op":"status","request_id":"a","endpoint_id":"ag_other"}"#,
        r#"{"op":"pull","request_id":"a","wait_ms":251}"#,
        r#"{"op":"pull","request_id":"a","wait_ms":-1}"#,
        r#"{"op":"status","request_id":""}"#,
        r#"{"op":"exec","request_id":"a","command":"bad"}"#,
        r#"{"op":"status","request_id":"a"} {}"#,
    ] {
        assert!(
            Request::parse(bytes.as_bytes()).is_err(),
            "accepted {bytes}"
        );
    }
    assert!(Request::parse(b"\xff").is_err());
    assert!(Request::parse(br#"{"op":"status","request_id":"a"}"#).is_ok());
}

#[test]
fn submit_limits_are_utf8_bytes_and_context_count() {
    let mut value = serde_json::json!({"op":"submit","request_id":"r",
        "delivery_id":"d","message_id":"m","sender":{"kind":"human","id":"h"},
        "body":"hello","context_ids":[],"created_at":"2026-10-17T00:00:00Z"});
    assert!(Request::parse(&serde_json::to_vec(&value).unwrap()).is_ok());
    value["body"] = serde_json::json!("界".repeat(20_001));
    assert!(Request::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    value["body"] = serde_json::json!("ok");
    value["context_ids"] = serde_json::json!(vec!["context"; 33]);
    assert!(Request::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    value["context_ids"] = serde_json::json!([]);
    value["sender"]["token"] = serde_json::json!("synthetic");
    assert!(Request::parse(&serde_json::to_vec(&value).unwrap()).is_err());
}
