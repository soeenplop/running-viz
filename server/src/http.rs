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
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| if_none_match(v, etag));
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
fn if_none_match(value: &str, etag: &str) -> bool {
    if value.trim() == "*" {
        return true;
    }
    let mut rest = value;
    loop {
        rest = rest.trim_start_matches([' ', '\t', ',']);
        if rest.is_empty() {
            return false;
        }
        rest = rest.strip_prefix("W/").unwrap_or(rest);
        let Some(body) = rest.strip_prefix('"') else {
            return false;
        };
        let Some(end) = body.find('"') else {
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
        let tag = "\"abc\"";
        assert!(if_none_match("\"abc\"", tag));
        assert!(if_none_match("W/\"abc\"", tag));
        assert!(if_none_match("\"x\", W/\"abc\"", tag));
        assert!(if_none_match("*", tag));
        assert!(!if_none_match("\",*,\"", tag));
        assert!(!if_none_match("\"abcd\"", tag));
        assert!(!if_none_match("abc", tag));
        assert!(!if_none_match("", tag));
    }
}
