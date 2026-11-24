use anyhow::{Context, Result, bail};

/// Server configuration, read from environment variables.
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub endurain_url: String,
    /// Endurain accounts: me first, then the dogs (from `RUNNERS`, keys from `ENDURAIN_KEY_<NAME>`).
    pub runners: Vec<RunnerConfig>,
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
        })
    }
}

fn required(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("missing environment variable {name}"))
}
