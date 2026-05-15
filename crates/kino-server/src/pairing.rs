//! Public client pairing endpoints.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, State, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use kino_core::{Id, Pairing, PairingPlatform, PairingStatus, Timestamp};
use kino_db::Db;
use rand::{Rng, rngs::OsRng};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{auth::AuthenticatedUser, token};

const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
const PAIRING_CODE_SPACE: u32 = 1_000_000;

type PairingRow = (
    Id,
    String,
    String,
    String,
    String,
    Option<Id>,
    Timestamp,
    Timestamp,
    Option<Timestamp>,
);

#[derive(Clone)]
pub(crate) struct PairingState {
    db: Db,
    token_store: PairingTokenStore,
}

/// Plaintext token staged between admin approval and the client's first poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingToken {
    /// Plaintext bearer token returned once to the paired client.
    pub token: String,
    /// Persisted device token id linked to the pairing.
    pub token_id: Id,
    /// User id that owns the linked device token.
    pub user_id: Id,
    /// Token staging expiry; expired entries are removed by the pairing reaper.
    pub expires_at: Timestamp,
}

/// In-memory one-shot store for approved pairing plaintext tokens.
#[derive(Debug, Clone, Default)]
pub struct PairingTokenStore {
    inner: Arc<Mutex<HashMap<Id, PendingToken>>>,
}

impl PairingTokenStore {
    /// Create an empty pairing token store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage a plaintext token for the pairing id.
    pub async fn insert(&self, pairing_id: Id, token: PendingToken) {
        self.inner.lock().await.insert(pairing_id, token);
    }

    /// Remove and return a staged token for the pairing id.
    pub async fn take(&self, pairing_id: Id) -> Option<PendingToken> {
        self.inner.lock().await.remove(&pairing_id)
    }

    /// Remove staged tokens whose expiry is at or before `now`.
    pub async fn purge_expired(&self, now: Timestamp) -> usize {
        let mut tokens = self.inner.lock().await;
        let before = tokens.len();
        tokens.retain(|_, token| token.expires_at > now);
        before - tokens.len()
    }
}

#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreatePairingRequest {
    /// Client-supplied device label shown to an admin.
    device_name: String,
    /// Client platform requesting a pairing code.
    platform: PairingPlatform,
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub(crate) struct CreatePairingResponse {
    /// Persisted pairing id.
    pairing_id: Id,
    /// Six-digit base10 pairing code.
    code: String,
    /// Pairing code expiry timestamp.
    expires_at: Timestamp,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(tag = "status", rename_all = "lowercase")]
pub(crate) enum PairingStatusResponse {
    /// Pairing is waiting for admin approval.
    Pending,
    /// Pairing was approved and the one-shot token is included.
    Approved {
        /// Plaintext bearer token returned only once.
        token: String,
        /// Persisted device token id.
        token_id: Id,
        /// User id that owns the device token.
        user_id: Id,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminPairingSummary {
    /// Persisted pairing id.
    pairing_id: Id,
    /// Six-digit base10 pairing code.
    code: String,
    /// Client-supplied device label shown to an admin.
    device_name: String,
    /// Client platform requesting approval.
    platform: PairingPlatform,
    /// Pairing creation timestamp.
    created_at: Timestamp,
    /// Pairing code expiry timestamp.
    expires_at: Timestamp,
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub(crate) struct ListPairingsResponse {
    /// Pending pairings visible to an admin.
    pairings: Vec<AdminPairingSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub(crate) struct ApprovePairingResponse {
    /// Approved pairing id.
    pairing_id: Id,
    /// Last six characters of the one-shot plaintext token.
    token_preview: String,
}

pub(crate) fn router(db: Db, token_store: PairingTokenStore) -> Router {
    Router::new()
        .route("/api/v1/pairings", post(create_pairing))
        .route("/api/v1/pairings/{code}", get(get_pairing))
        .with_state(PairingState { db, token_store })
}

pub(crate) fn admin_router(db: Db, token_store: PairingTokenStore) -> Router {
    Router::new()
        .route("/api/v1/admin/pairings", get(list_pairings))
        .route(
            "/api/v1/admin/pairings/{pairing_id}/approve",
            post(approve_pairing),
        )
        .route(
            "/api/v1/admin/pairings/{pairing_id}/reject",
            post(reject_pairing),
        )
        .with_state(PairingState { db, token_store })
}

#[utoipa::path(
    post,
    path = "/api/v1/pairings",
    tag = "pairings",
    request_body = CreatePairingRequest,
    responses(
        (status = 201, description = "Pairing code created", body = CreatePairingResponse),
        (status = 400, description = "Invalid pairing request", body = PairingErrorResponse),
        (status = 500, description = "Pairing creation failed", body = PairingErrorResponse)
    )
)]
pub(crate) async fn create_pairing(
    State(state): State<PairingState>,
    payload: Result<Json<CreatePairingRequest>, JsonRejection>,
) -> PairingResult<(StatusCode, Json<CreatePairingResponse>)> {
    let Json(payload) = payload.map_err(|err| PairingApiError::InvalidRequest(err.to_string()))?;
    let device_name = payload.device_name.trim();
    if device_name.is_empty() {
        return Err(PairingApiError::EmptyDeviceName);
    }

    let created_at = Timestamp::now();
    let expires_at = Timestamp::from_offset(
        created_at.as_offset() + time::Duration::seconds(PAIRING_TTL.as_secs() as i64),
    );
    let pairing = kino_db::pairings::try_insert_with_fresh_code(
        &state.db,
        generate_pairing_code,
        device_name.to_owned(),
        payload.platform,
        created_at,
        expires_at,
    )
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreatePairingResponse {
            pairing_id: pairing.id,
            code: pairing.code,
            expires_at: pairing.expires_at,
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/api/v1/pairings/{code}",
    tag = "pairings",
    params(
        ("code" = String, Path, description = "Six-digit pairing code")
    ),
    responses(
        (status = 200, description = "Pairing status", body = PairingStatusResponse),
        (status = 404, description = "Pairing code not found", body = PairingErrorResponse),
        (status = 410, description = "Pairing expired or already consumed", body = PairingErrorResponse),
        (status = 500, description = "Pairing lookup failed", body = PairingErrorResponse)
    )
)]
pub(crate) async fn get_pairing(
    State(state): State<PairingState>,
    Path(code): Path<String>,
) -> PairingResult<Json<PairingStatusResponse>> {
    let Some(pairing) = kino_db::pairings::find_by_code(&state.db, &code).await? else {
        return Err(PairingApiError::NotFound);
    };
    let now = Timestamp::now();
    let pending_token = if pairing.status == PairingStatus::Approved && pairing.expires_at > now {
        state.token_store.take(pairing.id).await
    } else {
        None
    };
    let response = status_response_for_pairing(&pairing, pending_token, now)?;

    if matches!(response, PairingStatusResponse::Approved { .. }) {
        let rows = kino_db::pairings::mark_consumed(&state.db, pairing.id).await?;
        if rows == 0 {
            return Err(PairingApiError::Gone);
        }
    }

    Ok(Json(response))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/pairings",
    tag = "pairings",
    responses(
        (status = 200, description = "Pending pairings visible to admin", body = ListPairingsResponse),
        (status = 401, description = "Bearer token missing or invalid", body = PairingErrorResponse),
        (status = 500, description = "Pairing list failed", body = PairingErrorResponse)
    )
)]
pub(crate) async fn list_pairings(
    State(state): State<PairingState>,
) -> PairingResult<Json<ListPairingsResponse>> {
    let rows = sqlx::query_as::<_, (Id, String, String, String, Timestamp, Timestamp)>(
        r#"
        SELECT id, code, device_name, platform, created_at, expires_at
        FROM pairings
        WHERE status = 'pending'
        ORDER BY created_at, id
        "#,
    )
    .fetch_all(state.db.read_pool())
    .await?;

    let pairings = rows
        .into_iter()
        .map(admin_summary_from_row)
        .collect::<PairingResult<Vec<_>>>()?;

    Ok(Json(ListPairingsResponse { pairings }))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/pairings/{pairing_id}/approve",
    tag = "pairings",
    params(
        ("pairing_id" = Id, Path, description = "Persisted pairing id")
    ),
    responses(
        (status = 200, description = "Pairing approved", body = ApprovePairingResponse),
        (status = 401, description = "Bearer token missing or invalid", body = PairingErrorResponse),
        (status = 404, description = "Pairing not found", body = PairingErrorResponse),
        (status = 409, description = "Pairing already resolved", body = PairingErrorResponse),
        (status = 500, description = "Pairing approval failed", body = PairingErrorResponse)
    )
)]
pub(crate) async fn approve_pairing(
    State(state): State<PairingState>,
    AuthenticatedUser { user, .. }: AuthenticatedUser,
    Path(pairing_id): Path<Id>,
) -> PairingResult<Json<ApprovePairingResponse>> {
    let Some(pairing) = find_pairing_by_id(&state.db, pairing_id).await? else {
        return Err(PairingApiError::NotFound);
    };
    ensure_pending(&pairing)?;

    let label = format!("{} (paired)", pairing.device_name);
    let minted = token::mint_device_token(&state.db, user.id, label).await?;
    let approved_at = Timestamp::now();
    let rows =
        kino_db::pairings::approve(&state.db, pairing.id, minted.token.id, approved_at).await?;
    if rows == 0 {
        return Err(resolve_missing_or_conflict(&state.db, pairing.id).await?);
    }

    state
        .token_store
        .insert(
            pairing.id,
            PendingToken {
                token: minted.plaintext.clone(),
                token_id: minted.token.id,
                user_id: user.id,
                expires_at: pairing.expires_at,
            },
        )
        .await;

    Ok(Json(ApprovePairingResponse {
        pairing_id: pairing.id,
        token_preview: token_preview(&minted.plaintext),
    }))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/pairings/{pairing_id}/reject",
    tag = "pairings",
    params(
        ("pairing_id" = Id, Path, description = "Persisted pairing id")
    ),
    responses(
        (status = 204, description = "Pairing rejected"),
        (status = 401, description = "Bearer token missing or invalid", body = PairingErrorResponse),
        (status = 404, description = "Pairing not found", body = PairingErrorResponse),
        (status = 409, description = "Pairing already resolved", body = PairingErrorResponse),
        (status = 500, description = "Pairing rejection failed", body = PairingErrorResponse)
    )
)]
pub(crate) async fn reject_pairing(
    State(state): State<PairingState>,
    Path(pairing_id): Path<Id>,
) -> PairingResult<StatusCode> {
    let Some(pairing) = find_pairing_by_id(&state.db, pairing_id).await? else {
        return Err(PairingApiError::NotFound);
    };
    ensure_pending(&pairing)?;

    let rows =
        kino_db::pairings::update_status(&state.db, pairing.id, PairingStatus::Expired, None)
            .await?;
    if rows == 0 {
        return Err(resolve_missing_or_conflict(&state.db, pairing.id).await?);
    }

    Ok(StatusCode::NO_CONTENT)
}

pub(crate) type PairingResult<T> = std::result::Result<T, PairingApiError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PairingApiError {
    #[error("pairing device name must not be empty")]
    EmptyDeviceName,

    #[error("invalid pairing request: {0}")]
    InvalidRequest(String),

    #[error("pairing code not found")]
    NotFound,

    #[error("pairing expired or already consumed")]
    Gone,

    #[error("pairing token is no longer available")]
    TokenLost,

    #[error("pairing has already been approved, rejected, expired, or consumed")]
    AlreadyResolved,

    #[error("invalid pairing platform in database: {0}")]
    InvalidPersistedPlatform(String),

    #[error("invalid pairing status in database: {0}")]
    InvalidPersistedStatus(String),

    #[error(transparent)]
    Token(#[from] token::TokenApiError),

    #[error(transparent)]
    Db(#[from] kino_db::Error),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
}

#[derive(Deserialize, Serialize, utoipa::ToSchema)]
pub(crate) struct PairingErrorResponse {
    error: String,
}

impl IntoResponse for PairingApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::EmptyDeviceName | Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::AlreadyResolved => StatusCode::CONFLICT,
            Self::Gone | Self::TokenLost => StatusCode::GONE,
            Self::InvalidPersistedPlatform(_)
            | Self::InvalidPersistedStatus(_)
            | Self::Token(_)
            | Self::Db(_)
            | Self::Sqlx(_) => {
                tracing::error!(error = %self, "pairing api failed");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        (
            status,
            Json(PairingErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response()
    }
}

fn status_response_for_pairing(
    pairing: &Pairing,
    pending_token: Option<PendingToken>,
    now: Timestamp,
) -> PairingResult<PairingStatusResponse> {
    if pairing.expires_at <= now {
        return Err(PairingApiError::Gone);
    }

    match pairing.status {
        PairingStatus::Pending => Ok(PairingStatusResponse::Pending),
        PairingStatus::Approved => {
            let token = pending_token
                .filter(|token| token.expires_at > now)
                .ok_or(PairingApiError::TokenLost)?;
            Ok(PairingStatusResponse::Approved {
                token: token.token,
                token_id: token.token_id,
                user_id: token.user_id,
            })
        }
        PairingStatus::Expired | PairingStatus::Consumed => Err(PairingApiError::Gone),
    }
}

fn admin_summary_from_row(
    row: (Id, String, String, String, Timestamp, Timestamp),
) -> PairingResult<AdminPairingSummary> {
    let platform = row
        .3
        .parse::<PairingPlatform>()
        .map_err(|_| PairingApiError::InvalidPersistedPlatform(row.3.clone()))?;

    Ok(AdminPairingSummary {
        pairing_id: row.0,
        code: row.1,
        device_name: row.2,
        platform,
        created_at: row.4,
        expires_at: row.5,
    })
}

async fn find_pairing_by_id(db: &Db, pairing_id: Id) -> PairingResult<Option<Pairing>> {
    let row: Option<PairingRow> = sqlx::query_as(
        r#"
        SELECT
            id,
            code,
            device_name,
            platform,
            status,
            token_id,
            created_at,
            expires_at,
            approved_at
        FROM pairings
        WHERE id = ?1
        "#,
    )
    .bind(pairing_id)
    .fetch_optional(db.read_pool())
    .await?;

    row.map(pairing_from_row).transpose()
}

fn pairing_from_row(row: PairingRow) -> PairingResult<Pairing> {
    let platform = row
        .3
        .parse::<PairingPlatform>()
        .map_err(|_| PairingApiError::InvalidPersistedPlatform(row.3.clone()))?;
    let status = row
        .4
        .parse::<PairingStatus>()
        .map_err(|_| PairingApiError::InvalidPersistedStatus(row.4.clone()))?;

    Ok(Pairing {
        id: row.0,
        code: row.1,
        device_name: row.2,
        platform,
        status,
        token_id: row.5,
        created_at: row.6,
        expires_at: row.7,
        approved_at: row.8,
    })
}

fn ensure_pending(pairing: &Pairing) -> PairingResult<()> {
    if pairing.status == PairingStatus::Pending {
        Ok(())
    } else {
        Err(PairingApiError::AlreadyResolved)
    }
}

async fn resolve_missing_or_conflict(db: &Db, pairing_id: Id) -> PairingResult<PairingApiError> {
    if find_pairing_by_id(db, pairing_id).await?.is_some() {
        Ok(PairingApiError::AlreadyResolved)
    } else {
        Ok(PairingApiError::NotFound)
    }
}

fn token_preview(plaintext: &str) -> String {
    plaintext[plaintext.len().saturating_sub(6)..].to_owned()
}

fn generate_pairing_code() -> String {
    format!("{:06}", OsRng.gen_range(0..PAIRING_CODE_SPACE))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use kino_core::{
        Config, DeviceToken, LibraryConfig, OcrConfig, SEEDED_USER_ID, TranscodeConfig,
        config::{LogFormat, ProvidersConfig, ServerConfig, TmdbConfig},
    };
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn state_machine_returns_pending_before_expiry() -> Result<(), Box<dyn std::error::Error>> {
        let pairing = pairing_with_status(PairingStatus::Pending)?;
        let now: Timestamp = "2026-05-14T01:01:00Z".parse()?;

        let response = status_response_for_pairing(&pairing, None, now)?;

        assert_eq!(response, PairingStatusResponse::Pending);
        Ok(())
    }

    #[test]
    fn state_machine_returns_approved_with_token() -> Result<(), Box<dyn std::error::Error>> {
        let pairing = pairing_with_status(PairingStatus::Approved)?;
        let now: Timestamp = "2026-05-14T01:01:00Z".parse()?;
        let token = pending_token("paired-token", pairing.expires_at);

        let response = status_response_for_pairing(&pairing, Some(token.clone()), now)?;

        assert_eq!(
            response,
            PairingStatusResponse::Approved {
                token: token.token,
                token_id: token.token_id,
                user_id: token.user_id
            }
        );
        Ok(())
    }

    #[test]
    fn state_machine_rejects_approved_without_token() -> Result<(), Box<dyn std::error::Error>> {
        let pairing = pairing_with_status(PairingStatus::Approved)?;
        let now: Timestamp = "2026-05-14T01:01:00Z".parse()?;

        let err = status_response_for_pairing(&pairing, None, now).unwrap_err();

        assert!(matches!(err, PairingApiError::TokenLost));
        Ok(())
    }

    #[test]
    fn state_machine_rejects_expired_pairing() -> Result<(), Box<dyn std::error::Error>> {
        let pairing = pairing_with_status(PairingStatus::Pending)?;
        let now = pairing.expires_at;

        let err = status_response_for_pairing(&pairing, None, now).unwrap_err();

        assert!(matches!(err, PairingApiError::Gone));
        Ok(())
    }

    #[test]
    fn state_machine_rejects_consumed_pairing() -> Result<(), Box<dyn std::error::Error>> {
        let pairing = pairing_with_status(PairingStatus::Consumed)?;
        let now: Timestamp = "2026-05-14T01:01:00Z".parse()?;

        let err = status_response_for_pairing(&pairing, None, now).unwrap_err();

        assert!(matches!(err, PairingApiError::Gone));
        Ok(())
    }

    #[tokio::test]
    async fn token_store_takes_once_and_purges_expired() -> Result<(), Box<dyn std::error::Error>> {
        let store = PairingTokenStore::new();
        let first_id = Id::new();
        let second_id = Id::new();
        let expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let later: Timestamp = "2026-05-14T01:10:00Z".parse()?;

        store
            .insert(first_id, pending_token("first-token", expires_at))
            .await;
        assert!(store.take(first_id).await.is_some());
        assert!(store.take(first_id).await.is_none());

        store
            .insert(second_id, pending_token("second-token", expires_at))
            .await;
        assert_eq!(store.purge_expired(later).await, 1);
        assert!(store.take(second_id).await.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn pairing_happy_path_returns_token_once() -> Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let store = PairingTokenStore::new();
        let app = router(db.clone(), store.clone());

        let response = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/pairings",
                r#"{"device_name":"Living room Apple TV","platform":"tvos"}"#,
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::CREATED);
        let created: CreatePairingResponse = read_json(response).await?;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/pairings/{}", created.code))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let pending: PairingStatusResponse = read_json(response).await?;
        assert_eq!(pending, PairingStatusResponse::Pending);

        let token = "paired-device-token";
        let token_id = insert_device_token(&db, "Living room Apple TV", token).await?;
        let pairing = kino_db::pairings::find_by_code(&db, &created.code)
            .await?
            .unwrap();
        kino_db::pairings::approve(&db, pairing.id, token_id, Timestamp::now()).await?;
        store
            .insert(
                pairing.id,
                PendingToken {
                    token: token.to_owned(),
                    token_id,
                    user_id: SEEDED_USER_ID,
                    expires_at: created.expires_at,
                },
            )
            .await;

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/pairings/{}", created.code))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let approved: PairingStatusResponse = read_json(response).await?;
        assert_eq!(
            approved,
            PairingStatusResponse::Approved {
                token: token.to_owned(),
                token_id,
                user_id: SEEDED_USER_ID
            }
        );
        let consumed = kino_db::pairings::find_by_code(&db, &created.code)
            .await?
            .unwrap();
        assert_eq!(consumed.status, PairingStatus::Consumed);
        assert_eq!(consumed.token_id, Some(token_id));
        assert!(consumed.approved_at.is_some());

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/pairings/{}", created.code))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::GONE);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn expired_pending_pairing_returns_gone() -> Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let created_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let pairing = Pairing::new(
            Id::new(),
            "111111",
            "Bedroom iPad",
            PairingPlatform::Ios,
            created_at,
            expires_at,
        );
        kino_db::pairings::insert(&db, &pairing).await?;

        let app = router(db.clone(), PairingTokenStore::new());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/pairings/111111")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::GONE);
        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_code_returns_not_found() -> Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let app = router(db.clone(), PairingTokenStore::new());

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/pairings/000000")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn invalid_create_body_returns_bad_request() -> Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let app = router(db.clone(), PairingTokenStore::new());

        let missing_name = app
            .clone()
            .oneshot(json_request(
                "POST",
                "/api/v1/pairings",
                r#"{"platform":"ios"}"#,
            )?)
            .await?;
        assert_eq!(missing_name.status(), StatusCode::BAD_REQUEST);

        let unknown_platform = app
            .oneshot(json_request(
                "POST",
                "/api/v1/pairings",
                r#"{"device_name":"Kitchen Mac","platform":"visionos"}"#,
            )?)
            .await?;
        assert_eq!(unknown_platform.status(), StatusCode::BAD_REQUEST);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admin_list_pairings_returns_pending_in_creation_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let auth = authenticated_app().await?;
        let first = insert_pairing(
            &auth.db,
            "111111",
            "Bedroom iPad",
            PairingPlatform::Ios,
            PairingStatus::Pending,
            timestamp("2026-05-15T01:00:00Z")?,
        )
        .await?;
        let second = insert_pairing(
            &auth.db,
            "222222",
            "Living room Apple TV",
            PairingPlatform::Tvos,
            PairingStatus::Pending,
            timestamp("2026-05-15T01:01:00Z")?,
        )
        .await?;
        insert_pairing(
            &auth.db,
            "333333",
            "Expired Mac",
            PairingPlatform::Macos,
            PairingStatus::Expired,
            timestamp("2026-05-15T01:02:00Z")?,
        )
        .await?;
        insert_pairing(
            &auth.db,
            "444444",
            "Approved TV",
            PairingPlatform::Tvos,
            PairingStatus::Approved,
            timestamp("2026-05-15T01:03:00Z")?,
        )
        .await?;

        let response = auth
            .app
            .oneshot(authed_empty_request(
                "GET",
                "/api/v1/admin/pairings",
                &auth.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        let listed: ListPairingsResponse = read_json(response).await?;
        assert_eq!(listed.pairings.len(), 2);
        assert_eq!(listed.pairings[0].pairing_id, first.id);
        assert_eq!(listed.pairings[1].pairing_id, second.id);

        auth.db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admin_approve_pairing_mints_token_and_returns_preview()
    -> Result<(), Box<dyn std::error::Error>> {
        let auth = authenticated_app().await?;
        let pairing = insert_pairing(
            &auth.db,
            "555555",
            "Living room Apple TV",
            PairingPlatform::Tvos,
            PairingStatus::Pending,
            Timestamp::now(),
        )
        .await?;

        let response = auth
            .app
            .clone()
            .oneshot(authed_empty_request(
                "POST",
                &format!("/api/v1/admin/pairings/{}/approve", pairing.id),
                &auth.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        let approved: ApprovePairingResponse = read_json(response).await?;
        assert_eq!(approved.pairing_id, pairing.id);

        let updated = kino_db::pairings::find_by_code(&auth.db, &pairing.code)
            .await?
            .unwrap();
        assert_eq!(updated.status, PairingStatus::Approved);
        let token_id = updated.token_id.unwrap();
        assert!(updated.approved_at.is_some());

        let token_user_id: Id =
            sqlx::query_scalar("SELECT user_id FROM device_tokens WHERE id = ?1")
                .bind(token_id)
                .fetch_one(auth.db.read_pool())
                .await?;
        assert_eq!(token_user_id, SEEDED_USER_ID);

        let staged = auth.store.inner.lock().await.get(&pairing.id).cloned();
        assert_eq!(
            staged,
            Some(PendingToken {
                token: staged.clone().unwrap().token,
                token_id,
                user_id: SEEDED_USER_ID,
                expires_at: pairing.expires_at,
            })
        );

        let response = auth
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/pairings/{}", pairing.code))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let polled: PairingStatusResponse = read_json(response).await?;
        let PairingStatusResponse::Approved { token, .. } = polled else {
            panic!("expected approved pairing response");
        };
        assert_eq!(approved.token_preview, token_preview(&token));

        let response = auth
            .app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/pairings/{}", pairing.code))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::GONE);

        auth.db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admin_approve_non_pending_pairing_returns_conflict()
    -> Result<(), Box<dyn std::error::Error>> {
        for (code, status) in [
            ("666661", PairingStatus::Approved),
            ("666662", PairingStatus::Consumed),
            ("666663", PairingStatus::Expired),
        ] {
            let auth = authenticated_app().await?;
            let pairing = insert_pairing(
                &auth.db,
                code,
                "Resolved device",
                PairingPlatform::Ios,
                status,
                Timestamp::now(),
            )
            .await?;

            let response = auth
                .app
                .oneshot(authed_empty_request(
                    "POST",
                    &format!("/api/v1/admin/pairings/{}/approve", pairing.id),
                    &auth.bearer,
                )?)
                .await?;

            assert_eq!(response.status(), StatusCode::CONFLICT);
            let error: PairingErrorResponse = read_json(response).await?;
            assert_eq!(error.error, PairingApiError::AlreadyResolved.to_string());
            auth.db.close().await;
        }

        Ok(())
    }

    #[tokio::test]
    async fn admin_approve_missing_pairing_returns_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let auth = authenticated_app().await?;

        let response = auth
            .app
            .oneshot(authed_empty_request(
                "POST",
                &format!("/api/v1/admin/pairings/{}/approve", Id::new()),
                &auth.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        auth.db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admin_reject_pairing_marks_expired() -> Result<(), Box<dyn std::error::Error>> {
        let auth = authenticated_app().await?;
        let pairing = insert_pairing(
            &auth.db,
            "777777",
            "Kitchen iPad",
            PairingPlatform::Ios,
            PairingStatus::Pending,
            Timestamp::now(),
        )
        .await?;

        let response = auth
            .app
            .clone()
            .oneshot(authed_empty_request(
                "POST",
                &format!("/api/v1/admin/pairings/{}/reject", pairing.id),
                &auth.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let updated = kino_db::pairings::find_by_code(&auth.db, &pairing.code)
            .await?
            .unwrap();
        assert_eq!(updated.status, PairingStatus::Expired);
        assert_eq!(updated.token_id, None);
        assert_eq!(updated.approved_at, None);

        let response = auth
            .app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/v1/pairings/{}", pairing.code))
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::GONE);

        auth.db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admin_reject_non_pending_pairing_returns_conflict()
    -> Result<(), Box<dyn std::error::Error>> {
        let auth = authenticated_app().await?;
        let pairing = insert_pairing(
            &auth.db,
            "888888",
            "Resolved device",
            PairingPlatform::Macos,
            PairingStatus::Consumed,
            Timestamp::now(),
        )
        .await?;

        let response = auth
            .app
            .oneshot(authed_empty_request(
                "POST",
                &format!("/api/v1/admin/pairings/{}/reject", pairing.id),
                &auth.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::CONFLICT);
        auth.db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admin_reject_missing_pairing_returns_not_found()
    -> Result<(), Box<dyn std::error::Error>> {
        let auth = authenticated_app().await?;

        let response = auth
            .app
            .oneshot(authed_empty_request(
                "POST",
                &format!("/api/v1/admin/pairings/{}/reject", Id::new()),
                &auth.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        auth.db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn admin_pairing_routes_require_valid_bearer() -> Result<(), Box<dyn std::error::Error>> {
        let auth = authenticated_app().await?;
        let pairing = insert_pairing(
            &auth.db,
            "999999",
            "Office Mac",
            PairingPlatform::Macos,
            PairingStatus::Pending,
            Timestamp::now(),
        )
        .await?;

        for (method, uri) in [
            ("GET", "/api/v1/admin/pairings".to_owned()),
            (
                "POST",
                format!("/api/v1/admin/pairings/{}/approve", pairing.id),
            ),
            (
                "POST",
                format!("/api/v1/admin/pairings/{}/reject", pairing.id),
            ),
        ] {
            let missing = auth
                .app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(&uri)
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

            let garbage = auth
                .app
                .clone()
                .oneshot(authed_empty_request(method, &uri, "garbage")?)
                .await?;
            assert_eq!(garbage.status(), StatusCode::UNAUTHORIZED);
        }

        auth.db.close().await;
        Ok(())
    }

    fn pairing_with_status(status: PairingStatus) -> Result<Pairing, Box<dyn std::error::Error>> {
        let created_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let mut pairing = Pairing::new(
            Id::new(),
            "123456",
            "Living room Apple TV",
            PairingPlatform::Tvos,
            created_at,
            expires_at,
        );
        pairing.status = status;
        Ok(pairing)
    }

    fn pending_token(token: &str, expires_at: Timestamp) -> PendingToken {
        PendingToken {
            token: token.to_owned(),
            token_id: Id::new(),
            user_id: SEEDED_USER_ID,
            expires_at,
        }
    }

    async fn insert_device_token(
        db: &Db,
        label: &str,
        plaintext: &str,
    ) -> Result<Id, Box<dyn std::error::Error>> {
        let token = DeviceToken::new(
            Id::new(),
            SEEDED_USER_ID,
            label,
            crate::auth::hash_token(plaintext),
            Timestamp::now(),
        );

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

        Ok(token.id)
    }

    struct AuthenticatedApp {
        db: Db,
        store: PairingTokenStore,
        app: Router,
        bearer: String,
    }

    async fn authenticated_app() -> Result<AuthenticatedApp, Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let store = PairingTokenStore::new();
        let bearer = "admin-pairing-test-token";
        insert_device_token(&db, "Admin token", bearer).await?;
        let app =
            crate::router_with_config_and_token_store(db.clone(), test_config(), store.clone());

        Ok(AuthenticatedApp {
            db,
            store,
            app,
            bearer: bearer.to_owned(),
        })
    }

    async fn insert_pairing(
        db: &Db,
        code: &str,
        device_name: &str,
        platform: PairingPlatform,
        status: PairingStatus,
        created_at: Timestamp,
    ) -> Result<Pairing, Box<dyn std::error::Error>> {
        let expires_at =
            Timestamp::from_offset(created_at.as_offset() + time::Duration::minutes(5));
        let mut pairing = Pairing::new(
            Id::new(),
            code,
            device_name,
            platform,
            created_at,
            expires_at,
        );

        match status {
            PairingStatus::Pending => {}
            PairingStatus::Expired => {
                pairing.status = PairingStatus::Expired;
            }
            PairingStatus::Approved | PairingStatus::Consumed => {
                let token_id =
                    insert_device_token(db, device_name, &format!("{code}-token")).await?;
                pairing.status = status;
                pairing.token_id = Some(token_id);
                pairing.approved_at = Some(created_at);
            }
        }

        kino_db::pairings::insert(db, &pairing).await?;
        Ok(pairing)
    }

    fn authed_empty_request(
        method: &str,
        uri: &str,
        bearer: &str,
    ) -> Result<Request<Body>, Box<dyn std::error::Error>> {
        Ok(Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .body(Body::empty())?)
    }

    fn test_config() -> Config {
        Config {
            database_path: PathBuf::from("kino.db"),
            library_root: PathBuf::from("."),
            library: LibraryConfig::default(),
            server: ServerConfig::default(),
            tmdb: TmdbConfig::default(),
            ocr: OcrConfig::default(),
            providers: ProvidersConfig::default(),
            transcode: TranscodeConfig::default(),
            log_level: "info".to_owned(),
            log_format: LogFormat::Pretty,
        }
    }

    fn timestamp(value: &str) -> Result<Timestamp, Box<dyn std::error::Error>> {
        Ok(value.parse()?)
    }

    fn json_request(
        method: &str,
        uri: &str,
        body: &str,
    ) -> Result<Request<Body>, Box<dyn std::error::Error>> {
        Ok(Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))?)
    }

    async fn read_json<T: serde::de::DeserializeOwned>(
        response: Response,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}
