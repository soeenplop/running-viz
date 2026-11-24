use anyhow::Result;
use running_viz::{
    AppState, MIGRATOR,
    config::Config,
    display::{self, Display},
    http,
    render::demo_stats,
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

    // Demo data until the Endurain stats are wired in; re-rendered so the date stays current.
    let display = Display::new(&demo_stats(display::today()));
    let refresh = display.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        loop {
            tick.tick().await;
            refresh.update(&demo_stats(display::today()));
        }
    });

    let app = http::router(AppState { db, display });
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port)).await?;
    tracing::info!(port = config.port, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
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
