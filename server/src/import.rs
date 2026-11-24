//! HealthFit → Dropbox → Endurain import of my workouts.
//!
//! 1. `fetch_new_files`: reads Dropbox changes page by page. Each new FIT file (bytes,
//!    facts and its queued upload) is stored in its own transaction; the cursor is saved
//!    after the whole page. A crash mid-page re-reads the page and skips known files
//!    (unique content hash), so nothing is lost or duplicated.
//! 2. `process_uploads`: a resumable state machine per upload
//!    (`pending → job_started → uploaded → done`). Every step is persisted before the
//!    next external call, so a restart continues where it stopped.

use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{dropbox::Dropbox, endurain::Account, fit};

/// Uploads handled per pass (Endurain allows 10 open uploads per user).
const BATCH: i64 = 5;

/// Imports new Dropbox files, then advances the uploads. Returns how many runs were created.
pub async fn run(
    db: &PgPool,
    dropbox: &Dropbox,
    me: &Account,
    since: Option<NaiveDate>,
) -> Result<usize> {
    fetch_new_files(db, dropbox, since).await?;
    // Endurain parses uploads within seconds: poll a few rounds so a new run
    // reaches the display now instead of on the next pass.
    let mut created = 0;
    for _ in 0..10 {
        created += process_uploads(db, me).await?;
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM uploads WHERE kind = 'original' AND status IN ('pending', 'job_started', 'uploaded')",
        )
        .fetch_one(db)
        .await?;
        if open == 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Ok(created)
}

pub async fn fetch_new_files(
    db: &PgPool,
    dropbox: &Dropbox,
    since: Option<NaiveDate>,
) -> Result<()> {
    let cursor: Option<String> = sqlx::query_scalar("SELECT cursor FROM dropbox_state")
        .fetch_optional(db)
        .await?;
    let mut page = match cursor {
        Some(c) => dropbox.list_continue(&c).await?,
        None => dropbox.list().await?,
    };
    loop {
        for entry in &page.entries {
            let (Some(id), Some(hash)) = (&entry.id, &entry.content_hash) else {
                continue;
            };
            if entry.tag != "file" || !entry.name.to_lowercase().ends_with(".fit") {
                continue;
            }
            let known: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM fit_files WHERE content_hash = $1)",
            )
            .bind(hash)
            .fetch_one(db)
            .await?;
            if known {
                continue;
            }
            let bytes = dropbox.download(id).await?;
            let mut tx = db.begin().await?;
            store_file(
                &mut tx,
                id,
                entry.path_display.as_deref().unwrap_or(&entry.name),
                hash,
                bytes,
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
        page = dropbox.list_continue(&page.cursor).await?;
    }
}

/// Stores one FIT file and, unless it is skipped, queues its upload.
async fn store_file(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    dropbox_id: &str,
    path: &str,
    hash: &str,
    bytes: Vec<u8>,
    since: Option<NaiveDate>,
) -> Result<()> {
    let summary = fit::summary(&bytes);
    let skip = match &summary {
        Err(err) => {
            tracing::warn!(path, "unreadable FIT file: {err:#}");
            Some("unreadable")
        }
        Ok(s) if since.is_some_and(|d| s.start.date_naive() < d) => Some("before_since"),
        Ok(s) => {
            // the same workout exported twice (e.g. from two apps): same start ±60 s
            let duplicate: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM fit_files WHERE skip_reason IS NULL
                   AND start_time BETWEEN $1 AND $2)",
            )
            .bind(s.start - Duration::seconds(60))
            .bind(s.start + Duration::seconds(60))
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
        sqlx::query("INSERT INTO uploads (id, kind, fit_file_id, idempotency_key) VALUES ($1, 'original', $2, $3)")
            .bind(Uuid::new_v4())
            .bind(file_id)
            .bind(format!("fit:{hash}"))
            .execute(&mut **tx)
            .await?;
        tracing::info!(path, "queued upload");
    }
    Ok(())
}

/// Advances pending original uploads by one step each. Returns how many runs were created.
pub async fn process_uploads(db: &PgPool, me: &Account) -> Result<usize> {
    let rows = sqlx::query(
        "SELECT u.id, u.status, u.job_id, u.idempotency_key, f.dropbox_path, f.id AS fit_id
         FROM uploads u JOIN fit_files f ON f.id = u.fit_file_id
         WHERE u.kind = 'original' AND u.status IN ('pending', 'job_started', 'uploaded')
         ORDER BY u.created_at LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(db)
    .await?;
    let mut created = 0;
    for row in rows {
        let id: Uuid = row.get("id");
        let result = step(db, me, &row).await;
        match result {
            Ok(true) => created += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::warn!(%id, "upload step failed: {err:#}");
                sqlx::query("UPDATE uploads SET attempts = attempts + 1, last_error = $2, updated_at = now() WHERE id = $1")
                    .bind(id)
                    .bind(format!("{err:#}"))
                    .execute(db)
                    .await?;
            }
        }
    }
    Ok(created)
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
            sqlx::query("UPDATE uploads SET status = 'job_started', job_id = $2, updated_at = now() WHERE id = $1")
                .bind(id)
                .bind(&job.id)
                .execute(db)
                .await?;
            Ok(false)
        }
        "job_started" => {
            let job_id: String = row.get("job_id");
            let job = me.job(&job_id).await?;
            match (job.status.as_str(), job.activity_ids.as_slice()) {
                ("queued" | "running", _) => Ok(false),
                ("succeeded", [activity]) => {
                    sqlx::query(
                        "UPDATE uploads SET status = 'uploaded', endurain_activity_id = $2, updated_at = now()
                         WHERE id = $1",
                    )
                    .bind(id)
                    .bind(activity)
                    .execute(db)
                    .await?;
                    finish(db, id).await
                }
                _ => {
                    // 0 or several ids, or a failed job: never guess, leave it for a human
                    tracing::warn!(%id, status = job.status, error = ?job.error_code, "upload needs attention");
                    sqlx::query(
                        "UPDATE uploads SET status = 'needs_attention', raw_job = $2, last_error = $3,
                                updated_at = now() WHERE id = $1",
                    )
                    .bind(id)
                    .bind(serde_json::to_value(&job)?)
                    .bind(job.error_code.clone())
                    .execute(db)
                    .await?;
                    Ok(false)
                }
            }
        }
        "uploaded" => finish(db, id).await,
        _ => Ok(false),
    }
}

/// Records the run for an uploaded original and marks the upload done (one transaction).
async fn finish(db: &PgPool, upload_id: Uuid) -> Result<bool> {
    let mut tx = db.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO runs (id, fit_file_id, endurain_activity_id, sport, start_time, end_time, distance_m, has_gps)
         SELECT $2, f.id, u.endurain_activity_id, f.sport, f.start_time, f.end_time, f.distance_m, f.has_gps
         FROM uploads u JOIN fit_files f ON f.id = u.fit_file_id
         WHERE u.id = $1
         ON CONFLICT (fit_file_id) DO NOTHING",
    )
    .bind(upload_id)
    .bind(Uuid::new_v4())
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

/// Start of the import window from `DROPBOX_IMPORT_SINCE` (`YYYY-MM-DD`).
pub fn parse_since(value: Option<&str>) -> Result<Option<NaiveDate>> {
    value
        .filter(|v| !v.trim().is_empty())
        .map(|v| {
            v.trim()
                .parse()
                .context("DROPBOX_IMPORT_SINCE must be YYYY-MM-DD")
        })
        .transpose()
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
    fn since_parsing() {
        assert_eq!(parse_since(None).unwrap(), None);
        assert_eq!(parse_since(Some("")).unwrap(), None);
        assert_eq!(
            parse_since(Some("2026-11-01")).unwrap(),
            NaiveDate::from_ymd_opt(2026, 11, 1)
        );
        assert!(parse_since(Some("Nov")).is_err());
    }
}
