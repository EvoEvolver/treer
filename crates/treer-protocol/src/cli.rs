//! Versioned transport for the business-command interpreter owned by the Controller.
use serde::{Deserialize, Serialize};
pub const CLI_PROTOCOL_VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CliRequest {
    pub version: u32,
    pub request_id: String,
    pub argv: Vec<String>,
    pub terminal: bool,
    pub cols: u16,
    pub rows: u16,
    #[serde(default)]
    pub capabilities: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CliInput {
    Stdin { data: Vec<u8> },
    Eof,
    Resize { cols: u16, rows: u16 },
    Cancel,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CliOutput {
    StdinReady {
        bytes: usize,
    },
    Session {
        path: String,
        terminal: bool,
    },
    Forward {
        path: String,
        listen: String,
    },
    FileTransfer {
        path: String,
        local: String,
        remote: String,
        download: bool,
        overwrite: bool,
        mode: u32,
    },
    Stdout {
        data: Vec<u8>,
    },
    Stderr {
        data: Vec<u8>,
    },
    Exit {
        code: i32,
    },
}
