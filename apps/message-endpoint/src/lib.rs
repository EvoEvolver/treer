//! App-owned Core Message envelope/queue helpers. No runtime activation yet.
#![forbid(unsafe_code)]
#[cfg(unix)]
pub mod core;
pub mod failures;
#[cfg(unix)]
pub mod hub;
#[cfg(unix)]
mod private_store;
pub mod protocol;
#[cfg(unix)]
pub mod relay;
pub mod store;
#[cfg(unix)]
pub mod telegram;
pub mod transfers;
