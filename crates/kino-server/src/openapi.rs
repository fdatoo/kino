use axum::{Json, Router, routing::get};
use utoipa::OpenApi as _;
use utoipa::openapi::server::Server;

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        crate::admin_config::get_config,
        crate::playback::get_progress,
        crate::playback::record_progress,
        crate::request::list_catalog_items,
        crate::request::get_catalog_item,
        crate::session_admin::list_sessions,
        crate::stream::source_file,
        crate::stream::transcode_output,
        crate::request::get_catalog_item_image,
        crate::request::reocr_subtitle_track,
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
