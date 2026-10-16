use axum::extract::{Extension, Path, Query, State};
use axum::routing::{any, delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;

use crate::audit::{self, NewPlatformAuditEvent};
use crate::auth::{AuthFailure, AuthStore};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    let mut router = Router::new()
        .route("/api/admin/dashboard", get(dashboard))
        .route("/api/admin/traffic", get(platform_traffic))
        .route(
            "/api/admin/invitations",
            get(list_invitations).post(create_invitation),
        )
        .route(
            "/api/admin/invitations/{invitation_id}",
            delete(revoke_invitation),
        );
    // Tombstones also cover old mutation URLs and prevent ingress fallback from
    // interpreting removed admin routes as a service request. Never inspect IDs.
    for resource in ["users", "machines", "agents", "organizations", "activity"] {
        let path = format!("/api/admin/{resource}");
        router = router
            .route(&path, any(details_removed))
            .route(&format!("{path}/{{*rest}}"), any(details_removed));
    }
    router.layer(axum::middleware::map_response(
        |mut response: axum::response::Response| async move {
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        },
    ))
}

fn invitation_id(token: &str) -> String {
    format!(
        "inv_{:x}",
        Sha256::digest(format!("treer.admin-invitation/v1:{token}"))
    )
}

async fn details_removed() -> (axum::http::StatusCode, Json<Value>) {
    (
        axum::http::StatusCode::GONE,
        Json(
            json!({"error":{"code":"admin_details_removed","message":"Platform administration exposes aggregate statistics only"}}),
        ),
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlatformTrafficQuery {
    #[serde(default = "default_hours", deserialize_with = "traffic_hours")]
    hours: u16,
}

fn default_hours() -> u16 {
    24
}

fn traffic_hours<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u16, D::Error> {
    let hours = u16::deserialize(deserializer)?;
    if matches!(hours, 6 | 24 | 168 | 720) {
        Ok(hours)
    } else {
        Err(serde::de::Error::custom("hours must be 6, 24, 168 or 720"))
    }
}

async fn platform_traffic(
    Extension(auth): Extension<AuthStore>,
    Query(query): Query<PlatformTrafficQuery>,
) -> Result<Json<Value>, AuthFailure> {
    // Only persisted machine-level metering: all Proxies see shared PG totals.
    // Agent-level metering mirrors these bytes and must not be added again.
    let now = chrono::Utc::now().timestamp();
    let cutoff = now.div_euclid(3600) * 3600 - i64::from(query.hours - 1) * 3600;
    let rows = sqlx::query(
        "SELECT window_start, traffic_class, \
         CAST(SUM(payload_bytes) AS BIGINT) AS payload_bytes, \
         CAST(SUM(payload_frames) AS BIGINT) AS payload_frames, \
         CAST(SUM(billable_bytes) AS BIGINT) AS billable_bytes FROM (\
           SELECT window_start, traffic_class, payload_bytes, payload_frames, billable_bytes \
           FROM traffic_usage_hourly WHERE window_start >= $1 AND window_start <= $2 \
           UNION ALL \
           SELECT window_start, 'virtual_network', payload_bytes, payload_frames, payload_bytes \
           FROM machine_traffic_hourly WHERE window_start >= $1 AND window_start <= $2\
         ) AS usage GROUP BY window_start, traffic_class \
         ORDER BY window_start DESC, traffic_class",
    )
    .bind(cutoff)
    .bind(now)
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let traffic = rows
        .into_iter()
        .map(
            |row| -> Result<treer_protocol::PlatformTrafficRecord, AuthFailure> {
                let timestamp: i64 = row.get("window_start");
                let window_start =
                    chrono::DateTime::from_timestamp(timestamp, 0).ok_or_else(|| {
                        AuthFailure::database(sqlx::Error::Protocol(
                            "invalid traffic timestamp".into(),
                        ))
                    })?;
                Ok(treer_protocol::PlatformTrafficRecord {
                    window_start,
                    traffic_class: row.get("traffic_class"),
                    payload_bytes: row.get::<i64, _>("payload_bytes") as u64,
                    payload_frames: row.get::<i64, _>("payload_frames") as u64,
                    billable_bytes: row.get::<i64, _>("billable_bytes") as u64,
                })
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({"hours":query.hours,"traffic":traffic})))
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

async fn list_invitations(
    Extension(auth): Extension<AuthStore>,
) -> Result<Json<Value>, AuthFailure> {
    let rows = sqlx::query(
        "SELECT 'inv_' || encode(sha256(convert_to('treer.admin-invitation/v1:' || token, 'UTF8')), 'hex') AS invitation_id, \
         CASE WHEN token ~ '^[A-Za-z0-9_-]{32,}$' THEN left(token, 6) ELSE '[redacted]' END AS token_prefix, \
         created_at, used_at FROM invitations WHERE kind = 'personal' \
         ORDER BY created_at DESC, token DESC LIMIT 100",
    )
    .fetch_all(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    let invitations: Vec<treer_protocol::PlatformInvitationRecord> = rows
        .into_iter()
        .map(|row| treer_protocol::PlatformInvitationRecord {
            invitation_id: row.get("invitation_id"),
            token_prefix: row.get("token_prefix"),
            created_at: row.get("created_at"),
            used_at: row.get("used_at"),
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
        &invitation_id(&token),
        None,
        json!({ "kind": "personal" }),
    )
    .await;
    Ok(Json(json!({ "token": token, "url": url.as_str() })))
}

async fn revoke_invitation(
    Extension(auth): Extension<AuthStore>,
    Path(invitation_id): Path<String>,
) -> Result<Json<Value>, AuthFailure> {
    let not_found =
        || AuthFailure::not_found("invitation_not_found", "pending invitation does not exist");
    if !invitation_id.strip_prefix("inv_").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        return Err(not_found());
    }
    let result = sqlx::query(
        "DELETE FROM invitations WHERE kind = 'personal' AND used_at IS NULL \
         AND 'inv_' || encode(sha256(convert_to('treer.admin-invitation/v1:' || token, 'UTF8')), 'hex') = $1",
    )
    .bind(&invitation_id)
    .execute(&auth.pool())
    .await
    .map_err(AuthFailure::database)?;
    if result.rows_affected() == 0 {
        return Err(not_found());
    }
    record(
        &auth,
        "invitation.revoked",
        "invitation",
        &invitation_id,
        None,
        json!({}),
    )
    .await;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;
    use crate::state::AppState;
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
        middleware,
    };
    use chrono::Utc;
    use tower::ServiceExt;

    fn test_router(store: &AuthStore) -> Router {
        routes()
            .route_layer(middleware::from_fn_with_state(
                store.clone(),
                crate::auth::require_admin,
            ))
            .route(
                "/api/admin/login",
                axum::routing::post(crate::auth::admin_login),
            )
            .layer(Extension(store.clone()))
            .with_state(AppState::new())
    }

    async fn admin_cookie(app: &Router) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"password":"admin-password"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn aggregate_traffic_requires_admin_cookie_and_rejects_identity_filters() {
        let store = AuthStore::for_test("admin-password").await;
        let app = test_router(&store);
        for (header, value) in [
            ("authorization", "Bearer synthetic-user-or-oauth-token"),
            ("cookie", "treer_session=synthetic-user-token"),
            ("x-treer-operator-credential", "synthetic-operator-token"),
            ("x-treer-workload-credential", "synthetic-workload-token"),
            ("cookie", "treer_admin_session=invalid-session"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/api/admin/traffic")
                        .header(header, value)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let cookie = admin_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/traffic")
                    .header(
                        "authorization",
                        format!("Bearer {}", cookie.split_once('=').unwrap().1),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "even a valid admin session must use the cookie transport"
        );
        for hours in ["6", "24", "168", "720"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/admin/traffic?hours={hours}"))
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        for query in [
            "hours=0",
            "hours=1",
            "hours=721",
            "hours=-1",
            "hours=x",
            "hours=6&workspace_id=secret",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/admin/traffic?{query}"))
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }

    #[tokio::test]
    async fn removed_admin_details_and_mutations_never_expose_resource_existence() {
        let store = AuthStore::for_test("admin-password").await;
        insert_user(&store, "usr_private", "private@example.test", "Private").await;
        let app = test_router(&store);
        let cookie = admin_cookie(&app).await;
        let mut expected = None;
        for path in [
            "users",
            "users/usr_private",
            "users/missing",
            "users/usr_private/password-reset",
            "users/usr_private/revoke-sessions",
            "machines",
            "machines/srv_private",
            "agents",
            "organizations",
            "activity",
        ] {
            for method in ["GET", "POST", "DELETE"] {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(format!("/api/admin/{path}"))
                            .header("cookie", &cookie)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::GONE);
                let body = to_bytes(response.into_body(), 4096).await.unwrap();
                if let Some(expected) = &expected {
                    assert_eq!(&body, expected);
                } else {
                    expected = Some(body);
                }
            }
        }
        let resets: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM password_reset_tokens")
            .fetch_one(&store.pool())
            .await
            .unwrap();
        assert_eq!(resets, 0);
    }

    #[tokio::test]
    async fn platform_traffic_sums_workspaces_and_legacy_rows_without_agent_double_counting() {
        let store = AuthStore::for_test("admin-password").await;
        for ws in ["ws-a", "ws-b"] {
            store.seed_test_workspace(ws).await;
        }
        let now = Utc::now().timestamp();
        let hour = now.div_euclid(3600) * 3600;
        for (ws, bytes, timestamp, class) in [
            ("ws-a", 10i64, hour, "virtual_network"),
            ("ws-b", 20, hour, "virtual_network"),
            ("ws-b", 7, hour, "service_ingress"),
            ("ws-a", 100, hour - 6 * 3600, "service_ingress"),
            ("ws-a", 1000, hour + 3600, "service_ingress"),
        ] {
            sqlx::query("INSERT INTO traffic_usage_hourly(workspace_id,window_start,traffic_class,source_type,source_id,destination_type,destination_id,payload_bytes,payload_frames,billable_bytes,meter_version,updated_at) VALUES($1,$2,$3,'machine','srv_private','client','private-client',$4,1,$4,1,'test')")
                .bind(ws).bind(timestamp).bind(class).bind(bytes).execute(&store.pool()).await.unwrap();
        }
        sqlx::query("INSERT INTO machine_traffic_hourly(workspace_id,window_start,source_server_id,destination_server_id,payload_bytes,payload_frames,updated_at) VALUES('ws-a',$1,'legacy-private','private-dest',5,2,'test')").bind(hour).execute(&store.pool()).await.unwrap();
        // Mirrored Agent metering is deliberately excluded from platform totals.
        sqlx::query("INSERT INTO agent_traffic_usage_hourly(workspace_id,window_start,traffic_class,source_type,source_id,destination_type,destination_id,payload_bytes,payload_frames,billable_bytes,meter_version,updated_at) VALUES('ws-a',$1,'virtual_network','agent','ag_private','agent','ag_dest',9999,1,9999,1,'test')").bind(hour).execute(&store.pool()).await.unwrap();
        let Json(body) = platform_traffic(
            Extension(store.clone()),
            Query(PlatformTrafficQuery { hours: 6 }),
        )
        .await
        .unwrap();
        let records = body["traffic"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        let network = records
            .iter()
            .find(|v| v["traffic_class"] == "virtual_network")
            .unwrap();
        assert_eq!(network["payload_bytes"], 35);
        assert_eq!(network["billable_bytes"], 35);
        assert_eq!(network["payload_frames"], 4);
        assert_eq!(
            records
                .iter()
                .find(|v| v["traffic_class"] == "service_ingress")
                .unwrap()["payload_bytes"],
            7
        );
        for record in records {
            let keys: std::collections::BTreeSet<_> = record
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                [
                    "window_start",
                    "traffic_class",
                    "payload_bytes",
                    "payload_frames",
                    "billable_bytes"
                ]
                .into_iter()
                .collect()
            );
            let decoded: treer_protocol::PlatformTrafficRecord =
                serde_json::from_value(record.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), *record);
        }
        let encoded = body.to_string();
        for fragment in ["ws-a", "ws-b", "private", "ag_dest"] {
            assert!(!encoded.contains(fragment));
        }
        // Independent Proxy state objects use the same persisted database view.
        let app_a = test_router(&store);
        let app_b = test_router(&store);
        let cookie = admin_cookie(&app_a).await;
        let mut responses = Vec::new();
        for app in [app_a, app_b] {
            let response = app
                .oneshot(
                    Request::builder()
                        .uri("/api/admin/traffic?hours=6")
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            responses.push(to_bytes(response.into_body(), 65536).await.unwrap());
        }
        assert_eq!(responses[0], responses[1]);
    }

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
    async fn personal_invitations_can_be_listed_and_revoked() {
        let store = AuthStore::for_test("admin-password").await;
        let (token, _) = store.create_personal_invitation().await.expect("invite");
        let Json(listed) = list_invitations(Extension(store.clone()))
            .await
            .expect("list");
        assert_eq!(listed["invitations"].as_array().unwrap().len(), 1);
        let Json(_) = revoke_invitation(Extension(store.clone()), Path(invitation_id(&token)))
            .await
            .expect("revoke");
        let Json(empty) = list_invitations(Extension(store)).await.expect("empty");
        assert!(empty["invitations"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn invitation_listing_redacts_bearers_and_revoke_uses_only_admin_handle() {
        let store = AuthStore::for_test("admin-password").await;
        let (pending, _) = store.create_personal_invitation().await.unwrap();
        let (used, _) = store.create_personal_invitation().await.unwrap();
        sqlx::query("UPDATE invitations SET used_at='used-time' WHERE token=$1")
            .bind(&used)
            .execute(&store.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO invitations(token,created_at,created_by,kind) VALUES('short','test','platform-admin','personal')").execute(&store.pool()).await.unwrap();
        let app = test_router(&store);
        let cookie = admin_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/admin/invitations")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        for secret in [&pending, &used, "short"] {
            assert!(!text.contains(secret));
        }
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        let records = body["invitations"].as_array().unwrap();
        assert_eq!(records.len(), 3);
        for record in records {
            let keys: std::collections::BTreeSet<_> = record
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            assert_eq!(
                keys,
                ["invitation_id", "token_prefix", "created_at", "used_at"]
                    .into_iter()
                    .collect()
            );
            let decoded: treer_protocol::PlatformInvitationRecord =
                serde_json::from_value(record.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), *record);
        }
        assert_eq!(
            records
                .iter()
                .find(|v| v["invitation_id"] == invitation_id(&pending))
                .unwrap()["token_prefix"],
            &pending[..6]
        );
        assert_eq!(
            records
                .iter()
                .find(|v| v["invitation_id"] == invitation_id(&used))
                .unwrap()["used_at"],
            "used-time"
        );
        assert_eq!(
            records
                .iter()
                .find(|v| v["invitation_id"] == invitation_id("short"))
                .unwrap()["token_prefix"],
            "[redacted]"
        );
        // A non-bearer handle cannot be consumed by the registration API.
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM invitations WHERE token=$1)")
                .bind(invitation_id(&pending))
                .fetch_one(&store.pool())
                .await
                .unwrap();
        assert!(!exists);
        for (id, expected) in [
            (pending.clone(), StatusCode::NOT_FOUND),
            (invitation_id(&used), StatusCode::NOT_FOUND),
            (invitation_id("missing"), StatusCode::NOT_FOUND),
            (invitation_id(&pending), StatusCode::OK),
            (invitation_id(&pending), StatusCode::NOT_FOUND),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(format!("/api/admin/invitations/{id}"))
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
        let audits: Vec<String> = sqlx::query_scalar(
            "SELECT resource_id FROM platform_audit_events WHERE resource_kind='invitation'",
        )
        .fetch_all(&store.pool())
        .await
        .unwrap();
        assert_eq!(audits, vec![invitation_id(&pending)]);
    }

    #[tokio::test]
    async fn invitation_creation_returns_link_once_and_listing_is_bounded() {
        let store = AuthStore::for_test("admin-password").await;
        let app = test_router(&store);
        let cookie = admin_cookie(&app).await;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/admin/invitations")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let created: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        let token = created["token"].as_str().unwrap();
        assert!(created["url"].as_str().unwrap().contains(token));
        let resource: String = sqlx::query_scalar(
            "SELECT resource_id FROM platform_audit_events WHERE action='invitation.created'",
        )
        .fetch_one(&store.pool())
        .await
        .unwrap();
        assert_eq!(resource, invitation_id(token));
        sqlx::query("INSERT INTO invitations(token,created_at,created_by,kind) SELECT 'inv_' || lpad(n::text,32,'0'),'test','platform-admin','personal' FROM generate_series(1,110) AS n").execute(&store.pool()).await.unwrap();
        let Json(listed) = list_invitations(Extension(store.clone())).await.unwrap();
        assert_eq!(listed["invitations"].as_array().unwrap().len(), 100);
        assert!(!listed.to_string().contains(token));
        for path in [
            "/api/admin/traffic",
            "/api/admin/dashboard",
            "/api/admin/users",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.headers()["cache-control"], "no-store");
        }
    }
}
