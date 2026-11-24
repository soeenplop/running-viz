//! Needs a Postgres: set TEST_DATABASE_URL (CI does). Skipped otherwise.

use axum::{body::Body, http::Request};
use running_viz::{AppState, MIGRATOR, http};
use tower::ServiceExt;

#[tokio::test]
async fn migrations_apply_and_healthz_is_ok() {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set, skipping");
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    MIGRATOR.run(&db).await.unwrap();

    let res = http::router(AppState { db })
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}
