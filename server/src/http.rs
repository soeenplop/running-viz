use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

use crate::AppState;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/display.bin", get(display_bin))
        .route("/display.png", get(display_png))
        .with_state(state)
}

/// Liveness: the process runs and the database answers.
async fn healthz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => (StatusCode::OK, Json(json!({ "status": "ok" }))),
        Err(err) => {
            tracing::warn!(%err, "health check: database unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "database unavailable" })),
            )
        }
    }
}

/// Raw 1-bit framebuffer for the e-paper display.
async fn display_bin(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let frame = state.display.current();
    image(
        &headers,
        &frame.etag,
        "application/octet-stream",
        frame.bin.clone(),
    )
}

/// The same image as PNG, for the browser.
async fn display_png(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let frame = state.display.current();
    image(&headers, &frame.etag, "image/png", frame.png.clone())
}

/// Serves `body` with an ETag; answers `304` if the client already has it.
fn image(headers: &HeaderMap, etag: &str, content_type: &'static str, body: Vec<u8>) -> Response {
    let etag_value = HeaderValue::from_str(etag).expect("etag is ascii");
    let cache = [
        (header::ETAG, etag_value),
        (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
    ];
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if matches {
        return (StatusCode::NOT_MODIFIED, cache).into_response();
    }
    (
        cache,
        [(header::CONTENT_TYPE, HeaderValue::from_static(content_type))],
        body,
    )
        .into_response()
}
