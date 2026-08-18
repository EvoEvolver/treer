use std::sync::Arc;

use anyhow::{anyhow, Context};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use treer_protocol::{
    WorkloadIdentityClaims, WorkloadIdentityTokenResponse, WorkloadIdentityVerifyResponse,
};
use url::Url;
use uuid::Uuid;

use crate::auth::AuthStore;

const SIGNING_KEY_SECRET_NAME: &str = "workload_identity_ed25519_v1";
const TOKEN_TTL_SECONDS: i64 = 60;
const MAX_CLOCK_SKEW_SECONDS: i64 = 5;

#[derive(Clone)]
pub struct IdentityIssuer {
    signing_key: Arc<SigningKey>,
    issuer: Arc<str>,
    key_id: Arc<str>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JwtHeader {
    alg: String,
    typ: String,
    kid: String,
}

impl IdentityIssuer {
    pub async fn load(auth: &AuthStore, public_url: &Url) -> anyhow::Result<Self> {
        let candidate = random_signing_key();
        let encoded = auth
            .load_or_create_proxy_secret(SIGNING_KEY_SECRET_NAME, &candidate)
            .await
            .context("failed to load workload identity signing key")?;
        let key_bytes: [u8; 32] = encoded.try_into().map_err(|value: Vec<u8>| {
            anyhow!(
                "stored workload identity signing key has invalid length {}",
                value.len()
            )
        })?;
        let signing_key = SigningKey::from_bytes(&key_bytes);
        let public_key = URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
        let key_id = format!("treer-{}", &public_key[..16]);
        Ok(Self {
            signing_key: Arc::new(signing_key),
            issuer: public_url.as_str().trim_end_matches('/').to_string().into(),
            key_id: key_id.into(),
        })
    }

    pub fn issue(
        &self,
        workspace_id: &str,
        machine_id: &str,
        agent_id: &str,
        service_id: &str,
    ) -> anyhow::Result<WorkloadIdentityTokenResponse> {
        let now = Utc::now();
        let expires_at = now + Duration::seconds(TOKEN_TTL_SECONDS);
        let claims = WorkloadIdentityClaims {
            iss: self.issuer.to_string(),
            sub: agent_id.to_string(),
            aud: service_id.to_string(),
            workspace_id: workspace_id.to_string(),
            machine_id: machine_id.to_string(),
            service_id: service_id.to_string(),
            iat: now.timestamp(),
            exp: expires_at.timestamp(),
            jti: format!("wit_{}", Uuid::new_v4().simple()),
        };
        let header = JwtHeader {
            alg: "EdDSA".to_string(),
            typ: "JWT".to_string(),
            kid: self.key_id.to_string(),
        };
        let encoded_header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header)?);
        let encoded_claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims)?);
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        let signature = self.signing_key.sign(signing_input.as_bytes());
        let access_token = format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        );
        Ok(WorkloadIdentityTokenResponse {
            access_token,
            token_type: "Bearer".to_string(),
            expires_in: TOKEN_TTL_SECONDS as u64,
            expires_at,
            audience: service_id.to_string(),
        })
    }

    pub fn verify(&self, token: &str, audience: &str) -> WorkloadIdentityVerifyResponse {
        let claims = self.verify_at(token, audience, Utc::now()).ok();
        WorkloadIdentityVerifyResponse {
            active: claims.is_some(),
            claims,
        }
    }

    pub fn jwks(&self) -> Value {
        json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "alg": "EdDSA",
                "use": "sig",
                "kid": self.key_id.as_ref(),
                "x": URL_SAFE_NO_PAD.encode(self.signing_key.verifying_key().as_bytes()),
            }]
        })
    }

    fn verify_at(
        &self,
        token: &str,
        audience: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<WorkloadIdentityClaims> {
        let mut parts = token.split('.');
        let encoded_header = parts.next().context("missing JWT header")?;
        let encoded_claims = parts.next().context("missing JWT claims")?;
        let encoded_signature = parts.next().context("missing JWT signature")?;
        if parts.next().is_some() {
            return Err(anyhow!("JWT has too many segments"));
        }
        let header: JwtHeader = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(encoded_header)
                .context("invalid JWT header encoding")?,
        )?;
        if header.alg != "EdDSA" || header.typ != "JWT" || header.kid != self.key_id.as_ref() {
            return Err(anyhow!("JWT header does not match this issuer"));
        }
        let signature = Signature::from_slice(
            &URL_SAFE_NO_PAD
                .decode(encoded_signature)
                .context("invalid JWT signature encoding")?,
        )?;
        let signing_input = format!("{encoded_header}.{encoded_claims}");
        self.signing_key
            .verifying_key()
            .verify(signing_input.as_bytes(), &signature)
            .context("invalid JWT signature")?;
        let claims: WorkloadIdentityClaims = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(encoded_claims)
                .context("invalid JWT claims encoding")?,
        )?;
        if claims.iss != self.issuer.as_ref()
            || claims.aud != audience
            || claims.service_id != audience
            || claims.exp <= now.timestamp()
            || claims.iat > now.timestamp() + MAX_CLOCK_SKEW_SECONDS
        {
            return Err(anyhow!("JWT claims are not valid for this request"));
        }
        Ok(claims)
    }
}

fn random_signing_key() -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn tokens_are_signed_audience_bound_and_persistent() {
        let auth = AuthStore::in_memory("admin").await;
        let public_url = Url::parse("https://treer.example/").expect("public URL");
        let first = IdentityIssuer::load(&auth, &public_url)
            .await
            .expect("first issuer");
        let second = IdentityIssuer::load(&auth, &public_url)
            .await
            .expect("reloaded issuer");
        let token = first
            .issue("workspace-a", "server-a", "agent-a", "service-a")
            .expect("issue token");

        let verified = second.verify(&token.access_token, "service-a");
        assert!(verified.active);
        let claims = verified.claims.expect("verified claims");
        assert_eq!(claims.sub, "agent-a");
        assert_eq!(claims.workspace_id, "workspace-a");
        assert_eq!(claims.machine_id, "server-a");
        assert_eq!(claims.aud, "service-a");
        assert!(!second.verify(&token.access_token, "service-b").active);

        let mut segments = token
            .access_token
            .split('.')
            .map(str::to_string)
            .collect::<Vec<_>>();
        segments[1].push('x');
        assert!(!second.verify(&segments.join("."), "service-a").active);
    }

    #[tokio::test]
    async fn expired_tokens_are_inactive() {
        let auth = AuthStore::in_memory("admin").await;
        let issuer = IdentityIssuer::load(
            &auth,
            &Url::parse("https://treer.example/").expect("public URL"),
        )
        .await
        .expect("issuer");
        let token = issuer
            .issue("workspace-a", "server-a", "agent-a", "service-a")
            .expect("issue token");
        let future = Utc::now() + Duration::seconds(TOKEN_TTL_SECONDS + 1);
        assert!(issuer
            .verify_at(&token.access_token, "service-a", future)
            .is_err());
    }
}
