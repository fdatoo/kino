use std::{fs, io};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use tower::util::ServiceExt;

#[tokio::test]
async fn admin_router_serves_index_fallback_and_hashed_assets()
-> Result<(), Box<dyn std::error::Error>> {
    let app = kino_admin::router();
    let index = fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/web/dist/index.html"))?;

    let index_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(index_response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&index_response, header::CACHE_CONTROL),
        Some("no-cache")
    );
    assert_eq!(response_bytes(index_response).await?, index);

    let fallback_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/admin/foo")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(fallback_response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&fallback_response, header::CACHE_CONTROL),
        Some("no-cache")
    );
    assert_eq!(response_bytes(fallback_response).await?, index);

    let asset = hashed_asset()?;
    let asset_response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/admin/assets/{}", asset.name))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(asset_response.status(), StatusCode::OK);
    assert_eq!(
        header_value(&asset_response, header::CACHE_CONTROL),
        Some("public, max-age=31536000, immutable")
    );
    assert_eq!(response_bytes(asset_response).await?, asset.bytes);

    Ok(())
}

struct Asset {
    name: String,
    bytes: Vec<u8>,
}

fn hashed_asset() -> Result<Asset, Box<dyn std::error::Error>> {
    let assets_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/web/dist/assets");
    for entry in fs::read_dir(assets_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }

        let name = entry.file_name().into_string().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "asset filename is not utf-8")
        })?;
        if !name.contains('-') {
            continue;
        }

        return Ok(Asset {
            bytes: fs::read(entry.path())?,
            name,
        });
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no hashed asset in web/dist/assets",
    )
    .into())
}

async fn response_bytes(response: axum::response::Response) -> Result<Vec<u8>, axum::Error> {
    Ok(to_bytes(response.into_body(), usize::MAX).await?.to_vec())
}

fn header_value(response: &axum::response::Response, name: header::HeaderName) -> Option<&str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}
