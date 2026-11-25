//! Endurain client and stats against a mocked API.

use chrono::NaiveDate;
use running_viz::{
    display::Display,
    endurain::Account,
    render::demo_stats,
    stats,
    sync::{self, SyncStatus},
};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path, query_param},
};

async fn mock(server: &MockServer, p: &str, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(p))
        .and(header("X-API-Key", "k"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

fn activity(id: u32, start: &str, km: f64) -> serde_json::Value {
    json!({ "id": id.to_string(), "activity_type": "run", "distance": (km * 1000.0) as i64,
            "total_timer_time": km * 330.0, "start_time": start, "name": "Run" })
}

#[tokio::test]
async fn fetches_name_goal_and_all_pages() {
    let server = MockServer::start().await;
    mock(
        &server,
        "/api/v1/profile",
        json!({ "name": "Sören", "username": "soeren" }),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/profile/goals"))
        .and(header("X-API-Key", "k"))
        .and(query_param("interval", "monthly"))
        .and(query_param("activity_type", "run"))
        .and(query_param("goal_type", "distance"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [{ "goal_distance": 200000 }], "total": 1 })),
        )
        .mount(&server)
        .await;
    let page1: Vec<_> = (0..100)
        .map(|i| activity(i, "2026-12-01T07:00:00Z", 0.1))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/v1/activities"))
        .and(query_param("start_date", "2026-11-30"))
        .and(query_param("end_date", "2027-01-01"))
        .and(query_param("num_records", "100"))
        .and(query_param("page_number", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "items": page1, "total": 101 })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/activities"))
        .and(query_param("start_date", "2026-11-30"))
        .and(query_param("end_date", "2027-01-01"))
        .and(query_param("num_records", "100"))
        .and(query_param("page_number", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "items": [activity(100, "2026-12-03T07:00:00Z", 10.0)], "total": 101 }),
        ))
        .mount(&server)
        .await;

    let me = Account::new(reqwest::Client::new(), &server.uri(), "k");
    let today = NaiveDate::from_ymd_opt(2026, 12, 14).unwrap();
    let runner = stats::runner(&me, stats::MY_RUN_TYPES, 1.0, today)
        .await
        .unwrap();
    assert_eq!(runner.name, "Sören");
    assert_eq!(runner.goal_km, 200.0);
    assert!((runner.km - 20.0).abs() < 1e-9);
    assert_eq!(runner.pace_s_per_km, Some(330));
    assert!((runner.daily_km[0] - 10.0).abs() < 1e-9 && runner.daily_km[2] == 10.0);
}

#[tokio::test]
async fn http_errors_fail_the_refresh() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "title": "Unauthorized" })))
        .mount(&server)
        .await;
    let me = Account::new(reqwest::Client::new(), &server.uri(), "bad");
    let today = NaiveDate::from_ymd_opt(2026, 12, 14).unwrap();
    let err = stats::fetch(&me, &[], today).await.unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");
}

/// A successful refresh followed by a 401: the image and the last success survive,
/// and the status reports the failure.
#[tokio::test]
async fn failed_refresh_keeps_image_and_reports_failure() {
    let server = MockServer::start().await;
    mock(&server, "/api/v1/profile", json!({ "name": "Sören" })).await;
    mock(
        &server,
        "/api/v1/profile/goals",
        json!({ "items": [], "total": 0 }),
    )
    .await;
    mock(
        &server,
        "/api/v1/activities",
        json!({ "items": [activity(1, "2026-11-02T07:00:00Z", 5.0)], "total": 1 }),
    )
    .await;

    let accounts = [Account::new(reqwest::Client::new(), &server.uri(), "k")];
    let display = Display::new(&demo_stats(NaiveDate::from_ymd_opt(2026, 12, 14).unwrap()));
    let status = SyncStatus::default();

    sync::refresh(&accounts, &display, &status).await;
    let ok = status.snapshot();
    assert!(ok.last_success.is_some() && ok.last_error.is_none());
    let etag = display.current().etag.clone();

    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "title": "Unauthorized" })))
        .mount(&server)
        .await;
    sync::refresh(&accounts, &display, &status).await;
    let failed = status.snapshot();
    assert_eq!(failed.last_success, ok.last_success);
    assert!(failed.last_error.unwrap().contains("401"));
    assert_eq!(display.current().etag, etag);
}
