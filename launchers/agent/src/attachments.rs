use crate::{image_format, store::Store};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    id: String,
    mime: String,
    extension: String,
    size: usize,
    digest: String,
    session: String,
    retained: bool,
    created_ms: i64,
}
impl Entry {
    fn metadata(&self) -> Value {
        json!({"id":self.id,"mime":self.mime,"size":self.size})
    }
}
#[derive(Default, Serialize, Deserialize)]
struct Journal {
    entries: Vec<Entry>,
}
pub struct Attachments {
    store: Store,
    journal: Journal,
    client: reqwest::Client,
    base: reqwest::Url,
    agent: String,
    credential: String,
    quota_bytes: usize,
    entry_limit: usize,
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.starts_with("img_")
        && id[4..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Attachments {
    pub fn open(
        root: &Path,
        cwd: &Path,
        agent: String,
        url: &str,
        credential: String,
    ) -> Result<Self> {
        Store::open(root, cwd)?;
        let base = reqwest::Url::parse(url)?;
        if agent.is_empty()
            || credential.is_empty()
            || base.scheme() != "http"
            || !matches!(base.host_str(), Some("127.0.0.1" | "[::1]"))
            || !base.username().is_empty()
            || base.password().is_some()
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
        {
            bail!("agent_private_state_unavailable");
        }
        let images = root.join("rust-images");
        match fs::DirBuilder::new().mode(0o700).create(&images) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        let store = Store::open(&images, cwd)?;
        let journal: Journal = match store.load()? {
            Some(v) => serde_json::from_value(v)
                .map_err(|_| anyhow::anyhow!("attachment_invalid_journal"))?,
            None => Journal::default(),
        };
        let mut ids = HashSet::new();
        if journal.entries.len() > 8192 {
            bail!("attachment_invalid_journal");
        }
        for e in &journal.entries {
            if !valid_id(&e.id)
                || !ids.insert(&e.id)
                || !["png", "jpg", "webp", "gif"].contains(&e.extension.as_str())
                || !["image/png", "image/jpeg", "image/webp", "image/gif"]
                    .contains(&e.mime.as_str())
                || e.size == 0
                || e.size > image_format::FILE_LIMIT
                || e.session.is_empty()
                || e.session.len() > 256
                || e.digest.len() != 64
                || !e.digest.bytes().all(|c| c.is_ascii_hexdigit())
            {
                bail!("attachment_invalid_journal");
            }
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(5))
            .build()?;
        Ok(Self {
            store,
            journal,
            client,
            base,
            agent,
            credential,
            quota_bytes: fixture_limit("RUST_SPIKE_IMAGE_QUOTA_BYTES", 256 * 1024 * 1024),
            entry_limit: fixture_limit("RUST_SPIKE_IMAGE_ENTRY_LIMIT", 8192),
        })
    }
    fn save(&self) -> Result<()> {
        self.store.save(&serde_json::to_value(&self.journal)?)
    }
    fn path(&self, e: &Entry) -> PathBuf {
        self.store.root.join(format!("{}.{}", e.id, e.extension))
    }
    fn entry(&self, id: &str, session: &str) -> Result<&Entry> {
        self.journal
            .entries
            .iter()
            .find(|e| valid_id(id) && e.id == id && e.session == session)
            .ok_or_else(|| anyhow::anyhow!("attachment_not_found"))
    }
    pub fn read(&self, id: &str, session: &str) -> Result<(Value, Vec<u8>)> {
        let e = self.entry(id, session)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.path(e))?;
        let m = file.metadata()?;
        if !m.is_file()
            || m.len() != e.size as u64
            || m.uid() != rustix::process::geteuid().as_raw()
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
        {
            bail!("attachment_unsafe_storage");
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(image_format::FILE_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        let (mime, extension) = image_format::format(&bytes)?;
        if bytes.len() != e.size
            || mime != e.mime
            || extension != e.extension
            || format!("{:x}", Sha256::digest(&bytes)) != e.digest
        {
            bail!("attachment_invalid_image");
        }
        Ok((e.metadata(), bytes))
    }
    async fn quota(
        &self,
        path: &str,
        method: reqwest::Method,
        body: Option<Value>,
    ) -> Result<Value> {
        async fn call(
            this: &Attachments,
            path: &str,
            method: reqwest::Method,
            body: Option<Value>,
        ) -> Result<Value> {
            let mut request = this
                .client
                .request(method, this.base.join(&format!("api/private-state{path}"))?)
                .header("x-treer-agent-id", &this.agent)
                .header("x-treer-workload-credential", &this.credential);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let mut response = request.send().await?;
            let success = response.status().is_success();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await? {
                if chunk.len() > 65536 - bytes.len() {
                    bail!("agent_private_state_unavailable");
                }
                bytes.extend_from_slice(&chunk);
            }
            let value: Value = serde_json::from_slice(&bytes)?;
            if !success {
                bail!("agent_private_state_unavailable");
            }
            Ok(value)
        }
        call(self, path, method, body)
            .await
            .map_err(|_| anyhow::anyhow!("agent_private_state_unavailable"))
    }
    pub async fn upload(&mut self, session: &str, bytes: &[u8]) -> Result<Value> {
        if session.is_empty() || session.len() > 256 {
            bail!("attachment_session_required");
        }
        let (mime, extension) = image_format::format(bytes)?;
        let expired = self
            .journal
            .entries
            .iter()
            .filter(|e| {
                !e.retained
                    && chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_sub(e.created_ms)
                        >= 3_600_000
            })
            .map(|e| (e.id.clone(), e.session.clone()))
            .collect::<Vec<_>>();
        for (id, session) in expired {
            let entry = self.entry(&id, &session)?.clone();
            match fs::symlink_metadata(self.path(&entry)) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.journal.entries.retain(|e| e.id != id);
                    self.save()?;
                    eprintln!("Rust spike: missing expired image entry removed");
                }
                _ => self.discard(&id, &session)?,
            }
        }
        if self.journal.entries.len() >= self.entry_limit
            || self.journal.entries.iter().map(|e| e.size).sum::<usize>() + bytes.len()
                > self.quota_bytes
        {
            bail!("attachment_quota_exceeded");
        }
        let reserved = self
            .quota(
                "/reservations",
                reqwest::Method::POST,
                Some(json!({"bytes":bytes.len()})),
            )
            .await?;
        let reservation = reserved["reservation_id"]
            .as_str()
            .filter(|s| {
                s.len() == 68
                    && s.starts_with("psr_")
                    && s[4..]
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
            .ok_or_else(|| anyhow::anyhow!("agent_private_state_unavailable"))?
            .to_owned();
        let entry = Entry {
            id: format!("img_{}", uuid::Uuid::new_v4().simple()),
            mime: mime.into(),
            extension: extension.into(),
            size: bytes.len(),
            digest: format!("{:x}", Sha256::digest(bytes)),
            session: session.into(),
            retained: false,
            created_ms: chrono::Utc::now().timestamp_millis(),
        };
        let path = self.path(&entry);
        let mut created = false;
        let result: Result<Value> = async {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)?;
            created = true;
            file.write_all(bytes)?;
            file.sync_all()?;
            self.journal.entries.push(entry.clone());
            self.save()?;
            self.quota(
                &format!("/reservations/{reservation}/commit"),
                reqwest::Method::POST,
                None,
            )
            .await?;
            Ok(entry.metadata())
        }
        .await;
        if result.is_err() {
            self.journal.entries.retain(|e| e.id != entry.id);
            let removed = !created || fs::remove_file(&path).is_ok();
            let saved = self.save().is_ok();
            let mut released = false;
            for _ in 0..2 {
                if self
                    .quota(
                        &format!("/reservations/{reservation}"),
                        reqwest::Method::DELETE,
                        None,
                    )
                    .await
                    .is_ok()
                {
                    released = true;
                    break;
                }
            }
            if !released {
                eprintln!(
                    "Rust spike: attachment reservation release failed; Controller expiry required"
                );
            }
            if !removed || !saved {
                bail!("attachment_storage_unavailable");
            }
            bail!("attachment_upload_failed");
        }
        result
    }
    pub fn resolve(&self, ids: &Value, session: &str) -> Result<Vec<(Value, PathBuf)>> {
        let ids = ids
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("attachment_invalid_ids"))?;
        if ids.len() > image_format::COUNT_LIMIT {
            bail!("attachment_invalid_ids");
        }
        let mut seen = HashSet::new();
        let mut total = 0;
        let mut result = vec![];
        for id in ids {
            let id = id
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("attachment_invalid_ids"))?;
            if !seen.insert(id) {
                bail!("attachment_invalid_ids");
            }
            let e = self.entry(id, session)?;
            total += e.size;
            if total > image_format::PROMPT_LIMIT {
                bail!("attachment_prompt_too_large");
            }
            let (metadata, _) = self.read(id, session)?;
            result.push((metadata, self.path(e)));
        }
        Ok(result)
    }
    pub fn retain(&mut self, ids: &Value, session: &str) -> Result<()> {
        self.resolve(ids, session)?;
        for id in ids.as_array().unwrap() {
            if let Some(e) = self
                .journal
                .entries
                .iter_mut()
                .find(|e| e.id == id.as_str().unwrap())
            {
                e.retained = true;
            }
        }
        self.save()
    }
    pub fn discard(&mut self, id: &str, session: &str) -> Result<()> {
        let e = self.entry(id, session)?.clone();
        if e.retained {
            bail!("attachment_already_sent");
        }
        self.read(id, session)?;
        fs::remove_file(self.path(&e))?;
        self.journal.entries.retain(|e| e.id != id);
        self.save()
    }
}
fn fixture_limit(name: &str, default: usize) -> usize {
    if cfg!(feature = "fixture-harness") {
        std::env::var(name)
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|n| *n > 0)
            .map(|n: usize| n.min(default))
            .unwrap_or(default)
    } else {
        default
    }
}
