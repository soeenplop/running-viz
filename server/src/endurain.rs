//! Minimal Endurain API client (v0.21), authenticated per account with an API key.

use anyhow::{Context, Result};
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
    #[serde(default)]
    pub end_time: Option<DateTime<Utc>>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Background ingestion job (`/jobs/{id}`).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct Job {
    pub id: String,
    /// queued | running | succeeded | failed
    pub status: String,
    #[serde(default)]
    pub activity_ids: Vec<String>,
    pub error_code: Option<String>,
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

impl Account {
    /// Starts an upload job. Replaying the same `idempotency_key` returns the original job.
    pub async fn upload(
        &self,
        filename: &str,
        bytes: Vec<u8>,
        idempotency_key: &str,
    ) -> Result<Job> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(filename.to_owned());
        let form = reqwest::multipart::Form::new().part("file", part);
        send(
            self.http
                .post(format!("{}/activities/upload", self.base))
                .header("X-API-Key", &self.key)
                .header("Idempotency-Key", idempotency_key)
                .multipart(form),
        )
        .await
    }

    /// The job and its full JSON (kept for `needs_attention` diagnostics).
    pub async fn job(&self, id: &str) -> Result<(Job, serde_json::Value)> {
        let raw: serde_json::Value = send(self.get(&format!("/jobs/{id}"))).await?;
        let job = serde_json::from_value(raw.clone()).context("unexpected endurain job")?;
        Ok((job, raw))
    }

    pub async fn activity(&self, id: &str) -> Result<Activity> {
        send(self.get(&format!("/activities/{id}"))).await
    }

    /// The activity's current ETag (needed for `If-Match`), or `None` if it is gone.
    pub async fn activity_etag(&self, id: &str) -> Result<Option<String>> {
        let res = self
            .get(&format!("/activities/{id}"))
            .send()
            .await
            .context("endurain request failed")?;
        if res.status().as_u16() == 404 {
            return Ok(None);
        }
        if !res.status().is_success() {
            return Err(error(res).await.into());
        }
        res.headers()
            .get("ETag")
            .and_then(|v| v.to_str().ok())
            .map(|v| Some(v.to_owned()))
            .context("endurain activity without ETag")
    }

    /// Sets name and type of an activity (fresh ETag first, so the write is never stale).
    pub async fn rename(&self, id: &str, name: &str, activity_type: &str) -> Result<()> {
        let etag = self
            .activity_etag(id)
            .await?
            .context("activity to rename is gone")?;
        let _: serde_json::Value = send(
            self.http
                .patch(format!("{}/activities/{id}", self.base))
                .header("X-API-Key", &self.key)
                .header("If-Match", etag)
                .json(&serde_json::json!({ "name": name, "activity_type": activity_type })),
        )
        .await?;
        Ok(())
    }

    /// Deletes an activity; an activity that is already gone counts as deleted.
    pub async fn delete(&self, id: &str) -> Result<()> {
        let Some(etag) = self.activity_etag(id).await? else {
            return Ok(());
        };
        let res = self
            .http
            .delete(format!("{}/activities/{id}", self.base))
            .header("X-API-Key", &self.key)
            .header("If-Match", etag)
            .send()
            .await
            .context("endurain request failed")?;
        if res.status().is_success() || res.status().as_u16() == 404 {
            Ok(())
        } else {
            Err(error(res).await.into())
        }
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
        return Err(ApiError {
            status: status.as_u16(),
            title,
        }
        .into());
    }
    res.json().await.context("unexpected endurain response")
}

/// Endurain errors are problem+json without secrets; keep only the status and title.
async fn error(res: reqwest::Response) -> ApiError {
    let status = res.status().as_u16();
    let title = res
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|v| v["title"].as_str().map(str::to_owned))
        .unwrap_or_default();
    ApiError { status, title }
}

/// An HTTP error answer from Endurain (status + problem title, no body).
#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub title: String,
}

impl ApiError {
    /// A 4xx about this request that retrying cannot fix. Account problems (401/403:
    /// expired key, missing scope), timeouts, rate limits and the "too many open
    /// uploads" conflict stay retryable: fixing them recovers every queued upload.
    pub fn is_permanent(&self) -> bool {
        (400..500).contains(&self.status) && ![401, 403, 408, 409, 429].contains(&self.status)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "endurain answered {} {}", self.status, self.title)
    }
}

impl std::error::Error for ApiError {}
