pub mod config;
pub mod display;
pub mod endurain;
pub mod http;
pub mod render;
pub mod stats;

use sqlx::PgPool;

/// Shared state for HTTP handlers and background tasks.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub display: display::Display,
}

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
