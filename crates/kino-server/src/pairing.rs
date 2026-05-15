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

const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
const PAIRING_CODE_SPACE: u32 = 1_000_000;

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

pub(crate) fn router(db: Db, token_store: PairingTokenStore) -> Router {
    Router::new()
        .route("/api/v1/pairings", post(create_pairing))
        .route("/api/v1/pairings/{code}", get(get_pairing))
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

    #[error(transparent)]
    Db(#[from] kino_db::Error),
}

#[derive(Serialize, utoipa::ToSchema)]
pub(crate) struct PairingErrorResponse {
    error: String,
}

impl IntoResponse for PairingApiError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::EmptyDeviceName | Self::InvalidRequest(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Gone | Self::TokenLost => StatusCode::GONE,
            Self::Db(_) => {
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

fn generate_pairing_code() -> String {
    format!("{:06}", OsRng.gen_range(0..PAIRING_CODE_SPACE))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use kino_core::{DeviceToken, SEEDED_USER_ID};
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
