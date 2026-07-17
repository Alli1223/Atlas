//! The `atlas` binary.

use std::net::SocketAddr;
use std::process::ExitCode;

use anyhow::Context;
use atlas::api::{self, AppState};
use atlas::auth::seed;
use atlas::config::Config;
use atlas::db::{self, Db};
use atlas::telemetry;
use tokio::net::TcpListener;
use tokio::signal;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // Configuration can fail before tracing exists, so report to stderr
            // directly. `{err:#}` prints the whole anyhow chain on one line,
            // which is what makes a missing variable readable rather than a
            // bare "invalid configuration".
            eprintln!("atlas: fatal: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    // Configuration is read before the runtime starts: there is no point paying
    // for a thread pool we may be about to abandon.
    let config = Config::load()?;
    telemetry::init(&config)?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to start the tokio runtime")?
        .block_on(serve(config))
}

async fn serve(config: Config) -> anyhow::Result<()> {
    tracing::info!(
        version = atlas::VERSION,
        // Safe to log: `Config`'s Debug redacts the master key.
        config = ?config,
        "starting atlas"
    );

    config
        .ensure_dirs()
        .with_context(|| format!("failed to create data dir {}", config.data_dir.display()))?;

    let db = Db::connect(&config).await?;

    // Migrate before serving, never alongside: a request that arrives against a
    // half-migrated schema fails in ways that are very hard to read.
    db::migrate::run(&db).await?;

    // Seed the default administrator, but only into a completely empty
    // instance. Idempotent, so this is safe on every boot — see
    // `auth::seed::ensure_default_admin` for why the condition is "no users"
    // rather than "no account called Admin".
    seed::ensure_default_admin(&db)
        .await
        .context("failed to seed the default administrator")?;

    let listener = TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("failed to bind {}", config.bind_addr))?;

    let addr = listener
        .local_addr()
        .context("failed to read the listener address")?;
    tracing::info!(%addr, docs = api::DOCS_PATH, "atlas is listening");

    let app = api::router(AppState::new(db.clone(), config));

    // `into_make_service_with_connect_info` rather than the bare service: it is
    // what puts the peer address in the request extensions, and without it the
    // per-IP login lockout has no address to count against and silently degrades
    // to per-username only. See `auth::extract::ClientInfo`.
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .context("server error");

    // Close the pools explicitly so WAL checkpointing and `PRAGMA optimize` get
    // a chance to run. Dropping the pool does not await that.
    tracing::info!("shutting down; closing database pools");
    db.close().await;

    result
}

/// Resolves on SIGINT (Ctrl-C) or SIGTERM.
///
/// SIGTERM is the one that matters in practice: it is what Docker, systemd and
/// Kubernetes send. Handling only Ctrl-C means every real deployment kills
/// Atlas mid-request after its grace period.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = signal::ctrl_c().await {
            tracing::error!(error = %err, "failed to install the Ctrl-C handler");
            // Never resolve, rather than reporting a shutdown nobody asked for.
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to install the SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received SIGINT"),
        () = terminate => tracing::info!("received SIGTERM"),
    }
}
