use super::*;
use crate::api::ApiFailure;
use treer_protocol::{SendAppEmailRequest, SendAppEmailResponse};

fn valid_address(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && value.len() <= 254
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~@".contains(&b))
}

pub(crate) fn email_allowlisted(address: &str, allowlist: &str) -> bool {
    let domain = address.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
    allowlist
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .any(|entry| {
            if let Some(allowed_domain) = entry.strip_prefix('@') {
                domain.eq_ignore_ascii_case(allowed_domain)
            } else {
                address.eq_ignore_ascii_case(entry)
            }
        })
}

impl AuthStore {
    pub(crate) async fn send_app_email(
        &self,
        workspace: &str,
        app: &str,
        request: &SendAppEmailRequest,
        allowlist: &str,
    ) -> Result<SendAppEmailResponse, ApiFailure> {
        let to = request.to.to_ascii_lowercase();
        if !valid_address(&to)
            || request.subject.is_empty()
            || request.subject.len() > 200
            || request.subject.chars().any(char::is_control)
            || request.text.is_empty()
            || request.text.len() > 4096
            || request.idempotency_key.is_empty()
            || request.idempotency_key.len() > 128
        {
            return Err(ApiFailure::bad_request(
                "email_invalid",
                "invalid email fields (subject 1-200 bytes, text 1-4096 bytes, key 1-128 bytes)",
            ));
        }
        let member: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM workspaces w JOIN organization_members m ON m.organization_id=w.organization_id JOIN users u ON u.id=m.user_id WHERE w.workspace_id=$1 AND lower(u.email)=$2 AND u.email_verified=TRUE)")
            .bind(workspace).bind(&to).fetch_one(&self.pool).await
            .map_err(|_| ApiFailure::internal("email_storage_failed", "email storage unavailable"))?;
        if !member && !email_allowlisted(&to, allowlist) {
            return Err(ApiFailure::forbidden(
                "email_recipient_denied",
                "recipient is not a verified organization member or allowlisted",
            ));
        }
        let sender = self.email_sender.as_ref().ok_or_else(|| {
            ApiFailure::service_unavailable(
                "email_not_configured",
                "Proxy email sender is not configured",
            )
        })?;
        let hash = format!("{:x}", Sha256::digest(to.as_bytes()));
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(request).expect("serializable request"))
        );
        let mut tx = self.pool.begin().await.map_err(email_storage)?;
        // Serialize reservations across replicas. Rate counters and idempotency survive restarts.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('treer_app_email'))")
            .execute(&mut *tx)
            .await
            .map_err(email_storage)?;
        let old = sqlx::query("SELECT email_id, fingerprint, status, (updated_at > now()-interval '60 seconds') AS recent FROM app_email_sends WHERE workspace_id=$1 AND app_id=$2 AND idempotency_key=$3")
            .bind(workspace).bind(app).bind(&request.idempotency_key).fetch_optional(&mut *tx).await.map_err(email_storage)?;
        let email_id = if let Some(row) = old {
            if row.get::<String, _>("fingerprint") != fingerprint {
                return Err(ApiFailure::bad_request(
                    "email_idempotency_conflict",
                    "idempotency key was used for different content",
                ));
            }
            let status: String = row.get("status");
            let id: String = row.get("email_id");
            if status == "sent" {
                return Ok(SendAppEmailResponse {
                    email_id: id,
                    status,
                });
            }
            if status == "sending" && row.get::<bool, _>("recent") {
                return Err(ApiFailure::service_unavailable(
                    "email_in_progress",
                    "email delivery is in progress",
                ));
            }
            id
        } else {
            format!("email_{}", Uuid::new_v4().simple())
        };
        let counts = sqlx::query("SELECT count(*) FILTER(WHERE app_id=$2) AS app_count, count(*) FILTER(WHERE recipient_hash=$3) AS recipient_count FROM app_email_attempts WHERE workspace_id=$1 AND created_at > now()-interval '1 hour'")
            .bind(workspace).bind(app).bind(&hash).fetch_one(&mut *tx).await.map_err(email_storage)?;
        if counts.get::<i64, _>("app_count") >= 20 || counts.get::<i64, _>("recipient_count") >= 5 {
            return Err(ApiFailure {
                status: StatusCode::TOO_MANY_REQUESTS,
                error: ProtocolError::new(
                    "email_rate_limited",
                    "hourly App or recipient limit exceeded",
                ),
            });
        }
        sqlx::query("INSERT INTO app_email_sends(workspace_id,app_id,idempotency_key,email_id,fingerprint,status,updated_at) VALUES($1,$2,$3,$4,$5,'sending',now()) ON CONFLICT(workspace_id,app_id,idempotency_key) DO UPDATE SET status='sending',updated_at=now()")
            .bind(workspace).bind(app).bind(&request.idempotency_key).bind(&email_id).bind(fingerprint).execute(&mut *tx).await.map_err(email_storage)?;
        let attempt = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO app_email_attempts(attempt_id,workspace_id,app_id,recipient_hash,status) VALUES($1,$2,$3,$4,'sending')")
            .bind(&attempt).bind(workspace).bind(app).bind(&hash).execute(&mut *tx).await.map_err(email_storage)?;
        tx.commit().await.map_err(email_storage)?;
        // Generate HTML exclusively from escaped plain text. Provider errors never enter logs/API.
        let success = sender
            .send(
                &to,
                &request.subject,
                format!("<pre>{}</pre>", escape_html(&request.text)),
                request.text.clone(),
            )
            .await
            .is_ok();
        let status = if success { "sent" } else { "failed" };
        let mut tx = self.pool.begin().await.map_err(email_storage)?;
        sqlx::query("UPDATE app_email_sends SET status=$1,updated_at=now() WHERE email_id=$2")
            .bind(status)
            .bind(&email_id)
            .execute(&mut *tx)
            .await
            .map_err(email_storage)?;
        sqlx::query("UPDATE app_email_attempts SET status=$1 WHERE attempt_id=$2")
            .bind(status)
            .bind(attempt)
            .execute(&mut *tx)
            .await
            .map_err(email_storage)?;
        tx.commit().await.map_err(email_storage)?;
        if !success {
            return Err(ApiFailure::bad_gateway(
                "email_delivery_failed",
                "email provider delivery failed",
            ));
        }
        Ok(SendAppEmailResponse {
            email_id,
            status: status.into(),
        })
    }
}
fn email_storage(_: sqlx::Error) -> ApiFailure {
    ApiFailure::internal("email_storage_failed", "email storage unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_allowlist() {
        assert!(email_allowlisted("a@example.com", "@example.com"));
        assert!(email_allowlisted("a@example.com", "A@EXAMPLE.COM"));
        assert!(!email_allowlisted("a@sub.example.com", "@example.com"));
        assert!(!email_allowlisted("a@evil-example.com", "@example.com"));
        assert!(!valid_address("a@example.com\r\nBcc:b@evil.com"));
        assert!(serde_json::from_value::<SendAppEmailRequest>(json!({
            "to":"a@example.test", "subject":"test", "text":"text",
            "idempotency_key":"k", "from":"forged@example.test"
        }))
        .is_err());
    }
}
