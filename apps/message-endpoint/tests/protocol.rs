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
        .fragments("ag_endpoint", 1)
        .unwrap();
    assert!(parts.len() > 1);
    assert!(parts.iter().all(|text| text.len() <= MAX_BODY));
    let first = Fragment::parse(&parts[0]).unwrap();
    let mut assembly = Assembly::new(&first, "ag_endpoint", 1, Direction::Outbound).unwrap();
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
    let parts = outbound("hello".into()).fragments("ag_one", 1).unwrap();
    let first = Fragment::parse(&parts[0]).unwrap();
    let key = first.idempotency_key();
    assert_eq!(
        parts,
        outbound("hello".into()).fragments("ag_one", 1).unwrap()
    );
    let mut assembly = Assembly::new(&first, "ag_one", 1, Direction::Outbound).unwrap();
    assert!(assembly.push(&first).unwrap().is_some());
    assert!(assembly.push(&first).unwrap().is_some());
    let changed = outbound("changed".into()).fragments("ag_one", 1).unwrap();
    let changed = Fragment::parse(&changed[0]).unwrap();
    assert_eq!(changed.idempotency_key(), key);
    assert!(assembly.push(&changed).is_err());
}
#[test]
fn endpoint_generation_direction_and_source_isolate_transfer_keys() {
    let first = outbound("hello".into()).fragments("ag_one", 1).unwrap();
    for (id, generation, source) in [
        ("ag_two", 1, "del_one"),
        ("ag_one", 2, "del_one"),
        ("ag_one", 1, "d2"),
    ] {
        let mut envelope = outbound("hello".into());
        if let Envelope::Outbound { delivery_id, .. } = &mut envelope {
            *delivery_id = source.into();
        }
        let other = envelope.fragments(id, generation).unwrap();
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
    let parts = outbound("hello".into()).fragments("ag_one", 1).unwrap();
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
    let parts = outbound("hello".into()).fragments("ag_one", 1).unwrap();
    let mut frame = Fragment::parse(&parts[0]).unwrap();
    frame.payload_digest = "0".repeat(64);
    assert!(Assembly::new(&frame, "ag_one", 1, frame.direction)
        .unwrap()
        .push(&frame)
        .is_err());
    let mut frame = Fragment::parse(&parts[0]).unwrap();
    frame.direction = Direction::Inbound;
    assert!(Assembly::new(&frame, "ag_one", 1, frame.direction)
        .unwrap()
        .push(&frame)
        .is_err());
}
#[test]
fn oversized_body_and_contexts_fail_before_send() {
    assert!(outbound("a".repeat(MAX_BODY + 1))
        .fragments("ag_one", 1)
        .is_err());
    assert!(outbound("   ".into()).fragments("ag_one", 1).is_err());
    assert!(outbound("hi".into()).fragments("ag_one", 0).is_err());
    let mut envelope = outbound("hi".into());
    if let Envelope::Outbound { context_ids, .. } = &mut envelope {
        *context_ids = vec!["ctx".into(); 33];
    }
    assert!(envelope.fragments("ag_one", 1).is_err());
}

#[test]
fn receiver_recomputes_transfer_identity_and_checks_local_pins() {
    let parts = outbound("hello".into()).fragments("ag_one", 1).unwrap();
    let mut first = Fragment::parse(&parts[0]).unwrap();
    assert!(Assembly::new(&first, "ag_one", 2, Direction::Outbound).is_err());
    assert!(Assembly::new(&first, "ag_one", 1, Direction::Inbound).is_err());
    let mut wrong_endpoint = Assembly::new(&first, "ag_other", 1, Direction::Outbound).unwrap();
    assert!(wrong_endpoint.push(&first).is_err());
    first.transfer_id = "0".repeat(64);
    assert!(Assembly::new(&first, "ag_one", 1, Direction::Outbound)
        .unwrap()
        .push(&first)
        .is_err());
}

#[test]
fn internally_tagged_envelope_rejects_duplicate_missing_and_unknown_fields() {
    let good = serde_json::to_string(&outbound("hello".into())).unwrap();
    assert!(serde_json::from_str::<Envelope>(&good).is_ok());
    for bad in [
        good.replacen('{', "{\"kind\":\"outbound\",", 1),
        good.replace("\"kind\":\"outbound\",", ""),
        good.replace("\"kind\":\"outbound\"", "\"kind\":\"execute\""),
        good.replacen('{', "{\"command\":\"do not execute\",", 1),
        good.replacen('{', "{\"body\":\"duplicate\",", 1),
    ] {
        assert!(serde_json::from_str::<Envelope>(&bad).is_err());
    }
}

#[test]
fn failure_notice_rejects_free_text_provenance_unknown_codes_and_unbounded_fields() {
    let base = serde_json::json!({"kind":"failure_notice","job_id":"failure_synthetic","recipient":"ag_target","transfer_id":"a".repeat(64),"code":"transport_rejected","attempts":2});
    let notice: Envelope = serde_json::from_value(base.clone()).unwrap();
    notice.validate().unwrap();
    for field in ["body", "source", "context_ids", "command"] {
        let mut value = base.clone();
        value[field] = serde_json::json!("untrusted");
        assert!(serde_json::from_value::<Envelope>(value).is_err());
    }
    for (key, value) in [
        ("code", serde_json::json!("custom_error")),
        ("attempts", serde_json::json!(-1)),
        ("attempts", serde_json::json!(65536)),
    ] {
        let mut bad = base.clone();
        bad[key] = value;
        assert!(serde_json::from_value::<Envelope>(bad).is_err());
    }
    for (key, value) in [
        ("transfer_id", serde_json::json!("not-a-digest")),
        ("attempts", serde_json::json!(257)),
        ("recipient", serde_json::json!("")),
    ] {
        let mut bad = base.clone();
        bad[key] = value;
        assert!(serde_json::from_value::<Envelope>(bad)
            .unwrap()
            .validate()
            .is_err());
    }
}
