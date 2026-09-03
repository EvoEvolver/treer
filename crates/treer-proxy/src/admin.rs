use std::collections::HashMap;

use axum::extract::{Extension, Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;

use crate::audit::{self, NewPlatformAuditEvent};
use crate::auth::{AuthFailure, AuthStore};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct AdminListQuery {
    q: Option<String>,
    limit: Option<u16>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/admin/dashboard", get(dashboard))
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users/{user_id}", get(get_user))
        .route(
            "/api/admin/users/{user_id}/password-reset",
            post(reset_user_password),
        )
        .route(
            "/api/admin/users/{user_id}/revoke-sessions",
            post(revoke_user_sessions),
        )
        .route("/api/admin/machines", get(list_machines))
        .route("/api/admin/machines/{server_id}", get(get_machine))
        .route("/api/admin/agents", get(list_agents))
        .route("/api/admin/organizations", get(list_organizations))
        .route(
            "/api/admin/invitations",
            get(list_invitations).post(create_invitation),
        )
        .route("/api/admin/invitations/{token}", delete(revoke_invitation))
        .route("/api/admin/activity", get(list_activity))
}

fn page_limit(limit: Option<u16>) -> i64 {
    i64::from(limit.unwrap_or(50).clamp(1, 100))
}

fn search_pattern(query: Option<&str>) -> Option<String> {
    let trimmed = query.map(str::trim).filter(|value| !value.is_empty())?;
    let escaped = trimmed
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Some(format!("%{escaped}%"))
}

async fn record(
    auth: &AuthStore,
    action: &str,
    resource_kind: &str,
    resource_id: &str,
    resource_name: Option<&str>,
    payload: Value,
) {
    if let Err(error) = audit::record_platform(
        &auth.pool(),
        NewPlatformAuditEvent {
            action,
            resource_kind,
            resource_id,
            resource_name,
            payload,
        },
    )
    .await
    {
        tracing::error!(%error, action, "failed to write platform audit event");
    }
}

async fn dashboard(
    Extension(auth): Extension<AuthStore>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AuthFailure> {
    Ok(Json(json!({
        "user_count": auth.user_count().await?,
        "organization_count": auth.organization_count().await?,
        "machine_count": auth.active_machine_count().await?,
        "agent_count": state.platform_agent_count().await,
    })))
}

async fn list_users(
    Extension(auth): Extension<AuthStore>,
    Query(query): Query<AdminListQuery>,
) -> Result<Json<Value>, AuthFailure> {
    let limit = page_limit(query.limit);
    let pattern = search_pattern(query.q.as_deref());
    let rows = sqlx::query(
        "SELECT id, email, preferred_name, email_verified, created_at \
         FROM users \
         WHERE $1::TEXT IS NULL \
            OR email ILIKE $1 ESCAPE '\\' \
            OR preferred_name ILIKE $1 ESCAPE '\\' \
         ORDER BY created_at DESC, id DESC \
         LIMIT $2",
    )
    .bind(pattern)
    .bind(limit)
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let users: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "user_id": row.get::<String, _>("id"),
                "email": row.get::<String, _>("email"),
                "preferred_name": row.get::<String, _>("preferred_name"),
                "email_verified": row.get::<bool, _>("email_verified"),
                "created_at": row.get::<String, _>("created_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "users": users })))
}

async fn get_user(
    Extension(auth): Extension<AuthStore>,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, AuthFailure> {
    let user = sqlx::query(
        "SELECT id, email, preferred_name, email_verified, created_at FROM users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&auth.pool())
    .await
    .map_err(AuthFailure::database)?
    .ok_or_else(|| AuthFailure::not_found("user_not_found", "user does not exist"))?;
    let organizations = sqlx::query(
        "SELECT o.organization_id, o.name, m.role \
         FROM organization_members m \
         JOIN organizations o ON o.organization_id = m.organization_id \
         WHERE m.user_id = $1 \
         ORDER BY lower(o.name)",
    )
    .bind(&user_id)
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let workspaces = sqlx::query(
        "SELECT w.workspace_id, w.name, w.organization_id \
         FROM workspaces w \
         JOIN organization_members m ON m.organization_id = w.organization_id \
         WHERE m.user_id = $1 AND w.deleted_at IS NULL \
         ORDER BY lower(w.name)",
    )
    .bind(&user_id)
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let oauth_providers = sqlx::query_scalar::<_, String>(
        "SELECT provider FROM oauth_identities WHERE user_id = $1 ORDER BY provider",
    )
    .bind(&user_id)
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    Ok(Json(json!({
        "user": {
            "user_id": user.get::<String, _>("id"),
            "email": user.get::<String, _>("email"),
            "preferred_name": user.get::<String, _>("preferred_name"),
            "email_verified": user.get::<bool, _>("email_verified"),
            "created_at": user.get::<String, _>("created_at"),
            "password_login": true,
            "oauth_providers": oauth_providers,
            "organizations": organizations.into_iter().map(|row| json!({
                "organization_id": row.get::<String, _>("organization_id"),
                "name": row.get::<String, _>("name"),
                "role": row.get::<String, _>("role"),
            })).collect::<Vec<_>>(),
            "workspaces": workspaces.into_iter().map(|row| json!({
                "workspace_id": row.get::<String, _>("workspace_id"),
                "name": row.get::<String, _>("name"),
                "organization_id": row.get::<String, _>("organization_id"),
            })).collect::<Vec<_>>(),
        }
    })))
}

async fn reset_user_password(
    Extension(auth): Extension<AuthStore>,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, AuthFailure> {
    let user = sqlx::query("SELECT id, email, preferred_name FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&auth.pool())
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("user_not_found", "user does not exist"))?;
    let email: String = user.get("email");
    let name: String = user.get("preferred_name");
    let pending = auth.create_password_reset(&email).await?.ok_or_else(|| {
        AuthFailure::too_many_requests(
            "password_reset_rate_limited",
            "a password reset was already issued recently",
        )
    })?;
    let emailed = auth.has_email_sender();
    if emailed {
        auth.spawn_password_reset_email(pending.recipient.clone(), pending.url.clone());
    }
    record(
        &auth,
        "user.password_reset_issued",
        "user",
        &user_id,
        Some(&name),
        json!({ "email": email, "emailed": emailed }),
    )
    .await;
    Ok(Json(json!({
        "url": pending.url.as_str(),
        "emailed": emailed,
    })))
}

async fn revoke_user_sessions(
    Extension(auth): Extension<AuthStore>,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, AuthFailure> {
    let name = sqlx::query_scalar::<_, String>("SELECT preferred_name FROM users WHERE id = $1")
        .bind(&user_id)
        .fetch_optional(&auth.pool())
        .await
        .map_err(AuthFailure::database)?
        .ok_or_else(|| AuthFailure::not_found("user_not_found", "user does not exist"))?;
    let result = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(&user_id)
        .execute(&auth.pool())
        .await
        .map_err(AuthFailure::database)?;
    record(
        &auth,
        "user.sessions_revoked",
        "user",
        &user_id,
        Some(&name),
        json!({ "revoked": result.rows_affected() }),
    )
    .await;
    Ok(Json(
        json!({ "ok": true, "revoked": result.rows_affected() }),
    ))
}

async fn list_machines(
    Extension(auth): Extension<AuthStore>,
    State(state): State<AppState>,
) -> Result<Json<Value>, AuthFailure> {
    let live: HashMap<_, _> = state
        .live_servers()
        .await
        .into_iter()
        .map(|server| (server.server_id.clone(), server))
        .collect();
    let rows = sqlx::query(
        "SELECT m.server_id, m.workspace_id, m.created_at, m.enrolled_by, \
                w.name AS workspace_name, n.name AS machine_name \
         FROM machines m \
         JOIN workspaces w ON w.workspace_id = m.workspace_id \
         LEFT JOIN machine_names n ON n.server_id = m.server_id \
         WHERE m.revoked_at IS NULL AND w.deleted_at IS NULL \
         ORDER BY coalesce(n.name, m.server_id), m.server_id",
    )
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let machines: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let server_id: String = row.get("server_id");
            let live = live.get(&server_id);
            json!({
                "server_id": server_id,
                "name": live.map(|server| server.name.clone()).filter(|name| !name.is_empty())
                    .or_else(|| row.get::<Option<String>, _>("machine_name"))
                    .unwrap_or_else(|| row.get::<String, _>("server_id")),
                "hostname": live.map(|server| server.hostname.clone()).unwrap_or_default(),
                "workspace_id": row.get::<String, _>("workspace_id"),
                "workspace_name": row.get::<String, _>("workspace_name"),
                "created_at": row.get::<String, _>("created_at"),
                "enrolled_by": row.get::<String, _>("enrolled_by"),
                "status": if live.is_some() { "online" } else { "offline" },
                "last_seen_at": live.map(|server| server.last_seen_at.to_rfc3339()),
                "root": live.map(|server| server.root.clone()),
            })
        })
        .collect();
    Ok(Json(json!({ "machines": machines })))
}

async fn get_machine(
    Extension(auth): Extension<AuthStore>,
    State(state): State<AppState>,
    Path(server_id): Path<String>,
) -> Result<Json<Value>, AuthFailure> {
    let row = sqlx::query(
        "SELECT m.server_id, m.workspace_id, m.created_at, m.enrolled_by, \
                w.name AS workspace_name, n.name AS machine_name \
         FROM machines m \
         JOIN workspaces w ON w.workspace_id = m.workspace_id \
         LEFT JOIN machine_names n ON n.server_id = m.server_id \
         WHERE m.server_id = $1 AND m.revoked_at IS NULL AND w.deleted_at IS NULL",
    )
    .bind(&server_id)
    .fetch_optional(&auth.pool())
    .await
    .map_err(AuthFailure::database)?
    .ok_or_else(|| AuthFailure::not_found("machine_not_found", "machine does not exist"))?;
    let live = state.live_server(&server_id).await;
    let agents = state.live_agents_on_server(&server_id).await;
    Ok(Json(json!({
        "machine": {
            "server_id": server_id,
            "name": live.as_ref().map(|server| server.name.clone()).filter(|name| !name.is_empty())
                .or_else(|| row.get::<Option<String>, _>("machine_name"))
                .unwrap_or_else(|| row.get::<String, _>("server_id")),
            "hostname": live.as_ref().map(|server| server.hostname.clone()).unwrap_or_default(),
            "workspace_id": row.get::<String, _>("workspace_id"),
            "workspace_name": row.get::<String, _>("workspace_name"),
            "created_at": row.get::<String, _>("created_at"),
            "enrolled_by": row.get::<String, _>("enrolled_by"),
            "status": if live.is_some() { "online" } else { "offline" },
            "last_seen_at": live.as_ref().map(|server| server.last_seen_at.to_rfc3339()),
            "root": live.as_ref().map(|server| server.root.clone()),
            "agents": agents,
        }
    })))
}

async fn list_agents(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "agents": state.live_agents().await }))
}

async fn list_organizations(
    Extension(auth): Extension<AuthStore>,
) -> Result<Json<Value>, AuthFailure> {
    let rows = sqlx::query(
        "SELECT o.organization_id, o.name, o.created_at, \
                u.id AS owner_id, u.preferred_name AS owner_name, u.email AS owner_email, \
                (SELECT COUNT(*) FROM workspaces w \
                    WHERE w.organization_id = o.organization_id AND w.deleted_at IS NULL) AS workspace_count, \
                (SELECT COUNT(*) FROM machines m \
                    JOIN workspaces w ON w.workspace_id = m.workspace_id \
                    WHERE w.organization_id = o.organization_id AND w.deleted_at IS NULL \
                    AND m.revoked_at IS NULL) AS machine_count \
         FROM organizations o \
         LEFT JOIN organization_members om ON om.organization_id = o.organization_id AND om.role = 'owner' \
         LEFT JOIN users u ON u.id = om.user_id \
         ORDER BY lower(o.name)",
    )
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let organizations: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "organization_id": row.get::<String, _>("organization_id"),
                "name": row.get::<String, _>("name"),
                "created_at": row.get::<String, _>("created_at"),
                "owner_id": row.get::<Option<String>, _>("owner_id"),
                "owner_name": row.get::<Option<String>, _>("owner_name"),
                "owner_email": row.get::<Option<String>, _>("owner_email"),
                "workspace_count": row.get::<i64, _>("workspace_count"),
                "machine_count": row.get::<i64, _>("machine_count"),
            })
        })
        .collect();
    Ok(Json(json!({ "organizations": organizations })))
}

async fn list_invitations(
    Extension(auth): Extension<AuthStore>,
) -> Result<Json<Value>, AuthFailure> {
    let rows = sqlx::query(
        "SELECT token, created_at FROM invitations \
         WHERE kind = 'personal' AND used_at IS NULL \
         ORDER BY created_at DESC",
    )
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let invitations: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            let token: String = row.get("token");
            let mut url = auth.app_public_url().clone();
            url.set_path("/");
            url.set_query(None);
            url.query_pairs_mut().append_pair("invite", &token);
            json!({
                "token": token,
                "created_at": row.get::<String, _>("created_at"),
                "url": url.as_str(),
            })
        })
        .collect();
    Ok(Json(json!({ "invitations": invitations })))
}

async fn create_invitation(
    Extension(auth): Extension<AuthStore>,
) -> Result<Json<Value>, AuthFailure> {
    let (token, url) = auth.create_personal_invitation().await?;
    record(
        &auth,
        "invitation.created",
        "invitation",
        &token,
        None,
        json!({ "kind": "personal" }),
    )
    .await;
    Ok(Json(json!({ "token": token, "url": url.as_str() })))
}

async fn revoke_invitation(
    Extension(auth): Extension<AuthStore>,
    Path(token): Path<String>,
) -> Result<Json<Value>, AuthFailure> {
    let result = sqlx::query(
        "DELETE FROM invitations WHERE token = $1 AND kind = 'personal' AND used_at IS NULL",
    )
    .bind(&token)
    .execute(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    if result.rows_affected() == 0 {
        return Err(AuthFailure::not_found(
            "invitation_not_found",
            "pending invitation does not exist",
        ));
    }
    record(
        &auth,
        "invitation.revoked",
        "invitation",
        &token,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

async fn list_activity(
    Extension(auth): Extension<AuthStore>,
    Query(query): Query<AdminListQuery>,
) -> Result<Json<Value>, AuthFailure> {
    let events = audit::list_platform(&auth.pool(), page_limit(query.limit))
        .await
        .map_err(AuthFailure::database)?;
    Ok(Json(json!({ "events": events })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tokio::sync::mpsc;
    use treer_protocol::{AgentInfo, AgentStatus, ServerInfo, ServerStatus};
    use uuid::Uuid;

    use crate::auth::AuthStore;
    use crate::state::AppState;

    async fn insert_user(store: &AuthStore, id: &str, email: &str, name: &str) {
        sqlx::query(
            "INSERT INTO users(id, email, preferred_name, password_hash, email_verified, created_at) \
             VALUES($1, $2, $3, 'hash', false, $4)",
        )
        .bind(id)
        .bind(email)
        .bind(name)
        .bind(Utc::now().to_rfc3339())
        .execute(&store.pool())
        .await
        .expect("insert user");
    }

    fn test_server(server_id: &str, workspace_id: &str) -> ServerInfo {
        let now = Utc::now();
        ServerInfo {
            server_id: server_id.to_string(),
            workspace_id: workspace_id.to_string(),
            name: "live-host".to_string(),
            hostname: "live-host.example".to_string(),
            root: "/tmp".to_string(),
            controller_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "controller".to_string(),
            },
            host_build: treer_protocol::BuildInfo {
                version: "0.1.2".to_string(),
                git_commit: "host".to_string(),
            },
            supervision: None,
            labels: Default::default(),
            available_agents: None,
            status: ServerStatus::Online,
            connected_at: now,
            last_seen_at: now,
        }
    }

    fn test_agent(agent_id: &str, server_id: &str, workspace_id: &str) -> AgentInfo {
        let now = Utc::now();
        AgentInfo {
            agent_id: agent_id.to_string(),
            workspace_id: workspace_id.to_string(),
            server_id: server_id.to_string(),
            kind: "command".to_string(),
            name: "reviewer".to_string(),
            cwd: ".".to_string(),
            status: AgentStatus::Idle,
            pid: None,
            started_at: now,
            updated_at: now,
            exited_at: None,
            exit_code: None,
            output_revision: 0,
            interface: None,
        }
    }

    #[tokio::test]
    async fn dashboard_counts_users_and_organizations() {
        let store = AuthStore::for_test("admin-password").await;
        store.seed_test_workspace("ws-a").await;
        insert_user(&store, "usr_a", "ada@example.com", "Ada").await;
        let state = AppState::new();
        let Json(body) = dashboard(Extension(store), State(state))
            .await
            .expect("dashboard");
        assert_eq!(body["user_count"], 1);
        assert_eq!(body["organization_count"], 1);
        assert_eq!(body["machine_count"], 0);
        assert_eq!(body["agent_count"], 0);
    }

    #[tokio::test]
    async fn users_can_be_searched_and_reset() {
        let store = AuthStore::for_test("admin-password").await;
        insert_user(&store, "usr_ada", "ada@example.com", "Ada").await;
        insert_user(&store, "usr_bob", "bob@example.com", "Bob").await;
        let Json(listed) = list_users(
            Extension(store.clone()),
            Query(AdminListQuery {
                q: Some("ada".to_string()),
                limit: None,
            }),
        )
        .await
        .expect("list users");
        let users = listed["users"].as_array().expect("users");
        assert_eq!(users.len(), 1);
        assert_eq!(users[0]["email"], "ada@example.com");

        let Json(reset) =
            reset_user_password(Extension(store.clone()), Path("usr_ada".to_string()))
                .await
                .expect("issue reset");
        assert!(reset["url"].as_str().unwrap().contains("reset="));
        assert_eq!(reset["emailed"], false);

        let again = reset_user_password(Extension(store.clone()), Path("usr_ada".to_string()))
            .await
            .expect_err("rate limited");
        assert_eq!(again.into_parts().1.code, "password_reset_rate_limited");
    }

    #[tokio::test]
    async fn sessions_can_be_revoked() {
        let store = AuthStore::for_test("admin-password").await;
        insert_user(&store, "usr_ada", "ada@example.com", "Ada").await;
        sqlx::query(
            "INSERT INTO sessions(token, user_id, created_at, expires_at) VALUES($1, $2, $3, $4)",
        )
        .bind("sess_1")
        .bind("usr_ada")
        .bind(Utc::now().to_rfc3339())
        .bind((Utc::now() + chrono::Duration::hours(1)).to_rfc3339())
        .execute(&store.pool())
        .await
        .expect("insert session");
        let Json(body) =
            revoke_user_sessions(Extension(store.clone()), Path("usr_ada".to_string()))
                .await
                .expect("revoke");
        assert_eq!(body["revoked"], 1);
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = 'usr_ada'")
                .fetch_one(&store.pool())
                .await
                .expect("count sessions");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn machines_join_enrollment_with_live_agents() {
        let store = AuthStore::for_test("admin-password").await;
        store.seed_test_workspace("ws-a").await;
        let enrollment = store
            .create_machine_enrollment("ws-a", "admin")
            .await
            .expect("enroll");
        let claimed = store
            .claim_machine_enrollment(&enrollment)
            .await
            .expect("claim");
        store
            .set_machine_name("ws-a", &claimed.server_id, "builder")
            .await
            .expect("name");

        let state = AppState::new();
        let Json(offline) = list_machines(Extension(store.clone()), State(state.clone()))
            .await
            .expect("offline list");
        assert_eq!(offline["machines"][0]["status"], "offline");
        assert_eq!(offline["machines"][0]["name"], "builder");

        let (tx, _rx) = mpsc::unbounded_channel();
        let mut server = test_server(&claimed.server_id, "ws-a");
        server.name = "builder".to_string();
        state
            .register_server(server, Uuid::new_v4(), tx)
            .await
            .expect("online");
        state
            .test_insert_agent(test_agent("ag_1", &claimed.server_id, "ws-a"))
            .await;

        let Json(online) = get_machine(
            Extension(store),
            State(state.clone()),
            Path(claimed.server_id.clone()),
        )
        .await
        .expect("machine");
        assert_eq!(online["machine"]["status"], "online");
        assert_eq!(online["machine"]["agents"][0]["name"], "reviewer");
        let Json(agents) = list_agents(State(state)).await;
        assert_eq!(agents["agents"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn personal_invitations_can_be_listed_and_revoked() {
        let store = AuthStore::for_test("admin-password").await;
        let (token, _) = store.create_personal_invitation().await.expect("invite");
        let Json(listed) = list_invitations(Extension(store.clone()))
            .await
            .expect("list");
        assert_eq!(listed["invitations"].as_array().unwrap().len(), 1);
        let Json(_) = revoke_invitation(Extension(store.clone()), Path(token))
            .await
            .expect("revoke");
        let Json(empty) = list_invitations(Extension(store)).await.expect("empty");
        assert!(empty["invitations"].as_array().unwrap().is_empty());
    }
}
