//! App-owned Core Message envelope/queue helpers. No runtime activation yet.
#![forbid(unsafe_code)]
#[cfg(unix)]
mod private_store;
pub mod protocol;
pub mod store;
pub mod transfers;
