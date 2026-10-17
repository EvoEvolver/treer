//! Executable composition: private state + own Core workers + owner HTTP.
//! Transport workers and audited configuration writes remain a separate increment.
use crate::{
    core::Core,
    hub::Hub,
    owner_auth::OwnerAuth,
    scheduler::CoreWorkers,
    settings_http::Settings,
    store::{Limits, Store},
};
use serde::Deserialize;
use serde_json::json;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use thiserror::Error;
#[derive(Debug, Error)]
pub enum Error {
    #[error("hub_configuration_unavailable")]
    Configuration,
    #[error("hub_storage_unavailable")]
    Storage,
    #[error("hub_runtime_unavailable")]
    Runtime,
}
fn listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 8786)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    hub_id: String,
    workspace_id: String,
    service_id: String,
    proxy_url: String,
    public_url: String,
    /// Explicit assertion checked at startup; public settings unsupported.
    access: String,
    cli: PathBuf,
    cli_sha256: String,
    state_directory: PathBuf,
    #[serde(default = "listen")]
    listen: SocketAddr,
}
impl Config {
    pub fn load(
        path: &Path,
        own_id: &str,
        injected_workspace: Option<&str>,
    ) -> Result<Self, Error> {
        let raw = crate::private_store::read_config(path).map_err(|_| Error::Configuration)?;
        let config: Self = serde_json::from_slice(&raw).map_err(|_| Error::Configuration)?;
        config.validate(own_id, injected_workspace)?;
        Ok(config)
    }
    fn validate(&self, own_id: &str, injected_workspace: Option<&str>) -> Result<(), Error> {
        if own_id != self.hub_id
            || !self.hub_id.starts_with("app_")
            || !crate::store::valid_id(&self.hub_id)
            || !crate::store::valid_id(&self.workspace_id)
            || injected_workspace.is_some_and(|id| id != self.workspace_id)
            || self.access != "workspace"
            || !self.listen.ip().is_loopback()
            || self.listen.port() == 0
            || !self.cli.is_absolute()
            || !self.state_directory.is_absolute()
            || self.cli_sha256.len() != 64
            || !self
                .cli_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}
/// Call from a synchronous main. Reqwest's blocking client and owned Core
/// threads are constructed outside Tokio; no global logger is installed.
pub fn run(config: Config) -> Result<(), Error> {
    if crate::relay::hash_executable(&config.cli).map_err(|_| Error::Configuration)?
        != config.cli_sha256
    {
        return Err(Error::Configuration);
    }
    let core = Core::new(&config.cli, &config.hub_id).map_err(|_| Error::Configuration)?;
    let auth = OwnerAuth::new(
        &config.proxy_url,
        &config.service_id,
        &config.workspace_id,
        &config.public_url,
    )
    .map_err(|_| Error::Configuration)?;
    let store =
        Store::open(&config.state_directory, Limits::default()).map_err(|_| Error::Storage)?;
    let hub = Arc::new(Mutex::new(
        Hub::new(store, core, &config.hub_id, &config.workspace_id).map_err(|_| Error::Storage)?,
    ));
    let listener = std::net::TcpListener::bind(config.listen).map_err(|_| Error::Runtime)?;
    listener.set_nonblocking(true).map_err(|_| Error::Runtime)?;
    let workers = Arc::new(Mutex::new(
        CoreWorkers::start(hub.clone()).map_err(|_| Error::Runtime)?,
    ));
    let snapshot_hub = hub.clone();
    let snapshot_workers = workers.clone();
    let snapshot = Arc::new(move || {
        let hub = snapshot_hub.lock().map_err(|_| ())?;
        let endpoints = hub
            .binding_ids()
            .map_err(|_| ())?
            .into_iter()
            .map(|id| hub.view(&id).map_err(|_| ()))
            .collect::<Result<Vec<_>, _>>()?;
        let status = snapshot_workers.lock().map_err(|_| ())?.status();
        Ok(json!({"endpoints":endpoints,"workers":status}))
    });
    let serve = || -> Result<(), Error> {
        let settings = Settings::new(auth, snapshot, include_str!("../dist/index.html"))
            .map_err(|_| Error::Configuration)?
            .with_assets(
                include_str!("../dist/app.js"),
                include_str!("../dist/app.css"),
            );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(16)
            .enable_all()
            .build()
            .map_err(|_| Error::Runtime)?;
        runtime.block_on(async {
            let listener =
                tokio::net::TcpListener::from_std(listener).map_err(|_| Error::Runtime)?;
            axum::serve(
                listener,
                settings
                    .router()
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|_| Error::Runtime)
        })
    };
    let result = serve();
    // Stop only the threads owned by this process. No external signals/restarts.
    if let Ok(mut workers) = workers.lock() {
        workers.stop();
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        serde_json::from_value(json!({"hub_id":"app_hub","workspace_id":"ws_test","service_id":"svc_hub","proxy_url":"https://proxy.example/","public_url":"https://hub.example/","access":"workspace","cli":"/private/cli","cli_sha256":"a".repeat(64),"state_directory":"/private/state"})).unwrap()
    }
    #[test]
    fn runtime_requires_own_app_identity_private_access_and_loopback_listener() {
        let c = config();
        assert!(c.validate("app_hub", Some("ws_test")).is_ok());
        assert!(c.listen.ip().is_loopback());
        assert!(c.validate("ag_other", None).is_err());
        assert!(c.validate("app_hub", Some("ws_other")).is_err());
        let mut c = config();
        c.access = "public".into();
        assert!(c.validate("app_hub", None).is_err());
        let mut c = config();
        c.listen = "0.0.0.0:8786".parse().unwrap();
        assert!(c.validate("app_hub", None).is_err());
        let mut c = config();
        c.cli_sha256 = "not-a-hash".into();
        assert!(c.validate("app_hub", None).is_err());
    }
}
