//! Optional mirror of the runs into SparkyFitness (to compare it with Endurain).
//!
//! SparkyFitness replaces all workouts of one `source` on a date with each import, so the
//! mirror always sends a whole day per account: my runs, or the runs a dog has a finished
//! copy of. A day is only sent again when its content changed (hash in `sparky_days`).
//! Records carry time, distance and the GPS track only - no heart rate or other body data.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};
use chrono_tz::Europe::Berlin;
use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{gpx, stats::DOG_RUN_TYPES, stats::MY_RUN_TYPES};

/// The `source` of every record we send; entries of other sources are never touched.
pub const SOURCE: &str = "running-viz";

/// `sparky_days.payload_hash` while a POST for that day may be in flight.
const PENDING: &str = "pending";

/// One SparkyFitness account.
#[derive(Clone)]
pub struct SparkyAccount {
    http: Client,
    base: String,
    key: String,
}

impl SparkyAccount {
    pub fn new(http: Client, base_url: &str, key: &str) -> Self {
        Self {
            http,
            base: base_url.trim_end_matches('/').to_owned(),
            key: key.to_owned(),
        }
    }

    async fn post_day(&self, records: &[Value]) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/api/health-data", self.base))
            .header("x-api-key", &self.key)
            .header("X-Workout-Model-Version", "2")
            .json(records)
            .send()
            .await
            .context("sparkyfitness request failed")?;
        if !res.status().is_success() {
            bail!("sparkyfitness answered {}", res.status());
        }
        let body: Value = res
            .json()
            .await
            .context("unexpected sparkyfitness response")?;
        let errors = body["errors"].as_array().map_or(0, Vec::len);
        if errors > 0 {
            bail!("sparkyfitness rejected {errors} record(s)");
        }
        Ok(())
    }

    /// Deletes our own entries of a day (used when a day has no runs any more).
    async fn clear_day(&self, day: NaiveDate) -> Result<()> {
        let res = self
            .http
            .get(format!("{}/api/exercise-entries/by-date", self.base))
            .query(&[("selectedDate", day.to_string())])
            .header("x-api-key", &self.key)
            .send()
            .await
            .context("sparkyfitness request failed")?;
        if !res.status().is_success() {
            bail!("sparkyfitness answered {}", res.status());
        }
        let entries: Vec<Value> = res
            .json()
            .await
            .context("unexpected sparkyfitness response")?;
        for e in entries {
            // ownership from the stored import metadata, never from the editable notes
            let snapshot = &e["exercise_snapshot"];
            if snapshot["source"].as_str() != Some(SOURCE)
                || snapshot["source_id"].as_str().is_none()
            {
                continue;
            }
            let id = e["id"].as_str().context("sparkyfitness entry without id")?;
            let res = self
                .http
                .delete(format!("{}/api/exercise-entries/{id}", self.base))
                .header("x-api-key", &self.key)
                .send()
                .await
                .context("sparkyfitness request failed")?;
            if !res.status().is_success() && res.status().as_u16() != 404 {
                bail!("sparkyfitness answered {}", res.status());
            }
        }
        Ok(())
    }
}

/// id, start, end, distance (m), FIT bytes
type RunRow = (Uuid, DateTime<Utc>, DateTime<Utc>, i32, Vec<u8>);

/// A run as SparkyFitness sees it.
#[derive(Debug, Clone)]
pub struct Run {
    pub id: Uuid,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub distance_m: i32,
    pub fit: Vec<u8>,
}

/// The record for one run: time, distance (km) and GPS (every 5th point), nothing else.
pub fn record(run: &Run) -> Value {
    let points: Vec<Value> = gpx::track(&run.fit)
        .unwrap_or_default()
        .iter()
        .step_by(5)
        .map(|p| {
            let mut v = json!({
                "t": p.time.to_rfc3339_opts(SecondsFormat::Secs, true),
                "lat": (p.lat * 1e7).round() / 1e7,
                "lon": (p.lon * 1e7).round() / 1e7,
            });
            if let Some(e) = p.ele {
                v["alt"] = json!((e * 10.0).round() / 10.0);
            }
            v
        })
        .collect();
    let mut r = json!({
        "type": "ExerciseSession",
        "timestamp": run.start.to_rfc3339_opts(SecondsFormat::Secs, true),
        "record_timezone": "Europe/Berlin",
        "duration": duration_s(run),
        "distance": f64::from(run.distance_m) / 1000.0,
        "activityType": "running",
        "source": SOURCE,
        "source_id": run.id.to_string(),
    });
    if !points.is_empty() {
        r["gps_points"] = Value::Array(points);
    }
    r
}

/// Moving time from the FIT file (pauses excluded); elapsed time if the file has none.
fn duration_s(run: &Run) -> i64 {
    crate::fit::summary(&run.fit)
        .ok()
        .and_then(|s| s.timer_s)
        .map_or_else(
            || (run.end - run.start).num_seconds().max(0),
            |t| t.round() as i64,
        )
}

/// Groups runs by their local (Europe/Berlin) start date into day payloads.
pub fn days(runs: &[Run]) -> BTreeMap<NaiveDate, Vec<Value>> {
    let mut sorted = runs.to_vec();
    sorted.sort_by_key(|r| r.start);
    let mut out: BTreeMap<NaiveDate, Vec<Value>> = BTreeMap::new();
    for r in &sorted {
        out.entry(r.start.with_timezone(&Berlin).date_naive())
            .or_default()
            .push(record(r));
    }
    out
}

fn hash(records: &[Value]) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(records).unwrap_or_default(),
    ))
}

/// Mirrors the current and the previous month for every configured account.
/// `accounts`: (account name, `None` for me or `Some(dog)`, SparkyFitness account).
pub async fn mirror(
    db: &PgPool,
    accounts: &[(String, Option<String>, SparkyAccount)],
    now: DateTime<Utc>,
) -> Result<()> {
    let (since_day, since) = crate::stats::window_start(now);
    let mut first_error = None;
    for (name, dog, account) in accounts {
        if let Err(err) = mirror_account(db, name, dog.as_deref(), account, since, since_day).await
        {
            tracing::warn!(account = name, "sparkyfitness mirror failed: {err:#}");
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn mirror_account(
    db: &PgPool,
    name: &str,
    dog: Option<&str>,
    account: &SparkyAccount,
    since: DateTime<Utc>,
    since_day: NaiveDate,
) -> Result<()> {
    let rows: Vec<RunRow> = match dog {
        None => {
            sqlx::query_as(
                "SELECT r.id, r.start_time, r.end_time, r.distance_m, f.bytes
                 FROM runs r JOIN fit_files f ON f.id = r.fit_file_id
                 WHERE NOT r.gone AND r.sport = ANY($1) AND r.start_time >= $2",
            )
            .bind(MY_RUN_TYPES)
            .bind(since)
            .fetch_all(db)
            .await?
        }
        Some(dog) => {
            sqlx::query_as(
                "SELECT DISTINCT r.id, r.start_time, r.end_time, r.distance_m, f.bytes
                 FROM runs r JOIN fit_files f ON f.id = r.fit_file_id
                 JOIN uploads u ON u.run_id = r.id AND u.kind = 'copy' AND u.dog = $1 AND u.status = 'done'
                 WHERE NOT r.gone AND r.sport = ANY($2) AND r.start_time >= $3",
            )
            .bind(dog)
            .bind(DOG_RUN_TYPES)
            .bind(since)
            .fetch_all(db)
            .await?
        }
    };
    let runs: Vec<Run> = rows
        .into_iter()
        .map(|(id, start, end, distance_m, fit)| Run {
            id,
            start,
            end,
            distance_m,
            fit,
        })
        .collect();
    let mut wanted = days(&runs);
    wanted.retain(|d, _| *d >= since_day);

    // days we sent before that now have no runs must be cleared
    let known: Vec<(NaiveDate, String)> = sqlx::query_as(
        "SELECT day, payload_hash FROM sparky_days WHERE account = $1 AND day >= $2",
    )
    .bind(name)
    .bind(since_day)
    .fetch_all(db)
    .await?;
    for (day, old_hash) in &known {
        if !wanted.contains_key(day) && !old_hash.is_empty() {
            // pending first: a DELETE that succeeds remotely but fails for us must not
            // leave the old hash, or a later identical payload would be skipped
            store(db, name, *day, PENDING).await?;
            account.clear_day(*day).await?;
            store(db, name, *day, "").await?;
        }
    }
    for (day, records) in &wanted {
        let h = hash(records);
        if known.iter().any(|(d, old)| d == day && *old == h) {
            continue;
        }
        // recorded before the request: a day whose POST may have reached SparkyFitness is
        // reconciled later even if its runs disappear or revert to the previous content
        store(db, name, *day, PENDING).await?;
        account.post_day(records).await?;
        store(db, name, *day, &h).await?;
        tracing::info!(account = name, %day, runs = records.len(), "mirrored to sparkyfitness");
    }
    Ok(())
}

async fn store(db: &PgPool, account: &str, day: NaiveDate, hash: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO sparky_days (account, day, payload_hash) VALUES ($1, $2, $3)
         ON CONFLICT (account, day) DO UPDATE SET payload_hash = EXCLUDED.payload_hash, updated_at = now()",
    )
    .bind(account)
    .bind(day)
    .bind(hash)
    .execute(db)
    .await?;
    Ok(())
}
