//! HealthFit → Dropbox → Endurain import of my workouts.
//!
//! 1. `fetch_new_files`: reads Dropbox changes page by page. Each new FIT file (bytes,
//!    facts and its queued upload) is stored in its own transaction; the cursor is saved
//!    after the whole page. A crash mid-page re-reads the page and skips known files
//!    (unique content hash), so nothing is lost or duplicated. A reset cursor starts a
//!    full listing again, which also skips known files.
//! 2. `process_uploads`: a resumable state machine per upload
//!    (`pending → job_started → uploaded → done`). Every step is persisted before the
//!    next external call, so a restart continues where it stopped. Answers that retrying
//!    cannot fix move the upload to `needs_attention`; it never blocks the others.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Europe::Berlin;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    dropbox::Dropbox,
    endurain::{Account, ApiError},
    fit,
};

/// Uploads handled per round (Endurain allows 10 open uploads per user).
const BATCH: i64 = 5;

/// Owner decision: workouts before November 2026 are not imported.
pub const DEFAULT_SINCE: (i32, u32, u32) = (2026, 11, 1);

/// Result of one import pass.
#[derive(Debug, Default)]
pub struct Outcome {
    pub runs_created: usize,
    pub fetch_error: Option<String>,
    pub failed_steps: usize,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.fetch_error.is_none() && self.failed_steps == 0
    }
}

/// Imports new Dropbox files, then advances the uploads. A Dropbox failure does not
/// stop uploads that are already queued.
pub async fn run(db: &PgPool, dropbox: &Dropbox, me: &Account, since: NaiveDate) -> Outcome {
    let mut out = Outcome::default();
    if let Err(err) = fetch_new_files(db, dropbox, since).await {
        tracing::warn!("dropbox fetch failed: {err:#}");
        out.fetch_error = Some(format!("{err}"));
    }
    // Endurain parses uploads within seconds: poll a few rounds so a new run
    // reaches the display now instead of on the next pass.
    for _ in 0..10 {
        match process_uploads(db, me).await {
            Ok(round) => {
                out.runs_created += round.runs_created;
                out.failed_steps += round.failed_steps;
            }
            Err(err) => {
                tracing::warn!("upload round failed: {err:#}");
                out.failed_steps += 1;
                break;
            }
        }
        let open: i64 = match sqlx::query_scalar(
            "SELECT count(*) FROM uploads WHERE kind = 'original' AND status IN ('pending', 'job_started', 'uploaded')",
        )
        .fetch_one(db)
        .await
        {
            Ok(n) => n,
            Err(_) => break,
        };
        if open == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    out
}

pub async fn fetch_new_files(db: &PgPool, dropbox: &Dropbox, since: NaiveDate) -> Result<()> {
    let cursor: Option<String> = sqlx::query_scalar("SELECT cursor FROM dropbox_state")
        .fetch_optional(db)
        .await?;
    let mut page = match cursor {
        Some(c) => match dropbox.list_continue(&c).await? {
            Some(page) => page,
            None => {
                tracing::info!("dropbox cursor was reset: listing the folder again");
                dropbox.list().await?
            }
        },
        None => dropbox.list().await?,
    };
    loop {
        for entry in &page.entries {
            let (Some(id), Some(listed_hash)) = (&entry.id, &entry.content_hash) else {
                continue;
            };
            if entry.tag != "file" || !entry.name.to_lowercase().ends_with(".fit") {
                continue;
            }
            if known(db, listed_hash).await? {
                continue;
            }
            let Some(download) = dropbox.download(id).await? else {
                continue; // deleted since the listing
            };
            // the file may have changed since the listing: trust the downloaded bytes' hash
            if download.content_hash != *listed_hash && known(db, &download.content_hash).await? {
                continue;
            }
            let mut tx = db.begin().await?;
            store_file(
                &mut tx,
                id,
                entry.path_display.as_deref().unwrap_or(&entry.name),
                &download.content_hash,
                download.bytes,
                since,
            )
            .await?;
            tx.commit().await?;
        }
        sqlx::query(
            "INSERT INTO dropbox_state (id, cursor) VALUES (TRUE, $1)
             ON CONFLICT (id) DO UPDATE SET cursor = EXCLUDED.cursor",
        )
        .bind(&page.cursor)
        .execute(db)
        .await?;
        if !page.has_more {
            return Ok(());
        }
        page = match dropbox.list_continue(&page.cursor).await? {
            Some(page) => page,
            None => dropbox.list().await?,
        };
    }
}

async fn known(db: &PgPool, hash: &str) -> Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM fit_files WHERE content_hash = $1)")
            .bind(hash)
            .fetch_one(db)
            .await?,
    )
}

/// True if the workout started (Europe/Berlin) before `since`.
pub fn before_since(start: DateTime<Utc>, since: NaiveDate) -> bool {
    start.with_timezone(&Berlin).date_naive() < since
}

/// Stores one FIT file and, unless it is skipped, queues its upload.
async fn store_file(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    dropbox_id: &str,
    path: &str,
    hash: &str,
    bytes: Vec<u8>,
    since: NaiveDate,
) -> Result<()> {
    let summary = fit::summary(&bytes);
    let skip = match &summary {
        Err(err) => {
            tracing::warn!(path, "unreadable FIT file: {err:#}");
            Some("unreadable")
        }
        Ok(s) if before_since(s.start, since) => Some("before_since"),
        Ok(s) => {
            // the same workout exported twice (e.g. from two apps):
            // same sport, start and end within 60 s
            let duplicate: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM fit_files WHERE skip_reason IS NULL AND sport = $1
                   AND start_time BETWEEN $2 AND $3 AND end_time BETWEEN $4 AND $5)",
            )
            .bind(&s.sport)
            .bind(s.start - Duration::seconds(60))
            .bind(s.start + Duration::seconds(60))
            .bind(s.end - Duration::seconds(60))
            .bind(s.end + Duration::seconds(60))
            .fetch_one(&mut **tx)
            .await?;
            duplicate.then_some("duplicate")
        }
    };
    let s = summary.as_ref().ok();
    let file_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO fit_files (id, dropbox_path, dropbox_id, content_hash, bytes, sport, sub_sport,
                                start_time, end_time, distance_m, has_gps, skip_reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(file_id)
    .bind(path)
    .bind(dropbox_id)
    .bind(hash)
    .bind(bytes)
    .bind(s.map(|s| s.sport.clone()))
    .bind(s.map(|s| s.sub_sport.clone()))
    .bind(s.map(|s| s.start))
    .bind(s.map(|s| s.end))
    .bind(s.map(|s| s.distance_m))
    .bind(s.map(|s| s.has_gps))
    .bind(skip)
    .execute(&mut **tx)
    .await?;
    if skip.is_none() {
        sqlx::query(
            "INSERT INTO uploads (id, kind, fit_file_id, idempotency_key) VALUES ($1, 'original', $2, $3)",
        )
        .bind(Uuid::new_v4())
        .bind(file_id)
        .bind(format!("fit:{hash}"))
        .execute(&mut **tx)
        .await?;
        tracing::info!(path, "queued upload");
    }
    Ok(())
}

/// Outcome of one upload round.
#[derive(Debug, Default)]
pub struct Round {
    pub runs_created: usize,
    pub failed_steps: usize,
}

/// Advances open original uploads by one step each, least recently tried first,
/// so a failing upload never starves the others.
pub async fn process_uploads(db: &PgPool, me: &Account) -> Result<Round> {
    let rows = sqlx::query(
        "SELECT u.id, u.status, u.job_id, u.idempotency_key, f.dropbox_path, f.id AS fit_id
         FROM uploads u JOIN fit_files f ON f.id = u.fit_file_id
         WHERE u.kind = 'original' AND u.status IN ('pending', 'job_started', 'uploaded')
         ORDER BY u.updated_at, u.created_at LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(db)
    .await?;
    let mut round = Round::default();
    for row in rows {
        let id: Uuid = row.get("id");
        match step(db, me, &row).await {
            Ok(true) => round.runs_created += 1,
            Ok(false) => {}
            Err(err) => {
                round.failed_steps += 1;
                let permanent = err
                    .downcast_ref::<ApiError>()
                    .is_some_and(ApiError::is_permanent);
                tracing::warn!(%id, permanent, "upload step failed: {err:#}");
                let status = if permanent { "needs_attention" } else { "keep" };
                sqlx::query(
                    "UPDATE uploads SET attempts = attempts + 1, last_error = $2, updated_at = now(),
                            status = CASE WHEN $3 = 'needs_attention' THEN 'needs_attention' ELSE status END
                     WHERE id = $1",
                )
                .bind(id)
                .bind(format!("{err}"))
                .bind(status)
                .execute(db)
                .await?;
            }
        }
    }
    Ok(round)
}

/// One state transition. Returns `true` when a run was created.
async fn step(db: &PgPool, me: &Account, row: &sqlx::postgres::PgRow) -> Result<bool> {
    let id: Uuid = row.get("id");
    let status: String = row.get("status");
    match status.as_str() {
        "pending" => {
            let bytes: Vec<u8> = sqlx::query_scalar("SELECT bytes FROM fit_files WHERE id = $1")
                .bind(row.get::<Uuid, _>("fit_id"))
                .fetch_one(db)
                .await?;
            let path: String = row.get("dropbox_path");
            let name = path.rsplit('/').next().unwrap_or("activity.fit");
            let key: String = row.get("idempotency_key");
            let job = me.upload(&ascii_filename(name), bytes, &key).await?;
            sqlx::query(
                "UPDATE uploads SET status = 'job_started', job_id = $2, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&job.id)
            .execute(db)
            .await?;
            Ok(false)
        }
        "job_started" => {
            let job_id: String = row.get("job_id");
            let (job, raw) = me.job(&job_id).await?;
            match (job.status.as_str(), job.activity_ids.as_slice()) {
                ("queued" | "running", _) => {
                    touch(db, id).await?;
                    Ok(false)
                }
                ("succeeded", [activity]) => {
                    sqlx::query(
                        "UPDATE uploads SET status = 'uploaded', endurain_activity_id = $2, updated_at = now()
                         WHERE id = $1",
                    )
                    .bind(id)
                    .bind(activity)
                    .execute(db)
                    .await?;
                    finish(db, me, id, activity).await
                }
                _ => {
                    // 0 or several ids, or a failed job: never guess, leave it for a human
                    tracing::warn!(%id, status = job.status, error = ?job.error_code, "upload needs attention");
                    sqlx::query(
                        "UPDATE uploads SET status = 'needs_attention', raw_job = $2, last_error = $3,
                                updated_at = now() WHERE id = $1",
                    )
                    .bind(id)
                    .bind(raw)
                    .bind(job.error_code.clone())
                    .execute(db)
                    .await?;
                    Ok(false)
                }
            }
        }
        "uploaded" => {
            let activity: String =
                sqlx::query_scalar("SELECT endurain_activity_id FROM uploads WHERE id = $1")
                    .bind(id)
                    .fetch_one(db)
                    .await?;
            finish(db, me, id, &activity).await
        }
        _ => Ok(false),
    }
}

/// Moves a still-running upload to the back of the queue.
async fn touch(db: &PgPool, id: Uuid) -> Result<()> {
    sqlx::query("UPDATE uploads SET updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(db)
        .await?;
    Ok(())
}

/// Records the run with Endurain's normalized facts (type, start, end, distance) and
/// marks the upload done, in one transaction.
async fn finish(db: &PgPool, me: &Account, upload_id: Uuid, activity_id: &str) -> Result<bool> {
    let activity = me.activity(activity_id).await?;
    let mut tx = db.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO runs (id, fit_file_id, endurain_activity_id, sport, start_time, end_time, distance_m, has_gps)
         SELECT $2, f.id, $3, $4, COALESCE($5, f.start_time), COALESCE($6, f.end_time), $7, f.has_gps
         FROM uploads u JOIN fit_files f ON f.id = u.fit_file_id
         WHERE u.id = $1
         ON CONFLICT (fit_file_id) DO NOTHING",
    )
    .bind(upload_id)
    .bind(Uuid::new_v4())
    .bind(&activity.id)
    .bind(&activity.activity_type)
    .bind(activity.start_time)
    .bind(activity.end_time)
    .bind(activity.distance as i32)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query("UPDATE uploads SET status = 'done', updated_at = now() WHERE id = $1")
        .bind(upload_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await.context("commit run")?;
    Ok(inserted == 1)
}

/// Endurain stores the uploaded file name; keep it ASCII and free of path characters.
fn ascii_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.to_lowercase().ends_with(".fit") {
        cleaned
    } else {
        format!("{cleaned}.fit")
    }
}

/// Start of the import window: `DROPBOX_IMPORT_SINCE` (`YYYY-MM-DD`), default 2026-11-01.
pub fn parse_since(value: Option<&str>) -> Result<NaiveDate> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => v.parse().context("DROPBOX_IMPORT_SINCE must be YYYY-MM-DD"),
        None => {
            let (y, m, d) = DEFAULT_SINCE;
            Ok(NaiveDate::from_ymd_opt(y, m, d).expect("valid default"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_become_ascii() {
        assert_eq!(
            ascii_filename("2026-11-24-102009-Laufen outdoor-Sören’s Apple Watch 11.fit"),
            "2026-11-24-102009-Laufen_outdoor-S_ren_s_Apple_Watch_11.fit"
        );
    }

    #[test]
    fn since_defaults_to_november_2026() {
        let nov = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        assert_eq!(parse_since(None).unwrap(), nov);
        assert_eq!(parse_since(Some(" ")).unwrap(), nov);
        assert_eq!(
            parse_since(Some("2026-12-01")).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 1).unwrap()
        );
        assert!(parse_since(Some("Nov")).is_err());
    }

    #[test]
    fn since_uses_berlin_midnight() {
        let since = parse_since(None).unwrap();
        // 1 Nov 00:30 Berlin (CET) = 31 Oct 23:30 UTC -> imported
        assert!(!before_since(
            "2026-10-31T23:30:00Z".parse().unwrap(),
            since
        ));
        // 31 Oct 23:30 Berlin = 31 Oct 22:30 UTC -> skipped
        assert!(before_since("2026-10-31T22:30:00Z".parse().unwrap(), since));
    }
}
