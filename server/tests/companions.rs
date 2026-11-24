//! "Who came along?": claim matching, planned copies, copy uploads and the endpoint.
//! DB tests need TEST_DATABASE_URL (fresh database per test).

mod common;

use std::collections::HashMap;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::{Workout, fit_file, fresh_db};
use running_viz::{
    AppState, CompanionsApi,
    companions::{self, Claim},
    display::Display,
    endurain::Account,
    http,
    render::demo_stats,
};
use serde_json::json;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, Request as MockRequest, Respond, ResponseTemplate,
    matchers::{header, method, path, path_regex},
};

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn dogs() -> Vec<String> {
    vec!["anori".into(), "nanuq".into()]
}

/// Inserts an imported run (with a synthetic GPS FIT file); returns the run id.
async fn run(db: &PgPool, start: &str, minutes: i64) -> Uuid {
    let (file, run) = (Uuid::new_v4(), Uuid::new_v4());
    let start = t(start);
    let end = start + Duration::minutes(minutes);
    let bytes = fit_file(&Workout {
        seconds: (minutes * 60) as u32,
        ..Workout::run(&start.to_rfc3339(), 60, 5000)
    });
    sqlx::query(
        "INSERT INTO fit_files (id, dropbox_path, content_hash, bytes, sport, start_time, end_time, distance_m, has_gps)
         VALUES ($1, '/x.fit', $2, $3, 'running', $4, $5, 5000, true)",
    )
    .bind(file)
    .bind(Uuid::new_v4().to_string())
    .bind(bytes)
    .bind(start)
    .bind(end)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, fit_file_id, endurain_activity_id, sport, start_time, end_time, distance_m, has_gps)
         VALUES ($1, $2, $3, 'run', $4, $5, 5000, true)",
    )
    .bind(run)
    .bind(file)
    .bind(format!("orig-{run}"))
    .bind(start)
    .bind(end)
    .execute(db)
    .await
    .unwrap();
    run
}

/// Records a claim as if it arrived right when the workout ended.
async fn claim(db: &PgPool, dogs: &[&str], start: Option<&str>, end: Option<&str>) -> Uuid {
    let id = companions::record(
        db,
        &Claim {
            dogs: dogs.iter().map(|d| d.to_string()).collect(),
            started_at: start.map(t),
            ended_at: end.map(t),
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE companion_claims SET received_at = COALESCE(ended_at, received_at) WHERE id = $1",
    )
    .bind(id)
    .execute(db)
    .await
    .unwrap();
    id
}

async fn claim_state(db: &PgPool, id: Uuid) -> (String, Option<Uuid>) {
    sqlx::query_as("SELECT status, run_id FROM companion_claims WHERE id = $1")
        .bind(id)
        .fetch_one(db)
        .await
        .unwrap()
}

/// (dog, status, generation) of all copies of a run.
async fn copies(db: &PgPool, run: Uuid) -> Vec<(String, String, i32)> {
    sqlx::query_as(
        "SELECT dog, status, generation FROM uploads WHERE run_id = $1 ORDER BY dog, generation",
    )
    .bind(run)
    .fetch_all(db)
    .await
    .unwrap()
}

async fn set_copies_done(db: &PgPool, run: Uuid) {
    sqlx::query("UPDATE uploads SET status = 'done', job_id = 'j', endurain_activity_id = 'c' WHERE run_id = $1 AND status = 'pending'")
        .bind(run)
        .execute(db)
        .await
        .unwrap();
}

async fn plan(db: &PgPool, now: &str) {
    companions::match_claims(db, t(now)).await.unwrap();
    companions::plan_copies(db, &dogs()).await.unwrap();
}

#[tokio::test]
async fn claim_waits_for_its_own_run() {
    let Some(db) = fresh_db().await else { return };
    // A: 07:30–08:00 is synced; B: 08:05–08:10 is not yet
    let a = run(&db, "2026-12-01T07:30:00Z", 30).await;
    let ca = claim(
        &db,
        &["nanuq"],
        Some("2026-12-01T07:30:00Z"),
        Some("2026-12-01T08:00:00Z"),
    )
    .await;
    plan(&db, "2026-12-01T08:01:00Z").await;
    set_copies_done(&db, a).await;

    let cb = claim(
        &db,
        &[],
        Some("2026-12-01T08:05:00Z"),
        Some("2026-12-01T08:10:00Z"),
    )
    .await;
    plan(&db, "2026-12-01T08:11:00Z").await;
    assert_eq!(claim_state(&db, cb).await.0, "pending");
    assert_eq!(
        copies(&db, a).await,
        vec![("nanuq".into(), "done".into(), 0)],
        "A's copy survives"
    );

    let b = run(&db, "2026-12-01T08:05:30Z", 4).await;
    plan(&db, "2026-12-01T08:20:00Z").await;
    assert_eq!(claim_state(&db, cb).await, ("matched".into(), Some(b)));
    assert_eq!(claim_state(&db, ca).await, ("matched".into(), Some(a)));
    assert_eq!(
        copies(&db, a).await,
        vec![("nanuq".into(), "done".into(), 0)]
    );
}

#[tokio::test]
async fn correction_affects_only_its_run_and_regenerates() {
    let Some(db) = fresh_db().await else { return };
    let mon = run(&db, "2026-11-30T07:00:00Z", 30).await;
    let tue = run(&db, "2026-12-01T07:00:00Z", 30).await;
    claim(
        &db,
        &["nanuq"],
        Some("2026-11-30T07:00:00Z"),
        Some("2026-11-30T07:30:00Z"),
    )
    .await;
    claim(
        &db,
        &["anori"],
        Some("2026-12-01T07:00:00Z"),
        Some("2026-12-01T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-01T08:00:00Z").await;
    set_copies_done(&db, mon).await;
    set_copies_done(&db, tue).await;

    // correction for Tuesday: Nanuq instead of Anori (duplicate claims are harmless)
    for _ in 0..2 {
        claim(
            &db,
            &["nanuq"],
            Some("2026-12-01T07:00:00Z"),
            Some("2026-12-01T07:30:00Z"),
        )
        .await;
    }
    plan(&db, "2026-12-01T09:00:00Z").await;
    assert_eq!(
        copies(&db, mon).await,
        vec![("nanuq".into(), "done".into(), 0)],
        "Monday untouched"
    );
    assert_eq!(
        copies(&db, tue).await,
        vec![
            ("anori".into(), "delete_pending".into(), 0),
            ("nanuq".into(), "pending".into(), 0)
        ]
    );

    // Anori after all: the deleted copy comes back as a new generation with a new key
    sqlx::query("UPDATE uploads SET status = 'deleted' WHERE run_id = $1 AND dog = 'anori'")
        .bind(tue)
        .execute(&db)
        .await
        .unwrap();
    claim(
        &db,
        &["anori", "nanuq"],
        Some("2026-12-01T07:00:00Z"),
        Some("2026-12-01T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-01T10:00:00Z").await;
    let key: String = sqlx::query_scalar("SELECT idempotency_key FROM uploads WHERE run_id = $1 AND dog = 'anori' AND generation = 1")
        .bind(tue)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(key, format!("run:{tue}:anori:1"));
}

#[tokio::test]
async fn empty_list_ambiguity_expiry_and_fallback() {
    let Some(db) = fresh_db().await else { return };
    let r = run(&db, "2026-12-02T07:00:00Z", 30).await;
    claim(
        &db,
        &["anori", "nanuq"],
        Some("2026-12-02T07:00:00Z"),
        Some("2026-12-02T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-02T08:00:00Z").await;
    set_copies_done(&db, r).await;
    claim(
        &db,
        &[],
        Some("2026-12-02T07:00:00Z"),
        Some("2026-12-02T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-02T08:05:00Z").await;
    assert!(
        copies(&db, r)
            .await
            .iter()
            .all(|(_, s, _)| s == "delete_pending"),
        "nobody → copies removed"
    );

    // two runs within the tolerance: no guessing
    run(&db, "2026-12-03T07:00:00Z", 30).await;
    run(&db, "2026-12-03T07:01:00Z", 30).await;
    let amb = claim(
        &db,
        &["nanuq"],
        Some("2026-12-03T07:00:30Z"),
        Some("2026-12-03T07:30:30Z"),
    )
    .await;
    plan(&db, "2026-12-03T08:00:00Z").await;
    assert_eq!(claim_state(&db, amb).await.0, "pending");

    // nothing matches for 24 h -> expired
    sqlx::query("UPDATE companion_claims SET received_at = $2 WHERE id = $1")
        .bind(amb)
        .bind(t("2026-12-03T07:31:00Z"))
        .execute(&db)
        .await
        .unwrap();
    plan(&db, "2026-12-04T08:00:00Z").await;
    assert_eq!(claim_state(&db, amb).await.0, "expired");

    // fallback without times: the run that ended in [received − 5 min, received + 1 min]
    let f = run(&db, "2026-12-05T07:00:00Z", 30).await;
    let fb = claim(&db, &["anori"], None, None).await;
    sqlx::query("UPDATE companion_claims SET received_at = $2 WHERE id = $1")
        .bind(fb)
        .bind(t("2026-12-05T07:32:00Z"))
        .execute(&db)
        .await
        .unwrap();
    plan(&db, "2026-12-05T07:33:00Z").await;
    assert_eq!(claim_state(&db, fb).await, ("matched".into(), Some(f)));
}

#[tokio::test]
async fn no_copies_for_treadmill_runs() {
    let Some(db) = fresh_db().await else { return };
    let r = run(&db, "2026-12-06T07:00:00Z", 30).await;
    sqlx::query("UPDATE runs SET sport = 'treadmill_run', has_gps = false WHERE id = $1")
        .bind(r)
        .execute(&db)
        .await
        .unwrap();
    claim(
        &db,
        &["nanuq"],
        Some("2026-12-06T07:00:00Z"),
        Some("2026-12-06T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-06T08:00:00Z").await;
    assert!(copies(&db, r).await.is_empty());
}

// ---------- copy uploads against mocked Endurain accounts ----------

struct JobFor;
impl Respond for JobFor {
    fn respond(&self, req: &MockRequest) -> ResponseTemplate {
        let key = req.headers["idempotency-key"]
            .to_str()
            .unwrap()
            .replace(':', "-");
        ResponseTemplate::new(202)
            .set_body_json(json!({ "id": format!("job-{key}"), "status": "queued" }))
    }
}
struct JobDone;
impl Respond for JobDone {
    fn respond(&self, req: &MockRequest) -> ResponseTemplate {
        let id = req.url.path().rsplit('/').next().unwrap().to_owned();
        ResponseTemplate::new(200)
            .set_body_json(json!({ "id": id, "status": "succeeded", "activity_ids": ["copy-1"] }))
    }
}

async fn dog_server() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/activities/upload"))
        .respond_with(JobFor)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path_regex("^/api/v1/jobs/"))
        .respond_with(JobDone)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/activities/copy-1"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ETag", "\"3\"")
                .set_body_json(json!({ "id": "copy-1", "activity_type": "run", "distance": 5000 })),
        )
        .mount(&s)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/activities/copy-1"))
        .and(header("If-Match", "\"3\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/activities/copy-1"))
        .and(header("If-Match", "\"3\""))
        .respond_with(ResponseTemplate::new(204))
        .mount(&s)
        .await;
    s
}

async fn me_server() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex("^/api/v1/activities/orig-"))
        .respond_with(ResponseTemplate::new(200).insert_header("ETag", "\"1\"").set_body_json(json!({ "id": "o", "activity_type": "run", "distance": 5000, "name": "Morgenrunde" })))
        .mount(&s)
        .await;
    s
}

#[tokio::test]
async fn copy_is_uploaded_as_plain_gpx_renamed_and_deleted() {
    let Some(db) = fresh_db().await else { return };
    let (dog, me) = (dog_server().await, me_server().await);
    let me_acc = Account::new(reqwest::Client::new(), &me.uri(), "me");
    let accounts = HashMap::from([(
        "nanuq".to_string(),
        Account::new(reqwest::Client::new(), &dog.uri(), "dog"),
    )]);
    let r = run(&db, "2026-12-07T07:00:00Z", 30).await;
    claim(
        &db,
        &["nanuq"],
        Some("2026-12-07T07:00:00Z"),
        Some("2026-12-07T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-07T08:00:00Z").await;
    for _ in 0..3 {
        companions::process_copies(&db, &me_acc, &accounts)
            .await
            .unwrap();
    }
    assert_eq!(
        copies(&db, r).await,
        vec![("nanuq".into(), "done".into(), 0)]
    );

    let reqs = dog.received_requests().await.unwrap();
    let upload = reqs
        .iter()
        .find(|r| r.url.path() == "/api/v1/activities/upload")
        .unwrap();
    let body = String::from_utf8_lossy(&upload.body);
    assert!(body.contains("<trkpt") && body.contains(".gpx"));
    for banned in ["heart", "cadence", "power", "<hr>", "gpxtpx"] {
        assert!(
            !body.to_lowercase().contains(banned),
            "GPX must not contain {banned}"
        );
    }
    let patch = reqs.iter().find(|r| r.method.as_str() == "PATCH").unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&patch.body).unwrap();
    assert_eq!(
        sent,
        json!({ "name": "Morgenrunde", "activity_type": "run" })
    );

    // nobody after all -> the copy is deleted with If-Match
    claim(
        &db,
        &[],
        Some("2026-12-07T07:00:00Z"),
        Some("2026-12-07T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-07T09:00:00Z").await;
    companions::process_copies(&db, &me_acc, &accounts)
        .await
        .unwrap();
    assert_eq!(
        copies(&db, r).await,
        vec![("nanuq".into(), "deleted".into(), 0)]
    );
}

#[tokio::test]
async fn deleted_original_removes_copies() {
    let Some(db) = fresh_db().await else { return };
    let me = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex("^/api/v1/activities/"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&me)
        .await;
    let me_acc = Account::new(reqwest::Client::new(), &me.uri(), "me");
    let r = run(&db, "2026-12-08T07:00:00Z", 30).await;
    claim(
        &db,
        &["anori"],
        Some("2026-12-08T07:00:00Z"),
        Some("2026-12-08T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-08T08:00:00Z").await;
    set_copies_done(&db, r).await;

    companions::reconcile(&db, &me_acc, t("2026-12-09T03:00:00Z"))
        .await
        .unwrap();
    plan(&db, "2026-12-09T03:00:00Z").await;
    assert_eq!(
        copies(&db, r).await,
        vec![("anori".into(), "delete_pending".into(), 0)]
    );
}

// ---------- endpoint ----------

fn app(db: PgPool) -> axum::Router {
    http::router(AppState {
        db,
        display: Display::new(&demo_stats(NaiveDate::from_ymd_opt(2026, 12, 1).unwrap())),
        sync: Default::default(),
        webhook: None,
        companions: Some(CompanionsApi {
            token: "s3cret".into(),
            dogs: dogs(),
            trigger: Default::default(),
            last: Default::default(),
        }),
    })
}

fn post(token: &str, body: &str) -> Request<Body> {
    Request::post("/api/companions")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn endpoint_checks_token_dogs_and_rate() {
    let lazy = PgPool::connect_lazy("postgres://unused@localhost/unused").unwrap();
    let r = app(lazy.clone())
        .oneshot(post("wrong", r#"{"dogs":[]}"#))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = app(lazy.clone())
        .oneshot(post("s3cret", r#"{"dogs":["rex"]}"#))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    let big = format!(r#"{{"dogs":[],"x":"{}"}}"#, "a".repeat(5000));
    let r = app(lazy).oneshot(post("s3cret", &big)).await.unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let Some(db) = fresh_db().await else { return };
    let app = app(db.clone());
    let body = r#"{"dogs":["Nanuq"," anori","nanuq"],"started_at":"2026-12-01T07:00:00Z","ended_at":"2026-12-01T07:30:00Z"}"#;
    let r = app.clone().oneshot(post("s3cret", body)).await.unwrap();
    assert_eq!(r.status(), StatusCode::ACCEPTED);
    let r = app.oneshot(post("s3cret", body)).await.unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    let stored: Vec<String> = sqlx::query_scalar("SELECT dogs FROM companion_claims")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(stored, vec!["anori", "nanuq"]);
}

/// Stuck-copy cases: a stale ETag (412) is retried, a deleted original still lets the
/// copy finish, and an unwanted copy stuck in needs_attention is deleted.
#[tokio::test]
async fn copies_do_not_get_stuck_after_upload() {
    let Some(db) = fresh_db().await else { return };
    let dog = dog_server().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/activities/copy-1"))
        .respond_with(
            ResponseTemplate::new(412).set_body_json(json!({ "title": "Precondition Failed" })),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&dog)
        .await;
    let me = MockServer::start().await; // original deleted: every GET is a 404
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&me)
        .await;
    let me_acc = Account::new(reqwest::Client::new(), &me.uri(), "me");
    let accounts = HashMap::from([(
        "nanuq".to_string(),
        Account::new(reqwest::Client::new(), &dog.uri(), "dog"),
    )]);
    let r = run(&db, "2026-12-10T07:00:00Z", 30).await;
    claim(
        &db,
        &["nanuq"],
        Some("2026-12-10T07:00:00Z"),
        Some("2026-12-10T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-10T08:00:00Z").await;
    for _ in 0..4 {
        companions::process_copies(&db, &me_acc, &accounts)
            .await
            .unwrap();
    }
    assert_eq!(
        copies(&db, r).await,
        vec![("nanuq".into(), "done".into(), 0)]
    );
    let patch = dog
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .last()
        .unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&patch.body).unwrap();
    assert_eq!(sent["name"], "Run");

    // a copy that ended in needs_attention but exists in Endurain is still removed
    sqlx::query("UPDATE uploads SET status = 'needs_attention' WHERE run_id = $1")
        .bind(r)
        .execute(&db)
        .await
        .unwrap();
    claim(
        &db,
        &[],
        Some("2026-12-10T07:00:00Z"),
        Some("2026-12-10T07:30:00Z"),
    )
    .await;
    plan(&db, "2026-12-10T09:00:00Z").await;
    assert_eq!(
        copies(&db, r).await,
        vec![("nanuq".into(), "delete_pending".into(), 0)]
    );
}
