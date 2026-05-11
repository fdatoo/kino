use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{Id, Timestamp};
use serde::Deserialize;
use serde_json::Value;
use tower::util::ServiceExt;

#[derive(Debug, Deserialize)]
struct CreateTokenResponse {
    token: String,
    token_id: Id,
    label: String,
    created_at: Timestamp,
}

#[derive(Debug, Deserialize)]
struct ListTokensResponse {
    tokens: Vec<TokenSummary>,
}

#[derive(Debug, Deserialize)]
struct TokenSummary {
    token_id: Id,
    label: String,
    last_seen_at: Option<Timestamp>,
    revoked_at: Option<Timestamp>,
    created_at: Timestamp,
}

#[tokio::test]
async fn token_api_mints_token_and_persists_only_hash() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db.clone());

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/admin/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"label":"Living room Apple TV"}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CREATED);
    let created: CreateTokenResponse = response_json(response).await?;
    assert_eq!(created.label, "Living room Apple TV");
    assert_eq!(created.token.len(), 43);
    assert_eq!(created.token_id.as_uuid().get_version_num(), 7);
    assert!(created.created_at <= Timestamp::now());

    let stored: (String, String) =
        sqlx::query_as("SELECT label, hash FROM device_tokens WHERE id = ?1")
            .bind(created.token_id)
            .fetch_one(db.read_pool())
            .await?;

    assert_eq!(stored.0, "Living room Apple TV");
    assert_ne!(stored.1, created.token);
    assert_eq!(stored.1.len(), 64);
    assert!(stored.1.chars().all(|c| c.is_ascii_hexdigit()));

    Ok(())
}

#[tokio::test]
async fn token_api_lists_token_metadata_without_plaintext() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);
    let created = create_token(&app, "Office iPad").await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/admin/tokens")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body: Value = serde_json::from_slice(&bytes)?;
    assert!(body.get("token").is_none());
    assert!(body.get("hash").is_none());
    assert!(
        body.pointer("/tokens/0/token").is_none(),
        "list response must not expose plaintext token"
    );
    assert!(
        body.pointer("/tokens/0/hash").is_none(),
        "list response must not expose token hash"
    );

    let listed: ListTokensResponse = serde_json::from_value(body)?;
    assert_eq!(listed.tokens.len(), 1);
    assert_eq!(listed.tokens[0].token_id, created.token_id);
    assert_eq!(listed.tokens[0].label, "Office iPad");
    assert_eq!(listed.tokens[0].last_seen_at, None);
    assert_eq!(listed.tokens[0].revoked_at, None);
    assert_eq!(listed.tokens[0].created_at, created.created_at);

    Ok(())
}

#[tokio::test]
async fn token_api_rejects_empty_label() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/admin/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"label":""}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    Ok(())
}

#[tokio::test]
async fn token_api_revokes_token() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db.clone());
    let created = create_token(&app, "Kitchen display").await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("DELETE")
                .uri(format!("/api/v1/admin/tokens/{}", created.token_id))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let revoked_at: Option<Timestamp> =
        sqlx::query_scalar("SELECT revoked_at FROM device_tokens WHERE id = ?1")
            .bind(created.token_id)
            .fetch_one(db.read_pool())
            .await?;

    assert!(revoked_at.is_some());

    Ok(())
}

async fn create_token(
    app: &axum::Router,
    label: &str,
) -> Result<CreateTokenResponse, Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/admin/tokens")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"label":"{label}"}}"#)))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await
}

async fn response_json<T: serde::de::DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
