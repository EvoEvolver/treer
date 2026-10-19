//! Stage-2 receipt contract only; not dispatched by the runtime yet.
//! A pinned relay's assertion can locate a reply in one binding. It never proves
//! Core acceptance, readership, historical access or isolation.
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const CAPABILITY: &str = "treer.telegram.app-asserted-receipts.v1";
const MAX_RECEIPT: usize = 4096;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub account_id: String,
    pub chat_id: i64,
    pub topic_id: Option<i64>,
    pub endpoint_id: String,
    pub generation: i64,
    pub recipient_id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    AppAsserted,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub protocol: String,
    pub verification: Verification,
    pub scope: Scope,
    pub job_id: String,
    pub payload_digest: String,
    pub native_message_id: i64,
    pub final_core_message_id: String,
}

/// Constructed from the current binding and durable handoff, never from a
/// receipt's own body. The sender comes from the outer Core envelope.
pub struct Expected<'a> {
    pub scope: &'a Scope,
    pub job_id: &'a str,
    pub payload_digest: &'a str,
    pub native_message_id: i64,
}

#[derive(Debug, Error)]
#[error("telegram_receipt_rejected")]
pub struct Rejected;

fn identifier(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix.len() <= 128
            && suffix
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    })
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl Scope {
    fn valid(&self) -> bool {
        identifier(&self.account_id, "")
            && self.chat_id != 0
            && self.topic_id.is_none_or(|id| id > 0)
            && identifier(&self.endpoint_id, "ag_")
            && identifier(&self.recipient_id, "ag_")
            && self.generation > 0
    }
}
impl Receipt {
    /// Does not refetch a Core message or grant access to its context. A caller
    /// must separately let Core enforce the sending relay's context visibility.
    pub fn parse_for(
        wire: &[u8],
        core_sender_id: &str,
        expected: Expected<'_>,
    ) -> Result<Self, Rejected> {
        if wire.len() > MAX_RECEIPT {
            return Err(Rejected);
        }
        let receipt: Self = serde_json::from_slice(wire).map_err(|_| Rejected)?;
        if receipt.protocol != CAPABILITY
            || !expected.scope.valid()
            || receipt.scope != *expected.scope
            || core_sender_id != expected.scope.endpoint_id
            || !identifier(expected.job_id, "")
            || receipt.job_id != expected.job_id
            || !digest(expected.payload_digest)
            || receipt.payload_digest != expected.payload_digest
            || expected.native_message_id <= 0
            || receipt.native_message_id != expected.native_message_id
            || !identifier(&receipt.final_core_message_id, "msg_")
        {
            return Err(Rejected);
        }
        Ok(receipt)
    }

    pub fn verification_label(&self) -> &'static str {
        "not independently verified"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn scope() -> Scope {
        Scope {
            account_id: "synthetic_account".into(),
            chat_id: -123,
            topic_id: Some(7),
            endpoint_id: "ag_relay".into(),
            generation: 2,
            recipient_id: "ag_recipient".into(),
        }
    }
    fn wire() -> Value {
        json!({"protocol":CAPABILITY,"verification":"app_asserted","scope":scope(),
            "job_id":"job_one","payload_digest":"a".repeat(64),
            "native_message_id":123,"final_core_message_id":"msg_final"})
    }
    fn parse(wire: Value, sender: &str) -> Result<Receipt, Rejected> {
        Receipt::parse_for(
            &serde_json::to_vec(&wire).unwrap(),
            sender,
            Expected {
                scope: &scope(),
                job_id: "job_one",
                payload_digest: &"a".repeat(64),
                native_message_id: 123,
            },
        )
    }
    #[test]
    fn valid_assertion_is_not_independent_proof() {
        let receipt = parse(wire(), "ag_relay").unwrap();
        assert_eq!(receipt.verification_label(), "not independently verified");
        assert_eq!(receipt.final_core_message_id, "msg_final");
    }
    #[test]
    fn cross_binding_and_source_confusion_rejected() {
        for (key, value) in [
            ("account_id", json!("other_account")),
            ("chat_id", json!(-124)),
            ("topic_id", json!(8)),
            ("topic_id", Value::Null),
            ("endpoint_id", json!("ag_other")),
            ("generation", json!(3)),
            ("recipient_id", json!("ag_other")),
        ] {
            let mut body = wire();
            body["scope"][key] = value;
            assert!(parse(body, "ag_relay").is_err(), "{key}");
        }
        assert!(parse(wire(), "ag_forged_sender").is_err());
    }
    #[test]
    fn handoff_conflicts_and_verification_upgrade_are_rejected() {
        for (key, value) in [
            ("job_id", json!("job_other")),
            ("payload_digest", json!("b".repeat(64))),
            ("native_message_id", json!(124)),
            ("final_core_message_id", json!("ag_not_a_message")),
            ("verification", json!("core_verified")),
            ("protocol", json!("treer.telegram.app-asserted-receipts.v2")),
            ("extra", json!("SYNTHETIC_UNTRUSTED")),
        ] {
            let mut body = wire();
            body[key] = value;
            assert!(parse(body, "ag_relay").is_err(), "{key}");
        }
    }
    #[test]
    fn malformed_duplicate_and_oversized_wire_rejected_with_fixed_error() {
        let expected_scope = scope();
        let hash = "a".repeat(64);
        let run = |wire: &[u8]| {
            Receipt::parse_for(
                wire,
                "ag_relay",
                Expected {
                    scope: &expected_scope,
                    job_id: "job_one",
                    payload_digest: &hash,
                    native_message_id: 123,
                },
            )
        };
        let duplicate = serde_json::to_string(&wire()).unwrap().replacen(
            "{",
            r#"{"verification":"app_asserted","#,
            1,
        );
        for body in [
            b"not-json".as_slice(),
            duplicate.as_bytes(),
            &vec![b' '; MAX_RECEIPT + 1],
        ] {
            assert_eq!(
                run(body).err().unwrap().to_string(),
                "telegram_receipt_rejected"
            );
        }
    }
}
