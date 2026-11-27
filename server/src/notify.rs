//! Push after import: "Wer war dabei?" via ntfy, with one-tap answers.
//!
//! Each new outdoor run (last 24 h, no answer yet) is announced once. The buttons call
//! `PUT /api/runs/{id}/dogs?sig=…` with a signature that is valid for that run only, so
//! the ntfy server never sees the real token. Tapping the message opens the dogs page.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Datelike, Duration, Utc};
use chrono_tz::Europe::Berlin;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::stats::DOG_RUN_TYPES;

const WEEKDAYS: [&str; 7] = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];

#[derive(Clone)]
pub struct Ntfy {
    pub client: reqwest::Client,
    /// ntfy server, e.g. `https://ntfy.sh`
    pub server: String,
    /// secret topic name
    pub topic: String,
    /// where this server is reachable, e.g. `https://running-viz-api.soeenplop.dev`
    pub public_url: String,
    /// the companions token; only used to sign per-run links
    pub secret: String,
    /// dog usernames, in display order
    pub dogs: Vec<String>,
}

impl Ntfy {
    /// From `NTFY_URL` (`https://ntfy.sh/<topic>`); `None` if unset.
    pub fn from_url(
        url: &str,
        public_url: &str,
        secret: &str,
        dogs: &[String],
    ) -> Result<Option<Self>> {
        let url = url.trim().trim_end_matches('/');
        if url.is_empty() {
            return Ok(None);
        }
        let (server, topic) = url
            .rsplit_once('/')
            .filter(|(s, t)| s.starts_with("http") && !t.is_empty() && !s.ends_with('/'))
            .context("NTFY_URL must look like https://ntfy.sh/<topic>")?;
        if public_url.trim().is_empty() {
            bail!("NTFY_URL needs PUBLIC_URL (where the buttons send the answer)");
        }
        Ok(Some(Self {
            client: reqwest::Client::new(),
            server: server.to_owned(),
            topic: topic.to_owned(),
            public_url: public_url.trim().trim_end_matches('/').to_owned(),
            secret: secret.to_owned(),
            dogs: dogs.to_vec(),
        }))
    }
}

/// Signature that allows setting the dogs of exactly this run (hex HMAC-SHA256).
pub fn signature(secret: &str, run: Uuid) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(format!("dogs:{run}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Announces new unanswered outdoor runs; each run once, also if sending fails for good
/// after a day (older runs are no longer announced).
pub async fn pass(db: &PgPool, ntfy: &Ntfy, now: DateTime<Utc>) -> Result<()> {
    let types: Vec<String> = DOG_RUN_TYPES.iter().map(|t| t.to_string()).collect();
    let runs = sqlx::query(
        "SELECT r.id, r.start_time, r.distance_m FROM runs r
         WHERE r.notified_at IS NULL AND NOT r.gone AND r.has_gps AND r.sport = ANY($1)
           AND r.end_time > $2
           AND NOT EXISTS (SELECT 1 FROM companion_claims c WHERE c.run_id = r.id)
         ORDER BY r.start_time",
    )
    .bind(&types)
    .bind(now - Duration::hours(24))
    .fetch_all(db)
    .await?;
    for r in runs {
        let id: Uuid = r.get("id");
        let msg = message(ntfy, id, r.get("start_time"), r.get("distance_m"));
        let res = ntfy.client.post(&ntfy.server).json(&msg).send().await?;
        if !res.status().is_success() {
            bail!("ntfy answered {}", res.status());
        }
        sqlx::query("UPDATE runs SET notified_at = $2 WHERE id = $1")
            .bind(id)
            .bind(now)
            .execute(db)
            .await?;
        tracing::info!(%id, "asked who came along");
    }
    Ok(())
}

/// The ntfy JSON message: "Beide" plus one button per dog (ntfy allows three).
fn message(ntfy: &Ntfy, run: Uuid, start: DateTime<Utc>, distance_m: i32) -> Value {
    let url = format!(
        "{}/api/runs/{run}/dogs?sig={}",
        ntfy.public_url,
        signature(&ntfy.secret, run)
    );
    let button = |label: String, dogs: &[String]| {
        json!({
            "action": "http",
            "label": label,
            "url": url,
            "method": "PUT",
            "headers": { "Content-Type": "application/json" },
            "body": json!({ "dogs": dogs }).to_string(),
            "clear": true,
        })
    };
    let mut actions = Vec::new();
    if ntfy.dogs.len() > 1 {
        let all = if ntfy.dogs.len() == 2 {
            "Beide"
        } else {
            "Alle"
        };
        actions.push(button(all.into(), &ntfy.dogs));
    }
    for dog in &ntfy.dogs {
        actions.push(button(capitalize(dog), std::slice::from_ref(dog)));
    }
    actions.truncate(3);
    let local = start.with_timezone(&Berlin);
    json!({
        "topic": ntfy.topic,
        "title": "Wer war dabei?",
        "message": format!(
            "Lauf {} {} · {} km",
            WEEKDAYS[local.weekday().num_days_from_monday() as usize],
            local.format("%d.%m., %H:%M"),
            format!("{:.1}", f64::from(distance_m) / 1000.0).replace('.', ","),
        ),
        "tags": ["dog"],
        "click": format!("{}/dogs", ntfy.public_url),
        "actions": actions,
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ntfy() -> Ntfy {
        Ntfy::from_url(
            "https://ntfy.sh/secret-topic",
            "https://api.example/",
            "s3cret",
            &["nanuq".into(), "anori".into()],
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn parses_url() {
        let n = ntfy();
        assert_eq!(
            (n.server.as_str(), n.topic.as_str()),
            ("https://ntfy.sh", "secret-topic")
        );
        assert_eq!(n.public_url, "https://api.example");
        assert!(Ntfy::from_url("", "x", "s", &[]).unwrap().is_none());
        assert!(Ntfy::from_url("https://ntfy.sh/", "x", "s", &[]).is_err());
        assert!(Ntfy::from_url("https://ntfy.sh/t", "", "s", &[]).is_err());
    }

    #[test]
    fn message_has_signed_buttons() {
        let run = Uuid::nil();
        let start = "2026-11-27T07:47:36Z".parse().unwrap();
        let m = message(&ntfy(), run, start, 5429);
        assert_eq!(m["message"], "Lauf Fr 27.11., 08:47 · 5,4 km");
        assert_eq!(m["click"], "https://api.example/dogs");
        let actions = m["actions"].as_array().unwrap();
        let labels: Vec<&str> = actions
            .iter()
            .map(|a| a["label"].as_str().unwrap())
            .collect();
        assert_eq!(labels, ["Beide", "Nanuq", "Anori"]);
        assert_eq!(actions[0]["body"], r#"{"dogs":["nanuq","anori"]}"#);
        let url = actions[1]["url"].as_str().unwrap();
        assert!(url.ends_with(&format!("?sig={}", signature("s3cret", run))));
        assert!(
            !m.to_string().contains("s3cret"),
            "the token never leaves the server"
        );
        assert_ne!(
            signature("s3cret", run),
            signature("s3cret", Uuid::new_v4())
        );
    }
}
