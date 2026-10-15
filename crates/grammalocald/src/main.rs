//! Daemon entry point: loads config, starts the Grammalecte worker, and
//! serves the HTTP API on 127.0.0.1 until interrupted.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use grammalocald::config::{self, Config};
use grammalocald::server::{self, AppState};
use grammalocald::{Grammalecte, Router};

#[tokio::main]
async fn main() -> Result<()> {
    let dir = config::config_dir()?;
    let config = Config::load_or_create(&dir)?;

    let engine = Grammalecte::spawn_default()
        .await
        .context("starting grammalecte worker")?;
    let mut router = Router::with_default(engine, server::default_lang(&config));
    router.set_revision(config_revision(&config));

    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    let state = Arc::new(AppState::new(config, router).persist_to(dir));
    server::spawn_recycler(state.clone());

    println!("grammalocald listening on http://{addr}");
    axum::serve(listener, server::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;
    Ok(())
}

/// Hashes the settings that change check results, so cached entries are
/// invalidated when the dictionary or rule list changes.
fn config_revision(config: &Config) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    config.personal_dictionary.hash(&mut hasher);
    config.disabled_rules.hash(&mut hasher);
    config.default_language.hash(&mut hasher);
    hasher.finish()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
