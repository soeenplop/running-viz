pub mod companions;
pub mod config;
pub mod display;
pub mod dropbox;
pub mod endurain;
pub mod fit;
pub mod gpx;
pub mod http;
pub mod import;
pub mod render;
pub mod sparky;
pub mod stats;
pub mod sync;

use sqlx::PgPool;

/// Shared state for HTTP handlers and background tasks.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub display: display::Display,
    pub sync: sync::SyncStatus,
    /// Dropbox webhook: app secret for signatures + trigger for an immediate sync.
    pub webhook: Option<Webhook>,
    /// Shortcut endpoint; `None` if no token is configured.
    pub companions: Option<CompanionsApi>,
}

#[derive(Clone)]
pub struct CompanionsApi {
    pub token: String,
    /// Dog usernames that may appear in a claim.
    pub dogs: Vec<String>,
    pub trigger: std::sync::Arc<tokio::sync::Notify>,
    /// Time of the last accepted claim (simple flood protection).
    pub last: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
}

#[derive(Clone)]
pub struct Webhook {
    pub app_secret: String,
    pub trigger: std::sync::Arc<tokio::sync::Notify>,
}

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
