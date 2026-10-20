//! Machine-local metrics internals. Not connected to startup or query routing
//! until collection, IPC, persistence and authorization gates are implemented.
pub mod aggregation;
pub mod collector_state;
pub mod field_cache;
pub mod field_query;
pub mod history;
pub mod response_encoding;
pub mod stored_frame;
