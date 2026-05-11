use axum::{Json, Router, routing::get};
use utoipa::OpenApi as _;
use utoipa::openapi::server::Server;

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        crate::admin_config::get_config,
        crate::playback::record_progress,
        crate::request::list_catalog_items,
        crate::stream::source_file,
        crate::request::get_catalog_item_image,
        crate::token::create_token,
        crate::token::list_tokens,
        crate::token::revoke_token
    ),
    info(title = "Kino API", version = "0.1.0-phase-2"),
    tags(
        (name = "requests", description = "Request lifecycle operations"),
        (name = "stream", description = "Source media streaming operations"),
        (name = "library", description = "Library catalog operations"),
        (name = "playback", description = "Playback state operations"),
        (name = "admin", description = "Administrative operations")
    )
)]
pub(crate) struct ApiDoc;

pub(crate) fn router(public_base_url: impl Into<String>) -> Router {
    let public_base_url = public_base_url.into();
    Router::new().route(
        "/api/openapi.json",
        get(move || {
            let public_base_url = public_base_url.clone();
            async move { Json(spec(public_base_url)) }
        }),
    )
}

pub(crate) fn spec(public_base_url: impl Into<String>) -> utoipa::openapi::OpenApi {
    let mut spec = ApiDoc::openapi();
    spec.servers = Some(vec![Server::new(public_base_url.into())]);
    spec
}
