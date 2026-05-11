//! HTTP API server for Kino.

use std::{net::SocketAddr, path::PathBuf};

use axum::{Router, middleware};
use kino_core::Config;
use kino_db::Db;

pub mod auth;
mod openapi;
mod playback;
mod request;
pub mod session_reaper;
mod token;

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
    router_with_library_root_and_public_base_url(
        db,
        library_root,
        kino_core::config::ServerConfig::default().public_base_url,
    )
}

/// Build the Kino HTTP router with an explicit public base URL for OpenAPI.
pub fn router_with_public_base_url(db: Db, public_base_url: impl Into<String>) -> Router {
    router_with_library_root_and_public_base_url(db, PathBuf::from("."), public_base_url)
}

/// Build the Kino HTTP router with explicit library and OpenAPI settings.
pub fn router_with_library_root_and_public_base_url(
    db: Db,
    library_root: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Router {
    let library_root = library_root.into();
    let artwork_cache_dir = kino_core::config::default_artwork_cache_dir(&library_root);
    router_with_library_root_artwork_cache_and_public_base_url(
        db,
        library_root,
        artwork_cache_dir,
        public_base_url,
    )
}

/// Serve the Kino HTTP API until the listener exits.
pub async fn serve(config: &Config, db: Db) -> Result<()> {
    serve_with_library_root_artwork_cache_and_public_base_url(
        config.server.listen,
        db,
        config.library_root.clone(),
        config.artwork_cache_dir(),
        config.server.public_base_url.clone(),
    )
    .await
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
    serve_with_library_root_and_public_base_url(
        listen,
        db,
        library_root,
        kino_core::config::ServerConfig::default().public_base_url,
    )
    .await
}

/// Serve the Kino HTTP API with explicit library and OpenAPI settings.
pub async fn serve_with_library_root_and_public_base_url(
    listen: SocketAddr,
    db: Db,
    library_root: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Result<()> {
    let library_root = library_root.into();
    let artwork_cache_dir = kino_core::config::default_artwork_cache_dir(&library_root);
    serve_with_library_root_artwork_cache_and_public_base_url(
        listen,
        db,
        library_root,
        artwork_cache_dir,
        public_base_url,
    )
    .await
}

/// Serve the Kino HTTP API with explicit library, artwork cache, and OpenAPI settings.
pub async fn serve_with_library_root_artwork_cache_and_public_base_url(
    listen: SocketAddr,
    db: Db,
    library_root: impl Into<PathBuf>,
    artwork_cache_dir: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!(listen = %local_addr, "server listening");
    axum::serve(
        listener,
        router_with_library_root_artwork_cache_and_public_base_url(
            db,
            library_root,
            artwork_cache_dir,
            public_base_url,
        ),
    )
    .await?;
    Ok(())
}

/// Build the Kino HTTP router with explicit library, artwork cache, and OpenAPI settings.
pub fn router_with_library_root_artwork_cache_and_public_base_url(
    db: Db,
    library_root: impl Into<PathBuf>,
    artwork_cache_dir: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Router {
    let auth_state = auth::AuthState { db: db.clone() };
    let protected_api = Router::new()
        .merge(request::router(
            db.clone(),
            library_root.into(),
            artwork_cache_dir.into(),
        ))
        .merge(token::router(db.clone()))
        .merge(playback::router(db))
        .route_layer(middleware::from_fn_with_state(
            auth_state,
            auth::require_auth,
        ));

    Router::new()
        .merge(openapi::router(public_base_url))
        .merge(protected_api)
        .merge(kino_admin::router())
}
