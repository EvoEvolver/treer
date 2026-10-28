use super::{client, json_response, now, Config};
use crate::{Binding, Error, Owner};
use serde::Deserialize;
/// Constructible only by a successful configured Proxy response. Never decoded JWT claims.
pub(super) struct Verified {
    subject: String,
    pub human: bool,
}
impl Verified {
    pub fn proof<'a>(&'a self, binding: &'a Binding) -> Owner<'a> {
        Owner {
            binding,
            subject: &self.subject,
        }
    }
}
pub(super) struct Verifier {
    client: reqwest::blocking::Client,
    config: Config,
}
#[derive(Deserialize)]
struct Reply {
    active: bool,
    claims: Option<Claims>,
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    aud: String,
    workspace_id: String,
    service_id: String,
    principal_kind: String,
    role: Option<String>,
    iat: i64,
    exp: i64,
}
impl Verifier {
    pub fn new(config: Config) -> Result<Self, Error> {
        Ok(Self {
            client: client()?,
            config,
        })
    }
    pub fn verify(&self, assertion: &str) -> Result<Verified, Error> {
        if assertion.is_empty()
            || assertion.len() > 8192
            || !assertion.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Denied);
        }
        let url = self
            .config
            .proxy
            .join("/.treer/apps/identity/verify")
            .map_err(|_| Error::Denied)?;
        let r = self
            .client
            .post(url)
            .timeout(std::time::Duration::from_secs(3))
            .json(&serde_json::json!({"token":assertion,"audience":self.config.service}))
            .send()
            .map_err(|_| Error::Denied)?;
        let (status, v) = json_response(r, 16384).map_err(|_| Error::Denied)?;
        if status != 200 {
            return Err(Error::Denied);
        }
        let r: Reply = serde_json::from_value(v).map_err(|_| Error::Denied)?;
        let c = r.claims.filter(|_| r.active).ok_or(Error::Denied)?;
        let time = now()?;
        // Humans must be current workspace owners. Agents reach this management service
        // only with an identity Core issued under its `identity.token.issue` Policy for
        // this service; that grant is the authorization. This App's own execution
        // identity is refused: external token holders act as it and must not manage tokens.
        let human = match c.principal_kind.as_str() {
            "human" if c.role.as_deref() == Some("owner") => true,
            "agent" if c.sub != self.config.agent => false,
            _ => return Err(Error::Denied),
        };
        if c.aud != self.config.service
            || c.service_id != self.config.service
            || c.workspace_id != self.config.workspace
            || c.exp <= time
            || c.iat > time + 30
            || c.iat < time - 90
            || c.exp <= c.iat
            || c.exp
                .checked_sub(c.iat)
                .is_none_or(|lifetime| lifetime > 60)
            || c.sub.is_empty()
            || c.sub.len() > 128
            || !c
                .sub
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        {
            return Err(Error::Denied);
        }
        Ok(Verified {
            subject: c.sub,
            human,
        })
    }
}
