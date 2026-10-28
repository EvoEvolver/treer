use crate::{Binding, Error};
use serde::Deserialize;
use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};
use url::Url;
/// A private, operator-owned file. No credentials or client-selected destinations.
#[derive(Clone, Deserialize)]
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
    /// Absolute path of the native `treer` CLI for `POST /v1/cli`; omit to disable the gateway.
    /// Checked by `--version` at startup, never by hash, so Treer updates keep working.
    #[serde(default)]
    pub treer_cli: Option<PathBuf>,
    /// Retired pins from older releases. Accepted so existing config files keep loading; ignored.
    #[serde(default, rename = "policy_acceptance_sha256")]
    pub legacy_policy_acceptance_sha256: Option<serde_json::Value>,
    #[serde(default, rename = "prompt_review_sha256")]
    pub legacy_prompt_review_sha256: Option<serde_json::Value>,
}
impl Config {
    pub fn load(path: &Path) -> Result<Self, Error> {
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
        Ok(c)
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
        if self.treer_cli.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub(crate) fn binding(&self) -> Binding {
        Binding {
            workspace: self.workspace.clone(),
            app: self.app.clone(),
            agent: self.agent.clone(),
        }
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
