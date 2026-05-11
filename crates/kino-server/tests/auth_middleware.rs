use axum::{
    Extension, Json, Router,
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
    middleware,
    routing::get,
};
use kino_core::{Id, Timestamp, user::User};
use kino_server::auth::{AuthState, AuthenticatedUser, require_auth};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tower::util::ServiceExt;

#[derive(Debug, Deserialize)]
struct CreateTokenResponse {
    token: String,
    token_id: Id,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProtectedResponse {
    user_id: Id,
    token_id: Id,
    display_name: String,
}

#[tokio::test]
async fn auth_middleware_accepts_valid_bearer_token() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let token = create_token(&db, "Office iPad").await?;
    let app = protected_router(db.clone());

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/protected")
                .header(header::AUTHORIZATION, format!("Bearer {}", token.token))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: ProtectedResponse = response_json(response).await?;
    assert_eq!(body.token_id, token.token_id);
    assert_eq!(body.display_name, "Owner");

    wait_for_last_seen(&db, token.token_id).await?;

    Ok(())
}

#[tokio::test]
async fn auth_middleware_rejects_missing_bearer_token() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = protected_router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/protected")
                .body(Body::empty())?,
        )
        .await?;

    assert_auth_error(response, StatusCode::UNAUTHORIZED, "missing_bearer").await
}

#[tokio::test]
async fn auth_middleware_rejects_malformed_bearer_token() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let app = protected_router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/protected")
                .header(header::AUTHORIZATION, "Basic not-a-bearer-token")
                .body(Body::empty())?,
        )
        .await?;

    assert_auth_error(response, StatusCode::UNAUTHORIZED, "malformed_bearer").await
}

#[tokio::test]
async fn auth_middleware_rejects_unknown_bearer_token() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = protected_router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/protected")
                .header(header::AUTHORIZATION, "Bearer unknown-token")
                .body(Body::empty())?,
        )
        .await?;

    assert_auth_error(response, StatusCode::UNAUTHORIZED, "invalid_token").await
}

#[tokio::test]
async fn auth_middleware_rejects_revoked_bearer_token() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let token = create_token(&db, "Office iPad").await?;
    let app = protected_router(db.clone());

    sqlx::query("UPDATE device_tokens SET revoked_at = ?1 WHERE id = ?2")
        .bind(Timestamp::now())
        .bind(token.token_id)
        .execute(db.write_pool())
        .await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/protected")
                .header(header::AUTHORIZATION, format!("Bearer {}", token.token))
                .body(Body::empty())?,
        )
        .await?;

    assert_auth_error(response, StatusCode::UNAUTHORIZED, "revoked_token").await
}

fn protected_router(db: kino_db::Db) -> Router {
    Router::new()
        .route("/protected", get(protected_handler))
        .route_layer(middleware::from_fn_with_state(
            AuthState { db },
            require_auth,
        ))
}

async fn protected_handler(
    AuthenticatedUser { user, token_id }: AuthenticatedUser,
    Extension(extension_user): Extension<User>,
    Extension(extension_token_id): Extension<Id>,
) -> Json<ProtectedResponse> {
    assert_eq!(extension_user, user);
    assert_eq!(extension_token_id, token_id);

    Json(ProtectedResponse {
        user_id: user.id,
        token_id,
        display_name: user.display_name,
    })
}

async fn create_token(
    db: &kino_db::Db,
    label: &str,
) -> Result<CreateTokenResponse, Box<dyn std::error::Error>> {
    let app = kino_server::router(db.clone());
    let response = app
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

async fn wait_for_last_seen(
    db: &kino_db::Db,
    token_id: Id,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..20 {
        let last_seen_at: Option<Timestamp> =
            sqlx::query_scalar("SELECT last_seen_at FROM device_tokens WHERE id = ?1")
                .bind(token_id)
                .fetch_one(db.read_pool())
                .await?;
        if last_seen_at.is_some() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    panic!("last_seen_at was not updated");
}

async fn assert_auth_error(
    response: axum::response::Response,
    status: StatusCode,
    error: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(response.status(), status);
    let body: Value = response_json(response).await?;
    assert_eq!(body, serde_json::json!({ "error": error }));
    Ok(())
}

async fn response_json<T: serde::de::DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
