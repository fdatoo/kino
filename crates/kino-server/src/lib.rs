//! HTTP API server for Kino.

use std::net::SocketAddr;

use axum::Router;
use kino_core::Config;
use kino_db::Db;

mod request;

pub use request::ListRequestsResponse;

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
    Router::new().merge(request::router(db))
}

/// Serve the Kino HTTP API until the listener exits.
pub async fn serve(config: &Config, db: Db) -> Result<()> {
    serve_on(config.server.listen, db).await
}

/// Serve the Kino HTTP API on an explicit socket address.
pub async fn serve_on(listen: SocketAddr, db: Db) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(listen = %local_addr, "server listening");
    axum::serve(listener, router(db)).await?;
    Ok(())
}
