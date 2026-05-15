use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use kino_core::{Id, Timestamp, device_token::DeviceToken, user::SEEDED_USER_ID};
use kino_db::Db;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sqlx::Row;

#[derive(Clone)]
pub(crate) struct TokenState {
    db: Db,
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateTokenRequest {
    /// Operator-facing label for the device token.
    label: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct CreateTokenResponse {
    /// Plaintext bearer token. This value is returned only once.
    token: String,
    /// Persisted device token id.
    token_id: Id,
    /// Operator-facing label for the device token.
    label: String,
    /// Token creation timestamp.
    created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct TokenSummary {
    /// Persisted device token id.
    token_id: Id,
    /// Operator-facing label for the device token.
    label: String,
    /// Last successful authentication time.
    last_seen_at: Option<Timestamp>,
    /// Revocation time, if the token has been revoked.
    revoked_at: Option<Timestamp>,
    /// Token creation timestamp.
    created_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct ListTokensResponse {
    /// Device tokens owned by the seeded user.
    tokens: Vec<TokenSummary>,
}

pub(crate) struct MintedDeviceToken {
    pub(crate) plaintext: String,
    pub(crate) token: DeviceToken,
}

pub(crate) fn router(db: Db) -> Router {
    Router::new()
        .route("/api/v1/admin/tokens", post(create_token).get(list_tokens))
        .route("/api/v1/admin/tokens/{token_id}", delete(revoke_token))
        .with_state(TokenState { db })
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/tokens",
    tag = "admin",
    request_body = CreateTokenRequest,
    responses(
        (status = 201, description = "Device token minted", body = CreateTokenResponse),
        (status = 400, description = "Invalid token request", body = TokenErrorResponse),
        (status = 500, description = "Token issuance failed", body = TokenErrorResponse)
    )
)]
pub(crate) async fn create_token(
    State(state): State<TokenState>,
    Json(payload): Json<CreateTokenRequest>,
) -> TokenResult<(StatusCode, Json<CreateTokenResponse>)> {
    if payload.label.trim().is_empty() {
        return Err(TokenApiError::EmptyLabel);
    }

    let minted = mint_device_token(&state.db, SEEDED_USER_ID, payload.label).await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateTokenResponse {
            token: minted.plaintext,
            token_id: minted.token.id,
            label: minted.token.label,
            created_at: minted.token.created_at,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/tokens",
    tag = "admin",
    responses(
        (status = 200, description = "Device token summaries", body = ListTokensResponse),
        (status = 500, description = "Token list failed", body = TokenErrorResponse)
    )
)]
pub(crate) async fn list_tokens(
    State(state): State<TokenState>,
) -> TokenResult<Json<ListTokensResponse>> {
    let rows = sqlx::query(
        r#"
        SELECT
            id,
            label,
            last_seen_at,
            revoked_at,
            created_at
        FROM device_tokens
        WHERE user_id = ?1
        ORDER BY created_at, id
        "#,
    )
    .bind(SEEDED_USER_ID)
    .fetch_all(state.db.read_pool())
    .await?;

    let tokens = rows
        .iter()
        .map(token_summary_from_row)
        .collect::<TokenResult<Vec<_>>>()?;

    Ok(Json(ListTokensResponse { tokens }))
}

#[utoipa::path(
    delete,
    path = "/api/v1/admin/tokens/{token_id}",
    tag = "admin",
    params(
        ("token_id" = Id, Path, description = "Persisted device token id")
    ),
    responses(
        (status = 204, description = "Device token revoked"),
        (status = 404, description = "Device token not found", body = TokenErrorResponse),
        (status = 500, description = "Token revocation failed", body = TokenErrorResponse)
    )
)]
pub(crate) async fn revoke_token(
    State(state): State<TokenState>,
    Path(token_id): Path<Id>,
) -> TokenResult<StatusCode> {
    let revoked_at = Timestamp::now();
    let result = sqlx::query(
        r#"
        UPDATE device_tokens
        SET revoked_at = ?1
        WHERE id = ?2 AND user_id = ?3
        "#,
    )
    .bind(revoked_at)
    .bind(token_id)
    .bind(SEEDED_USER_ID)
    .execute(state.db.write_pool())
    .await?;

    if result.rows_affected() == 0 {
        return Err(TokenApiError::NotFound);
    }

    Ok(StatusCode::NO_CONTENT)
}

pub(crate) type TokenResult<T> = std::result::Result<T, TokenApiError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum TokenApiError {
    #[error("token label must not be empty")]
    EmptyLabel,

    #[error("device token not found")]
    NotFound,

    #[error("secure random token generation failed: {0}")]
    Random(#[from] rand::Error),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct TokenErrorResponse {
    error: String,
}

impl IntoResponse for TokenApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::EmptyLabel => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Random(_) | Self::Sqlx(_) => {
                tracing::error!(error = %self, "token api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (
            status,
            Json(TokenErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

impl From<DeviceToken> for TokenSummary {
    fn from(token: DeviceToken) -> Self {
        Self {
            token_id: token.id,
            label: token.label,
            last_seen_at: token.last_seen_at,
            revoked_at: token.revoked_at,
            created_at: token.created_at,
        }
    }
}

fn generate_plaintext_token() -> TokenResult<String> {
    let mut bytes = [0_u8; 32];
    OsRng.try_fill_bytes(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(crate) async fn mint_device_token(
    db: &Db,
    user_id: Id,
    label: impl Into<String>,
) -> TokenResult<MintedDeviceToken> {
    let label = label.into();
    if label.trim().is_empty() {
        return Err(TokenApiError::EmptyLabel);
    }

    let plaintext = generate_plaintext_token()?;
    let hash = crate::auth::hash_token(&plaintext);
    let token = DeviceToken::new(Id::new(), user_id, label, hash, Timestamp::now());

    sqlx::query(
        r#"
        INSERT INTO device_tokens (
            id,
            user_id,
            label,
            hash,
            last_seen_at,
            revoked_at,
            created_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
    )
    .bind(token.id)
    .bind(token.user_id)
    .bind(&token.label)
    .bind(&token.hash)
    .bind(token.last_seen_at)
    .bind(token.revoked_at)
    .bind(token.created_at)
    .execute(db.write_pool())
    .await?;

    Ok(MintedDeviceToken { plaintext, token })
}

fn token_summary_from_row(row: &sqlx::sqlite::SqliteRow) -> TokenResult<TokenSummary> {
    Ok(TokenSummary {
        token_id: row.try_get("id")?,
        label: row.try_get("label")?,
        last_seen_at: row.try_get("last_seen_at")?,
        revoked_at: row.try_get("revoked_at")?,
        created_at: row.try_get("created_at")?,
    })
}
