//! Embedded admin SPA assets and axum routes.

use std::path::{Component, Path};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::Path as AxumPath,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use include_dir::{Dir, include_dir};

const ASSET_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const INDEX_CACHE_CONTROL: &str = "no-cache";

static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/web/dist");

/// Errors produced while serving embedded admin assets.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The requested embedded asset is not present in the compiled bundle.
    #[error("embedded admin asset not found: {path}")]
    AssetNotFound {
        /// Path below `web/dist/assets` that was requested.
        path: String,
    },

    /// The embedded bundle was compiled without its HTML entrypoint.
    #[error("embedded admin index.html is missing")]
    IndexMissing,
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self {
            Self::AssetNotFound { .. } => StatusCode::NOT_FOUND,
            Self::IndexMissing => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (status, self.to_string()).into_response()
    }
}

/// Build an axum router that serves the admin SPA below `/admin`.
pub fn router() -> Router {
    Router::new()
        .route("/admin", get(index))
        .route("/admin/", get(index))
        .nest(
            "/admin",
            Router::new()
                .route("/assets/{*path}", get(asset))
                .fallback(get(index)),
        )
}

async fn asset(AxumPath(path): AxumPath<String>) -> Result<Response, Error> {
    let asset_path = format!("assets/{path}");
    if !is_safe_asset_path(&asset_path) {
        return Err(Error::AssetNotFound { path });
    }

    let file = DIST
        .get_file(asset_path.as_str())
        .ok_or(Error::AssetNotFound { path })?;
    Ok(bytes_response(
        file.contents(),
        ASSET_CACHE_CONTROL,
        content_type_for(file.path()),
    ))
}

async fn index() -> Result<Response, Error> {
    let file = DIST.get_file("index.html").ok_or(Error::IndexMissing)?;
    Ok(bytes_response(
        file.contents(),
        INDEX_CACHE_CONTROL,
        "text/html; charset=utf-8",
    ))
}

fn bytes_response(
    bytes: &'static [u8],
    cache_control: &'static str,
    content_type: &'static str,
) -> Response {
    let mut response = Body::from(Bytes::from_static(bytes)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static(cache_control),
    );
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    response
}

fn is_safe_asset_path(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("css") => "text/css; charset=utf-8",
        Some("gif") => "image/gif",
        Some("html") => "text/html; charset=utf-8",
        Some("ico") => "image/x-icon",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("wasm") => "application/wasm",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}
