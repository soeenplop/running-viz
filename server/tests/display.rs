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
        sync: Default::default(),
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

async fn get_with(inm: &[u8]) -> (StatusCode, usize) {
    let res = app()
        .oneshot(
            Request::get("/display.bin")
                .header(
                    header::IF_NONE_MATCH,
                    axum::http::HeaderValue::from_bytes(inm).unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    (
        status,
        res.into_body().collect().await.unwrap().to_bytes().len(),
    )
}

#[tokio::test]
async fn weak_and_unrelated_validators() {
    let etag = running_viz::display::Frame::new(&demo_stats(day())).etag;
    assert_eq!(
        get_with(format!("W/{etag}").as_bytes()).await,
        (StatusCode::NOT_MODIFIED, 0)
    );
    assert_eq!(get_with(b"\",*,\"").await, (StatusCode::OK, BYTES));
    let mut non_ascii = b"\"\xff\", W/".to_vec();
    non_ascii.extend_from_slice(etag.as_bytes());
    assert_eq!(get_with(&non_ascii).await, (StatusCode::NOT_MODIFIED, 0));
}

/// March 2026 starts on a Sunday: six calendar rows must all fit, incl. the 31st.
#[test]
fn six_week_month_shows_last_day() {
    let last = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
    let img = render(&demo_stats(last));
    // The 31st is in row 6, column 2 (Tuesday) and drawn inverted (black) as today.
    let (cw, rh) = (384.0 / 7.0, 140 / 6);
    let x = 8 + cw as u32 + 5;
    let y = 160 + 5 * rh as u32 + 2;
    assert!(
        img.is_black(x, y),
        "today's cell of a 6-week month must be visible"
    );
    assert!(y + rh as u32 - 3 < 300);
    // the highlighted cell's bottom row stays solid (no text pixel cuts through it)
    let (cx, cy) = (8 + cw as u32, 160 + 5 * rh as u32 + rh as u32 - 3);
    assert!((cx..cx + cw as u32 - 2).all(|px| img.is_black(px, cy)));
}
