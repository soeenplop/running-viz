pub mod config;
pub mod display;
pub mod dropbox;
pub mod endurain;
pub mod fit;
pub mod http;
pub mod import;
pub mod render;
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
}

#[derive(Clone)]
pub struct Webhook {
    pub app_secret: String,
    pub trigger: std::sync::Arc<tokio::sync::Notify>,
}

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
