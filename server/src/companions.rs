//! "Who came along?": Shortcut claims → matched runs → GPX copies in the dogs' accounts.
//!
//! - `record`: stores a claim from the Shortcut.
//! - `match_claims`: binds pending claims to exactly one run (start and end ±2 min;
//!   without times: the run that ended within [received − 5 min, received + 1 min]).
//! - `plan_copies`: the newest matched claim per run decides which dogs should have a copy;
//!   missing copies are queued, unwanted finished copies are queued for deletion.
//! - `process_copies`: resumable per-copy state machine, like the original uploads:
//!   `pending → job_started → uploaded → (rename) → done`, `delete_pending → deleted`.
//! - `reconcile`: once a day, copies of originals deleted in Endurain are removed.

use std::collections::HashMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    endurain::{Account, ApiError},
    gpx,
    stats::DOG_RUN_TYPES,
};

const TOLERANCE_MIN: i64 = 2;
const BATCH: i64 = 5;

/// A Shortcut answer.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Claim {
    pub dogs: Vec<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
}

pub async fn record(db: &PgPool, claim: &Claim) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companion_claims (id, dogs, started_at, ended_at) VALUES ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(&claim.dogs)
    .bind(claim.started_at)
    .bind(claim.ended_at)
    .execute(db)
    .await?;
    Ok(id)
}

/// Binds pending claims to runs; never guesses between several candidates.
pub async fn match_claims(db: &PgPool, now: DateTime<Utc>) -> Result<()> {
    let tol = Duration::minutes(TOLERANCE_MIN);
    // claims older than 24 h expire before any matching: a late match could undo
    // corrections the owner has made by hand in the meantime
    sqlx::query(
        "UPDATE companion_claims SET status = 'expired' WHERE status = 'pending' AND received_at < $1",
    )
    .bind(now - Duration::hours(24))
    .execute(db)
    .await?;
    let claims = sqlx::query(
        "SELECT id, received_at, started_at, ended_at FROM companion_claims WHERE status = 'pending'",
    )
    .fetch_all(db)
    .await?;
    for c in claims {
        let id: Uuid = c.get("id");
        let received: DateTime<Utc> = c.get("received_at");
        let started: Option<DateTime<Utc>> = c.get("started_at");
        let ended: Option<DateTime<Utc>> = c.get("ended_at");
        let candidates: Vec<Uuid> = match (started, ended) {
            (Some(s), Some(e)) => sqlx::query_scalar(
                "SELECT id FROM runs WHERE start_time BETWEEN $1 AND $2 AND end_time BETWEEN $3 AND $4",
            )
            .bind(s - tol)
            .bind(s + tol)
            .bind(e - tol)
            .bind(e + tol),
            (None, Some(e)) => {
                sqlx::query_scalar("SELECT id FROM runs WHERE end_time BETWEEN $1 AND $2")
                    .bind(e - tol)
                    .bind(e + tol)
            }
            // fallback without workout times (accepted residual risk, see docs/PLAN.md)
            _ => sqlx::query_scalar("SELECT id FROM runs WHERE end_time BETWEEN $1 AND $2")
                .bind(received - Duration::minutes(5))
                .bind(received + Duration::minutes(1)),
        }
        .fetch_all(db)
        .await?;
        match candidates.as_slice() {
            [run] => {
                sqlx::query(
                    "UPDATE companion_claims SET status = 'matched', run_id = $2 WHERE id = $1",
                )
                .bind(id)
                .bind(run)
                .execute(db)
                .await?;
            }
            [] => {}
            many => {
                tracing::warn!(%id, candidates = many.len(), "claim matches several runs, not guessing");
            }
        }
    }
    Ok(())
}

/// Brings the queued copies in line with the newest claim per run.
pub async fn plan_copies(db: &PgPool, dogs: &[String]) -> Result<()> {
    let runs = sqlx::query(
        "SELECT DISTINCT ON (c.run_id) c.run_id, c.dogs, c.received_at, r.sport, r.has_gps, r.gone, r.fit_file_id
         FROM companion_claims c JOIN runs r ON r.id = c.run_id
         WHERE c.status = 'matched'
         ORDER BY c.run_id, c.received_at DESC",
    )
    .fetch_all(db)
    .await?;
    for r in runs {
        let run: Uuid = r.get("run_id");
        let wanted_dogs: Vec<String> = r.get("dogs");
        let claimed_at: DateTime<Utc> = r.get("received_at");
        let sport: String = r.get("sport");
        let eligible = DOG_RUN_TYPES.contains(&sport.as_str())
            && r.get::<bool, _>("has_gps")
            && !r.get::<bool, _>("gone");
        for dog in dogs {
            let want = eligible && wanted_dogs.contains(dog);
            let latest: Option<(Uuid, String, i32, Option<DateTime<Utc>>)> = sqlx::query_as(
                "SELECT id, status, generation, requested_at FROM uploads WHERE kind = 'copy' AND run_id = $1 AND dog = $2
                 ORDER BY generation DESC LIMIT 1",
            )
            .bind(run)
            .bind(dog)
            .fetch_optional(db)
            .await?;
            let active = latest.as_ref().filter(|(_, s, _, _)| s != "deleted");
            // a deleted copy (by us or by hand in Endurain) comes back only for a newer claim
            let fresh_claim = latest
                .as_ref()
                .is_none_or(|(_, _, _, requested)| requested.is_none_or(|r| claimed_at > r));
            match (want, active) {
                // an active copy has handled this claim: remember it, so a later manual
                // removal is not undone by the same (already handled) claim
                // (not while a deletion is pending: that one finishes first, and the claim
                // then creates the next generation)
                (true, Some((id, status, _, _))) if status != "delete_pending" => {
                    sqlx::query(
                        "UPDATE uploads SET requested_at = GREATEST(requested_at, $2) WHERE id = $1",
                    )
                    .bind(id)
                    .bind(claimed_at)
                    .execute(db)
                    .await?;
                }
                (true, None) if fresh_claim => {
                    let generation = latest.map_or(0, |(_, _, g, _)| g + 1);
                    sqlx::query(
                        "INSERT INTO uploads (id, kind, fit_file_id, run_id, dog, generation, idempotency_key, requested_at)
                         VALUES ($1, 'copy', $2, $3, $4, $5, $6, $7)",
                    )
                    .bind(Uuid::new_v4())
                    .bind(r.get::<Uuid, _>("fit_file_id"))
                    .bind(run)
                    .bind(dog)
                    .bind(generation)
                    .bind(format!("run:{run}:{dog}:{generation}"))
                    .bind(claimed_at)
                    .execute(db)
                    .await?;
                    tracing::info!(%run, dog, "queued dog copy");
                }
                // only finished copies are deleted; unfinished ones finish first (a crashed
                // upload may already exist in Endurain), then the next pass removes them
                // also copies stuck in needs_attention that exist in Endurain
                (false, Some((id, status, _, _)))
                    if status == "done"
                        || (status == "needs_attention" && has_activity(db, *id).await?) =>
                {
                    sqlx::query("UPDATE uploads SET status = 'delete_pending', updated_at = now() WHERE id = $1")
                        .bind(id)
                        .execute(db)
                        .await?;
                    tracing::info!(%run, dog, "dog copy to be deleted");
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Advances open copy uploads/deletions by one step each, least recently tried first.
/// Returns the number of failed steps.
pub async fn process_copies(
    db: &PgPool,
    me: &Account,
    dogs: &HashMap<String, Account>,
) -> Result<usize> {
    let rows = sqlx::query(
        "SELECT u.id, u.status, u.job_id, u.idempotency_key, u.dog, u.endurain_activity_id AS copy_id,
                u.fit_file_id, r.id AS run_id, r.endurain_activity_id AS original_id, r.sport
         FROM uploads u JOIN runs r ON r.id = u.run_id
         WHERE u.kind = 'copy' AND u.status IN ('pending', 'job_started', 'uploaded', 'delete_pending')
         ORDER BY u.updated_at LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(db)
    .await?;
    let mut failed = 0;
    for row in rows {
        let id: Uuid = row.get("id");
        let dog: String = row.get("dog");
        let Some(account) = dogs.get(&dog) else {
            tracing::warn!(dog, "no Endurain account configured for this dog");
            continue;
        };
        match step(db, me, account, &row).await {
            Ok(true) => {}
            Ok(false) => failed += 1,
            Err(err) => {
                failed += 1;
                let permanent = err
                    .downcast_ref::<ApiError>()
                    .is_some_and(ApiError::is_permanent)
                    || err.downcast_ref::<NoTrack>().is_some();
                tracing::warn!(%id, dog, permanent, "copy step failed: {err:#}");
                sqlx::query(
                    "UPDATE uploads SET attempts = attempts + 1, last_error = $2, updated_at = now(),
                            status = CASE WHEN $3 THEN 'needs_attention' ELSE status END
                     WHERE id = $1",
                )
                .bind(id)
                .bind(format!("{err}"))
                .bind(permanent)
                .execute(db)
                .await?;
            }
        }
    }
    Ok(failed)
}

#[derive(Debug)]
struct NoTrack;
impl std::fmt::Display for NoTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("run has no GPS track")
    }
}
impl std::error::Error for NoTrack {}

/// One transition. `Ok(false)` = moved to `needs_attention`.
async fn step(
    db: &PgPool,
    me: &Account,
    dog: &Account,
    row: &sqlx::postgres::PgRow,
) -> Result<bool> {
    let id: Uuid = row.get("id");
    let status: String = row.get("status");
    match status.as_str() {
        "pending" => {
            let bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM fit_files WHERE id = $1")
                .bind(row.get::<Uuid, _>("fit_file_id"))
                .fetch_one(db)
                .await?;
            let run: Uuid = row.get("run_id");
            let track = gpx::from_fit(&bytes, "Run").map_err(|_| NoTrack)?;
            let key: String = row.get("idempotency_key");
            let job = dog
                .upload(&format!("run-{run}.gpx"), track.into_bytes(), &key)
                .await?;
            set(db, id, "job_started", "job_id", &job.id).await?;
            Ok(true)
        }
        "job_started" => {
            let (job, raw) = dog.job(&row.get::<String, _>("job_id")).await?;
            match (job.status.as_str(), job.activity_ids.as_slice()) {
                ("queued" | "running", _) => {
                    touch(db, id).await?;
                    Ok(true)
                }
                ("succeeded", [copy]) => {
                    set(db, id, "uploaded", "endurain_activity_id", copy).await?;
                    rename(db, me, dog, id, copy, row).await?;
                    Ok(true)
                }
                _ => {
                    tracing::warn!(%id, status = job.status, "dog copy needs attention");
                    sqlx::query(
                        "UPDATE uploads SET status = 'needs_attention', raw_job = $2, last_error = $3,
                                updated_at = now() WHERE id = $1",
                    )
                    .bind(id)
                    .bind(raw)
                    .bind(job.error_code)
                    .execute(db)
                    .await?;
                    Ok(false)
                }
            }
        }
        "uploaded" => {
            let copy: String = row.get("copy_id");
            rename(db, me, dog, id, &copy, row).await?;
            Ok(true)
        }
        "delete_pending" => {
            let copy: String = row.get("copy_id");
            dog.delete(&copy).await?;
            sqlx::query("UPDATE uploads SET status = 'deleted', updated_at = now() WHERE id = $1")
                .bind(id)
                .execute(db)
                .await?;
            tracing::info!(%id, "dog copy deleted");
            Ok(true)
        }
        _ => Ok(true),
    }
}

/// Gives the copy the original's name and Endurain type, then marks it done.
async fn rename(
    db: &PgPool,
    me: &Account,
    dog: &Account,
    id: Uuid,
    copy: &str,
    row: &sqlx::postgres::PgRow,
) -> Result<()> {
    if dog.activity_etag(copy).await?.is_none() {
        return mark_removed(db, id).await;
    }
    let original: String = row.get("original_id");
    // the original may be gone (deleted in Endurain): the copy still gets a name,
    // and reconcile removes it later
    let name = match me.activity(&original).await {
        Ok(a) => a.name.unwrap_or_else(|| "Run".into()),
        Err(err)
            if err
                .downcast_ref::<ApiError>()
                .is_some_and(|e| e.status == 404) =>
        {
            "Run".into()
        }
        Err(err) => return Err(err),
    };
    match dog
        .rename(copy, &name, &row.get::<String, _>("sport"))
        .await
    {
        Ok(()) => {}
        // removed by hand between the ETag read and the PATCH
        Err(err)
            if err
                .downcast_ref::<ApiError>()
                .is_some_and(|e| e.status == 404) =>
        {
            return mark_removed(db, id).await;
        }
        Err(err) => return Err(err),
    }
    sqlx::query("UPDATE uploads SET status = 'done', updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// The copy was removed by hand in Endurain: respect that.
async fn mark_removed(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE uploads SET status = 'deleted', updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    tracing::info!(%id, "dog copy was removed in Endurain");
    Ok(())
}

async fn has_activity(db: &PgPool, id: Uuid) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT endurain_activity_id IS NOT NULL FROM uploads WHERE id = $1")
            .bind(id)
            .fetch_one(db)
            .await?,
    )
}

async fn set(db: &PgPool, id: Uuid, status: &str, column: &str, value: &str) -> Result<()> {
    // column is one of two fixed names (never user input)
    let sql = match column {
        "job_id" => "UPDATE uploads SET status = $2, job_id = $3, updated_at = now() WHERE id = $1",
        _ => {
            "UPDATE uploads SET status = $2, endurain_activity_id = $3, updated_at = now() WHERE id = $1"
        }
    };
    sqlx::query(sql)
        .bind(id)
        .bind(status)
        .bind(value)
        .execute(db)
        .await?;
    Ok(())
}

async fn touch(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE uploads SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Once a day: originals of the current and previous month that were deleted in Endurain
/// are marked `gone`, so their dog copies and SparkyFitness mirror entries are removed.
/// Only a 404 counts.
pub async fn reconcile(db: &PgPool, me: &Account, now: DateTime<Utc>) -> Result<()> {
    let this_month = now.date_naive().with_day(1).context("first of month")?;
    let since = (this_month - Duration::days(1))
        .with_day(1)
        .context("first of last month")?;
    let runs: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT r.id, r.endurain_activity_id FROM runs r WHERE NOT r.gone AND r.start_time >= $1",
    )
    .bind(since.and_hms_opt(0, 0, 0).expect("midnight").and_utc())
    .fetch_all(db)
    .await?;
    for (run, original) in runs {
        if me.activity_etag(&original).await?.is_none() {
            sqlx::query("UPDATE runs SET gone = TRUE WHERE id = $1")
                .bind(run)
                .execute(db)
                .await?;
            tracing::info!(%run, "original deleted in Endurain: removing dog copies");
        }
    }
    Ok(())
}

/// Claims → runs → copies; deleted originals are checked once a day.
pub async fn pass(
    db: &PgPool,
    me: &Account,
    dogs: &[String],
    dog_accounts: &std::collections::HashMap<String, Account>,
    last_reconcile: &mut Option<chrono::NaiveDate>,
) -> Result<()> {
    let now = chrono::Utc::now();
    if *last_reconcile != Some(now.date_naive()) {
        // independent of the rest: a failure is logged and retried on the next pass
        match reconcile(db, me, now).await {
            Ok(()) => *last_reconcile = Some(now.date_naive()),
            Err(err) => tracing::warn!("reconcile failed: {err:#}"),
        }
    }
    match_claims(db, now).await?;
    plan_copies(db, dogs).await?;
    for _ in 0..10 {
        process_copies(db, me, dog_accounts).await?;
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM uploads WHERE kind = 'copy'
             AND status IN ('pending', 'job_started', 'uploaded', 'delete_pending')",
        )
        .fetch_one(db)
        .await?;
        if open == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Ok(())
}
