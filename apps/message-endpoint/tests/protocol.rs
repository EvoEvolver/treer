use treer_message_endpoint::protocol::*;
fn outbound(body: String) -> Envelope {
    Envelope::Outbound {
        delivery_id: "del_one".into(),
        message_id: "msg_one".into(),
        original_sender: Sender {
            kind: SenderKind::Human,
            id: "human".into(),
        },
        body,
        context_ids: vec![],
    }
}
#[test]
fn maximal_escaped_body_fragments_fit_core_and_reassemble_out_of_order() {
    let original = "\u{0001}".repeat(MAX_BODY);
    let parts = outbound(original.clone())
        .fragments("ag_endpoint", 1, "del_one")
        .unwrap();
    assert!(parts.len() > 1);
    assert!(parts.iter().all(|text| text.len() <= MAX_BODY));
    let first = Fragment::parse(&parts[0]).unwrap();
    let mut assembly = Assembly::new(&first);
    let mut complete = None;
    for part in parts.iter().rev() {
        complete = assembly.push(&Fragment::parse(part).unwrap()).unwrap();
    }
    match complete.unwrap() {
        Envelope::Outbound { body, .. } => assert_eq!(body, original),
        _ => panic!("wrong direction"),
    }
}
#[test]
fn fragment_retry_same_key_conflicting_payload_rejected() {
    let parts = outbound("hello".into())
        .fragments("ag_one", 1, "del_one")
        .unwrap();
    let first = Fragment::parse(&parts[0]).unwrap();
    let key = first.idempotency_key();
    assert_eq!(
        parts,
        outbound("hello".into())
            .fragments("ag_one", 1, "del_one")
            .unwrap()
    );
    let mut assembly = Assembly::new(&first);
    assert!(assembly.push(&first).unwrap().is_some());
    assert!(assembly.push(&first).unwrap().is_some());
    let changed = outbound("changed".into())
        .fragments("ag_one", 1, "del_one")
        .unwrap();
    let changed = Fragment::parse(&changed[0]).unwrap();
    assert_eq!(changed.idempotency_key(), key);
    assert!(assembly.push(&changed).is_err());
}
#[test]
fn endpoint_generation_direction_and_source_isolate_transfer_keys() {
    let first = outbound("hello".into())
        .fragments("ag_one", 1, "d1")
        .unwrap();
    for (id, generation, source) in [
        ("ag_two", 1, "d1"),
        ("ag_one", 2, "d1"),
        ("ag_one", 1, "d2"),
    ] {
        let other = outbound("hello".into())
            .fragments(id, generation, source)
            .unwrap();
        assert_ne!(
            Fragment::parse(&first[0]).unwrap().idempotency_key(),
            Fragment::parse(&other[0]).unwrap().idempotency_key()
        );
    }
}
#[test]
fn forged_body_never_selects_inbound_path() {
    assert!(!from_hub(
        &Sender {
            kind: SenderKind::Agent,
            id: "ag_attacker".into()
        },
        "ag_hub"
    ));
    assert!(!from_hub(
        &Sender {
            kind: SenderKind::Human,
            id: "ag_hub".into()
        },
        "ag_hub"
    ));
    assert!(from_hub(
        &Sender {
            kind: SenderKind::Agent,
            id: "ag_hub".into()
        },
        "ag_hub"
    ));
}
#[test]
fn local_recipient_pin_is_independent_of_hub_envelope() {
    let mut envelope = Envelope::Deliver {
        job_id: "job1".into(),
        recipient: "ag_allowed".into(),
        body: "hello".into(),
        context_ids: vec![],
        source: ExternalSource {
            channel: Channel::Telegram,
            account_id: "bot".into(),
            conversation_id: "chat".into(),
            message_id: "external".into(),
        },
    };
    check_recipient(&envelope, &["ag_allowed".into()]).unwrap();
    if let Envelope::Deliver { recipient, .. } = &mut envelope {
        *recipient = "ag_other".into();
    }
    assert!(check_recipient(&envelope, &["ag_allowed".into()]).is_err());
}
#[test]
fn strict_fragment_json_size_base64_and_unknown_fields() {
    let parts = outbound("hello".into())
        .fragments("ag_one", 1, "d1")
        .unwrap();
    let text = &parts[0];
    let duplicate = text.replacen('{', "{\"part\":0,", 1);
    assert!(Fragment::parse(&duplicate).is_err());
    let unknown = text.replacen('{', "{\"hub_id\":\"ag_fake\",", 1);
    assert!(Fragment::parse(&unknown).is_err());
    assert!(Fragment::parse(&(text.to_owned() + " {} ")).is_err());
    assert!(Fragment::parse(&"x".repeat(MAX_BODY + 1)).is_err());
    let mut frame = Fragment::parse(text).unwrap();
    frame.data = "!not_base64".into();
    assert!(Fragment::parse(&serde_json::to_string(&frame).unwrap()).is_err());
}
#[test]
fn fragment_corruption_and_direction_mismatch_rejected() {
    let parts = outbound("hello".into())
        .fragments("ag_one", 1, "d1")
        .unwrap();
    let mut frame = Fragment::parse(&parts[0]).unwrap();
    frame.payload_digest = "0".repeat(64);
    assert!(Assembly::new(&frame).push(&frame).is_err());
    let mut frame = Fragment::parse(&parts[0]).unwrap();
    frame.direction = Direction::Inbound;
    assert!(Assembly::new(&frame).push(&frame).is_err());
}
#[test]
fn oversized_body_and_contexts_fail_before_send() {
    assert!(outbound("a".repeat(MAX_BODY + 1))
        .fragments("ag_one", 1, "d")
        .is_err());
    assert!(outbound("   ".into()).fragments("ag_one", 1, "d").is_err());
    assert!(outbound("hi".into()).fragments("ag_one", 0, "d").is_err());
    let mut envelope = outbound("hi".into());
    if let Envelope::Outbound { context_ids, .. } = &mut envelope {
        *context_ids = vec!["ctx".into(); 33];
    }
    assert!(envelope.fragments("ag_one", 1, "d").is_err());
}
