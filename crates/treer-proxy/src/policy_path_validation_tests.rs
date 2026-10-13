use super::*;
use serde_json::{json, Value};

fn value() -> Value {
    json!({"schema_version":2,"defaults":{},"rules":[{"id":"r","priority":0,"effect":"allow","subjects":[{"kind":"agent","tree":{"path_glob":"/team/*"}}],"actions":["agent.prompt"],"resources":[{}],"bindings":{"home":{"ref":"subject.tree.folder"}},"conditions":[{"op":"resource_below_folder","folder":{"var":"home"}}]}]})
}
fn validate(value: Value) -> Result<(), PolicyStoreError> {
    validate_document(&serde_json::from_value(value).unwrap())
}

#[test]
fn v2_contract_and_v1_compatibility_are_strict() {
    validate(value()).unwrap();
    let mut v = value();
    v["schema_version"] = json!(1);
    assert!(validate(v).is_err());
    let mut v = value();
    v["schema_version"] = json!(3);
    assert!(validate(v).is_err());
    for field in ["tree_context", "unknown"] {
        let mut v = value();
        v["rules"][0][field] = json!({});
        assert!(serde_json::from_value::<WorkspacePolicyDocument>(v).is_err());
    }
    let mut v = value();
    v["rules"][0]["bindings"]["home"] = json!({"var":"home"});
    assert!(validate(v).is_err());
    let mut v = value();
    v["rules"][0]["conditions"][0]["folder"] = json!({"var":"missing"});
    assert!(validate(v).is_err());
    let mut v = value();
    v["rules"][0]["conditions"][0]["folder"] = json!({"var":"home","path":"/"});
    assert!(serde_json::from_value::<WorkspacePolicyDocument>(v).is_err());
}

#[test]
fn path_shapes_and_relation_limits_are_enforced() {
    for path in [
        "relative",
        "/team/../other",
        "/team//other",
        "/team/",
        "/team/x*",
        "/team/**/x",
        "/team/?",
    ] {
        let mut v = value();
        v["rules"][0]["subjects"][0]["tree"]["path_glob"] = json!(path);
        assert!(validate(v).is_err(), "{path}");
    }
    let mut v = value();
    v["rules"][0]["subjects"][0]["tree"]["path_eq"] = json!("/team/a");
    assert!(validate(v).is_err());
    let mut v = value();
    v["rules"][0]["conditions"] = json!(vec![json!({"op":"same_folder"}); 33]);
    assert!(validate(v).is_err());
    let mut v = value();
    for n in 0..9 {
        v["rules"][0]["bindings"][format!("v{n}")] = json!({"path":"/"});
    }
    assert!(validate(v).is_err());
    let mut v = value();
    v["rules"][0]["subjects"][0]["tree"]["path_glob"] = json!(format!("/{}", "x".repeat(4097)));
    assert!(validate(v).is_err());
}

#[test]
fn serialized_bytes_and_rule_count_stay_bounded() {
    let mut v = value();
    let r = v["rules"][0].clone();
    v["rules"] = Value::Array(
        (0..1001)
            .map(|i| {
                let mut r = r.clone();
                r["id"] = json!(format!("r{i}"));
                r
            })
            .collect(),
    );
    assert!(validate(v).is_err());
    let mut v = value();
    v["defaults"]["x".repeat(MAX_POLICY_DOCUMENT_BYTES)] = json!("deny");
    assert!(validate(v).is_err());
}
