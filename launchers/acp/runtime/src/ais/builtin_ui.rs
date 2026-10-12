use axum::{http::header, routing::get, Router};

// Assets are launcher-owned and ship in every binary, without a frontend build.
pub(super) fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route(
            "/",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("builtin_ui/index.html"),
                )
            }),
        )
        .route(
            "/builtin.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("builtin_ui/style.css"),
                )
            }),
        )
        .route(
            "/builtin.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("builtin_ui/app.js"),
                )
            }),
        )
}

fn asset(content_type: &'static str, body: &'static str) -> impl axum::response::IntoResponse {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
}
