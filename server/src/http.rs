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
        .route(
            "/dropbox/webhook",
            get(webhook_challenge).post(webhook_notify),
        )
        .route(
            "/api/companions",
            axum::routing::post(companions).layer(axum::extract::DefaultBodyLimit::max(4096)),
        )
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
        // coarse on purpose (public endpoint); details are in the server log
        "import": match (state.webhook.is_some(), sync.import_ok) {
            (false, _) => "disabled",
            (true, None) => "pending",
            (true, Some(true)) => "ok",
            (true, Some(false)) => "failing",
        },
    });
    let code = if db_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(body))
}

#[derive(serde::Deserialize)]
struct Challenge {
    challenge: String,
}

/// Dropbox verifies the webhook URL by asking us to echo `challenge`.
async fn webhook_challenge(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<Challenge>,
) -> Response {
    if state.webhook.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    (
        [
            (header::CONTENT_TYPE, "text/plain"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        q.challenge,
    )
        .into_response()
}

/// Dropbox says something changed: check the signature, then start a sync right away.
async fn webhook_notify(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    let Some(webhook) = &state.webhook else {
        return StatusCode::NOT_FOUND;
    };
    let signature = headers
        .get("X-Dropbox-Signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !crate::dropbox::signature_valid(&webhook.app_secret, &body, signature) {
        return StatusCode::FORBIDDEN;
    }
    webhook.trigger.notify_one();
    StatusCode::OK
}

/// At most one accepted claim per minute (plan §3.2).
pub const CLAIM_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// The Shortcut's answer to "Who came along?" (needs `Authorization: Bearer <token>`).
async fn companions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, Json<Value>) {
    let reply = |code, msg: &str| (code, Json(json!({ "status": msg })));
    let Some(api) = &state.companions else {
        return reply(StatusCode::NOT_FOUND, "disabled");
    };
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !same_secret(given, &api.token) {
        return reply(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let Ok(mut claim) = serde_json::from_slice::<crate::companions::Claim>(&body) else {
        return reply(
            StatusCode::BAD_REQUEST,
            "expected {dogs, started_at, ended_at}",
        );
    };
    // a workout is identified by start and end; only a claim with neither uses the fallback
    match (claim.started_at, claim.ended_at) {
        (Some(_), None) => return reply(StatusCode::BAD_REQUEST, "started_at needs ended_at"),
        (Some(s), Some(e)) if s > e => {
            return reply(StatusCode::BAD_REQUEST, "started_at is after ended_at");
        }
        _ => {}
    }
    claim.dogs = claim.dogs.iter().map(|d| d.trim().to_lowercase()).collect();
    claim.dogs.sort();
    claim.dogs.dedup();
    if let Some(unknown) = claim.dogs.iter().find(|d| !api.dogs.contains(d)) {
        return reply(StatusCode::BAD_REQUEST, &format!("unknown dog {unknown}"));
    }
    {
        let mut last = api.last.lock().expect("claim lock");
        if last.is_some_and(|t| t.elapsed() < CLAIM_INTERVAL) {
            return reply(StatusCode::TOO_MANY_REQUESTS, "slow down");
        }
        *last = Some(std::time::Instant::now());
    }
    match crate::companions::record(&state.db, &claim).await {
        Ok(_) => {
            api.trigger.notify_one();
            // echo the normalized dogs, so the Shortcut can show what was saved
            let body = json!({ "status": "received", "dogs": claim.dogs });
            (StatusCode::ACCEPTED, Json(body))
        }
        Err(err) => {
            tracing::warn!("storing claim failed: {err:#}");
            reply(StatusCode::SERVICE_UNAVAILABLE, "try again")
        }
    }
}

/// Compares secrets without leaking their length or content through timing.
fn same_secret(given: &str, expected: &str) -> bool {
    use sha2::{Digest, Sha256};
    let (a, b) = (
        Sha256::digest(given.as_bytes()),
        Sha256::digest(expected.as_bytes()),
    );
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
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
