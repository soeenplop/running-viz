//! SparkyFitness mirror: day payloads, change detection and cleanup.
//! DB tests need TEST_DATABASE_URL (fresh database per test).

mod common;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use common::{Workout, fit_file, fresh_db};
use running_viz::sparky::{self, Run, SparkyAccount};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

fn t(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn run(start: &str, minutes: i64, km: f64) -> Run {
    let start = t(start);
    Run {
        id: Uuid::new_v4(),
        start,
        end: start + Duration::minutes(minutes),
        distance_m: (km * 1000.0) as i32,
        fit: fit_file(&Workout::run(
            &start.to_rfc3339(),
            (minutes * 60) as u32,
            (km * 1000.0) as u32,
        )),
    }
}

#[test]
fn runs_are_grouped_per_berlin_day_with_km_and_gps_only() {
    let runs = [
        run("2026-12-01T17:00:00Z", 30, 5.0),
        run("2026-12-01T06:00:00Z", 20, 3.5),
        // 23:30 UTC on 1 Dec is already 2 Dec in Berlin
        run("2026-12-01T23:30:00Z", 10, 2.0),
    ];
    let days = sparky::days(&runs);
    let d1 = &days[&NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()];
    assert_eq!(d1.len(), 2);
    assert_eq!(
        d1[0]["timestamp"], "2026-12-01T06:00:00Z",
        "sorted by start"
    );
    assert_eq!(d1[1]["distance"], json!(5.0), "kilometres");
    assert_eq!(d1[1]["duration"], json!(1800));
    assert_eq!(d1[1]["source"], "running-viz");
    assert_eq!(
        days[&NaiveDate::from_ymd_opt(2026, 12, 2).unwrap()].len(),
        1
    );

    let r = &d1[1];
    let points = r["gps_points"].as_array().unwrap();
    assert!(!points.is_empty());
    for banned in ["hr", "cad", "power", "hr_samples", "caloriesBurned"] {
        assert!(r.get(banned).is_none(), "{banned} must not be sent");
        assert!(points.iter().all(|p| p.get(banned).is_none()));
    }
}

async fn insert_run(db: &PgPool, r: &Run, sport: &str) {
    let file = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO fit_files (id, dropbox_path, content_hash, bytes) VALUES ($1, '/x.fit', $2, $3)",
    )
    .bind(file)
    .bind(Uuid::new_v4().to_string())
    .bind(&r.fit)
    .execute(db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO runs (id, fit_file_id, endurain_activity_id, sport, start_time, end_time, distance_m, has_gps)
         VALUES ($1, $2, $3, $4, $5, $6, $7, true)",
    )
    .bind(r.id)
    .bind(file)
    .bind(format!("a-{}", r.id))
    .bind(sport)
    .bind(r.start)
    .bind(r.end)
    .bind(r.distance_m)
    .execute(db)
    .await
    .unwrap();
}

async fn sparky_server() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/health-data"))
        .and(header("x-api-key", "k"))
        .and(header("X-Workout-Model-Version", "2"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "processed": [], "errors": [] })),
        )
        .mount(&s)
        .await;
    s
}

async fn posts(s: &MockServer) -> Vec<Value> {
    s.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/health-data")
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

#[tokio::test]
async fn mirror_sends_whole_days_once_and_clears_emptied_days() {
    let Some(db) = fresh_db().await else { return };
    let now = Utc::now();
    let day = (now - Duration::days(1)).date_naive();
    let a = run(&format!("{day}T06:00:00Z"), 30, 5.0);
    let b = run(&format!("{day}T16:00:00Z"), 20, 3.0);
    let treadmill = run(&format!("{day}T12:00:00Z"), 20, 3.0);
    insert_run(&db, &a, "run").await;
    insert_run(&db, &b, "trail_run").await;
    insert_run(&db, &treadmill, "treadmill_run").await;

    let s = sparky_server().await;
    let me = vec![(
        "soeren".to_string(),
        None,
        SparkyAccount::new(reqwest::Client::new(), &s.uri(), "k"),
    )];
    sparky::mirror(&db, &me, now).await.unwrap();
    let sent = posts(&s).await;
    assert_eq!(sent.len(), 1, "one request for the day");
    assert_eq!(
        sent[0].as_array().unwrap().len(),
        3,
        "both runs and the treadmill run of the day"
    );

    // nothing changed: nothing is sent again
    sparky::mirror(&db, &me, now).await.unwrap();
    assert_eq!(posts(&s).await.len(), 1);

    // all runs of the day deleted in Endurain: only our entries are removed
    sqlx::query("UPDATE runs SET gone = TRUE")
        .execute(&db)
        .await
        .unwrap();
    let snap = |source: &str| json!({ "source": source, "source_id": "x" });
    Mock::given(method("GET"))
        .and(path("/api/exercise-entries/by-date"))
        .and(query_param("selectedDate", day.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": "ours", "notes": "Source: running-viz, Activity Type: running", "exercise_snapshot": snap("running-viz") },
            { "id": "ours-edited", "notes": "edited by hand", "exercise_snapshot": snap("running-viz") },
            { "id": "manual-copied-note", "notes": "Source: running-viz, Activity Type: running", "exercise_snapshot": snap("manual") },
            { "id": "other", "notes": "Source: running-viz-probe, Activity Type: running", "exercise_snapshot": snap("running-viz-probe") },
            { "id": "no-snapshot", "notes": "Source: running-viz, Activity Type: running" }
        ])))
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&s)
        .await;
    sparky::mirror(&db, &me, now).await.unwrap();
    let deleted: Vec<String> = s
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .map(|r| r.url.path().to_owned())
        .collect();
    assert_eq!(
        deleted,
        vec![
            "/api/exercise-entries/ours",
            "/api/exercise-entries/ours-edited"
        ],
        "ownership comes from the stored source, not from the notes"
    );
}

#[tokio::test]
async fn dogs_get_only_runs_with_a_finished_copy() {
    let Some(db) = fresh_db().await else { return };
    let now = Utc::now();
    let day = (now - Duration::days(1)).date_naive();
    let with = run(&format!("{day}T06:00:00Z"), 30, 5.0);
    let without = run(&format!("{day}T16:00:00Z"), 20, 3.0);
    insert_run(&db, &with, "run").await;
    insert_run(&db, &without, "run").await;
    sqlx::query(
        "INSERT INTO uploads (id, kind, fit_file_id, run_id, dog, idempotency_key, status, job_id, endurain_activity_id)
         SELECT gen_random_uuid(), 'copy', fit_file_id, id, 'nanuq', 'k1', 'done', 'j', 'c' FROM runs WHERE id = $1",
    )
    .bind(with.id)
    .execute(&db)
    .await
    .unwrap();

    let s = sparky_server().await;
    let dog = vec![(
        "nanuq".to_string(),
        Some("nanuq".to_string()),
        SparkyAccount::new(reqwest::Client::new(), &s.uri(), "k"),
    )];
    sparky::mirror(&db, &dog, now).await.unwrap();
    let sent = posts(&s).await;
    assert_eq!(sent.len(), 1);
    let records = sent[0].as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["source_id"], with.id.to_string());
}

#[test]
fn duration_is_moving_time() {
    let start = t("2026-12-03T07:00:00Z");
    let paused = Run {
        id: Uuid::new_v4(),
        start,
        end: start + Duration::minutes(40),
        distance_m: 5000,
        fit: fit_file(&Workout {
            timer_s: Some(30 * 60),
            ..Workout::run(&start.to_rfc3339(), 40 * 60, 5000)
        }),
    };
    assert_eq!(
        sparky::record(&paused)["duration"],
        json!(1800),
        "10 min pause excluded"
    );
}

/// A POST that may have reached SparkyFitness (but failed for us) is still cleaned up
/// if the run disappears before the retry.
#[tokio::test]
async fn failed_post_is_reconciled_after_the_run_disappears() {
    let Some(db) = fresh_db().await else { return };
    let now = Utc::now();
    let day = (now - Duration::days(1)).date_naive();
    let r = run(&format!("{day}T06:00:00Z"), 30, 5.0);
    insert_run(&db, &r, "run").await;
    let s = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/health-data"))
        .respond_with(ResponseTemplate::new(504))
        .mount(&s)
        .await;
    let me = vec![(
        "soeren".to_string(),
        None,
        SparkyAccount::new(reqwest::Client::new(), &s.uri(), "k"),
    )];
    assert!(sparky::mirror(&db, &me, now).await.is_err());

    sqlx::query("UPDATE runs SET gone = TRUE")
        .execute(&db)
        .await
        .unwrap();
    Mock::given(method("GET"))
        .and(path("/api/exercise-entries/by-date"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": "accepted-anyway", "exercise_snapshot": { "source": "running-viz", "source_id": r.id.to_string() } }
        ])))
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&s)
        .await;
    sparky::mirror(&db, &me, now).await.unwrap();
    let deleted = s
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .count();
    assert_eq!(deleted, 1);
}

/// Deleting a solo run (no dog copy) in Endurain is noticed by the daily reconcile,
/// so the mirror removes it as well.
#[tokio::test]
async fn reconcile_covers_solo_runs() {
    let Some(db) = fresh_db().await else { return };
    let r = run(
        &format!(
            "{}T06:00:00Z",
            (Utc::now() - Duration::days(1)).date_naive()
        ),
        30,
        5.0,
    );
    insert_run(&db, &r, "treadmill_run").await;
    let endurain = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&endurain)
        .await;
    let me = running_viz::endurain::Account::new(reqwest::Client::new(), &endurain.uri(), "k");
    running_viz::companions::reconcile(&db, &me, Utc::now())
        .await
        .unwrap();
    let gone: bool = sqlx::query_scalar("SELECT gone FROM runs WHERE id = $1")
        .bind(r.id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(gone);
}

/// A DELETE that fails for us must not leave the old hash: when the dog is added back
/// before the retry, the identical day is sent again.
#[tokio::test]
async fn failed_clear_then_restore_sends_the_day_again() {
    let Some(db) = fresh_db().await else { return };
    let now = Utc::now();
    let day = (now - Duration::days(1)).date_naive();
    let r = run(&format!("{day}T06:00:00Z"), 30, 5.0);
    insert_run(&db, &r, "run").await;
    sqlx::query(
        "INSERT INTO uploads (id, kind, fit_file_id, run_id, dog, idempotency_key, status, job_id, endurain_activity_id)
         SELECT gen_random_uuid(), 'copy', fit_file_id, id, 'nanuq', 'k1', 'done', 'j', 'c' FROM runs WHERE id = $1",
    )
    .bind(r.id)
    .execute(&db)
    .await
    .unwrap();
    let s = sparky_server().await;
    Mock::given(method("GET"))
        .and(path("/api/exercise-entries/by-date"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": "copy", "exercise_snapshot": { "source": "running-viz", "source_id": r.id.to_string() } }
        ])))
        .mount(&s)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(504))
        .mount(&s)
        .await;
    let dog = vec![(
        "nanuq".to_string(),
        Some("nanuq".to_string()),
        SparkyAccount::new(reqwest::Client::new(), &s.uri(), "k"),
    )];
    sparky::mirror(&db, &dog, now).await.unwrap();
    assert_eq!(posts(&s).await.len(), 1);

    // Nanuq removed: the clear fails (504) ...
    sqlx::query("UPDATE uploads SET status = 'deleted'")
        .execute(&db)
        .await
        .unwrap();
    assert!(sparky::mirror(&db, &dog, now).await.is_err());
    // ... and is added back before the retry: the same day must be sent again
    sqlx::query("UPDATE uploads SET status = 'done'")
        .execute(&db)
        .await
        .unwrap();
    sparky::mirror(&db, &dog, now).await.unwrap();
    assert_eq!(posts(&s).await.len(), 2);
}

/// The reconcile window starts at Berlin midnight: a run on the 1st at 00:30 Berlin
/// (still the previous day in UTC) is checked too.
#[tokio::test]
async fn reconcile_window_uses_berlin_midnight() {
    let Some(db) = fresh_db().await else { return };
    let r = run("2026-12-31T23:30:00Z", 30, 5.0); // 1 Jan 2027 00:30 Berlin
    insert_run(&db, &r, "run").await;
    let endurain = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&endurain)
        .await;
    let me = running_viz::endurain::Account::new(reqwest::Client::new(), &endurain.uri(), "k");
    running_viz::companions::reconcile(&db, &me, t("2027-02-15T12:00:00Z"))
        .await
        .unwrap();
    let gone: bool = sqlx::query_scalar("SELECT gone FROM runs WHERE id = $1")
        .bind(r.id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(gone);
}
