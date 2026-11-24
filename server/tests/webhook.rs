//! Dropbox webhook endpoint: URL verification and signed notifications.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::NaiveDate;
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use running_viz::{AppState, Webhook, display::Display, http, render::demo_stats};
use tokio::sync::Notify;
use tower::ServiceExt;

fn app(webhook: Option<Webhook>) -> axum::Router {
    let db = sqlx::PgPool::connect_lazy("postgres://unused@localhost/unused").unwrap();
    let today = NaiveDate::from_ymd_opt(2026, 12, 14).unwrap();
    http::router(AppState {
        db,
        display: Display::new(&demo_stats(today)),
        sync: Default::default(),
        webhook,
    })
}

fn sign(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}

fn hook() -> (Webhook, Arc<Notify>) {
    let trigger = Arc::new(Notify::new());
    let webhook = Webhook {
        app_secret: "secret".into(),
        trigger: trigger.clone(),
    };
    (webhook, trigger)
}

#[tokio::test]
async fn challenge_is_echoed() {
    let (webhook, _) = hook();
    let res = app(Some(webhook))
        .oneshot(
            Request::get("/dropbox/webhook?challenge=abc123")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        res.into_body().collect().await.unwrap().to_bytes(),
        "abc123"
    );
}

#[tokio::test]
async fn signed_notification_triggers_sync() {
    let (webhook, trigger) = hook();
    let body = br#"{"list_folder":{"accounts":["dbid:x"]}}"#;
    let res = app(Some(webhook))
        .oneshot(
            Request::post("/dropbox/webhook")
                .header("X-Dropbox-Signature", sign("secret", body))
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    tokio::time::timeout(Duration::from_secs(1), trigger.notified())
        .await
        .expect("sync was triggered");
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    let (webhook, trigger) = hook();
    let res = app(Some(webhook))
        .oneshot(
            Request::post("/dropbox/webhook")
                .header("X-Dropbox-Signature", sign("wrong", b"{}"))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), trigger.notified())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn disabled_without_dropbox() {
    let res = app(None)
        .oneshot(
            Request::get("/dropbox/webhook?challenge=x")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
