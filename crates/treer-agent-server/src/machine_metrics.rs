//! Machine-local metrics internals. Not connected to startup or query routing
//! until collection, IPC, persistence and authorization gates are implemented.
pub mod aggregation;
pub mod field_cache;
pub mod history;
