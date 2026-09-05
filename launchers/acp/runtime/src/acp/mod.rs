pub mod adapter;
pub mod capabilities;
pub mod catalog;
pub mod grok;
pub mod mapper;
pub mod modes;
pub mod prompt;
pub mod rpc;
pub mod runtime;
pub mod terminal;
pub mod usage;

pub use adapter::{adapter_for, HarnessAdapter};
pub use catalog::{
    augment_path, builtin_agents, classify_availability, command_available, parse_command_models,
    AcpAgentDef,
};
pub use runtime::AcpRuntime;
