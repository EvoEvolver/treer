use super::*;

impl AuthStore {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn login(
        &self,
        email: &str,
        password: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        self.login_with_client(email, password, None).await
    }

    pub(super) async fn login_with_client(
        &self,
        email: &str,
        password: &str,
        client: Option<NativeClientAttribution>,
    ) -> Result<CurrentSession, AuthFailure> {
        let identifier = email.trim().to_ascii_lowercase();
        if identifier.is_empty()
            || identifier.len() > 254
            || identifier
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
        {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid email or password",
            ));
        }
        let row = sqlx::query(
            "SELECT id, email, preferred_name, password_hash FROM users \
             WHERE lower(email) = lower($1)",
        )
        .bind(&identifier)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let Some(row) = row else {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid email or password",
            ));
        };
        let password_hash: String = row.get("password_hash");
        if !verify_password(password, &password_hash) {
            return Err(AuthFailure::unauthorized(
                "invalid_credentials",
                "invalid email or password",
            ));
        }
        self.create_session(
            row.get("id"),
            row.get("email"),
            row.get("preferred_name"),
            client,
        )
        .await
    }

    pub(super) async fn request_password_reset(&self, email: &str) -> Result<(), AuthFailure> {
        normalize_email(email)?;
        let Some(sender) = &self.email_sender else {
            tracing::warn!("password reset requested but CLOUDFLARE_API_TOKEN is not configured");
            return Ok(());
        };
        let Some(pending) = self.create_password_reset(email).await? else {
            return Ok(());
        };
        let sender = sender.clone();
        let auth = self.clone();
        tokio::spawn(async move {
            if let Err(error) = sender
                .send_password_reset(&pending.recipient, &pending.url)
                .await
            {
                tracing::error!(%error, "failed to send password reset email");
                if let Err(error) = auth.revoke_password_reset(&pending.token_id).await {
                    tracing::error!(?error, "failed to revoke undelivered password reset token");
                }
            }
        });
        Ok(())
    }

    pub(crate) async fn create_password_reset(
        &self,
        email: &str,
    ) -> Result<Option<PendingPasswordReset>, AuthFailure> {
        let email = normalize_email(email)?;
        let token_id = format!("pwd_{}", Uuid::new_v4().simple());
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let secret_hash = hash_password(&secret)?;
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "DELETE FROM password_reset_tokens WHERE expires_at <= $1 \
             OR (used_at IS NOT NULL AND created_at <= $2)",
        )
        .bind(&now_text)
        .bind((now - Duration::days(1)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let user =
            sqlx::query("SELECT id, email FROM users WHERE lower(email) = lower($1) FOR UPDATE")
                .bind(email)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
        let Some(user) = user else {
            transaction.commit().await.map_err(AuthFailure::database)?;
            return Ok(None);
        };
        let user_id: String = user.get("id");
        let recent = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM password_reset_tokens \
             WHERE user_id = $1 AND used_at IS NULL AND created_at > $2",
        )
        .bind(&user_id)
        .bind((now - Duration::seconds(PASSWORD_RESET_RATE_LIMIT_SECONDS)).to_rfc3339())
        .fetch_one(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if recent > 0 {
            transaction.commit().await.map_err(AuthFailure::database)?;
            return Ok(None);
        }
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE user_id = $2 AND used_at IS NULL",
        )
        .bind(&now_text)
        .bind(&user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO password_reset_tokens(\
                 token_id, user_id, secret_hash, created_at, expires_at\
             ) VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&token_id)
        .bind(&user_id)
        .bind(secret_hash)
        .bind(&now_text)
        .bind((now + Duration::minutes(PASSWORD_RESET_TTL_MINUTES)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;

        let token = format!("{token_id}.{secret}");
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.set_query(None);
        url.set_fragment(None);
        url.query_pairs_mut().append_pair("reset", &token);
        Ok(Some(PendingPasswordReset {
            token_id,
            recipient: user.get("email"),
            url,
        }))
    }

    pub(super) async fn revoke_password_reset(&self, token_id: &str) -> Result<(), AuthFailure> {
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE token_id = $2 AND used_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(token_id)
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub(super) async fn reset_password(
        &self,
        token: &str,
        password: &str,
    ) -> Result<(), AuthFailure> {
        let password = validate_new_password(password)?;
        let (token_id, secret) = parse_password_reset_token(token)?;
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let row = sqlx::query(
            "SELECT user_id, secret_hash FROM password_reset_tokens \
             WHERE token_id = $1 AND used_at IS NULL AND expires_at > $2 FOR UPDATE",
        )
        .bind(token_id)
        .bind(&now)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_password_reset)?;
        let secret_hash: String = row.get("secret_hash");
        if !verify_password(secret, &secret_hash) {
            return Err(invalid_password_reset());
        }
        let user_id: String = row.get("user_id");
        let password_hash = hash_password(&password)?;
        sqlx::query("UPDATE users SET password_hash = $1, email_verified = TRUE WHERE id = $2")
            .bind(password_hash)
            .bind(&user_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE user_id = $2 AND used_at IS NULL",
        )
        .bind(&now)
        .bind(&user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(&user_id)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub(super) async fn create_session(
        &self,
        user_id: String,
        email: String,
        preferred_name: String,
        client: Option<NativeClientAttribution>,
    ) -> Result<CurrentSession, AuthFailure> {
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::days(SESSION_TTL_DAYS);
        sqlx::query("DELETE FROM sessions WHERE expires_at <= $1")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO sessions(token, user_id, created_at, expires_at, device_id, device_name, client) \
             VALUES($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&token)
        .bind(&user_id)
        .bind(now.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .bind(client.as_ref().and_then(|value| value.device_id.as_deref()))
        .bind(client.as_ref().and_then(|value| value.device_name.as_deref()))
        .bind(client.as_ref().map(|value| value.client.as_str()))
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(CurrentSession {
            token,
            user_id,
            email,
            preferred_name,
        })
    }

    pub(super) async fn session(&self, token: &str) -> Result<Option<CurrentSession>, AuthFailure> {
        let now = Utc::now().to_rfc3339();
        let row = sqlx::query(
            "SELECT u.id, u.email, u.preferred_name FROM sessions s \
             JOIN users u ON u.id = s.user_id WHERE s.token = $1 AND s.expires_at > $2",
        )
        .bind(token)
        .bind(now)
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok(row.map(|row| CurrentSession {
            token: token.to_string(),
            user_id: row.get("id"),
            email: row.get("email"),
            preferred_name: row.get("preferred_name"),
        }))
    }

    pub(super) async fn logout(&self, token: &str) -> Result<(), AuthFailure> {
        sqlx::query("DELETE FROM sessions WHERE token = $1")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub(super) async fn update_profile(
        &self,
        user_id: &str,
        email: &str,
        preferred_name: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        let email = normalize_email(email)?;
        let preferred_name = validate_preferred_name(preferred_name)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "UPDATE users SET email = $1, preferred_name = $2, \
             email_verified = CASE WHEN lower(email) = lower($1) \
                 THEN email_verified ELSE FALSE END WHERE id = $3",
        )
        .bind(&email)
        .bind(&preferred_name)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict("email_exists", "email is already registered")
            } else {
                AuthFailure::database(error)
            }
        })?;
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at = $1 \
             WHERE user_id = $2 AND used_at IS NULL",
        )
        .bind(Utc::now().to_rfc3339())
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(CurrentSession {
            token: String::new(),
            user_id: user_id.to_string(),
            email,
            preferred_name,
        })
    }

    pub(super) async fn admin_login(&self, password: &str) -> Result<AdminSession, AuthFailure> {
        if password != self.admin_password.as_ref() {
            return Err(AuthFailure::unauthorized(
                "invalid_admin_credentials",
                "invalid administrator password",
            ));
        }
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let expires_at = now + Duration::hours(ADMIN_SESSION_TTL_HOURS);
        sqlx::query("DELETE FROM admin_sessions WHERE expires_at <= $1")
            .bind(now.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query("INSERT INTO admin_sessions(token, created_at, expires_at) VALUES($1, $2, $3)")
            .bind(&token)
            .bind(now.to_rfc3339())
            .bind(expires_at.to_rfc3339())
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(AdminSession { token })
    }

    pub(super) async fn admin_session(
        &self,
        token: &str,
    ) -> Result<Option<AdminSession>, AuthFailure> {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM admin_sessions WHERE token = $1 AND expires_at > $2",
        )
        .bind(token)
        .bind(Utc::now().to_rfc3339())
        .fetch_one(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        Ok((exists != 0).then(|| AdminSession {
            token: token.to_string(),
        }))
    }

    pub(super) async fn admin_logout(&self, token: &str) -> Result<(), AuthFailure> {
        sqlx::query("DELETE FROM admin_sessions WHERE token = $1")
            .bind(token)
            .execute(&self.pool)
            .await
            .map_err(AuthFailure::database)?;
        Ok(())
    }

    pub(super) async fn create_invitation(
        &self,
        organization_id: &str,
        created_by: &str,
    ) -> Result<(String, Url), AuthFailure> {
        self.require_manager(organization_id, created_by).await?;
        let token = format!("inv_{}", Uuid::new_v4().simple());
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO invitations(\
             token, created_at, created_by, kind, organization_id, role) \
             VALUES($1, $2, $3, 'organization', $4, 'member')",
        )
        .bind(&token)
        .bind(Utc::now().to_rfc3339())
        .bind(created_by)
        .bind(organization_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(created_by),
                source: "api",
                action: "invitation.created",
                resource_kind: "invitation",
                resource_id: organization_id,
                resource_name: None,
                payload: json!({ "role": "member" }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", &token);
        Ok((token, url))
    }

    pub(crate) async fn create_personal_invitation(&self) -> Result<(String, Url), AuthFailure> {
        let token = format!("inv_{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO invitations(token, created_at, created_by, kind) \
             VALUES($1, $2, 'platform-admin', 'personal')",
        )
        .bind(&token)
        .bind(Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await
        .map_err(AuthFailure::database)?;
        let mut url = self.app_public_url.clone();
        url.set_path("/");
        url.query_pairs_mut().clear().append_pair("invite", &token);
        Ok((token, url))
    }

    pub async fn update_member_role(
        &self,
        organization_id: &str,
        actor: &str,
        target_user_id: &str,
        role: &str,
    ) -> Result<(), AuthFailure> {
        let actor_role = self
            .require_organization_member(organization_id, actor)
            .await?;
        if actor_role != "owner" {
            return Err(AuthFailure::forbidden(
                "organization_owner_required",
                "organization owner access required",
            ));
        }
        if !matches!(role, "admin" | "member") {
            return Err(AuthFailure::bad_request(
                "invalid_member_role",
                "member role must be admin or member",
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let target = sqlx::query(
            "SELECT m.role, u.preferred_name FROM organization_members m \
             JOIN users u ON u.id = m.user_id \
             WHERE m.organization_id = $1 AND m.user_id = $2 FOR UPDATE",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let old_role = target.as_ref().map(|row| row.get::<String, _>("role"));
        let target_name = target
            .as_ref()
            .map(|row| row.get::<String, _>("preferred_name"));
        let result = sqlx::query(
            "UPDATE organization_members SET role = $1 \
             WHERE organization_id = $2 AND user_id = $3 AND role != 'owner'",
        )
        .bind(role)
        .bind(organization_id)
        .bind(target_user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(AuthFailure::not_found(
                "member_not_found",
                "member does not exist or is the organization owner",
            ));
        }
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(actor),
                source: "api",
                action: "member.role_updated",
                resource_kind: "organization_member",
                resource_id: target_user_id,
                resource_name: target_name.as_deref(),
                payload: json!({ "old_role": old_role, "new_role": role }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn remove_member(
        &self,
        organization_id: &str,
        actor: &str,
        target_user_id: &str,
    ) -> Result<(), AuthFailure> {
        self.require_manager(organization_id, actor).await?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let target = sqlx::query(
            "SELECT m.role, u.preferred_name FROM organization_members m \
             JOIN users u ON u.id = m.user_id \
             WHERE m.organization_id = $1 AND m.user_id = $2 FOR UPDATE",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("member_not_found", "member does not exist"))?;
        let target_role: String = target.get("role");
        let target_name: String = target.get("preferred_name");
        if target_role == "owner" {
            return Err(AuthFailure::conflict(
                "owner_cannot_be_removed",
                "the organization owner cannot be removed",
            ));
        }
        sqlx::query(
            "DELETE FROM organization_group_members gm USING organization_groups g \
             WHERE gm.group_id = g.group_id AND g.organization_id = $1 AND gm.user_id = $2",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "DELETE FROM workspace_user_grants g USING workspaces w \
             WHERE g.workspace_id = w.workspace_id AND w.organization_id = $1 AND g.user_id = $2",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        sqlx::query(
            "DELETE FROM organization_members \
             WHERE organization_id = $1 AND user_id = $2",
        )
        .bind(organization_id)
        .bind(target_user_id)
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        audit::insert(
            &mut transaction,
            NewAuditEvent {
                organization_id,
                workspace_id: None,
                actor_kind: "user",
                actor_id: Some(actor),
                source: "api",
                action: "member.removed",
                resource_kind: "organization_member",
                resource_id: target_user_id,
                resource_name: Some(&target_name),
                payload: json!({ "role": target_role }),
            },
        )
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        Ok(())
    }

    pub async fn list_audit_events(
        &self,
        organization_id: &str,
        user_id: &str,
        workspace_id: Option<&str>,
        before: Option<i64>,
        limit: u16,
    ) -> Result<Vec<OrganizationAuditEvent>, AuthFailure> {
        self.require_manager(organization_id, user_id).await?;
        audit::list(
            &self.pool,
            organization_id,
            workspace_id,
            before,
            i64::from(limit.clamp(1, 100)),
        )
        .await
        .map_err(AuthFailure::database)
    }

    pub(super) fn oauth_public_config(&self) -> Value {
        json!({
            "github": self.oauth.github.is_some(),
            "google": self.oauth.google.is_some(),
            "invitation_required": self.oauth.invitation_required,
        })
    }

    pub(super) fn oauth_callback_url(&self, provider: &str) -> Url {
        let mut url = self.proxy_public_url.clone();
        url.set_path(&format!("/api/auth/oauth/{provider}/callback"));
        url.set_query(None);
        url.set_fragment(None);
        url
    }

    pub(super) async fn oauth_authorization_url(
        &self,
        provider: &str,
        invite: Option<&str>,
    ) -> Result<Url, AuthFailure> {
        let config = self
            .oauth
            .provider(provider)
            .ok_or_else(oauth_provider_unavailable)?;
        if invite.is_some_and(|value| value.is_empty() || value.len() > 256) {
            return Err(invalid_invitation());
        }
        let state = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let now = Utc::now();
        let now_text = now.to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        sqlx::query("DELETE FROM oauth_states WHERE expires_at <= $1")
            .bind(&now_text)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
        sqlx::query(
            "INSERT INTO oauth_states(state, provider, invite_token, created_at, expires_at) \
             VALUES($1, $2, $3, $4, $5)",
        )
        .bind(&state)
        .bind(provider)
        .bind(invite)
        .bind(&now_text)
        .bind((now + Duration::minutes(OAUTH_STATE_TTL_MINUTES)).to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        transaction.commit().await.map_err(AuthFailure::database)?;

        let callback_url = self.oauth_callback_url(provider);
        let mut url = config.authorize_url.clone();
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", callback_url.as_str())
            .append_pair("response_type", "code")
            .append_pair("state", &state);
        match provider {
            "github" => {
                query.append_pair("scope", "user:email");
            }
            "google" => {
                query
                    .append_pair("scope", "openid email profile")
                    .append_pair("prompt", "select_account");
            }
            _ => return Err(oauth_provider_unavailable()),
        }
        drop(query);
        Ok(url)
    }

    pub(super) async fn consume_oauth_state(
        &self,
        provider: &str,
        state: &str,
    ) -> Result<Option<String>, AuthFailure> {
        if state.len() != 64 || !state.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_oauth_state());
        }
        sqlx::query_scalar::<_, Option<String>>(
            "DELETE FROM oauth_states WHERE state = $1 AND provider = $2 AND expires_at > $3 \
             RETURNING invite_token",
        )
        .bind(state)
        .bind(provider)
        .bind(Utc::now().to_rfc3339())
        .fetch_optional(&self.pool)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_oauth_state)
    }

    pub(super) async fn exchange_oauth_code(
        &self,
        provider: &str,
        code: &str,
    ) -> Result<OAuthProfile, AuthFailure> {
        if code.is_empty() || code.len() > 2048 {
            return Err(oauth_login_failed());
        }
        let config = self
            .oauth
            .provider(provider)
            .cloned()
            .ok_or_else(oauth_provider_unavailable)?;
        let callback_url = self.oauth_callback_url(provider);
        let response = self
            .oauth_client
            .post(config.token_url.clone())
            .header(header::ACCEPT, "application/json")
            .form(&[
                ("client_id", config.client_id.as_ref()),
                ("client_secret", config.client_secret.as_ref()),
                ("code", code),
                ("redirect_uri", callback_url.as_str()),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .map_err(oauth_request_failed)?;
        let status = response.status();
        let token = response
            .json::<OAuthTokenResponse>()
            .await
            .map_err(oauth_request_failed)?;
        let Some(access_token) = token.access_token else {
            tracing::warn!(
                provider,
                %status,
                error = token.error.as_deref().unwrap_or("unknown"),
                description = token.error_description.as_deref().unwrap_or(""),
                "OAuth token exchange failed"
            );
            return Err(oauth_login_failed());
        };
        if !status.is_success() {
            tracing::warn!(provider, %status, "OAuth token endpoint returned an error");
            return Err(oauth_login_failed());
        }
        match provider {
            "github" => self.github_profile(&config, &access_token).await,
            "google" => self.google_profile(&config, &access_token).await,
            _ => Err(oauth_provider_unavailable()),
        }
    }

    pub(super) async fn github_profile(
        &self,
        config: &OAuthProviderConfig,
        access_token: &str,
    ) -> Result<OAuthProfile, AuthFailure> {
        let user_response = self
            .oauth_client
            .get(config.user_url.clone())
            .bearer_auth(access_token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(oauth_request_failed)?;
        if !user_response.status().is_success() {
            tracing::warn!(status = %user_response.status(), "GitHub user API failed");
            return Err(oauth_login_failed());
        }
        let user = user_response
            .json::<GithubUser>()
            .await
            .map_err(oauth_request_failed)?;
        let emails_url = config
            .emails_url
            .clone()
            .ok_or_else(oauth_provider_unavailable)?;
        let emails_response = self
            .oauth_client
            .get(emails_url)
            .bearer_auth(access_token)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(oauth_request_failed)?;
        if !emails_response.status().is_success() {
            tracing::warn!(status = %emails_response.status(), "GitHub email API failed");
            return Err(verified_email_required());
        }
        let emails = emails_response
            .json::<Vec<GithubEmail>>()
            .await
            .map_err(oauth_request_failed)?;
        let email = emails
            .iter()
            .find(|email| email.primary && email.verified)
            .or_else(|| emails.iter().find(|email| email.verified))
            .ok_or_else(verified_email_required)?;
        let email = normalize_email(&email.email)?;
        let preferred_name = provider_preferred_name(user.name.as_deref(), &user.login, &email)?;
        Ok(OAuthProfile {
            provider: "github",
            subject: user.id.to_string(),
            email,
            preferred_name,
        })
    }

    pub(super) async fn google_profile(
        &self,
        config: &OAuthProviderConfig,
        access_token: &str,
    ) -> Result<OAuthProfile, AuthFailure> {
        let response = self
            .oauth_client
            .get(config.user_url.clone())
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(oauth_request_failed)?;
        if !response.status().is_success() {
            tracing::warn!(status = %response.status(), "Google UserInfo API failed");
            return Err(oauth_login_failed());
        }
        let user = response
            .json::<GoogleUser>()
            .await
            .map_err(oauth_request_failed)?;
        if !user.email_verified {
            return Err(verified_email_required());
        }
        let email = normalize_email(&user.email)?;
        let fallback = email.split('@').next().unwrap_or("Treer user");
        let preferred_name = provider_preferred_name(user.name.as_deref(), fallback, &email)?;
        if user.sub.is_empty() || user.sub.len() > 255 {
            return Err(oauth_login_failed());
        }
        Ok(OAuthProfile {
            provider: "google",
            subject: user.sub,
            email,
            preferred_name,
        })
    }

    pub(super) async fn complete_oauth_login(
        &self,
        profile: OAuthProfile,
        invite: Option<&str>,
    ) -> Result<CurrentSession, AuthFailure> {
        let now = Utc::now().to_rfc3339();
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let linked = sqlx::query(
            "SELECT u.id, u.email, u.preferred_name FROM oauth_identities i \
             JOIN users u ON u.id = i.user_id \
             WHERE i.provider = $1 AND i.subject = $2 FOR UPDATE",
        )
        .bind(profile.provider)
        .bind(&profile.subject)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        if let Some(user) = linked {
            sqlx::query(
                "UPDATE oauth_identities SET email = $1, updated_at = $2 \
                 WHERE provider = $3 AND subject = $4",
            )
            .bind(&profile.email)
            .bind(&now)
            .bind(profile.provider)
            .bind(&profile.subject)
            .execute(&mut *transaction)
            .await
            .map_err(AuthFailure::database)?;
            let user_id = user.get("id");
            let email = user.get("email");
            let preferred_name = user.get("preferred_name");
            transaction.commit().await.map_err(AuthFailure::database)?;
            return self
                .create_session(user_id, email, preferred_name, None)
                .await;
        }

        let existing_user = sqlx::query(
            "SELECT id, email, preferred_name, email_verified FROM users \
             WHERE lower(email) = lower($1) FOR UPDATE",
        )
        .bind(&profile.email)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(AuthFailure::database)?;
        let (user_id, email, preferred_name, created) = if let Some(user) = existing_user {
            if !user.get::<bool, _>("email_verified") {
                let password_hash = hash_password(&format!(
                    "{}{}",
                    Uuid::new_v4().simple(),
                    Uuid::new_v4().simple()
                ))?;
                let user_id: String = user.get("id");
                sqlx::query(
                    "UPDATE users SET password_hash = $1, email_verified = TRUE WHERE id = $2",
                )
                .bind(password_hash)
                .bind(&user_id)
                .execute(&mut *transaction)
                .await
                .map_err(AuthFailure::database)?;
                sqlx::query("DELETE FROM sessions WHERE user_id = $1")
                    .bind(&user_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(AuthFailure::database)?;
            }
            (
                user.get("id"),
                user.get("email"),
                user.get("preferred_name"),
                false,
            )
        } else {
            let invitation = self
                .load_registration_invitation(&mut transaction, invite)
                .await?;
            let password_hash = hash_password(&format!(
                "{}{}",
                Uuid::new_v4().simple(),
                Uuid::new_v4().simple()
            ))?;
            let user_id = insert_user(
                &mut transaction,
                &profile.email,
                &profile.preferred_name,
                password_hash,
                true,
                &now,
            )
            .await?;
            apply_registration_membership(
                &mut transaction,
                invitation,
                &user_id,
                &profile.preferred_name,
                &now,
            )
            .await?;
            (
                user_id,
                profile.email.clone(),
                profile.preferred_name.clone(),
                true,
            )
        };
        sqlx::query(
            "INSERT INTO oauth_identities(provider, subject, user_id, email, created_at, updated_at) \
             VALUES($1, $2, $3, $4, $5, $5)",
        )
        .bind(profile.provider)
        .bind(&profile.subject)
        .bind(&user_id)
        .bind(&profile.email)
        .bind(&now)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if error
                .as_database_error()
                .is_some_and(|error| error.is_unique_violation())
            {
                AuthFailure::conflict(
                    "oauth_identity_conflict",
                    "this OAuth identity is already linked",
                )
            } else {
                AuthFailure::database(error)
            }
        })?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let session = self
            .create_session(user_id, email, preferred_name, None)
            .await?;
        if created {
            self.send_welcome_email(&session);
        }
        Ok(session)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) async fn register(
        &self,
        invite: Option<&str>,
        email: &str,
        preferred_name: &str,
        password: &str,
    ) -> Result<CurrentSession, AuthFailure> {
        let email = normalize_email(email)?;
        let preferred_name = validate_preferred_name(preferred_name)?;
        let password = validate_new_password(password)?;
        let password_hash = hash_password(&password)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let invitation = self
            .load_registration_invitation(&mut transaction, invite)
            .await?;
        let now = Utc::now().to_rfc3339();
        let user_id = insert_user(
            &mut transaction,
            &email,
            &preferred_name,
            password_hash,
            false,
            &now,
        )
        .await?;
        apply_registration_membership(
            &mut transaction,
            invitation,
            &user_id,
            &preferred_name,
            &now,
        )
        .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let session = self
            .create_session(user_id, email, preferred_name, None)
            .await?;
        self.send_welcome_email(&session);
        Ok(session)
    }

    pub(super) async fn register_with_client(
        &self,
        invite: Option<&str>,
        email: &str,
        preferred_name: &str,
        password: &str,
        client: Option<NativeClientAttribution>,
    ) -> Result<CurrentSession, AuthFailure> {
        let email = normalize_email(email)?;
        let preferred_name = validate_preferred_name(preferred_name)?;
        let password = validate_new_password(password)?;
        let password_hash = hash_password(&password)?;
        let mut transaction = self.pool.begin().await.map_err(AuthFailure::database)?;
        let invitation = self
            .load_registration_invitation(&mut transaction, invite)
            .await?;
        let now = Utc::now().to_rfc3339();
        let user_id = insert_user(
            &mut transaction,
            &email,
            &preferred_name,
            password_hash,
            false,
            &now,
        )
        .await?;
        apply_registration_membership(
            &mut transaction,
            invitation,
            &user_id,
            &preferred_name,
            &now,
        )
        .await?;
        transaction.commit().await.map_err(AuthFailure::database)?;
        let session = self
            .create_session(user_id, email, preferred_name, client)
            .await?;
        self.send_welcome_email(&session);
        Ok(session)
    }

    pub(super) async fn load_registration_invitation(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        invite: Option<&str>,
    ) -> Result<Option<RegistrationInvitation>, AuthFailure> {
        let Some(invite) = invite.filter(|value| !value.is_empty()) else {
            if self.oauth.invitation_required {
                return Err(AuthFailure::bad_request(
                    "invitation_required",
                    "a valid invitation is required to create an account",
                ));
            }
            return Ok(None);
        };
        let invitation = sqlx::query(
            "SELECT kind, organization_id, role FROM invitations \
             WHERE token = $1 AND used_at IS NULL FOR UPDATE",
        )
        .bind(invite)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(invalid_invitation)?;
        Ok(Some(RegistrationInvitation {
            token: invite.to_string(),
            kind: invitation.get("kind"),
            organization_id: invitation.get("organization_id"),
            role: invitation.get("role"),
        }))
    }

    pub(super) fn send_welcome_email(&self, session: &CurrentSession) {
        let Some(sender) = self.email_sender.clone() else {
            return;
        };
        let recipient = session.email.clone();
        let preferred_name = session.preferred_name.clone();
        let app_url = self.app_public_url.clone();
        tokio::spawn(async move {
            if let Err(error) = sender
                .send_welcome(&recipient, &preferred_name, &app_url)
                .await
            {
                tracing::error!(%error, "failed to send registration welcome email");
            }
        });
    }
}

pub(super) async fn insert_user(
    transaction: &mut Transaction<'_, Postgres>,
    email: &str,
    preferred_name: &str,
    password_hash: String,
    email_verified: bool,
    now: &str,
) -> Result<String, AuthFailure> {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO users(id, email, preferred_name, password_hash, email_verified, created_at) \
         VALUES($1, $2, $3, $4, $5, $6)",
    )
    .bind(&user_id)
    .bind(email)
    .bind(preferred_name)
    .bind(password_hash)
    .bind(email_verified)
    .bind(now)
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .is_some_and(|error| error.is_unique_violation())
        {
            AuthFailure::conflict("email_exists", "email is already registered")
        } else {
            AuthFailure::database(error)
        }
    })?;
    Ok(user_id)
}

pub(super) async fn apply_registration_membership(
    transaction: &mut Transaction<'_, Postgres>,
    invitation: Option<RegistrationInvitation>,
    user_id: &str,
    preferred_name: &str,
    now: &str,
) -> Result<(), AuthFailure> {
    if let Some(invitation) = &invitation {
        let result = sqlx::query(
            "UPDATE invitations SET used_at = $1, used_by = $2 \
             WHERE token = $3 AND used_at IS NULL",
        )
        .bind(now)
        .bind(user_id)
        .bind(&invitation.token)
        .execute(&mut **transaction)
        .await
        .map_err(AuthFailure::database)?;
        if result.rows_affected() != 1 {
            return Err(invalid_invitation());
        }
    }

    match invitation.as_ref().map(|value| value.kind.as_str()) {
        None | Some("personal") => {
            let organization_id = format!("org_{}", Uuid::new_v4().simple());
            let organization_name = format!("{preferred_name} Personal");
            sqlx::query(
                "INSERT INTO organizations(organization_id, name, created_at, created_by) \
                 VALUES($1, $2, $3, $4)",
            )
            .bind(&organization_id)
            .bind(organization_name)
            .bind(now)
            .bind(user_id)
            .execute(&mut **transaction)
            .await
            .map_err(AuthFailure::database)?;
            sqlx::query(
                "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
                 VALUES($1, $2, 'owner', $3)",
            )
            .bind(organization_id)
            .bind(user_id)
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        Some("organization") => {
            let invitation = invitation.as_ref().expect("matched invitation");
            let organization_id = invitation.organization_id.as_deref().ok_or_else(|| {
                AuthFailure::internal(
                    "invalid_invitation_state",
                    "organization invitation has no organization".to_string(),
                )
            })?;
            let role = invitation.role.as_deref().ok_or_else(|| {
                AuthFailure::internal(
                    "invalid_invitation_state",
                    "organization invitation has no role".to_string(),
                )
            })?;
            sqlx::query(
                "INSERT INTO organization_members(organization_id, user_id, role, joined_at) \
                 VALUES($1, $2, $3, $4)",
            )
            .bind(organization_id)
            .bind(user_id)
            .bind(role)
            .bind(now)
            .execute(&mut **transaction)
            .await
            .map_err(AuthFailure::database)?;
        }
        Some(_) => {
            return Err(AuthFailure::internal(
                "invalid_invitation_state",
                "invitation has an unsupported kind".to_string(),
            ));
        }
    }
    Ok(())
}
