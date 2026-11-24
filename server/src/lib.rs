pub mod config;
pub mod display;
pub mod http;
pub mod render;

use sqlx::PgPool;

/// Shared state for HTTP handlers and background tasks.
#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub display: display::Display,
}

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
