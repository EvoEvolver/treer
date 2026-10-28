use crate::{Binding, Error};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};
use url::Url;
/// A private, operator-owned file. No credentials or client-selected destinations.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub workspace: String,
    pub app: String,
    pub service: String,
    pub agent: String,
    pub machine: String,
    pub controller: Url,
    pub proxy: Url,
    pub public_origin: Url,
    pub owner_origin: Url,
    pub console_origin: Url,
    pub tunnel_origin: Url,
    pub console_url: Url,
    pub listen: SocketAddr,
    /// Workspace-only management socket, isolated from anonymous parser admission.
    pub owner_listen: SocketAddr,
    pub state: PathBuf,
    /// Legacy metadata retained in serialized Config to preserve existing token binding.
    /// This field no longer gates owner activation.
    pub policy_acceptance_sha256: Option<String>,
    /// Legacy metadata; no longer gates prompt. Keep unchanged on initialized stores.
    pub prompt_review_sha256: Option<String>,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::load_with_digest(path).map(|(config, _)| config)
    }
    pub fn load_with_digest(path: &Path) -> Result<(Self, String), Error> {
        use std::os::unix::{fs::MetadataExt, fs::OpenOptionsExt};
        // O_NOFOLLOW plus metadata on the opened fd avoids trusting a symlink race.
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)
            .map_err(|_| Error::Invalid)?;
        let m = f.metadata().map_err(|_| Error::Invalid)?;
        if !m.is_file()
            || m.uid() != rustix::process::geteuid().as_raw()
            || m.mode() & 0o077 != 0
            || m.len() > 16384
        {
            return Err(Error::Invalid);
        }
        use std::io::Read;
        let mut b = Vec::new();
        f.by_ref()
            .take(16385)
            .read_to_end(&mut b)
            .map_err(|_| Error::Invalid)?;
        if b.len() > 16384 {
            return Err(Error::Invalid);
        }
        let c: Self = serde_json::from_slice(&b).map_err(|_| Error::Invalid)?;
        c.validate()?;
        Ok((c, format!("{:x}", Sha256::digest(&b))))
    }
    pub(crate) fn validate(&self) -> Result<(), Error> {
        for id in [
            &self.workspace,
            &self.app,
            &self.service,
            &self.agent,
            &self.machine,
        ] {
            if id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            {
                return Err(Error::Invalid);
            }
        }
        if !loopback(&self.controller)
            || self.controller.path() != "/"
            || !self.listen.ip().is_loopback()
            || !self.owner_listen.ip().is_loopback()
            || (self.listen.port() != 0 && self.listen == self.owner_listen)
            || !self.state.is_absolute()
        {
            return Err(Error::Invalid);
        }
        for u in [
            &self.proxy,
            &self.public_origin,
            &self.owner_origin,
            &self.console_origin,
            &self.tunnel_origin,
        ] {
            if !clean_url(u) || (u.scheme() != "https" && !loopback(u)) || u.path() != "/" {
                return Err(Error::Invalid);
            }
        }
        if [
            &self.owner_origin,
            &self.console_origin,
            &self.tunnel_origin,
        ]
        .iter()
        .any(|u| self.public_origin.origin() == u.origin())
        {
            return Err(Error::Invalid);
        }
        if !clean_url(&self.console_url)
            || self.console_url.origin() != self.console_origin.origin()
        {
            return Err(Error::Invalid);
        }
        for pin in [&self.policy_acceptance_sha256, &self.prompt_review_sha256]
            .into_iter()
            .flatten()
        {
            if pin.len() != 64
                || !pin
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
    pub(crate) fn binding(&self, credential: &str) -> Result<Binding, Error> {
        self.validate()?;
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(self).map_err(|_| Error::Invalid)?);
        h.update(Sha256::digest(credential.as_bytes()));
        Ok(Binding {
            workspace: self.workspace.clone(),
            app: self.app.clone(),
            service: self.service.clone(),
            agent: self.agent.clone(),
            configuration: format!("{:x}", h.finalize()),
        })
    }
    pub(crate) fn origin_allowed(&self, s: &str) -> bool {
        [
            &self.owner_origin,
            &self.tunnel_origin,
            &self.console_origin,
        ]
        .iter()
        .any(|u| s == u.origin().ascii_serialization())
    }
}
fn clean_url(u: &Url) -> bool {
    u.host_str().is_some()
        && u.username().is_empty()
        && u.password().is_none()
        && u.fragment().is_none()
        && u.query().is_none()
}
fn loopback(u: &Url) -> bool {
    clean_url(u)
        && u.scheme() == "http"
        && u.host_str()
            .and_then(|s| s.trim_matches(['[', ']']).parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback())
}
