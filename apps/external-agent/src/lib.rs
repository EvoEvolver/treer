//! External Agent App: fixed workload transport and independently verified owner management.
#![cfg(unix)]
mod private_store;
pub mod runtime;
mod store;
pub use store::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("storage unavailable")]
    Storage(#[from] rusqlite::Error),
    #[error("private storage unavailable")]
    PrivateStorage,
    #[error("private storage busy")]
    Busy,
    #[error("authentication required")]
    Denied,
    #[error("external API disabled")]
    Disabled,
    #[error("capacity unavailable")]
    Capacity,
    #[error("invalid request")]
    Invalid,
}
