//! Canonical identity for the future mismatch ledger, not ACK authorization.
//! Durable parent/current-binding checks still belong in the writer transaction.
use crate::{
    core::AuthenticatedDelivery,
    protocol::SenderKind,
    store::{valid_id, Error},
    telegram_receipts::{Expected, Receipt},
};
use sha2::{Digest, Sha256};

/// Opaque, non-serializable and deliberately not Debug: hashes are not status.
pub struct Identity {
    delivery_key: [u8; 32],
    receipt_digest: [u8; 32],
    job: i64,
    endpoint: String,
    generation: i64,
    recipient: String,
}

fn framed_hash(domain: &[u8], fields: &[&[u8]]) -> [u8; 32] {
    let mut hash = Sha256::new();
    for field in std::iter::once(domain).chain(fields.iter().copied()) {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    hash.finalize().into()
}

impl Identity {
    /// `expected` and `job` must come from the authoritative durable handoff,
    /// workspace/hub from durable hub identity, never from the receipt itself.
    /// This repeats strict parsing; callers cannot supply an unchecked Receipt.
    pub fn from_authenticated(
        authenticated: &AuthenticatedDelivery,
        workspace: &str,
        hub: &str,
        job: i64,
        expected: Expected<'_>,
    ) -> Result<Self, Error> {
        let delivery = authenticated.delivery();
        if job <= 0
            || !valid_id(workspace)
            || !valid_id(hub)
            || authenticated.workspace() != workspace
            || delivery.recipient.kind != SenderKind::Agent
            || delivery.recipient.id != hub
            || delivery.message.sender.kind != SenderKind::Agent
            || !valid_id(&delivery.delivery_id)
            || !valid_id(&delivery.message.message_id)
        {
            return Err(Error::Invalid);
        }
        let receipt = Receipt::parse_for(
            delivery.message.body.as_bytes(),
            &delivery.message.sender.id,
            expected,
        )
        .map_err(|_| Error::Invalid)?;
        // This v1 persisted encoding is pinned by an independent golden vector.
        // Receipt/Scope field or serde/domain changes require a ledger migration.
        // Serialization of this fixed typed model is canonical for our fields:
        // input whitespace/object order disappear, all validated fields remain.
        // Only the fixed digest is retained; these transient bytes are not logged.
        let canonical = serde_json::to_vec(&receipt).map_err(|_| Error::Invalid)?;
        Ok(Self {
            delivery_key: framed_hash(
                b"treer-terminal-disposition-delivery-v1",
                &[
                    workspace.as_bytes(),
                    hub.as_bytes(),
                    delivery.delivery_id.as_bytes(),
                ],
            ),
            receipt_digest: framed_hash(
                b"treer-terminal-disposition-receipt-v1",
                &[
                    delivery.message.message_id.as_bytes(),
                    &job.to_be_bytes(),
                    &canonical,
                ],
            ),
            job,
            endpoint: receipt.scope.endpoint_id,
            generation: receipt.scope.generation,
            recipient: receipt.scope.recipient_id,
        })
    }

    /// Future writer must compare this complete tuple, not just the hash key.
    /// No caller receives an ACK capability from this storage projection.
    pub fn parent(&self) -> (i64, &str, i64, &str) {
        (self.job, &self.endpoint, self.generation, &self.recipient)
    }

    /// Fixed-width storage projection only; do not expose through logs/status.
    pub fn storage_hashes(&self) -> (&[u8; 32], &[u8; 32]) {
        (&self.delivery_key, &self.receipt_digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        core::{Delivery, Message, Principal},
        telegram_receipts::Scope,
    };
    use serde_json::{json, Value};

    fn wire() -> Value {
        json!({"protocol":crate::telegram_receipts::CAPABILITY,"verification":"app_asserted",
            "scope":{"account_id":"bot_test","chat_id":-100,"topic_id":7,"endpoint_id":"ag_endpoint","generation":1,"recipient_id":"ag_recipient"},
            "job_id":"job_test","payload_digest":"a".repeat(64),"native_message_id":2,"final_core_message_id":"msg_final"})
    }
    fn delivery(
        body: String,
        workspace: &str,
        hub: &str,
        key: &str,
        message: &str,
        sender: &str,
    ) -> AuthenticatedDelivery {
        AuthenticatedDelivery::fixture(
            Delivery {
                delivery_id: key.into(),
                recipient: Principal {
                    kind: SenderKind::Agent,
                    id: hub.into(),
                },
                message: Message {
                    message_id: message.into(),
                    sender: Principal {
                        kind: SenderKind::Agent,
                        id: sender.into(),
                    },
                    body,
                    context_ids: vec![],
                },
            },
            workspace,
        )
    }
    fn identity(
        value: &Value,
        workspace: &str,
        hub: &str,
        key: &str,
        message: &str,
        job: i64,
    ) -> Identity {
        let scope: Scope = serde_json::from_value(value["scope"].clone()).unwrap();
        let authenticated = delivery(
            value.to_string(),
            workspace,
            hub,
            key,
            message,
            &scope.endpoint_id,
        );
        Identity::from_authenticated(
            &authenticated,
            workspace,
            hub,
            job,
            Expected {
                scope: &scope,
                job_id: value["job_id"].as_str().unwrap(),
                payload_digest: value["payload_digest"].as_str().unwrap(),
                native_message_id: value["native_message_id"].as_i64().unwrap(),
            },
        )
        .unwrap()
    }

    #[test]
    fn v1_storage_hashes_match_independent_golden_vector() {
        // Calculated independently with Python hashlib + 8-byte big-endian
        // length framing + compact ordered JSON, not this module's hash helper.
        let identity = identity(&wire(), "ws_one", "ag_hub", "del_one", "msg_receipt", 1);
        let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(
            hex(&identity.delivery_key),
            "5f6eabf919fc0fea30b3972cca57cdd6de73e634e8a7c32122eacaf2ba6cb0d4"
        );
        assert_eq!(
            hex(&identity.receipt_digest),
            "3886c1666a3d814c2d1ff41ada01a620705e799cab3a59cf0f534a0e884c91bf"
        );
    }

    #[test]
    fn delivery_key_is_scoped_and_length_framed() {
        let v = wire();
        let baseline = identity(&v, "ws_one", "ag_hub", "del_one", "msg_receipt", 1);
        for (ws, hub, key) in [
            ("ws_two", "ag_hub", "del_one"),
            ("ws_one", "ag_other", "del_one"),
            ("ws_one", "ag_hub", "del_two"),
        ] {
            assert_ne!(
                baseline.delivery_key,
                identity(&v, ws, hub, key, "msg_receipt", 1).delivery_key
            );
        }
        assert_ne!(
            framed_hash(b"d", &[b"ab", b"c"]),
            framed_hash(b"d", &[b"a", b"bc"])
        );
        assert_ne!(framed_hash(b"d", &[b"a"]), framed_hash(b"other", &[b"a"]));
    }

    #[test]
    fn digest_binds_each_receipt_field_outer_message_and_durable_parent() {
        let baseline = identity(&wire(), "ws_one", "ag_hub", "del_one", "msg_receipt", 1);
        for field in [
            "account_id",
            "chat_id",
            "topic_id",
            "endpoint_id",
            "generation",
            "recipient_id",
            "job_id",
            "payload_digest",
            "native_message_id",
            "final_core_message_id",
        ] {
            let mut v = wire();
            match field {
                "account_id" => v["scope"][field] = json!("bot_other"),
                "chat_id" => v["scope"][field] = json!(-200),
                "topic_id" => v["scope"][field] = Value::Null,
                "endpoint_id" => v["scope"][field] = json!("ag_other"),
                "generation" => v["scope"][field] = json!(2),
                "recipient_id" => v["scope"][field] = json!("ag_other"),
                "job_id" => v[field] = json!("job_other"),
                "payload_digest" => v[field] = json!("b".repeat(64)),
                "native_message_id" => v[field] = json!(3),
                "final_core_message_id" => v[field] = json!("msg_other"),
                _ => unreachable!(),
            }
            let changed = identity(&v, "ws_one", "ag_hub", "del_one", "msg_receipt", 1);
            assert_eq!(baseline.delivery_key, changed.delivery_key);
            assert_ne!(baseline.receipt_digest, changed.receipt_digest, "{field}");
        }
        for (message, job) in [("msg_other", 1), ("msg_receipt", 2)] {
            assert_ne!(
                baseline.receipt_digest,
                identity(&wire(), "ws_one", "ag_hub", "del_one", message, job).receipt_digest
            );
        }
        assert_eq!(baseline.parent(), (1, "ag_endpoint", 1, "ag_recipient"));
        assert_eq!(
            baseline.storage_hashes(),
            (&baseline.delivery_key, &baseline.receipt_digest)
        );
    }

    #[test]
    fn canonicalization_ignores_formatting_but_strict_validation_still_applies() {
        let v = wire();
        let scope: Scope = serde_json::from_value(v["scope"].clone()).unwrap();
        let parse = |d: &AuthenticatedDelivery, workspace, hub, job| {
            Identity::from_authenticated(
                d,
                workspace,
                hub,
                job,
                Expected {
                    scope: &scope,
                    job_id: "job_test",
                    payload_digest: v["payload_digest"].as_str().unwrap(),
                    native_message_id: 2,
                },
            )
        };
        let good = delivery(
            serde_json::to_string_pretty(&v).unwrap(),
            "ws_one",
            "ag_hub",
            "del_one",
            "msg_receipt",
            "ag_endpoint",
        );
        let baseline = identity(&v, "ws_one", "ag_hub", "del_one", "msg_receipt", 1);
        assert_eq!(
            parse(&good, "ws_one", "ag_hub", 1).unwrap().receipt_digest,
            baseline.receipt_digest
        );
        assert!(parse(&good, "ws_other", "ag_hub", 1).is_err());
        assert!(parse(&good, "ws_one", "ag_other", 1).is_err());
        assert!(parse(&good, "ws_one", "ag_hub", 0).is_err());
        let bad_sender = delivery(
            v.to_string(),
            "ws_one",
            "ag_hub",
            "del_one",
            "msg_receipt",
            "ag_other",
        );
        assert!(parse(&bad_sender, "ws_one", "ag_hub", 1).is_err());
        let mut unknown = v.clone();
        unknown["unknown"] = json!(true);
        let bad_wire = delivery(
            unknown.to_string(),
            "ws_one",
            "ag_hub",
            "del_one",
            "msg_receipt",
            "ag_endpoint",
        );
        assert!(parse(&bad_wire, "ws_one", "ag_hub", 1).is_err());
        let mut bad_scope = v.clone();
        bad_scope["scope"]["chat_id"] = json!(-200);
        let bad = delivery(
            bad_scope.to_string(),
            "ws_one",
            "ag_hub",
            "del_one",
            "msg_receipt",
            "ag_endpoint",
        );
        assert!(parse(&bad, "ws_one", "ag_hub", 1).is_err());
    }
}
