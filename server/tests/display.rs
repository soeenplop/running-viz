//! Display endpoints and a pixel snapshot of the demo image. No database needed.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::NaiveDate;
use http_body_util::BodyExt;
use running_viz::{
    AppState,
    display::Display,
    http,
    render::{BYTES, demo_stats, render},
};
use tower::ServiceExt;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 12, 14).unwrap()
}

fn app() -> axum::Router {
    let db = sqlx::PgPool::connect_lazy("postgres://unused@localhost/unused").unwrap();
    http::router(AppState {
        db,
        display: Display::new(&demo_stats(day())),
    })
}

#[tokio::test]
async fn bin_has_etag_and_answers_304() {
    let res = app()
        .oneshot(Request::get("/display.bin").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let etag = res.headers()[header::ETAG].to_str().unwrap().to_owned();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(body.len(), BYTES);

    let res = app()
        .oneshot(
            Request::get("/display.bin")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn png_is_served() {
    let res = app()
        .oneshot(Request::get("/display.png").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "image/png");
}

/// Compares the demo image pixel by pixel with `tests/snapshots/demo.png`.
/// Regenerate after an intended layout change: `UPDATE_SNAPSHOTS=1 cargo test`.
#[test]
fn demo_snapshot() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/snapshots/demo.png");
    let png = render(&demo_stats(day())).to_png(1);
    if std::env::var_os("UPDATE_SNAPSHOTS").is_some() {
        std::fs::write(path, &png).unwrap();
    }
    let expected = std::fs::read(path).expect("snapshot missing: run with UPDATE_SNAPSHOTS=1");
    assert!(
        png == expected,
        "rendering changed; check and run UPDATE_SNAPSHOTS=1 cargo test"
    );
}
