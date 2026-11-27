use anyhow::{Context, Result, bail};

/// Server configuration, read from environment variables.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub endurain_url: String,
    /// Endurain accounts: me first, then the dogs (from `RUNNERS`, keys from `ENDURAIN_KEY_<NAME>`).
    pub runners: Vec<RunnerConfig>,
    /// HealthFit import; `None` if the Dropbox variables are not set.
    pub dropbox: Option<crate::dropbox::DropboxConfig>,
    /// Bearer token for `POST /api/companions` (the Shortcut); endpoint disabled if unset.
    pub shortcut_token: Option<String>,
    /// Workouts that started (Europe/Berlin) before this date are not imported.
    pub import_since: chrono::NaiveDate,
    /// Push "Wer war dabei?" after import (`https://ntfy.sh/<topic>`); off if unset.
    pub ntfy_url: Option<String>,
    /// Where this server is reachable from the phone (for the push buttons).
    pub public_url: String,
}

#[derive(Clone)]
pub struct RunnerConfig {
    pub username: String,
    pub api_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let names = std::env::var("RUNNERS").unwrap_or_else(|_| "soeren,nanuq,anori".into());
        let runners = names
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| {
                Ok(RunnerConfig {
                    username: n.to_owned(),
                    api_key: required(&format!("ENDURAIN_KEY_{}", n.to_uppercase()))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if runners.is_empty() {
            bail!("RUNNERS must name at least one account");
        }
        Ok(Self {
            database_url: required("DATABASE_URL")?,
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8000".into())
                .parse()
                .context("PORT must be a number")?,
            endurain_url: required("ENDURAIN_URL")?,
            runners,
            dropbox: dropbox()?,
            shortcut_token: std::env::var("SHORTCUT_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty()),
            import_since: crate::import::parse_since(
                std::env::var("DROPBOX_IMPORT_SINCE").ok().as_deref(),
            )?,
            ntfy_url: std::env::var("NTFY_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            public_url: std::env::var("PUBLIC_URL").unwrap_or_default(),
        })
    }
}

/// All three Dropbox credentials, or none (import disabled).
fn dropbox() -> Result<Option<crate::dropbox::DropboxConfig>> {
    let get = |n: &str| std::env::var(n).ok().filter(|v| !v.trim().is_empty());
    match (
        get("DROPBOX_APP_KEY"),
        get("DROPBOX_APP_SECRET"),
        get("DROPBOX_REFRESH_TOKEN"),
    ) {
        (Some(app_key), Some(app_secret), Some(refresh_token)) => {
            Ok(Some(crate::dropbox::DropboxConfig {
                app_key,
                app_secret,
                refresh_token,
                folder: get("DROPBOX_FOLDER").unwrap_or_else(|| "/Apps/HealthFitExporter".into()),
            }))
        }
        (None, None, None) => Ok(None),
        _ => bail!(
            "set all of DROPBOX_APP_KEY, DROPBOX_APP_SECRET and DROPBOX_REFRESH_TOKEN, or none"
        ),
    }
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("missing environment variable {name}"))
}
