use anyhow::Result;
use running_viz::{
    AppState, CompanionsApi, MIGRATOR, Webhook, companions,
    config::Config,
    display::{self, Display},
    dropbox::Dropbox,
    endurain::Account,
    http, import, sparky, stats,
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
    let trigger = std::sync::Arc::new(tokio::sync::Notify::new());
    let dropbox = config
        .dropbox
        .clone()
        .map(|c| Dropbox::new(client.clone(), c));
    if dropbox.is_none() {
        tracing::warn!("Dropbox not configured: HealthFit import disabled");
    }
    let dog_names: Vec<String> = config
        .runners
        .iter()
        .skip(1)
        .map(|r| r.username.clone())
        .collect();
    let dog_accounts: std::collections::HashMap<String, Account> = dog_names
        .iter()
        .cloned()
        .zip(accounts.iter().skip(1).cloned())
        .collect();
    let companions_api = config.shortcut_token.clone().map(|token| CompanionsApi {
        token,
        dogs: dog_names.clone(),
        trigger: trigger.clone(),
        last: Default::default(),
    });
    if companions_api.is_none() {
        tracing::warn!("SHORTCUT_TOKEN not set: \"who came along\" endpoint disabled");
    }
    let webhook = config.dropbox.as_ref().map(|c| Webhook {
        app_secret: c.app_secret.clone(),
        trigger: trigger.clone(),
    });

    // Optional SparkyFitness mirror: (account name, dog or None for me, account).
    let sparky_accounts: Vec<(String, Option<String>, sparky::SparkyAccount)> =
        match &config.sparky_url {
            Some(url) => config
                .runners
                .iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let key = r.sparky_key.as_ref()?;
                    let dog = (i > 0).then(|| r.username.clone());
                    Some((
                        r.username.clone(),
                        dog,
                        sparky::SparkyAccount::new(client.clone(), url, key),
                    ))
                })
                .collect(),
            None => Vec::new(),
        };
    if sparky_accounts.is_empty() {
        tracing::info!("SparkyFitness mirror off (SPARKY_URL / SPARKY_KEY_* not set)");
    }

    // Sync loop: every 5 minutes, or right away when the Dropbox webhook fires.
    let (loop_db, loop_display, loop_status, since) = (
        db.clone(),
        display.clone(),
        status.clone(),
        config.import_since,
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(300));
        let mut last_reconcile: Option<chrono::NaiveDate> = None;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = trigger.notified() => tracing::info!("dropbox webhook: syncing now"),
            }
            if let Some(dropbox) = &dropbox {
                let out = import::run(&loop_db, dropbox, &accounts[0], since).await;
                if out.runs_created > 0 {
                    tracing::info!(runs = out.runs_created, "imported runs");
                }
                loop_status.set_import_ok(out.ok());
            }
            if let Err(err) = companions::pass(
                &loop_db,
                &accounts[0],
                &dog_names,
                &dog_accounts,
                &mut last_reconcile,
            )
            .await
            {
                tracing::warn!("dog copies failed: {err:#}");
            }
            if !sparky_accounts.is_empty()
                && let Err(err) =
                    sparky::mirror(&loop_db, &sparky_accounts, chrono::Utc::now()).await
            {
                tracing::warn!("sparkyfitness mirror failed: {err:#}");
            }
            sync::refresh(&accounts, &loop_display, &loop_status).await;
        }
    });

    let app = http::router(AppState {
        db,
        display,
        sync: status,
        webhook,
        companions: companions_api,
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
