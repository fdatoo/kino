use std::path::PathBuf;

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    routing::delete,
};
use kino_core::Id;
use kino_db::Db;
use kino_library::CatalogService;

use crate::request::{ApiResult, ErrorResponse, parse_id};

#[derive(Clone)]
pub(crate) struct AppState {
    catalog: CatalogService,
    library_root: PathBuf,
}

pub(crate) fn router(db: Db, library_root: PathBuf) -> Router {
    Router::new()
        .route(
            "/api/v1/admin/library/items/{id}",
            delete(delete_catalog_item),
        )
        .with_state(AppState {
            catalog: CatalogService::new(db),
            library_root,
        })
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/library/items/{id}",
    tag = "admin",
    params(
        ("id" = Id, Path, description = "Media item id")
    ),
    responses(
        (status = 204, description = "Media item deleted"),
        (status = 400, description = "Invalid media item id", body = ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token"),
        (status = 404, description = "Media item not found", body = ErrorResponse),
        (status = 500, description = "Media item deletion failed", body = ErrorResponse)
    )
)]
pub(crate) async fn delete_catalog_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let id = parse_id(id)?;
    state
        .catalog
        .delete_media_item(id, &state.library_root)
        .await?;
    tracing::info!(media_item_id = %id, "media item deleted");
    Ok(StatusCode::NO_CONTENT)
}
