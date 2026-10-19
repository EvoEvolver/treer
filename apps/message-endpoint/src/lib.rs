//! App-owned Core Message envelope/queue helpers. Runtime activation remains operator-gated and defaults off.
#![forbid(unsafe_code)]
#[cfg(unix)]
pub mod activation_state;
#[cfg(unix)]
pub mod archive;
#[cfg(unix)]
mod cli_integrity;
#[cfg(unix)]
pub mod core;
pub mod failures;
#[cfg(unix)]
pub mod holds;
#[cfg(unix)]
pub mod hub;
#[cfg(unix)]
pub mod notices;
#[cfg(unix)]
mod owner_actions;
#[cfg(unix)]
mod private_store;
pub mod protocol;
#[cfg(unix)]
pub mod recovery;
#[cfg(unix)]
pub mod refetch;
#[cfg(unix)]
pub mod relay;
#[cfg(unix)]
pub mod scheduler;
pub mod store;
#[cfg(unix)]
pub mod telegram;
pub mod transfers;

#[cfg(all(unix, feature = "hub"))]
pub mod hub_runtime;
#[cfg(all(unix, feature = "hub"))]
pub mod oauth;
#[cfg(all(unix, feature = "hub"))]
pub mod owner_auth;
#[cfg(all(unix, feature = "hub"))]
pub mod settings_http;
#[cfg(all(unix, feature = "hub"))]
pub mod telegram_http;
#[cfg(all(unix, feature = "hub"))]
mod verification_gate;

#[cfg(all(unix, feature = "hub"))]
pub mod configuration;

#[cfg(all(unix, feature = "hub"))]
pub mod handoffs;

#[cfg(all(unix, feature = "hub"))]
pub mod telegram_workers;

#[cfg(all(unix, feature = "hub"))]
pub mod telegram_tokens;

#[cfg(all(unix, feature = "hub"))]
pub mod email;

#[cfg(all(unix, feature = "hub"))]
pub use settings_http::email_threads;

#[cfg(all(unix, feature = "hub"))]
pub mod email_workers;
