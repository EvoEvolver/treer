use std::time::Duration;

use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use serde_json::Value;
use url::Url;

use crate::api::ApiFailure;

#[derive(Clone)]
pub struct UpdaterClient {
    inner: Option<ConfiguredUpdater>,
}

#[derive(Clone)]
struct ConfiguredUpdater {
    base_url: Url,
    token: String,
    http: reqwest::Client,
}

impl UpdaterClient {
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn new(mut base_url: Url, token: String) -> anyhow::Result<Self> {
        if token.is_empty() {
            anyhow::bail!("TREER_UPDATER_TOKEN must be set when TREER_UPDATER_URL is configured");
        }
        if !matches!(base_url.scheme(), "http" | "https") {
            anyhow::bail!("TREER_UPDATER_URL must use http or https");
        }
        if !base_url.username().is_empty() || base_url.password().is_some() {
            anyhow::bail!("TREER_UPDATER_URL must not contain credentials");
        }
        if !base_url.path().ends_with('/') {
            let path = format!("{}/", base_url.path());
            base_url.set_path(&path);
        }
        Ok(Self {
            inner: Some(ConfiguredUpdater {
                http: reqwest::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()?,
                base_url,
                token,
            }),
        })
    }

    async fn forward(&self, method: Method, path: &str) -> Result<Response, ApiFailure> {
        let configured = self.inner.as_ref().ok_or_else(|| {
            ApiFailure::not_found(
                "updater_unconfigured",
                "this deployment does not run a control-plane updater sidecar",
            )
        })?;
        let url = configured.base_url.join(path).map_err(|_| {
            ApiFailure::internal("updater_misconfigured", "updater URL could not be joined")
        })?;
        let response = configured
            .http
            .request(method, url)
            .bearer_auth(&configured.token)
            .send()
            .await
            .map_err(|error| {
                tracing::warn!(%error, "updater sidecar request failed");
                ApiFailure::service_unavailable(
                    "updater_unreachable",
                    "control-plane updater sidecar is unreachable",
                )
            })?;
        let sidecar_status = response.status();
        if sidecar_status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiFailure::bad_gateway(
                "updater_unauthorized",
                "the updater sidecar rejected the shared token",
            ));
        }
        let status =
            StatusCode::from_u16(sidecar_status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let body = response.json::<Value>().await.map_err(|error| {
            tracing::warn!(%error, "updater sidecar returned a non-JSON body");
            ApiFailure::service_unavailable(
                "updater_unreachable",
                "control-plane updater sidecar returned an invalid response",
            )
        })?;
        Ok((status, Json(body)).into_response())
    }
}

pub async fn status(Extension(updater): Extension<UpdaterClient>) -> Result<Response, ApiFailure> {
    updater.forward(Method::GET, "v1/status").await
}

pub async fn check(Extension(updater): Extension<UpdaterClient>) -> Result<Response, ApiFailure> {
    updater.forward(Method::GET, "v1/check").await
}

pub async fn apply(Extension(updater): Extension<UpdaterClient>) -> Result<Response, ApiFailure> {
    updater.forward(Method::POST, "v1/apply").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::Router;
    use serde_json::json;
    use tokio::net::TcpListener;

    async fn spawn_sidecar() -> Url {
        let app = Router::new()
            .route(
                "/v1/status",
                get(|| async { Json(json!({"channel":"stable","services":[],"job":null})) }),
            )
            .route(
                "/v1/check",
                get(|| async {
                    Json(json!({
                        "channel": "stable",
                        "services": [],
                        "update_available": true,
                        "job": null
                    }))
                }),
            )
            .route(
                "/v1/apply",
                post(|| async {
                    (
                        StatusCode::ACCEPTED,
                        Json(json!({
                            "channel": "stable",
                            "job": {"id": "job1", "state": "running", "error": null}
                        })),
                    )
                }),
            );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind sidecar");
        let addr = listener.local_addr().expect("sidecar address");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("sidecar server");
        });
        tokio::task::yield_now().await;
        Url::parse(&format!("http://{addr}/")).expect("sidecar URL")
    }

    #[tokio::test]
    async fn disabled_client_reports_unconfigured() {
        let client = UpdaterClient::disabled();
        let response = client
            .forward(Method::GET, "v1/status")
            .await
            .expect_err("disabled updater")
            .into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn configured_client_forwards_json_and_status() {
        let sidecar = spawn_sidecar().await;
        let client = UpdaterClient::new(sidecar, "secret".to_string()).expect("client");
        let status = client
            .forward(Method::GET, "v1/status")
            .await
            .expect("status")
            .into_response();
        assert_eq!(status.status(), StatusCode::OK);
        let apply = client
            .forward(Method::POST, "v1/apply")
            .await
            .expect("apply")
            .into_response();
        assert_eq!(apply.status(), StatusCode::ACCEPTED);
    }

    #[test]
    fn new_rejects_empty_token_and_credentials_in_url() {
        assert!(UpdaterClient::new(
            Url::parse("http://updater:7420/").expect("url"),
            String::new()
        )
        .is_err());
        assert!(UpdaterClient::new(
            Url::parse("http://user:pass@updater:7420/").expect("url"),
            "secret".to_string()
        )
        .is_err());
    }
}
