use anyhow::Result;
use running_viz::{
    AppState, MIGRATOR,
    config::Config,
    display::{self, Display},
    endurain::Account,
    http, stats,
    sync::{self, SyncStatus},
};
use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let config = Config::from_env()?;
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config.database_url)
        .await?;
    MIGRATOR.run(&db).await?;

    // Accounts: me first, then the dogs.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let accounts: Vec<Account> = config
        .runners
        .iter()
        .map(|r| Account::new(client.clone(), &config.endurain_url, &r.api_key))
        .collect();
    let names: Vec<String> = config
        .runners
        .iter()
        .map(|r| capitalize(&r.username))
        .collect();

    // Show zeros until the first fetch succeeds; afterwards keep the last good stats on errors.
    let display = Display::new(&stats::empty(&names, display::today()));
    let status = SyncStatus::default();
    let (refresh_display, refresh_status) = (display.clone(), status.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            tick.tick().await;
            sync::refresh(&accounts, &refresh_display, &refresh_status).await;
        }
    });

    let app = http::router(AppState {
        db,
        display,
        sync: status,
    });
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port)).await?;
    tracing::info!(port = config.port, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {
        _ = ctrl_c => {},
        _ = term.recv() => {},
    }
}
