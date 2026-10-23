//! Executable composition: private state + own Core workers + owner HTTP.
//! Telegram workers and owner writes share the audited configuration boundary.
use crate::{
    core::Core,
    hub::Hub,
    owner_auth::OwnerAuth,
    scheduler::CoreWorkers,
    settings_http::{Settings, SettingsSnapshot},
    store::{Limits, Store},
};
use serde::Deserialize;
#[cfg(test)]
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
    /// Release/operator gate; a browser can never enable this.
    #[serde(default)]
    activation_enabled: bool,
    #[serde(default)]
    virtual_hostname: Option<String>,
    cli: PathBuf,
    cli_sha256: String,
    state_directory: PathBuf,
    #[serde(default = "listen")]
    listen: SocketAddr,
    #[serde(default)]
    telegram_accounts: Vec<crate::telegram_workers::AccountConfig>,
    #[serde(default)]
    email: Option<crate::email_workers::Config>,
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
            || injected_workspace != Some(self.workspace_id.as_str())
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
    // Keep the executable pin check before even reading transport secret files.
    if crate::relay::hash_executable(&config.cli).map_err(|_| Error::Configuration)?
        != config.cli_sha256
    {
        return Err(Error::Configuration);
    }

    let accounts = crate::telegram_workers::load(&config.telegram_accounts)
        .map_err(|_| Error::Configuration)?;
    run_prepared(config, accounts, async {
        let _ = tokio::signal::ctrl_c().await;
    })
}
// Same assembly for the executable and cfg(test) fixture. The public entry point
// always uses private account files and OS Ctrl-C; no config/feature/env override.
fn run_prepared(
    config: Config,
    accounts: Vec<crate::telegram_workers::Account>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Error> {
    run_assembled(config, accounts, shutdown, CoreWorkers::start)
}
// Production always selects CoreWorkers::start. Only cfg(test) assembly supplies
// a scheduling gate; it is absent from the executable's input/configuration.
fn run_assembled(
    config: Config,
    accounts: Vec<crate::telegram_workers::Account>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    start_workers: impl FnOnce(crate::scheduler::SharedHub<Core>) -> std::io::Result<CoreWorkers>,
) -> Result<(), Error> {
    if crate::relay::hash_executable(&config.cli).map_err(|_| Error::Configuration)?
        != config.cli_sha256
    {
        return Err(Error::Configuration);
    }
    let core = Core::new_pinned(&config.cli, &config.hub_id, &config.cli_sha256)
        .map_err(|_| Error::Configuration)?;
    let auth = OwnerAuth::new(
        &config.proxy_url,
        &config.service_id,
        &config.workspace_id,
        &config.public_url,
    )
    .map_err(|_| Error::Configuration)?;
    let store =
        Store::open(&config.state_directory, Limits::default()).map_err(|_| Error::Storage)?;
    let mut initial = Hub::new(store, core.clone(), &config.hub_id, &config.workspace_id)
        .map_err(|_| Error::Storage)?;
    let email_directory = Arc::new(
        crate::email::proxy::Sender::new(&config.proxy_url, &config.service_id)
            .map_err(|_| Error::Configuration)?,
    );
    let email_service = if let Some(email) = &config.email {
        email.validate().map_err(|_| Error::Configuration)?;
        crate::email::Queue::new(&mut initial.store).map_err(|_| Error::Storage)?;
        let member = crate::owner_auth::MemberAuth::new(
            &config.proxy_url,
            &config.service_id,
            &config.workspace_id,
            &config.public_url,
        )
        .map_err(|_| Error::Configuration)?;
        Some(member)
    } else {
        None
    };
    let startup_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|t| i64::try_from(t.as_secs()).ok())
        .ok_or(Error::Runtime)?;
    initial
        .enforce_startup_activation_gate(config.activation_enabled, startup_at)
        .map_err(|_| Error::Storage)?;
    let hub = Arc::new(Mutex::new(initial));
    let listener = std::net::TcpListener::bind(config.listen).map_err(|_| Error::Runtime)?;
    listener.set_nonblocking(true).map_err(|_| Error::Runtime)?;
    let workers = Arc::new(Mutex::new(
        start_workers(hub.clone()).map_err(|_| Error::Runtime)?,
    ));
    let handoffs = Arc::new(Mutex::new(
        crate::handoffs::CoreHandoffs::start(hub.clone()).map_err(|_| Error::Runtime)?,
    ));
    let telegram = Arc::new(Mutex::new(
        if config.activation_enabled {
            crate::telegram_workers::TelegramWorkers::start(hub.clone(), accounts)
        } else {
            crate::telegram_workers::TelegramWorkers::disabled(accounts)
        }
        .map_err(|_| Error::Runtime)?,
    ));
    let email_workers = Arc::new(Mutex::new(if config.activation_enabled {
        config
            .email
            .clone()
            .map(|email| {
                crate::email_workers::Workers::start(
                    hub.clone(),
                    core.clone(),
                    email_directory.clone(),
                    email,
                    config.public_url.clone(),
                )
            })
            .transpose()
            .map_err(|_| Error::Runtime)?
    } else {
        None
    }));
    let email_http = email_service
        .map(|auth| {
            let (read, reply, inbox) = (hub.clone(), hub.clone(), hub.clone());
            crate::email_threads::Service::new(
                auth,
                &config.public_url,
                Arc::new(move |member, id, now| {
                    let mut h = read
                        .lock()
                        .map_err(|_| crate::email::Error::PrivateStorage)?;
                    h.store
                        .recheck_private_storage()
                        .map_err(|_| crate::email::Error::PrivateStorage)?;
                    crate::email::Queue::new(&mut h.store)?.thread(id, member, now)
                }),
                Arc::new(move |member, id, key, body, now| {
                    let mut h = reply
                        .lock()
                        .map_err(|_| crate::email::Error::PrivateStorage)?;
                    h.store
                        .recheck_private_storage()
                        .map_err(|_| crate::email::Error::PrivateStorage)?;
                    crate::email::Queue::new(&mut h.store)?.reply(id, member, key, body, now)
                }),
            )
            .map(|service| {
                service.with_inbox(Arc::new(move |member, now| {
                    let mut h = inbox
                        .lock()
                        .map_err(|_| crate::email::Error::PrivateStorage)?;
                    h.store
                        .recheck_private_storage()
                        .map_err(|_| crate::email::Error::PrivateStorage)?;
                    crate::email::Queue::new(&mut h.store)?.inbox(member, now)
                }))
            })
        })
        .transpose()
        .map_err(|_| Error::Configuration)?;
    let token_service = Arc::new(
        crate::telegram_tokens::Service::new(config.telegram_accounts.clone())
            .map_err(|_| Error::Configuration)?,
    );
    let snapshot_tokens = token_service.clone();
    let snapshot_email = email_workers.clone();
    let email_configured = config.email.is_some();
    let activation_enabled = config.activation_enabled;
    let account_ready = telegram.lock().map_err(|_| Error::Runtime)?.readiness();
    let snapshot_telegram = telegram.clone();
    let configured_email_mode = config.email.as_ref().map(|c| c.mode).unwrap_or_default();
    let snapshot_handoffs = handoffs.clone();
    let snapshot_hub = hub.clone();
    let snapshot_workers = workers.clone();
    let snapshot = Arc::new(move || {
        let hub = snapshot_hub.lock().map_err(|_| ())?;
        let endpoints = hub
            .binding_ids()
            .map_err(|_| ())?
            .into_iter()
            .map(|id| hub.view(&id).map(Into::into).map_err(|_| ()))
            .collect::<Result<Vec<_>, _>>()?;
        let mut status = snapshot_workers.lock().map_err(|_| ())?.status();
        let hs = snapshot_handoffs.lock().map_err(|_| ())?.status();
        status.inbound_core_handoffs = hs.inbound_core_handoffs;
        status.notice_core_handoffs = hs.notice_core_handoffs;
        status.handoff_failures = hs.handoff_failures;
        status.handoffs_stopped = hs.handoffs_stopped;
        status.cli_integrity_failed = hub.backend().integrity_failed();
        status.last_handoff_failure = hub.last_core_handoff_failure().map_err(|_| ())?;
        let (audit_full, emergency_pause_bindings) = hub.owner_audit_capacity().map_err(|_| ())?;
        Ok(SettingsSnapshot {
            management_notices: crate::management::notice_status(&hub.store.db).map_err(|_| ())?,
            email: if email_configured {
                Some(crate::email_workers::Snapshot::read(
                    &hub.store,
                    snapshot_email.lock().map_err(|_| ())?.as_ref(),
                    configured_email_mode,
                )?)
            } else {
                None
            },
            endpoints,
            workers: status,
            audit_full,
            emergency_pause_bindings,
            telegram_accounts: snapshot_telegram.lock().map_err(|_| ())?.status(),
            activation_enabled,
            telegram_tokens: snapshot_tokens.snapshot().map_err(|_| ())?,
        })
    });
    let member_core = core.clone();
    let mutation = Arc::new(crate::configuration::core_executor(
        hub.clone(),
        core,
        config.workspace_id.clone(),
        crate::configuration::Activation {
            enabled: activation_enabled,
            account_ready,
            email_enabled: email_configured,
            member_current: Arc::new(move |id| {
                email_directory
                    .current_member(&member_core, id)
                    .unwrap_or(false)
            }),
        },
    ));
    let token_hub = hub.clone();
    let token_workers = telegram.clone();
    let token_mutation = Arc::new(move |owner: &crate::owner_auth::Owner, command| {
        token_service.execute(&token_hub, &token_workers, owner, command)
    });
    let request_audit = Arc::new(
        crate::request_audit::RequestAudit::start(&config.state_directory.join("management-audit"))
            .map_err(|_| Error::Storage)?,
    );
    let management_audit = Arc::new(move |request: &str, actor: Option<&str>, status: u16| {
        request_audit.record(request, actor, status)
    });
    let serve = || -> Result<(), Error> {
        let settings = Settings::new(auth, snapshot, include_str!("../dist/index.html"))
            .map_err(|_| Error::Configuration)?
            .with_configuration(mutation)
            .with_management_audit(management_audit)
            .with_tokens(token_mutation)
            .with_loopback_listener(config.listen)
            .map_err(|_| Error::Configuration)?
            // The workload HTTP proxy uses the stable service authority, while
            // workspace ingress uses the separately configured virtual host.
            .with_virtual_host(&format!("{}.treer", config.service_id))
            .map_err(|_| Error::Configuration)?
            .with_assets(
                include_str!("../dist/app.js"),
                include_str!("../dist/app.css"),
            );
        let settings = match email_http {
            Some(email) => settings.with_email(Arc::new(email)),
            None => settings,
        };
        let settings = match &config.virtual_hostname {
            Some(host) => settings
                .with_virtual_host(host)
                .map_err(|_| Error::Configuration)?,
            None => settings,
        };
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
            .with_graceful_shutdown({
                let workers = workers.clone();
                let handoffs = handoffs.clone();
                let telegram = telegram.clone();
                let email_workers = email_workers.clone();
                async move {
                    shutdown.await;
                    if let Ok(email) = email_workers.lock() {
                        if let Some(email) = email.as_ref() {
                            email.request_stop();
                        }
                    }
                    if let Ok(w) = workers.lock() {
                        w.request_stop();
                    }
                    if let Ok(w) = handoffs.lock() {
                        w.request_stop();
                    }
                    if let Ok(w) = telegram.lock() {
                        w.request_stop();
                    }
                }
            })
            .await
            .map_err(|_| Error::Runtime)
        })
    };
    let result = serve();
    // Stop only the threads owned by this process. No external signals/restarts.
    if let Ok(mut email) = email_workers.lock() {
        if let Some(email) = email.as_mut() {
            email.stop();
        }
    }
    if let Ok(mut telegram) = telegram.lock() {
        telegram.stop();
    }
    if let Ok(mut handoffs) = handoffs.lock() {
        handoffs.stop();
    }
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
    fn email_defaults_absent_and_proxy_transport_requires_explicit_configuration() {
        let c = config();
        assert!(c.email.is_none());
        assert!(!c.activation_enabled);
        let outbox: crate::email_workers::Config =
            serde_json::from_value(json!({"outbox_directory":"/private/outbox"})).unwrap();
        assert_eq!(outbox.mode, crate::email::Mode::Outbox);
        let proxy: crate::email_workers::Config =
            serde_json::from_value(json!({"outbox_directory":"/private/outbox","mode":"proxy"}))
                .unwrap();
        assert_eq!(proxy.mode, crate::email::Mode::Proxy);
        assert!(serde_json::from_value::<crate::email_workers::Config>(
            json!({"outbox_directory":"/private/outbox","mode":"smtp"}),
        )
        .is_err());
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

#[cfg(test)]
#[path = "hub_runtime_fixture.rs"]
mod positive_fixture;
