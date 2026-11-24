pub mod config;
pub mod display;
pub mod endurain;
pub mod http;
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
}

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
