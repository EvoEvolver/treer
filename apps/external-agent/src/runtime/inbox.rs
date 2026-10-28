//! Endpoint-scoped access to Core's durable deliveries; no App payload queue.
use super::{controller::ResultValue, http, server::App};
use crate::{Dispatched, Error, Operation, Outcome};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Register {
    pub key: String,
    pub name: String,
    pub days: u16,
    pub authority_confirmed: bool,
}
impl Register {
    pub fn validate(&self) -> Result<(), Error> {
        if self.authority_confirmed {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Ack {
    delivery_ids: Vec<String>,
    operation_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Revoke {
    id: String,
}
pub(super) enum Request {
    Revoke(Revoke),
    List,
    Register(Register),
    Poll { after: Option<String>, limit: u16 },
    Ack(Ack),
}
impl Request {
    pub fn parse(r: &http::Request) -> Result<Option<Self>, Error> {
        let (path, query) = r.path.split_once('?').unwrap_or((&r.path, ""));
        let value = match (r.method.as_str(), path) {
            ("POST", "/v1/external-agents/revoke-token") if query.is_empty() => {
                Self::Revoke(decode(r)?)
            }
            ("GET", "/v1/external-agents") if query.is_empty() => Self::List,
            ("POST", "/v1/external-agents") if query.is_empty() => {
                let v: Register = decode(r)?;
                v.validate()?;
                Self::Register(v)
            }
            ("GET", "/v1/inbox") => {
                let mut after = None;
                let mut limit = None;
                for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
                    match key.as_ref() {
                        "cursor" if after.is_none() && !value.is_empty() && value.len() <= 1024 => {
                            after = Some(value.into_owned())
                        }
                        "limit" if limit.is_none() => {
                            limit = Some(value.parse::<u16>().map_err(|_| Error::Invalid)?)
                        }
                        _ => return Err(Error::Invalid),
                    }
                }
                let limit = limit.unwrap_or(50);
                if !(1..=100).contains(&limit) {
                    return Err(Error::Invalid);
                }
                Self::Poll { after, limit }
            }
            ("POST", "/v1/inbox/ack") if query.is_empty() => {
                let v: Ack = decode(r)?;
                if v.delivery_ids.is_empty()
                    || v.delivery_ids.len() > 100
                    || !identifier(&v.operation_id)
                    || v.delivery_ids.iter().any(|s| !identifier(s))
                {
                    return Err(Error::Invalid);
                }
                Self::Ack(v)
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }
    pub fn operation(&self) -> Operation {
        match self {
            Self::List | Self::Register(_) | Self::Revoke(_) => Operation::Registry,
            Self::Poll { .. } => Operation::Inbox,
            Self::Ack(_) => Operation::Ack,
        }
    }
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn decode<T: serde::de::DeserializeOwned>(r: &http::Request) -> Result<T, Error> {
    if r.header("content-type") != Some("application/json") {
        return Err(Error::Invalid);
    }
    serde_json::from_slice(&r.body).map_err(|_| Error::Invalid)
}
impl App {
    pub(super) fn inbox_execute(&self, p: &Dispatched, input: Request) -> ResultValue {
        let registry = matches!(
            &input,
            Request::List | Request::Register(_) | Request::Revoke(_)
        );
        let result: Result<(u16, Value), Error> = (|| {
            match input {
                Request::Revoke(v)=> {
                    self.store.lock().map_err(|_|Error::PrivateStorage)?.revoke_created_external(p,&v.id)?;
                    Ok((200,json!({"revoked":true})))
                }
                Request::List=> {
                    let store=self.store.lock().map_err(|_|Error::PrivateStorage)?;
                    Ok((200,store.external_agents(&store.external_owner(p)?)?))
                }
                Request::Register(v)=> {
                    let mut store=self.store.lock().map_err(|_|Error::PrivateStorage)?;
                    let c=store.register_external_from(p,&v.key,&v.name,v.days)?;
                    let token=serde_json::to_value(&c.record).map_err(|_|Error::Invalid)?;
                    Ok((201,json!({"address":format!("{}/{}",self.config.agent,v.key),"key":v.key,"token":token,"plaintext":c.into_plaintext()})))
                }
                Request::Poll{after,limit}=> {
                    let endpoint=p.target().ok_or(Error::Denied)?;
                    let (status,mut v)=self.controller.request(reqwest::Method::POST,"/api/messages/receive",Some(json!({"app_endpoint":endpoint,"after":after,"limit":limit,"wait_milliseconds":0})))?;
                    if status==200 {
                        let deliveries=v["deliveries"].as_array_mut().ok_or(Error::Invalid)?;
                        for d in deliveries.iter_mut() {
                            if d["message"].get("kind").is_none() {d["message"]["kind"]=json!("message");}
                        }
                        if deliveries.len()>usize::from(limit) || deliveries.iter().any(|d| d["recipient"]["id"]!=self.config.agent || d["recipient"]["app_endpoint"]!=endpoint || d["message"]["workspace_id"]!=self.config.workspace || !matches!(d["message"]["kind"].as_str(),Some("message"|"prompt"))) {return Err(Error::Invalid)}
                    }
                    Ok((status,v))
                }
                Request::Ack(v)=>self.controller.request(reqwest::Method::POST,"/api/messages/ack",Some(json!({"app_endpoint":p.target().ok_or(Error::Denied)?,"delivery_ids":v.delivery_ids,"operation_id":v.operation_id}))),
            }
        })();
        match result {
            Ok((status @ 200..=299, value)) => ResultValue {
                status,
                value,
                outcome: Outcome::Completed,
            },
            Ok((status @ (400 | 403 | 404 | 409), _)) => ResultValue {
                status,
                value: json!({"error":{"code":"delivery_rejected"}}),
                outcome: Outcome::Unavailable,
            },
            Err(Error::Denied) if registry => ResultValue {
                status: 403,
                value: json!({"error":{"code":"creator_scope_required"}}),
                outcome: Outcome::Unavailable,
            },
            Err(Error::Invalid) if registry => ResultValue {
                status: 400,
                value: json!({"error":{"code":"invalid_request"}}),
                outcome: Outcome::Unavailable,
            },
            Err(Error::Capacity) => ResultValue {
                status: 429,
                value: json!({"error":{"code":"capacity_unavailable"}}),
                outcome: Outcome::Unavailable,
            },
            _ => ResultValue {
                status: 502,
                value: json!({"error":{"code":"outcome_unknown"}}),
                outcome: Outcome::Unknown,
            },
        }
    }
}
