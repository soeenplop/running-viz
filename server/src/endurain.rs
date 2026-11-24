//! Minimal Endurain API client (v0.21), authenticated per account with an API key.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;

/// Endurain access for one account.
#[derive(Clone)]
pub struct Account {
    http: Client,
    base: String,
    key: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Activity {
    pub id: String,
    pub activity_type: String,
    /// metres
    pub distance: i64,
    /// moving time in seconds
    pub total_timer_time: Option<f64>,
    pub start_time: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct Page<T> {
    items: Vec<T>,
    total: i64,
}

#[derive(Deserialize)]
struct Goal {
    goal_distance: Option<i64>,
}

#[derive(Deserialize)]
struct Profile {
    name: String,
}

impl Account {
    pub fn new(http: Client, base_url: &str, key: &str) -> Self {
        Self {
            http,
            base: format!("{}/api/v1", base_url.trim_end_matches('/')),
            key: key.to_owned(),
        }
    }

    fn get(&self, path: &str) -> RequestBuilder {
        self.http
            .get(format!("{}{path}", self.base))
            .header("X-API-Key", &self.key)
    }

    /// Display name from the Endurain profile (e.g. "Sören").
    pub async fn name(&self) -> Result<String> {
        let p: Profile = send(self.get("/profile")).await?;
        Ok(p.name)
    }

    /// Monthly run distance goal in km, if one is set.
    pub async fn monthly_run_goal_km(&self) -> Result<Option<f64>> {
        let page: Page<Goal> = send(self.get("/profile/goals").query(&[
            ("interval", "monthly"),
            ("activity_type", "run"),
            ("goal_type", "distance"),
        ]))
        .await?;
        Ok(page
            .items
            .first()
            .and_then(|g| g.goal_distance)
            .map(|m| m as f64 / 1000.0))
    }

    /// All activities with a start date in `[from, to]` (inclusive, Endurain's dates).
    pub async fn activities(&self, from: NaiveDate, to: NaiveDate) -> Result<Vec<Activity>> {
        const PAGE: i64 = 100;
        let mut all = Vec::new();
        for page in 1.. {
            let res: Page<Activity> = send(self.get("/activities").query(&[
                ("start_date", from.to_string()),
                ("end_date", to.to_string()),
                ("page_number", page.to_string()),
                ("num_records", PAGE.to_string()),
            ]))
            .await?;
            let done = res.items.is_empty() || page * PAGE >= res.total;
            all.extend(res.items);
            if done {
                break;
            }
        }
        Ok(all)
    }
}

async fn send<T: serde::de::DeserializeOwned>(req: RequestBuilder) -> Result<T> {
    let res = req.send().await.context("endurain request failed")?;
    let status = res.status();
    if !status.is_success() {
        // Endurain errors are problem+json without secrets; keep only the status and title.
        let title = res
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["title"].as_str().map(str::to_owned))
            .unwrap_or_default();
        bail!("endurain answered {status} {title}");
    }
    res.json().await.context("unexpected endurain response")
}
