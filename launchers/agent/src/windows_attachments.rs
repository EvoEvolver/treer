//! Windows text-only native entry. Image storage is not advertised until its
//! private ACL, quota reservation, retention and cleanup boundary is implemented.
use anyhow::{bail, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Attachments;
/// Existing nonempty stored image references cannot be admitted by the
/// Windows text-only backend, including references in restored checkpoints.
pub fn valid_id(_id: &str) -> bool {
    false
}
fn unsupported<T>() -> Result<T> {
    bail!("attachment_windows_unsupported")
}
impl Attachments {
    pub fn open(
        _root: &Path,
        _cwd: &Path,
        _agent: String,
        _url: &str,
        _credential: String,
    ) -> Result<Self> {
        unsupported()
    }
    pub fn read(&self, _id: &str, _session: &str) -> Result<(Value, Vec<u8>)> {
        unsupported()
    }
    pub async fn upload(&mut self, _session: &str, _bytes: &[u8]) -> Result<Value> {
        unsupported()
    }
    pub fn resolve(&self, ids: &Value, _session: &str) -> Result<Vec<(Value, PathBuf)>> {
        // Empty text-only submissions must remain usable. Anything other than
        // an explicitly empty list is refused before a provider sees it.
        if ids.as_array().is_some_and(|values| values.is_empty()) {
            Ok(Vec::new())
        } else {
            unsupported()
        }
    }
    pub fn retain(&mut self, ids: &Value, session: &str) -> Result<()> {
        self.resolve(ids, session).map(|_| ())
    }
    pub fn discard(&mut self, _id: &str, _session: &str) -> Result<()> {
        unsupported()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_only_is_explicit_and_images_are_refused() {
        assert!(!valid_id("img_0123456789abcdef0123456789abcdef"));
        let mut attachments = Attachments;
        assert!(attachments
            .resolve(&serde_json::json!([]), "session")
            .unwrap()
            .is_empty());
        assert!(attachments
            .retain(&serde_json::json!([]), "session")
            .is_ok());
        assert!(attachments
            .resolve(
                &serde_json::json!(["img_0123456789abcdef0123456789abcdef"]),
                "session"
            )
            .is_err());
        assert!(attachments
            .resolve(&serde_json::json!(null), "session")
            .is_err());
        assert!(attachments.read("id", "session").is_err());
    }
}
