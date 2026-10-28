//! Public device grant exchange and fresh-authenticated owner decisions.
use super::{
    http::{Request, Response},
    Config,
};
use crate::{Error, Owner, Store};
use serde::Deserialize;
use serde_json::json;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    key: String,
    name: String,
    #[serde(default = "expiry")]
    expires_in: i64,
}
fn expiry() -> i64 {
    600
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Poll {
    device_code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lookup {
    user_code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    user_code: String,
    approve: bool,
    days: u16,
    authority_confirmed: bool,
}
fn decode<T: serde::de::DeserializeOwned>(r: &Request) -> Result<T, Error> {
    serde_json::from_slice(&r.body).map_err(|_| Error::Invalid)
}
pub(super) fn public(store: &mut Store, config: &Config, r: &Request) -> Result<Response, Error> {
    match r.path.as_str() {
        "/v1/device/request" => {
            let v: Start = decode(r)?;
            let mut result = store.device_request(&v.key, &v.name, v.expires_in)?;
            result["verification_uri"] = json!(config.owner_origin.to_string());
            Ok(Response::json(201, result))
        }
        "/v1/device/token" => {
            let v: Poll = decode(r)?;
            let (status, result) = store.device_poll(&v.device_code)?;
            Ok(Response::json(status, result))
        }
        _ => Err(Error::Invalid),
    }
}
pub(super) fn owner(store: &mut Store, proof: &Owner<'_>, r: &Request) -> Result<Response, Error> {
    let value = if r.path == "/owner/device/lookup" {
        let v: Lookup = decode(r)?;
        store.device_lookup(proof, &v.user_code)?
    } else {
        let v: Decision = decode(r)?;
        store.device_decide(
            proof,
            &v.user_code,
            v.approve,
            v.days,
            v.authority_confirmed,
        )?
    };
    Ok(Response::json(200, value))
}
