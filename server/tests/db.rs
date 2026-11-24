//! Database tests. Need a Postgres: set TEST_DATABASE_URL (CI does). Skipped otherwise.

use axum::{body::Body, http::Request};
use running_viz::{AppState, MIGRATOR, display::Display, http, render::demo_stats};
use sqlx::{PgPool, Postgres, Transaction};
use tower::ServiceExt;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set, skipping");
        return None;
    };
    let db = PgPool::connect(&url).await.unwrap();
    MIGRATOR.run(&db).await.unwrap();
    Some(db)
}

#[tokio::test]
async fn healthz_is_ok() {
    let Some(db) = pool().await else { return };
    let res = http::router(AppState {
        db,
        display: Display::new(&demo_stats(running_viz::display::today())),
        sync: Default::default(),
        webhook: None,
        companions: None,
    })
    .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
    .await
    .unwrap();
    assert_eq!(res.status(), 200);
}

/// Runs `sql` inside a savepoint and reports whether Postgres accepted it.
async fn accepts(tx: &mut Transaction<'_, Postgres>, sql: &str) -> bool {
    sqlx::query("SAVEPOINT s").execute(&mut **tx).await.unwrap();
    let ok = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(&mut **tx)
        .await
        .is_ok();
    let cmd = if ok {
        "RELEASE SAVEPOINT s"
    } else {
        "ROLLBACK TO SAVEPOINT s"
    };
    sqlx::query(cmd).execute(&mut **tx).await.unwrap();
    ok
}

#[tokio::test]
async fn resumable_states_require_their_ids() {
    let Some(db) = pool().await else { return };
    let mut tx = db.begin().await.unwrap();
    let (fit, run) = (Uuid::new_v4(), Uuid::new_v4());
    let key = |k: &str| format!("{k}-{}", Uuid::new_v4());

    assert!(accepts(&mut tx, &format!(
        "INSERT INTO fit_files (id, dropbox_path, content_hash, bytes) VALUES ('{fit}', '/a.fit', '{}', '')",
        key("hash"))).await);
    assert!(accepts(&mut tx, &format!(
        "INSERT INTO runs (id, fit_file_id, endurain_activity_id, sport, start_time, end_time, distance_m, has_gps)
         VALUES ('{run}', '{fit}', 'a1', 'run', now(), now(), 1000, true)")).await);

    let upload = |kind: &str, run: &str, dog: &str, status: &str, job: &str, act: &str| {
        format!(
        "INSERT INTO uploads (id, kind, fit_file_id, run_id, dog, idempotency_key, status, job_id, endurain_activity_id)
         VALUES ('{}', '{kind}', '{fit}', {run}, {dog}, '{}', '{status}', {job}, {act})",
        Uuid::new_v4(), key("k"))
    };
    let run_ref = format!("'{run}'");

    // valid rows
    assert!(
        accepts(
            &mut tx,
            &upload("original", "NULL", "NULL", "pending", "NULL", "NULL")
        )
        .await
    );
    assert!(
        accepts(
            &mut tx,
            &upload("copy", &run_ref, "'nanuq'", "job_started", "'j1'", "NULL")
        )
        .await
    );
    assert!(
        accepts(
            &mut tx,
            &upload("copy", &run_ref, "'anori'", "done", "'j2'", "'a2'")
        )
        .await
    );
    assert!(
        accepts(
            &mut tx,
            "INSERT INTO companion_claims (id, dogs) VALUES (gen_random_uuid(), '{}')"
        )
        .await
    );

    // rejected rows
    assert!(
        !accepts(
            &mut tx,
            &upload("original", "NULL", "NULL", "job_started", "NULL", "NULL")
        )
        .await
    );
    assert!(
        !accepts(
            &mut tx,
            &upload("original", "NULL", "NULL", "uploaded", "'j'", "NULL")
        )
        .await
    );
    assert!(
        !accepts(
            &mut tx,
            &upload("copy", "NULL", "'nanuq'", "pending", "NULL", "NULL")
        )
        .await
    );
    assert!(
        !accepts(
            &mut tx,
            &upload("original", "NULL", "'nanuq'", "pending", "NULL", "NULL")
        )
        .await
    );
    assert!(!accepts(&mut tx,
        "INSERT INTO companion_claims (id, dogs, status) VALUES (gen_random_uuid(), '{}', 'matched')").await);

    tx.rollback().await.unwrap();
}
