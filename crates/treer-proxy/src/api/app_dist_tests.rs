//! Same-process App serving (#132): host matching, reserved prefixes, file
//! resolution inside the canonical dist directory, the nginx header set, the
//! runtime config document, ingress precedence and the same-origin embed
//! refusal.
use super::*;
use tower::ServiceExt;

struct Dist {
    root: PathBuf,
}

impl Dist {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("treer-app-dist-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(
            root.join("index.html"),
            "<!doctype html><title>treer</title>",
        )
        .unwrap();
        std::fs::write(root.join("assets/app.js"), "console.log(1)").unwrap();
        std::fs::write(root.join("favicon.svg"), "<svg/>").unwrap();
        let outside =
            std::env::temp_dir().join(format!("treer-outside-{}", Uuid::new_v4().simple()));
        std::fs::write(&outside, "secret").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        Self { root }
    }
}

impl Drop for Dist {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

async fn app_router(dist: &Dist, app_url: &str, proxy_url: &str) -> Router {
    let auth = AuthStore::for_test("admin-password").await;
    let messages = MessageStore::open(auth.pool()).await.unwrap();
    let proxy = Url::parse(proxy_url).unwrap();
    let app = Url::parse(app_url).unwrap();
    let identity = IdentityIssuer::load(&auth, &proxy).await.unwrap();
    let bootstrap = BootstrapConfig::new(
        proxy.clone(),
        PathBuf::from("dist"),
        Url::parse("https://github.example/releases/latest/download").unwrap(),
    )
    .with_app_dist(AppDistConfig::new(&dist.root, &app, &proxy).unwrap());
    router(
        AppState::new(),
        bootstrap,
        auth,
        PolicyEngine::allow_all(),
        identity,
        BrowserAccess::new(&app, &proxy).unwrap(),
        IngressConfig::new(
            Some(Url::parse("https://apps.treer.example/").unwrap()),
            &proxy,
            &app,
        )
        .unwrap(),
        messages,
        CapabilityRollout::all_enabled(),
        crate::updater::UpdaterClient::disabled(),
        crate::voice::VoiceServices::disabled(),
    )
}

async fn get(app: &Router, host: &str, path: &str) -> (StatusCode, HeaderMap, String) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn assert_app_headers(headers: &HeaderMap, cache: &str) {
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(
        headers[header::CONTENT_SECURITY_POLICY],
        "frame-ancestors 'none'; object-src 'none'; base-uri 'none'"
    );
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    assert_eq!(headers[header::CACHE_CONTROL], cache);
}

#[tokio::test]
async fn app_build_is_served_only_for_the_app_host_with_the_nginx_contract() {
    let dist = Dist::new();
    let app = app_router(
        &dist,
        "https://app.treer.example/",
        "https://proxy.treer.example/",
    )
    .await;
    // Pages and assets for the App host.
    let (status, headers, body) = get(&app, "app.treer.example", "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>treer</title>"));
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_app_headers(&headers, "no-cache");
    let (status, headers, body) = get(&app, "APP.treer.example.", "/assets/app.js").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "console.log(1)"));
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "text/javascript; charset=utf-8"
    );
    let (status, headers, _) = get(&app, "app.treer.example", "/favicon.svg").await;
    assert_eq!(
        (status, headers[header::CONTENT_TYPE].to_str().unwrap()),
        (StatusCode::OK, "image/svg+xml")
    );
    // SPA fallback for client routes, including unknown nested paths.
    for path in [
        "/workspaces/ws_x/machines",
        "/missing.js",
        "/assets/",
        "/assets",
    ] {
        let (status, headers, body) = get(&app, "app.treer.example", path).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains("<title>treer</title>"), "{path}");
        assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    }
    // Runtime config and App health, never cached.
    let (status, headers, body) = get(&app, "app.treer.example", "/config.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"proxy_url": "https://proxy.treer.example/"})
    );
    assert_app_headers(&headers, "no-store");
    let (status, _, body) = get(&app, "app.treer.example", "/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"service": "treer-app", "status": "ok"})
    );
    // Reserved Core prefixes are JSON 404s, never the page.
    for path in [
        "/api/nope",
        "/agent/x",
        "/artifacts/linux-x86_64/nope",
        "/.treer/x",
        "/.well-known/x",
        "/install.zsh",
    ] {
        let (status, headers, body) = get(&app, "app.treer.example", path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("application/json"),
            "{path}"
        );
        assert!(!body.contains("<title>"), "{path}");
    }
    // Traversal and symlink escape never read outside dist (they fall back to the page).
    for path in [
        "/../etc/passwd",
        "/assets/../../etc/passwd",
        "/escape",
        "/%2e%2e/secret",
        "/assets/..%2fapp.js",
    ] {
        let (status, _, body) = get(&app, "app.treer.example", path).await;
        assert!(!body.contains("secret"), "{path} leaked");
        assert!(
            status == StatusCode::OK
                || status == StatusCode::BAD_REQUEST
                || status == StatusCode::NOT_FOUND,
            "{path} {status}"
        );
        if status == StatusCode::OK {
            assert!(body.contains("<title>treer</title>"), "{path}");
        }
    }
    // Other hosts get the generic 404; the ingress host still reaches ingress.
    let (status, _, body) = get(&app, "proxy.treer.example", "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("route_not_found"));
    let (status, _, body) = get(&app, "svc.apps.treer.example", "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("ingress_not_found"), "{body}");
    // Route exclusivity: Core routes never execute on the App host, and the
    // Proxy host keeps them.
    let (status, _, body) = get(&app, "app.treer.example", "/api/health").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("route_not_found") && !body.contains("treer-proxy"));
    let (status, _, body) = get(&app, "proxy.treer.example", "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("treer-proxy"));
    let (status, _, _) = get(&app, "proxy.treer.example", "/config.json").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Strict Host allowlist: exact host:port with the scheme's default port.
    for host in [
        "app.treer.example:443",
        "APP.TREER.EXAMPLE",
        "proxy.treer.example:443",
    ] {
        let (status, _, _) = get(
            &app,
            host,
            if host.starts_with("proxy") {
                "/api/health"
            } else {
                "/"
            },
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{host}");
    }
    for host in [
        "evil.example",
        "app.treer.example:8443",
        "127.0.0.1",
        "localhost:8080",
        "treer.example",
        "app.treer.example.evil.example",
    ] {
        for path in ["/", "/api/health", "/config.json"] {
            let (status, _, body) = get(&app, host, path).await;
            assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{host}{path}");
            assert!(body.contains("host_not_configured"), "{host}{path} {body}");
        }
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/")
                .header(header::HOST, "app.treer.example")
                .header(header::HOST, "proxy.treer.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // Separate origins keep embedded App UI available (reaches the tunnel, not the refusal).
    let (status, _, body) = get(
        &app,
        "proxy.treer.example",
        "/api/workspaces/ws_x/agents/ag_x/interface/ui",
    )
    .await;
    assert!(
        !body.contains("embedded_ui_requires_separate_app_origin"),
        "{status} {body}"
    );
}

/// The documented zero-DNS local default: two `*.treer.localhost` names on one
/// port are the same site (Strict cookies unchanged) but different origins.
#[tokio::test]
async fn local_default_names_give_two_origins_on_one_port() {
    let dist = Dist::new();
    let app = app_router(
        &dist,
        "http://console.treer.localhost:8080/",
        "http://proxy.treer.localhost:8080/",
    )
    .await;
    let (status, _, body) = get(&app, "console.treer.localhost:8080", "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>treer</title>"));
    let (status, _, body) = get(&app, "console.treer.localhost:8080", "/config.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["proxy_url"],
        "http://proxy.treer.localhost:8080/"
    );
    let (status, _, _) = get(&app, "proxy.treer.localhost:8080", "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = get(&app, "proxy.treer.localhost:8080", "/").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get(&app, "console.treer.localhost:8080", "/api/health").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Same site is not same origin: a third name, bare localhost, the loopback
    // literal or another port are not configured and get the fixed 421.
    for host in [
        "app.treer.localhost:8080",
        "treer.localhost:8080",
        "localhost:8080",
        "127.0.0.1:8080",
        "console.treer.localhost",
        "console.treer.localhost:80",
        "[::1]:8080",
        "proxy.treer.localhost.:8081",
    ] {
        let (status, _, _) = get(&app, host, "/").await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{host}");
    }
    // Two different origins: embedded interfaces stay available on the Proxy host.
    let (_, _, body) = get(
        &app,
        "proxy.treer.localhost:8080",
        "/api/workspaces/ws_x/agents/ag_x/interface/ui",
    )
    .await;
    assert!(!body.contains("embedded_ui_requires_separate_app_origin"));
}

#[tokio::test]
async fn same_origin_app_disables_embedded_interfaces_and_virtual_hosts() {
    let dist = Dist::new();
    let app = app_router(&dist, "https://treer.example/", "https://treer.example/").await;
    let (status, headers, body) = get(&app, "treer.example", "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>treer</title>"));
    assert_app_headers(&headers, "no-cache");
    // One origin: Core routes and the Console both answer on it.
    let (status, _, _) = get(&app, "treer.example", "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    for path in [
        "/api/workspaces/ws_x/agents/ag_x/interface/ui",
        "/api/workspaces/ws_x/agents/ag_x/interface/ui/",
        "/api/workspaces/ws_x/agents/ag_x/interface/ui/index.html",
        "/api/workspaces/ws_x/virtual-hosts/svc/proxy",
        "/api/workspaces/ws_x/virtual-hosts/svc/proxy/",
        "/api/workspaces/ws_x/virtual-hosts/svc/proxy/x",
    ] {
        let (status, _, body) = get(&app, "treer.example", path).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert!(
            body.contains("embedded_ui_requires_separate_app_origin"),
            "{path} {body}"
        );
    }
}

#[test]
fn app_dist_configuration_requires_an_index_and_reports_the_origin_rule() {
    let dist = Dist::new();
    let proxy = Url::parse("https://proxy.treer.example/").unwrap();
    let separate = AppDistConfig::new(
        &dist.root,
        &Url::parse("https://app.treer.example/x").unwrap(),
        &proxy,
    )
    .unwrap();
    assert!(separate.enabled() && separate.embedding_allowed());
    let same = AppDistConfig::new(&dist.root, &proxy, &proxy).unwrap();
    assert!(same.enabled() && !same.embedding_allowed());
    assert!(AppDistConfig::disabled().embedding_allowed());
    assert!(!AppDistConfig::disabled().enabled());
    let empty = std::env::temp_dir().join(format!("treer-empty-dist-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&empty).unwrap();
    assert!(AppDistConfig::new(&empty, &proxy, &proxy).is_err());
    assert!(AppDistConfig::new(&empty.join("absent"), &proxy, &proxy).is_err());
    std::fs::remove_dir_all(empty).unwrap();
}
