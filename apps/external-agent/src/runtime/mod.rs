//! Network boundary. Configuration is trusted deployment input, never request data.
mod config;
mod controller;
mod device;
mod gateway;
mod http;
pub use gateway::CliGateway;
mod inbox;
mod integration;
mod owner;
mod preauth;
mod server;
pub use config::Config;
pub use server::App;
#[cfg(test)]
mod tests;
use crate::Error;
use reqwest::blocking::{Client, Response};
use serde_json::Value;
use std::{io::Read, time::Duration};
const BODY_LIMIT: usize = 64 * 1024;
const RESPONSE_LIMIT: usize = 1024 * 1024;
fn client() -> Result<Client, Error> {
    Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| Error::Invalid)
}
fn json_response(mut r: Response, limit: usize) -> Result<(u16, Value), Error> {
    let status = r.status().as_u16();
    if r.content_length().is_some_and(|n| n > limit as u64) {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    r.by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Invalid)?;
    if bytes.len() > limit {
        return Err(Error::Invalid);
    }
    let v = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    Ok((status, v))
}
fn now() -> Result<i64, Error> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::Invalid)
        .and_then(|v| i64::try_from(v.as_secs()).map_err(|_| Error::Invalid))
}
