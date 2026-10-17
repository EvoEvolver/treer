//! App-owned Core Message envelope/queue helpers. No runtime activation yet.
#![forbid(unsafe_code)]
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
pub mod oauth;
#[cfg(all(unix, feature = "hub"))]
pub mod owner_auth;
#[cfg(all(unix, feature = "hub"))]
pub mod telegram_http;
