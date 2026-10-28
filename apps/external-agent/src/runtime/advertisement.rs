//! Operator-supplied display metadata, never execution authority or token binding.
use super::Config;
use crate::Error;
use serde::Deserialize;
use std::{
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};
use url::Url;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataAdvertisement {
    #[serde(skip)]
    validated_config: String,
    schema_version: u8,
    config_sha256: String,
    workspace: String,
    app: String,
    service: String,
    agent: String,
    machine: String,
    listen: std::net::SocketAddr,
    data_service_id: String,
    data_base_url: Url,
}
impl DataAdvertisement {
    pub fn load(path: &Path, config: &Config, config_sha256: &str) -> Result<Self, Error> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(path)
            .map_err(|_| Error::Invalid)?;
        let m = file.metadata().map_err(|_| Error::Invalid)?;
        if !m.is_file()
            || m.uid() != rustix::process::geteuid().as_raw()
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
            || m.len() > 4096
        {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        file.by_ref()
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Invalid)?;
        if bytes.len() > 4096 {
            return Err(Error::Invalid);
        }
        let mut ad: Self = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
        ad.validate(config, config_sha256)?;
        ad.validated_config = serde_json::to_string(config).map_err(|_| Error::Invalid)?;
        Ok(ad)
    }
    fn validate(&self, c: &Config, digest: &str) -> Result<(), Error> {
        let u = &self.data_base_url;
        if self.schema_version != 1
            || self.config_sha256 != digest
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || (
                &self.workspace,
                &self.app,
                &self.service,
                &self.agent,
                &self.machine,
                self.listen,
            ) != (
                &c.workspace,
                &c.app,
                &c.service,
                &c.agent,
                &c.machine,
                c.listen,
            )
            || self.data_service_id == c.service
            || self.data_service_id.len() != 36
            || !self.data_service_id.starts_with("svc_")
            || !self.data_service_id[4..]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || u.scheme() != "https"
            || u.host_str().is_none()
            || !u.username().is_empty()
            || u.password().is_some()
            || u.path() != "/"
            || u.query().is_some()
            || u.fragment().is_some()
            || [
                &c.owner_origin,
                &c.console_origin,
                &c.tunnel_origin,
                &c.proxy,
            ]
            .iter()
            .any(|v| v.origin() == u.origin())
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub(super) fn matches(&self, config: &Config) -> bool {
        serde_json::to_string(config).is_ok_and(|value| value == self.validated_config)
    }
    pub(super) fn origin(&self) -> &Url {
        &self.data_base_url
    }
}
