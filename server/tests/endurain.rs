//! Endurain client and stats against a mocked API.

use chrono::NaiveDate;
use running_viz::{endurain::Account, stats};
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
    mock(
        &server,
        "/api/v1/profile/goals",
        json!({ "items": [{ "goal_distance": 200000 }], "total": 1 }),
    )
    .await;
    let page1: Vec<_> = (0..100)
        .map(|i| activity(i, "2026-12-01T07:00:00Z", 0.1))
        .collect();
    Mock::given(method("GET"))
        .and(path("/api/v1/activities"))
        .and(query_param("page_number", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "items": page1, "total": 101 })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/activities"))
        .and(query_param("page_number", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({ "items": [activity(100, "2026-12-03T07:00:00Z", 10.0)], "total": 101 }),
        ))
        .mount(&server)
        .await;

    let me = Account::new(reqwest::Client::new(), &server.uri(), "k");
    let today = NaiveDate::from_ymd_opt(2026, 12, 14).unwrap();
    let (runner, month) = stats::runner(&me, stats::MY_RUN_TYPES, 1.0, today)
        .await
        .unwrap();
    assert_eq!(runner.name, "Sören");
    assert_eq!(runner.goal_km, 200.0);
    assert!((runner.km - 20.0).abs() < 1e-9);
    assert_eq!(runner.pace_s_per_km, Some(330));
    assert!((month.daily_km[0] - 10.0).abs() < 1e-9 && month.daily_km[2] == 10.0);
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
