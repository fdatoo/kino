use axum::{Json, Router, routing::get};
use utoipa::OpenApi as _;
use utoipa::openapi::server::Server;

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        crate::admin_config::get_config,
        crate::catalog_admin::delete_catalog_item,
        crate::playback::get_progress,
        crate::playback::mark_watched,
        crate::playback::record_progress,
        crate::playback::unmark_watched,
        crate::request::cancel_request,
        crate::request::create_request,
        crate::request::get_request,
        crate::request::list_catalog_items,
        crate::request::list_requests,
        crate::request::get_catalog_item,
        crate::request::manual_import,
        crate::request::record_plan,
        crate::request::re_resolve,
        crate::request::reset_request,
        crate::request::resolve_request,
        crate::request::reprobe_source_file,
        crate::request::scan_library,
        crate::session_admin::list_sessions,
        crate::stream::media_playlist,
        crate::stream::master_playlist,
        crate::stream::live_init_segment,
        crate::stream::live_media_playlist,
        crate::stream::live_media_segment,
        crate::stream::source_file,
        crate::stream::subtitle_track,
        crate::stream::transcode_init_segment,
        crate::stream::transcode_media_playlist,
        crate::stream::transcode_media_segment,
        crate::stream::transcode_output,
        crate::request::get_catalog_item_image,
        crate::request::reocr_subtitle_track,
        crate::token::create_token,
        crate::token::list_tokens,
        crate::token::revoke_token,
        crate::transcode_admin::cache_stats,
        crate::transcode_admin::cancel_job,
        crate::transcode_admin::get_job,
        crate::transcode_admin::list_encoders,
        crate::transcode_admin::list_jobs,
        crate::transcode_admin::purge_cache,
        crate::transcode_admin::replan_source,
        crate::transcode_admin::retranscode_source
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
    // `just openapi` starts the binary, fetches this route, and rewrites the committed spec.
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

#[cfg(test)]
mod tests {
    use std::{env, fs};

    #[test]
    #[ignore = "developer helper for sandboxed OpenAPI regeneration"]
    fn write_openapi_json() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = env::var_os("KINO_OPENAPI_OUT") else {
            return Ok(());
        };
        let public_base_url = env::var("KINO_OPENAPI_PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
        let json = serde_json::to_vec(&super::spec(public_base_url))?;
        fs::write(path, json)?;
        Ok(())
    }
}
