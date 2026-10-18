//! Manifest-only offline validation. This does NOT authenticate a handoff,
//! validate object content/reachability, access files or permit activation.
use crate::bounded_json;
use serde::Deserialize;

pub const MANIFEST_BYTES: usize = 1024 * 1024;
const TOTAL_BYTES: u64 = 384 * 1024 * 1024;
const METADATA_BYTES: u64 = 32 * 1024 * 1024;
const IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Limit,
    Schema,
    Reference,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Limit => "handoff_limit",
            Self::Schema => "handoff_schema_invalid",
            Self::Reference => "handoff_reference_invalid",
        })
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    NodeCodex,
    RustCodex,
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Submissions,
    Goals,
    Images,
    Notifications,
    MessageAcks,
    Image,
}

/// A parsed candidate only. Never use this value as proof of Core authority.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub agent_id: String,
    pub machine_id: String,
    pub handoff_id: String,
    pub source_generation: u64,
    pub destination_generation: u64,
    pub source_backend: Backend,
    pub destination_backend: Backend,
    pub source_schema: String,
    pub destination_schema: String,
    pub thread_id: String,
    pub dispatch_fenced: bool,
    pub objects: Vec<Object>,
    pub submissions: String,
    pub goals: String,
    pub images: String,
    pub notifications: String,
    pub message_acks: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Object {
    pub sha256: String,
    pub bytes: u64,
    pub kind: Kind,
}
fn identity(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Validate only manifest grammar, bounded declarations and metadata references.
/// Callers must independently stream/verify objects, enforce cross-object
/// semantics, obtain Core authorization and refuse activation until all exist.
pub fn validate_manifest(bytes: &[u8]) -> Result<Manifest, Error> {
    if bytes.len() > MANIFEST_BYTES {
        return Err(Error::Limit);
    }
    let value = bounded_json::decode(bytes).map_err(|_| Error::Schema)?;
    let m: Manifest = serde_json::from_value(value).map_err(|_| Error::Schema)?;
    if m.format != "treer.launcher-handoff.v1"
        || m.source_schema != "launcher-envelope-v1"
        || m.destination_schema != "launcher-envelope-v1"
        || !identity(&m.agent_id)
        || !identity(&m.machine_id)
        || uuid::Uuid::parse_str(&m.handoff_id).is_err()
        || uuid::Uuid::parse_str(&m.handoff_id).is_ok_and(|v| v.to_string() != m.handoff_id)
        || m.source_generation == 0
        || m.destination_generation <= m.source_generation
        || m.destination_generation > SAFE_INTEGER
        || m.source_backend == m.destination_backend
        || m.thread_id.is_empty()
        || m.thread_id.len() > 256
        || m.thread_id.chars().any(char::is_control)
    {
        return Err(Error::Schema);
    }
    if m.objects.len() > 8_200 {
        return Err(Error::Limit);
    }
    let mut total = 0_u64;
    let mut previous = "";
    let mut images = 0;
    for object in &m.objects {
        if !digest(&object.sha256) || object.sha256.as_str() <= previous {
            return Err(Error::Reference);
        }
        previous = &object.sha256;
        let cap = if object.kind == Kind::Image {
            IMAGE_BYTES
        } else {
            METADATA_BYTES
        };
        if object.bytes == 0 || object.bytes > cap {
            return Err(Error::Limit);
        }
        total = total.checked_add(object.bytes).ok_or(Error::Limit)?;
        if total > TOTAL_BYTES {
            return Err(Error::Limit);
        }
        if object.kind == Kind::Image {
            images += 1;
        }
    }
    if images > 8_192 {
        return Err(Error::Limit);
    }
    for (reference, kind) in [
        (&m.submissions, Kind::Submissions),
        (&m.goals, Kind::Goals),
        (&m.images, Kind::Images),
        (&m.notifications, Kind::Notifications),
        (&m.message_acks, Kind::MessageAcks),
    ] {
        if !digest(reference)
            || m.objects.iter().filter(|o| o.kind == kind).count() != 1
            || !m
                .objects
                .iter()
                .any(|o| o.sha256 == *reference && o.kind == kind)
        {
            return Err(Error::Reference);
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    fn fixture() -> Value {
        let kinds = [
            "submissions",
            "goals",
            "images",
            "notifications",
            "message_acks",
        ];
        let objects: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| {
                json!({
                    "sha256":format!("{:064x}",i+1),"bytes":2,"kind":k,
                })
            })
            .collect();
        let mut m = json!({"format":"treer.launcher-handoff.v1","agent_id":"ag_fixture",
            "machine_id":"srv_fixture","handoff_id":"11111111-1111-4111-8111-111111111111",
            "source_generation":1,"destination_generation":2,"source_backend":"node-codex",
            "destination_backend":"rust-codex","source_schema":"launcher-envelope-v1",
            "destination_schema":"launcher-envelope-v1","thread_id":"synthetic-thread",
            "dispatch_fenced":true,"objects":objects});
        for (i, kind) in kinds.iter().enumerate() {
            m[*kind] = json!(format!("{:064x}", i + 1));
        }
        m
    }
    fn validate(value: &Value) -> Result<Manifest, Error> {
        validate_manifest(&serde_json::to_vec(value).unwrap())
    }
    #[test]
    fn both_directions_preserve_the_dispatch_fence() {
        let mut f = fixture();
        assert!(validate(&f).unwrap().dispatch_fenced);
        f["source_backend"] = json!("rust-codex");
        f["destination_backend"] = json!("node-codex");
        assert!(validate(&f).unwrap().dispatch_fenced);
    }
    #[test]
    fn rejects_unknown_fields_versions_identity_and_stale_generation() {
        for (key, value) in [
            ("credential", json!("credential-canary")),
            ("source_generation", json!(0)),
            ("destination_generation", json!(1)),
            ("destination_generation", json!(SAFE_INTEGER + 1)),
            ("format", json!("future")),
            ("source_schema", json!("future")),
            ("agent_id", json!("../escape")),
            ("thread_id", json!("bad\nthread")),
            ("handoff_id", json!("bad")),
            ("source_backend", json!("rust-codex")),
        ] {
            let mut f = fixture();
            f[key] = value;
            assert!(validate(&f).is_err(), "{key}");
        }
        let mut f = fixture();
        f["objects"][0]["path"] = json!("/no-access");
        assert!(validate(&f).is_err());
    }
    #[test]
    fn wire_byte_limit_accepts_boundary_and_refuses_one_over() {
        let mut bytes = serde_json::to_vec(&fixture()).unwrap();
        bytes.resize(MANIFEST_BYTES, b' ');
        assert!(validate_manifest(&bytes).is_ok());
        bytes.push(b' ');
        assert!(matches!(validate_manifest(&bytes), Err(Error::Limit)));
    }
    #[test]
    fn rejects_duplicate_keys_aliases_and_trailing_json() {
        let source = serde_json::to_string(&fixture()).unwrap();
        let duplicate = source.replacen(
            "\"dispatch_fenced\":true",
            "\"dispatch_fenced\":true,\"dispatch_fenced\":false",
            1,
        );
        assert!(validate_manifest(duplicate.as_bytes()).is_err());
        let escaped = source.replacen(
            "\"dispatch_fenced\":true",
            "\"dispatch_fenced\":true,\"dispatch_\\u0066enced\":false",
            1,
        );
        assert!(validate_manifest(escaped.as_bytes()).is_err());
        assert!(validate_manifest((source + "{}").as_bytes()).is_err());
    }
    #[test]
    fn digest_table_must_be_unique_sorted_typed_and_fully_referenced() {
        for mutation in 0..6 {
            let mut f = fixture();
            match mutation {
                0 => f["objects"][1]["sha256"] = f["objects"][0]["sha256"].clone(),
                1 => f["objects"].as_array_mut().unwrap().swap(0, 1),
                2 => f["submissions"] = f["goals"].clone(),
                3 => f["objects"][0]["sha256"] = json!("../escape"),
                4 => f["objects"][0]["kind"] = json!("image"),
                _ => {
                    f["objects"].as_array_mut().unwrap().pop();
                }
            }
            assert!(validate(&f).is_err());
        }
    }
    #[test]
    fn declared_object_bounds_and_total_are_checked_without_reading_objects() {
        let mut f = fixture();
        for o in f["objects"].as_array_mut().unwrap() {
            o["bytes"] = json!(METADATA_BYTES);
        }
        assert!(validate(&f).is_ok());
        f["objects"][0]["bytes"] = json!(METADATA_BYTES + 1);
        assert!(matches!(validate(&f), Err(Error::Limit)));
        f["objects"][0]["bytes"] = json!(0);
        assert!(matches!(validate(&f), Err(Error::Limit)));
        let mut f = fixture();
        for i in 6..=44 {
            f["objects"].as_array_mut().unwrap().push(json!({
            "sha256":format!("{i:064x}"),"kind":"image","bytes":IMAGE_BYTES}));
        }
        assert!(matches!(validate(&f), Err(Error::Limit)));
        assert_eq!(Error::Schema.to_string(), "handoff_schema_invalid");
    }
    #[test]
    fn exact_total_and_image_limits_accept_boundary_and_refuse_one_over() {
        let mut f = fixture();
        f["objects"][0]["bytes"] = json!(24 * 1024 * 1024 - 8);
        for i in 6..42 {
            f["objects"].as_array_mut().unwrap().push(json!({
            "sha256":format!("{i:064x}"),"kind":"image","bytes":IMAGE_BYTES}));
        }
        assert!(validate(&f).is_ok());
        f["objects"][0]["bytes"] = json!(24 * 1024 * 1024 - 7);
        assert!(matches!(validate(&f), Err(Error::Limit)));
        let mut f = fixture();
        f["objects"].as_array_mut().unwrap().push(json!({
            "sha256":format!("{:064x}",6),"kind":"image","bytes":IMAGE_BYTES+1}));
        assert!(matches!(validate(&f), Err(Error::Limit)));
    }
    #[test]
    fn identity_generation_and_image_entry_boundaries() {
        let mut f = fixture();
        f["agent_id"] = json!("a".repeat(128));
        f["source_generation"] = json!(SAFE_INTEGER - 1);
        f["destination_generation"] = json!(SAFE_INTEGER);
        assert!(validate(&f).is_ok());
        f["agent_id"] = json!("a".repeat(129));
        assert!(validate(&f).is_err());
        let mut f = fixture();
        for i in 6..8198 {
            f["objects"].as_array_mut().unwrap().push(json!({
            "sha256":format!("{i:064x}"),"kind":"image","bytes":1}));
        }
        assert!(validate(&f).is_ok());
        f["objects"].as_array_mut().unwrap().push(json!({
            "sha256":format!("{:064x}",8198),"kind":"image","bytes":1}));
        assert!(matches!(validate(&f), Err(Error::Limit)));
    }
}
