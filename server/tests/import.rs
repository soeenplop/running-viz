//! Upload state machine against a real Postgres (TEST_DATABASE_URL) and a mocked Endurain.

use running_viz::{MIGRATOR, endurain::Account, import};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

/// A fresh database per test, so parallel tests never see each other's rows.
async fn pool() -> Option<PgPool> {
    let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
        eprintln!("TEST_DATABASE_URL not set, skipping");
        return None;
    };
    let admin = PgPool::connect(&url).await.unwrap();
    let name = format!("t_{}", Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .execute(&admin)
        .await
        .unwrap();
    let (base, _) = url.rsplit_once('/').unwrap();
    let db = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
    MIGRATOR.run(&db).await.unwrap();
    Some(db)
}

/// Inserts a FIT file with its queued original upload; returns the upload id.
async fn queued_upload(db: &PgPool) -> Uuid {
    let (file, upload) = (Uuid::new_v4(), Uuid::new_v4());
    let hash = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO fit_files (id, dropbox_path, content_hash, bytes, sport, sub_sport, start_time, end_time, distance_m, has_gps)
         VALUES ($1, '/Apps/x/Lauf ö.fit', $2, '\\x01', 'running', 'generic', now(), now(), 5000, true)",
    )
    .bind(file)
    .bind(&hash)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO uploads (id, kind, fit_file_id, idempotency_key) VALUES ($1, 'original', $2, $3)")
        .bind(upload)
        .bind(file)
        .bind(format!("fit:{hash}"))
        .execute(db)
        .await
        .unwrap();
    upload
}

async fn state(db: &PgPool, id: Uuid) -> (String, Option<String>, Option<String>) {
    sqlx::query_as("SELECT status, job_id, endurain_activity_id FROM uploads WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn endurain(job: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/activities/upload"))
        .and(header("X-API-Key", "k"))
        .respond_with(
            ResponseTemplate::new(202).set_body_json(json!({ "id": "job-1", "status": "queued" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/jobs/job-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(job))
        .mount(&server)
        .await;
    server
}

/// Advances only the given upload (other tests' rows may exist in the same database).
async fn advance(db: &PgPool, me: &Account, id: Uuid) {
    for _ in 0..3 {
        import::process_uploads(db, me).await.unwrap();
        if state(db, id).await.0 != "pending" && state(db, id).await.0 != "job_started" {
            return;
        }
    }
}

#[tokio::test]
async fn upload_runs_through_and_creates_a_run() {
    let Some(db) = pool().await else { return };
    let server =
        endurain(json!({ "id": "job-1", "status": "succeeded", "activity_ids": ["act-1"] })).await;
    let me = Account::new(reqwest::Client::new(), &server.uri(), "k");
    let id = queued_upload(&db).await;

    advance(&db, &me, id).await;
    assert_eq!(
        state(&db, id).await,
        ("done".into(), Some("job-1".into()), Some("act-1".into()))
    );
    let runs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM runs r JOIN uploads u ON u.fit_file_id = r.fit_file_id WHERE u.id = $1 AND r.endurain_activity_id = 'act-1'",
    )
    .bind(id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(runs, 1);
}

/// Crash after Endurain accepted the upload but before we stored the job:
/// the row is still `pending`; the retry sends the same Idempotency-Key.
#[tokio::test]
async fn restart_after_202_replays_the_same_key() {
    let Some(db) = pool().await else { return };
    let server = endurain(json!({ "id": "job-1", "status": "running" })).await;
    let me = Account::new(reqwest::Client::new(), &server.uri(), "k");
    let id = queued_upload(&db).await;

    import::process_uploads(&db, &me).await.unwrap();
    sqlx::query("UPDATE uploads SET status = 'pending', job_id = NULL WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
    import::process_uploads(&db, &me).await.unwrap();

    let key: String = sqlx::query_scalar("SELECT idempotency_key FROM uploads WHERE id = $1")
        .bind(id)
        .fetch_one(&db)
        .await
        .unwrap();
    let sent: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/activities/upload")
        .map(|r| r.headers["idempotency-key"].to_str().unwrap().to_owned())
        .collect();
    assert!(sent.len() >= 2 && sent.iter().all(|k| *k == key));
    assert_eq!(state(&db, id).await.0, "job_started");
}

#[tokio::test]
async fn ambiguous_job_needs_attention() {
    let Some(db) = pool().await else { return };
    for ids in [json!([]), json!(["a", "b"])] {
        let server =
            endurain(json!({ "id": "job-1", "status": "succeeded", "activity_ids": ids })).await;
        let me = Account::new(reqwest::Client::new(), &server.uri(), "k");
        let id = queued_upload(&db).await;
        advance(&db, &me, id).await;
        assert_eq!(state(&db, id).await.0, "needs_attention");
    }
}
