//! Bounded private-state JSON decoding. Reject ambiguity before recovery uses it.
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 500_000;

pub fn decode(bytes: &[u8]) -> Result<Value, ()> {
    decode_with_limits(bytes, MAX_DEPTH, MAX_NODES)
}

/// Apply the same shape budget before publication, without a second JSON tree.
/// The writer must never publish a checkpoint its own reader would refuse.
pub fn admit(value: &Value) -> Result<(), ()> {
    fn walk(value: &Value, depth: usize, remaining: &mut usize) -> Result<(), ()> {
        if depth == 0 || *remaining == 0 {
            return Err(());
        }
        *remaining -= 1;
        match value {
            Value::Array(values) => {
                for v in values {
                    walk(v, depth - 1, remaining)?;
                }
            }
            Value::Object(values) => {
                for v in values.values() {
                    *remaining = remaining.checked_sub(1).ok_or(())?;
                    walk(v, depth - 1, remaining)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut remaining = MAX_NODES;
    walk(value, MAX_DEPTH, &mut remaining)
}

fn decode_with_limits(bytes: &[u8], depth: usize, nodes: usize) -> Result<Value, ()> {
    let mut remaining = nodes;
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let value = Seed {
        remaining: &mut remaining,
        depth,
    }
    .deserialize(&mut parser)
    .map_err(|_| ())?;
    parser.end().map_err(|_| ())?;
    Ok(value)
}

struct Seed<'a> {
    remaining: &'a mut usize,
    depth: usize,
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.depth == 0 || *self.remaining == 0 {
            return Err(de::Error::custom("private_state_json_limit"));
        }
        *self.remaining -= 1;
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded unambiguous JSON")
    }
    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| de::Error::custom("private_state_json_number"))
    }
    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(value.into())
    }
    fn visit_string<E: de::Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = array.next_element_seed(Seed {
            remaining: self.remaining,
            depth: self.depth - 1,
        })? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            // Count keys as nodes too. Never echo an untrusted key in an error.
            if *self.remaining == 0 {
                return Err(de::Error::custom("private_state_json_limit"));
            }
            *self.remaining -= 1;
            if values.contains_key(&key) {
                return Err(de::Error::custom("private_state_json_duplicate_key"));
            }
            let value = object.next_value_seed(Seed {
                remaining: self.remaining,
                depth: self.depth - 1,
            })?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_keys_including_escaped_equivalents_are_refused() {
        for bytes in [
            br#"{"dispatchBlocked":true,"dispatchBlocked":false}"#.as_slice(),
            br#"{"records":[{"receipt":{"id":"first","\u0069d":"second"}}]}"#,
            br#"{"goals":{"applying":true,"applying":false}}"#,
        ] {
            assert_eq!(decode(bytes), Err(()));
        }
        assert!(decode(br#"{"a":{"id":1},"b":{"id":2}}"#).is_ok());
    }
    #[test]
    fn counts_keys_values_and_container_depth_before_acceptance() {
        assert!(decode_with_limits(b"{\"a\":[null]}", 3, 4).is_ok());
        assert_eq!(decode_with_limits(b"{\"a\":[null]}", 2, 4), Err(()));
        assert_eq!(decode_with_limits(b"{\"a\":[null]}", 3, 3), Err(()));
        let nested = format!("{}null{}", "[".repeat(32), "]".repeat(32));
        assert_eq!(decode(nested.as_bytes()), Err(()));
        let wide = format!("[{}]", "null,".repeat(MAX_NODES) + "null");
        assert_eq!(decode(wide.as_bytes()), Err(()));
        assert_eq!(admit(&Value::Array(vec![Value::Null; MAX_NODES])), Err(()));
    }
    #[test]
    fn malformed_input_has_no_content_bearing_error() {
        for bytes in [
            b"{\"credential-canary\":NaN}".as_slice(),
            b"\"\xff\"",
            b"{}[]",
            b"[1e999]",
        ] {
            assert_eq!(decode(bytes), Err(()));
        }
    }
    #[test]
    fn ordinary_journal_and_unicode_values_round_trip() {
        let value = serde_json::json!({"schema":1,"threadId":"fixture","dispatchBlocked":true,
            "records":[{"text":"synthetic 文","receipt":{"id":"r","position":null}}],
            "goals":{"goal":null,"edits":[]},"numbers":[-2,3,0.5]});
        assert_eq!(decode(&serde_json::to_vec(&value).unwrap()), Ok(value));
    }
}
