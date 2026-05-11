//! HTTP API server for Kino.

use std::{net::SocketAddr, path::PathBuf};

use axum::Router;
use kino_core::Config;
use kino_db::Db;

mod request;

/// Errors produced by `kino-server`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Binding or serving the HTTP listener failed.
    #[error("http server error: {0}")]
    Io(#[from] std::io::Error),
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Build the Kino HTTP router.
pub fn router(db: Db) -> Router {
    router_with_library_root(db, PathBuf::from("."))
}

/// Build the Kino HTTP router with an explicit library root.
pub fn router_with_library_root(db: Db, library_root: impl Into<PathBuf>) -> Router {
    Router::new().merge(request::router(db, library_root.into()))
}

/// Serve the Kino HTTP API until the listener exits.
pub async fn serve(config: &Config, db: Db) -> Result<()> {
    serve_with_library_root(config.server.listen, db, config.library_root.clone()).await
}

/// Serve the Kino HTTP API on an explicit socket address.
pub async fn serve_on(listen: SocketAddr, db: Db) -> Result<()> {
    serve_with_library_root(listen, db, PathBuf::from(".")).await
}

/// Serve the Kino HTTP API on an explicit socket address and library root.
pub async fn serve_with_library_root(
    listen: SocketAddr,
    db: Db,
    library_root: impl Into<PathBuf>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(listen = %local_addr, "server listening");
    axum::serve(listener, router_with_library_root(db, library_root)).await?;
    Ok(())
}
