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
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/display.bin", get(display_bin))
        .route("/display.png", get(display_png))
        .with_state(state)
}

/// Fullscreen page showing the display image (web/index.html, embedded at build time).
async fn index() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../../web/index.html"))
}

/// Liveness plus sync state: 503 only if the database is down; a failing
/// Endurain refresh shows up as `"sync": "failing"` with the last success time.
async fn healthz(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let db_ok = match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => true,
        Err(err) => {
            tracing::warn!(%err, "health check: database unavailable");
            false
        }
    };
    let sync = state.sync.snapshot();
    let sync_state = match (&sync.last_error, sync.last_success) {
        (Some(_), _) => "failing",
        (None, Some(_)) => "ok",
        (None, None) => "pending",
    };
    let body = json!({
        "status": if db_ok { "ok" } else { "database unavailable" },
        "sync": sync_state,
        "last_sync": sync.last_success,
        "sync_error": sync.last_error,
    });
    let code = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body))
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
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .any(|v| if_none_match(v.as_bytes(), etag.as_bytes()));
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

/// Whether an `If-None-Match` value matches `etag` (weak comparison, RFC 9110 §13.1.2).
/// Accepts `*` or a comma-separated list of `"tag"` / `W/"tag"`; malformed lists never match.
fn if_none_match(value: &[u8], etag: &[u8]) -> bool {
    if value.trim_ascii() == b"*" {
        return true;
    }
    let mut rest = value;
    loop {
        while let [b' ' | b'\t' | b',', tail @ ..] = rest {
            rest = tail;
        }
        if rest.is_empty() {
            return false;
        }
        rest = rest.strip_prefix(b"W/").unwrap_or(rest);
        let Some(body) = rest.strip_prefix(b"\"") else {
            return false;
        };
        let Some(end) = body.iter().position(|&c| c == b'"') else {
            return false;
        };
        if &rest[..end + 2] == etag {
            return true;
        }
        rest = &body[end + 1..];
    }
}

#[cfg(test)]
mod tests {
    use super::if_none_match;

    #[test]
    fn if_none_match_parsing() {
        let m = |v: &[u8]| if_none_match(v, b"\"abc\"");
        assert!(m(b"\"abc\""));
        assert!(m(b"W/\"abc\""));
        assert!(m(b"\"x\", W/\"abc\""));
        assert!(m(b"\"\xff\", W/\"abc\""));
        assert!(m(b"*"));
        assert!(!m(b"\",*,\""));
        assert!(!m(b"\"abcd\""));
        assert!(!m(b"abc"));
        assert!(!m(b""));
    }
}
