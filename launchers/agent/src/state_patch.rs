//! WebSocket `patch` frames: a state encoded against the frame the same socket
//! sent before it. A working Agent publishes up to 10 states per second and a
//! state page can be several MB, nearly all of it turns that did not change.
//!
//! - top-level fields equal to the previous frame are named in `same`;
//! - inside `thread`, a turn equal to the previous turn with that id is sent as
//!   its id string, and inside a changed turn an unchanged item likewise;
//! - everything else is sent whole. Ids resolve against the first previous
//!   turn/item with that id, so duplicate ids decode the same on both sides.
use serde_json::{json, Map, Value};
use std::collections::HashMap;

fn first_by_id(values: &[Value]) -> HashMap<&str, &Value> {
    let mut map = HashMap::new();
    for value in values {
        if let Some(id) = value["id"].as_str() {
            map.entry(id).or_insert(value);
        }
    }
    map
}

fn referenced(next: &[Value], previous: &[Value], inner: Option<&str>) -> Vec<Value> {
    let before = first_by_id(previous);
    next.iter()
        .map(|value| {
            let Some(old) = value["id"].as_str().and_then(|id| before.get(id)) else {
                return value.clone();
            };
            if *old == value {
                return value["id"].clone();
            }
            match (inner, value.as_object()) {
                (Some(key), Some(object)) => match (value[key].as_array(), old[key].as_array()) {
                    (Some(next), Some(previous)) => {
                        let mut object = object.clone();
                        object.insert(key.into(), Value::Array(referenced(next, previous, None)));
                        Value::Object(object)
                    }
                    _ => value.clone(),
                },
                _ => value.clone(),
            }
        })
        .collect()
}

fn thread(next: &Value, previous: &Value) -> Value {
    let (Some(object), Some(turns), Some(old)) = (
        next.as_object(),
        next["turns"].as_array(),
        previous["turns"].as_array(),
    ) else {
        return next.clone();
    };
    if next["id"] != previous["id"] {
        return next.clone();
    }
    let mut object = object.clone();
    object.insert(
        "turns".into(),
        Value::Array(referenced(turns, old, Some("items"))),
    );
    Value::Object(object)
}

/// `previous` is the state (without `type`) this socket sent last.
pub fn encode(next: &Value, previous: &Value) -> Value {
    let (Some(fields), Some(before)) = (next.as_object(), previous.as_object()) else {
        let mut value = next.clone();
        value["type"] = json!("state");
        return value;
    };
    let mut out = Map::new();
    let mut same = Vec::new();
    for (key, value) in fields {
        match before.get(key) {
            Some(old) if old == value => same.push(json!(key)),
            Some(old) if key == "thread" => {
                out.insert(key.clone(), thread(value, old));
            }
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    out.insert("type".into(), json!("patch"));
    out.insert("same".into(), Value::Array(same));
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors ui/src/state/patch.ts so the encoding is checked end to end here.
    fn decode(patch: &Value, base: &Value) -> Value {
        fn resolve(values: &Value, previous: &Value, inner: Option<&str>) -> Value {
            let old: Vec<Value> = previous.as_array().cloned().unwrap_or_default();
            let before = first_by_id(&old);
            Value::Array(
                values
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| match value.as_str() {
                        Some(id) => (*before.get(id).expect("reference")).clone(),
                        None => {
                            let mut value = value.clone();
                            if let (Some(key), Some(id)) = (inner, value["id"].as_str()) {
                                if let Some(old) = before.get(id) {
                                    value[key] = resolve(&value[key], &old[key], None);
                                }
                            }
                            value
                        }
                    })
                    .collect(),
            )
        }
        let mut out = Map::new();
        for key in patch["same"].as_array().unwrap() {
            let key = key.as_str().unwrap();
            out.insert(key.into(), base[key].clone());
        }
        for (key, value) in patch.as_object().unwrap() {
            if key == "type" || key == "same" {
                continue;
            }
            let mut value = value.clone();
            if key == "thread" && value.is_object() && base[key]["id"] == value["id"] {
                value["turns"] = resolve(&value["turns"], &base[key]["turns"], Some("items"));
            }
            out.insert(key.clone(), value);
        }
        Value::Object(out)
    }

    fn state(turns: Value) -> Value {
        json!({"ready":true,"modelOptions":[{"id":"m"}],"history":{"limit":50},
            "thread":{"id":"t","status":"running","turns":turns}})
    }

    #[test]
    fn unchanged_turns_and_items_travel_as_ids() {
        let big = "x".repeat(10_000);
        let old = state(json!([
            {"id":"a","items":[{"id":"1","text":big}]},
            {"id":"b","items":[{"id":"2","text":big},{"id":"3","text":"he"}]},
        ]));
        let next = state(json!([
            {"id":"a","items":[{"id":"1","text":big}]},
            {"id":"b","items":[{"id":"2","text":big},{"id":"3","text":"hello"},{"id":"4","text":""}]},
        ]));
        let patch = encode(&next, &old);
        assert_eq!(patch["type"], "patch");
        assert_eq!(patch["same"], json!(["history", "modelOptions", "ready"]));
        assert_eq!(patch["thread"]["turns"][0], "a");
        assert_eq!(patch["thread"]["turns"][1]["items"][0], "2");
        assert!(patch.to_string().len() < 500);
        assert_eq!(decode(&patch, &old), next);
    }

    #[test]
    fn removed_fields_new_threads_and_duplicate_ids_decode_exactly() {
        let old = state(
            json!([{"id":"a","items":[{"id":"1","n":1},{"id":"1","n":2}]},{"id":"a","items":[]},{"items":[]}]),
        );
        let mut next = state(
            json!([{"id":"a","items":[{"id":"1","n":2},{"id":"1","n":1}]},{"items":[]},{"id":"c","items":[{"id":"1"}]}]),
        );
        next.as_object_mut().unwrap().remove("modelOptions");
        assert_eq!(decode(&encode(&next, &old), &old), next);
        let mut other = next.clone();
        other["thread"]["id"] = json!("t2");
        let patch = encode(&other, &next);
        assert_eq!(patch["thread"], other["thread"]);
        assert_eq!(decode(&patch, &next), other);
        let none = json!({"ready":false,"thread":null});
        assert_eq!(decode(&encode(&none, &next), &next), none);
        assert_eq!(decode(&encode(&next, &none), &none), next);
    }
}
