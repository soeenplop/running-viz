//! Import against a real Postgres (TEST_DATABASE_URL, fresh database per test)
//! with mocked Dropbox and Endurain APIs.

mod common;

use chrono::NaiveDate;
use common::{Workout, fit_file, fresh_db};
use running_viz::{
    dropbox::{Dropbox, DropboxConfig},
    endurain::Account,
    import,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, Request, Respond, ResponseTemplate,
    matchers::{body_partial_json, header, method, path, path_regex},
};

fn since() -> NaiveDate {
    import::parse_since(None).unwrap()
}

// ---------- Dropbox mock ----------

fn dropbox(server: &MockServer) -> Dropbox {
    let cfg = DropboxConfig {
        app_key: "key".into(),
        app_secret: "secret".into(),
        refresh_token: "refresh".into(),
        folder: "/Apps/HealthFitExporter".into(),
    };
    Dropbox::with_hosts(reqwest::Client::new(), cfg, &server.uri(), &server.uri())
}

async fn dropbox_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "access_token": "t", "expires_in": 14400 })),
        )
        .mount(&server)
        .await;
    server
}

fn entry(name: &str, hash: &str) -> Value {
    json!({ ".tag": "file", "id": format!("id:{name}"), "name": name,
            "path_display": format!("/Apps/HealthFitExporter/{name}"), "content_hash": hash })
}

fn page(entries: Vec<Value>, cursor: &str, has_more: bool) -> Value {
    json!({ "entries": entries, "cursor": cursor, "has_more": has_more })
}

async fn mount_list(server: &MockServer, body: Value) {
    Mock::given(method("POST"))
        .and(path("/2/files/list_folder"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mount_continue(server: &MockServer, cursor: &str, response: ResponseTemplate) {
    Mock::given(method("POST"))
        .and(path("/2/files/list_folder/continue"))
        .and(body_partial_json(json!({ "cursor": cursor })))
        .respond_with(response)
        .mount(server)
        .await;
}

fn arg(name: &str) -> String {
    json!({ "path": format!("id:{name}") }).to_string()
}

/// Serves `bytes` for file `name`, reporting `hash` as the downloaded content hash.
async fn mount_file(server: &MockServer, name: &str, hash: &str, bytes: Vec<u8>) {
    Mock::given(method("POST"))
        .and(path("/2/files/download"))
        .and(header("Dropbox-API-Arg", arg(name).as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header(
                    "Dropbox-API-Result",
                    json!({ "content_hash": hash }).to_string().as_str(),
                )
                .set_body_bytes(bytes),
        )
        .mount(server)
        .await;
}

fn run_fit(start: &str) -> Vec<u8> {
    fit_file(&Workout::run(start, 1800, 6000))
}

async fn hashes(db: &PgPool) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT content_hash, skip_reason FROM fit_files ORDER BY content_hash")
        .fetch_all(db)
        .await
        .unwrap()
}

async fn cursor(db: &PgPool) -> Option<String> {
    sqlx::query_scalar("SELECT cursor FROM dropbox_state")
        .fetch_optional(db)
        .await
        .unwrap()
}

#[tokio::test]
async fn pages_crash_mid_page_and_cursor_reset() {
    let Some(db) = fresh_db().await else { return };
    let server = dropbox_server().await;
    let dbx = dropbox(&server);
    mount_list(
        &server,
        page(vec![entry("a.fit", "h1"), entry("b.fit", "h2")], "c1", true),
    )
    .await;
    mount_continue(
        &server,
        "c1",
        ResponseTemplate::new(200).set_body_json(page(vec![entry("c.fit", "h3")], "c2", false)),
    )
    .await;
    mount_file(&server, "a.fit", "h1", run_fit("2026-11-02T07:00:00Z")).await;
    mount_file(&server, "c.fit", "h3", run_fit("2026-11-04T07:00:00Z")).await;
    // b.fit fails once (crash mid-page), then works
    Mock::given(method("POST"))
        .and(path("/2/files/download"))
        .and(header("Dropbox-API-Arg", arg("b.fit").as_str()))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    mount_file(&server, "b.fit", "h2", run_fit("2026-11-03T07:00:00Z")).await;

    assert!(import::fetch_new_files(&db, &dbx, since()).await.is_err());
    assert_eq!(
        hashes(&db).await.len(),
        1,
        "a.fit stored, page not finished"
    );
    assert_eq!(cursor(&db).await, None);

    import::fetch_new_files(&db, &dbx, since()).await.unwrap();
    assert_eq!(
        hashes(&db).await.len(),
        3,
        "re-read page skips a.fit, adds b and c"
    );
    assert_eq!(cursor(&db).await.as_deref(), Some("c2"));

    // the stored cursor expires: Dropbox answers reset, we list again and add nothing twice
    mount_continue(
        &server,
        "c2",
        ResponseTemplate::new(409).set_body_json(json!({ "error_summary": "reset/..." })),
    )
    .await;
    import::fetch_new_files(&db, &dbx, since()).await.unwrap();
    assert_eq!(hashes(&db).await.len(), 3);
    let uploads: i64 = sqlx::query_scalar("SELECT count(*) FROM uploads")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(uploads, 3);
}

#[tokio::test]
async fn changed_and_deleted_files_between_listing_and_download() {
    let Some(db) = fresh_db().await else { return };
    let server = dropbox_server().await;
    let dbx = dropbox(&server);
    mount_list(
        &server,
        page(
            vec![entry("gone.fit", "h-gone"), entry("changed.fit", "h-old")],
            "c1",
            false,
        ),
    )
    .await;
    Mock::given(method("POST"))
        .and(path("/2/files/download"))
        .and(header("Dropbox-API-Arg", arg("gone.fit").as_str()))
        .respond_with(
            ResponseTemplate::new(409)
                .set_body_json(json!({ "error_summary": "path/not_found/.." })),
        )
        .mount(&server)
        .await;
    mount_file(
        &server,
        "changed.fit",
        "h-new",
        run_fit("2026-11-05T07:00:00Z"),
    )
    .await;

    import::fetch_new_files(&db, &dbx, since()).await.unwrap();
    assert_eq!(hashes(&db).await, vec![("h-new".into(), None)]);
    let key: String = sqlx::query_scalar("SELECT idempotency_key FROM uploads")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(key, "fit:h-new");
}

#[tokio::test]
async fn skips_old_and_duplicate_workouts() {
    let Some(db) = fresh_db().await else { return };
    let server = dropbox_server().await;
    let dbx = dropbox(&server);
    let swim = Workout {
        sport: 5,
        gps: None,
        ..Workout::run("2026-11-10T07:00:00Z", 1800, 1000)
    };
    // starts 45 s later, ends 10 min later: a different workout
    let other_run = Workout::run("2026-11-10T07:00:45Z", 2400, 8000);
    // same workout exported by another app: 20 s later, slightly different distance
    let app_copy = fit_file(&Workout::run("2026-11-10T07:00:20Z", 1800, 6010));
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("old.fit", run_fit("2026-10-31T22:30:00Z")), // 31 Oct 23:30 Berlin
        ("midnight.fit", run_fit("2026-10-31T23:30:00Z")), // 1 Nov 00:30 Berlin
        ("watch.fit", run_fit("2026-11-10T07:00:00Z")),
        ("copy-from-app.fit", app_copy),
        ("other.fit", fit_file(&other_run)),
        ("swim.fit", fit_file(&swim)),
    ];
    let entries = files
        .iter()
        .map(|(n, _)| entry(n, &format!("h-{n}")))
        .collect();
    mount_list(&server, page(entries, "c1", false)).await;
    for (n, bytes) in &files {
        mount_file(&server, n, &format!("h-{n}"), bytes.clone()).await;
    }
    import::fetch_new_files(&db, &dbx, since()).await.unwrap();
    let got = hashes(&db).await;
    let reason = |n: &str| {
        got.iter()
            .find(|(h, _)| *h == format!("h-{n}"))
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(reason("old.fit").as_deref(), Some("before_since"));
    assert_eq!(reason("midnight.fit"), None);
    assert_eq!(reason("watch.fit"), None);
    assert_eq!(reason("copy-from-app.fit").as_deref(), Some("duplicate"));
    assert_eq!(
        reason("other.fit"),
        None,
        "different workout in the same minute"
    );
    assert_eq!(reason("swim.fit"), None, "different sport at the same time");
}

// ---------- Endurain mock ----------

/// Upload answers by idempotency key: `job-<key>`; the job succeeds with `act-<key>`.
struct JobFor;
impl Respond for JobFor {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let key = req.headers["idempotency-key"]
            .to_str()
            .unwrap()
            .replace(':', "-");
        ResponseTemplate::new(202)
            .set_body_json(json!({ "id": format!("job-{key}"), "status": "queued" }))
    }
}

struct JobStatus;
impl Respond for JobStatus {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let id = req.url.path().rsplit('/').next().unwrap().to_owned();
        let act = id.replacen("job-", "act-", 1);
        ResponseTemplate::new(200)
            .set_body_json(json!({ "id": id, "status": "succeeded", "activity_ids": [act] }))
    }
}

struct ActivityFor;
impl Respond for ActivityFor {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let id = req.url.path().rsplit('/').next().unwrap();
        ResponseTemplate::new(200).set_body_json(json!({
            "id": id, "activity_type": "trail_run", "distance": 6100,
            "start_time": "2026-11-10T07:00:05Z", "end_time": "2026-11-10T07:31:00Z" }))
    }
}

async fn endurain() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/activities/upload"))
        .respond_with(JobFor)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/api/v1/jobs/"))
        .respond_with(JobStatus)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/api/v1/activities/"))
        .respond_with(ActivityFor)
        .mount(&server)
        .await;
    server
}

/// Inserts a FIT file with an original upload in `status`; returns the upload id.
async fn queued_upload(db: &PgPool, status: &str, job: Option<&str>) -> Uuid {
    let (file, upload) = (Uuid::new_v4(), Uuid::new_v4());
    let hash = Uuid::new_v4().simple().to_string();
    sqlx::query(
        "INSERT INTO fit_files (id, dropbox_path, content_hash, bytes, sport, sub_sport, start_time, end_time, distance_m, has_gps)
         VALUES ($1, '/Apps/x/Lauf ö.fit', $2, '\\x01', 'running', 'generic', now(), now(), 5000, true)",
    )
    .bind(file)
    .bind(&hash)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO uploads (id, kind, fit_file_id, idempotency_key, status, job_id)
         VALUES ($1, 'original', $2, $3, $4, $5)",
    )
    .bind(upload)
    .bind(file)
    .bind(format!("fit:{hash}"))
    .bind(status)
    .bind(job)
    .execute(db)
    .await
    .unwrap();
    upload
}

async fn status(db: &PgPool, id: Uuid) -> String {
    sqlx::query_scalar("SELECT status FROM uploads WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

fn account(server: &MockServer) -> Account {
    Account::new(reqwest::Client::new(), &server.uri(), "k")
}

#[tokio::test]
async fn run_is_recorded_with_endurain_facts() {
    let Some(db) = fresh_db().await else { return };
    let server = endurain().await;
    let me = account(&server);
    let id = queued_upload(&db, "pending", None).await;
    for _ in 0..3 {
        import::process_uploads(&db, &me).await.unwrap();
    }
    assert_eq!(status(&db, id).await, "done");
    let (sport, distance): (String, i32) = sqlx::query_as("SELECT sport, distance_m FROM runs")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!((sport.as_str(), distance), ("trail_run", 6100));
}

/// Crash after Endurain accepted the upload but before we stored the job:
/// the row is still `pending`; the retry sends the same Idempotency-Key.
#[tokio::test]
async fn restart_after_202_replays_the_same_key() {
    let Some(db) = fresh_db().await else { return };
    let server = endurain().await;
    let me = account(&server);
    let id = queued_upload(&db, "pending", None).await;
    import::process_uploads(&db, &me).await.unwrap();
    sqlx::query("UPDATE uploads SET status = 'pending', job_id = NULL WHERE id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
    import::process_uploads(&db, &me).await.unwrap();

    let sent: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/v1/activities/upload")
        .map(|r| r.headers["idempotency-key"].to_str().unwrap().to_owned())
        .collect();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1]);
    assert_eq!(status(&db, id).await, "job_started");
}

#[tokio::test]
async fn ambiguous_or_failed_jobs_need_attention() {
    let Some(db) = fresh_db().await else { return };
    for job in [
        json!({ "id": "j", "status": "succeeded", "activity_ids": [] }),
        json!({ "id": "j", "status": "succeeded", "activity_ids": ["a", "b"] }),
        json!({ "id": "j", "status": "failed", "error_code": "invalid_file", "detail": "kept" }),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/jobs/j"))
            .respond_with(ResponseTemplate::new(200).set_body_json(job.clone()))
            .mount(&server)
            .await;
        let id = queued_upload(&db, "job_started", Some("j")).await;
        let round = import::process_uploads(&db, &account(&server))
            .await
            .unwrap();
        assert_eq!(
            round.failed_steps, 1,
            "the failure is reported by this round"
        );
        assert_eq!(status(&db, id).await, "needs_attention");
        let raw: Value = sqlx::query_scalar("SELECT raw_job FROM uploads WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(raw, job, "full upstream job is kept");
    }
}

/// Five uploads Endurain rejects for good must not block the sixth.
#[tokio::test]
async fn rejected_uploads_do_not_block_others() {
    let Some(db) = fresh_db().await else { return };
    let server = endurain().await;
    let mut bad = Vec::new();
    for _ in 0..5 {
        let id = queued_upload(&db, "pending", None).await;
        let key: String = sqlx::query_scalar("SELECT idempotency_key FROM uploads WHERE id = $1")
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v1/activities/upload"))
            .and(header("Idempotency-Key", key.as_str()))
            .respond_with(
                ResponseTemplate::new(413).set_body_json(json!({ "title": "Content Too Large" })),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        bad.push(id);
    }
    let good = queued_upload(&db, "pending", None).await;
    let me = account(&server);
    for _ in 0..4 {
        import::process_uploads(&db, &me).await.unwrap();
    }
    for id in bad {
        assert_eq!(status(&db, id).await, "needs_attention");
    }
    assert_eq!(status(&db, good).await, "done");
}

/// Dropbox is down: uploads that were already accepted still finish.
#[tokio::test]
async fn dropbox_outage_does_not_stop_uploads() {
    let Some(db) = fresh_db().await else { return };
    let dbx_server = dropbox_server().await;
    Mock::given(method("POST"))
        .and(path("/2/files/list_folder"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&dbx_server)
        .await;
    let server = endurain().await;
    let id = queued_upload(&db, "job_started", Some("job-x")).await;

    let out = import::run(&db, &dropbox(&dbx_server), &account(&server), since()).await;
    assert!(out.fetch_error.is_some());
    assert_eq!(out.runs_created, 1);
    assert_eq!(status(&db, id).await, "done");
}

/// An expired key (401) must not strand uploads: once the key works again, they finish.
#[tokio::test]
async fn auth_failure_is_retried() {
    let Some(db) = fresh_db().await else { return };
    let server = endurain().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/activities/upload"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "title": "Unauthorized" })))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    let me = account(&server);
    let id = queued_upload(&db, "pending", None).await;

    let round = import::process_uploads(&db, &me).await.unwrap();
    assert_eq!(round.failed_steps, 1);
    assert_eq!(status(&db, id).await, "pending");
    for _ in 0..3 {
        import::process_uploads(&db, &me).await.unwrap();
    }
    assert_eq!(status(&db, id).await, "done");
}
