//! Bearer-token authentication middleware.

use axum::{
    Json,
    extract::{FromRequestParts, State},
    http::{StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use kino_core::{Id, Timestamp, user::User};
use kino_db::Db;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;

/// State required by the bearer-token authentication middleware.
#[derive(Clone)]
pub struct AuthState {
    /// Database handle used to resolve token hashes and users.
    pub db: Db,
}

/// Authenticated user context placed in request extensions by the middleware.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    /// User resolved from the bearer token.
    pub user: User,
    /// Device token id used for the request.
    pub token_id: Id,
}

/// Require a valid bearer token and attach authenticated user context.
pub async fn require_auth(
    State(state): State<AuthState>,
    mut req: axum::extract::Request,
    next: Next,
) -> Result<Response, AuthError> {
    let plaintext = bearer_token(req.headers())?;
    let hash = hash_token(plaintext);
    let token = lookup_token(&state.db, &hash).await?;

    if token.revoked_at.is_some() {
        return Err(AuthError::Revoked);
    }

    let user = lookup_user(&state.db, token.user_id).await?;
    let authenticated = AuthenticatedUser {
        user: user.clone(),
        token_id: token.id,
    };

    req.extensions_mut().insert(user);
    req.extensions_mut().insert(token.id);
    req.extensions_mut().insert(authenticated);

    spawn_last_seen_update(state.db, token.id);

    Ok(next.run(req).await)
}

/// Errors returned by bearer-token authentication.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The request did not include an Authorization header.
    #[error("missing bearer token")]
    Missing,

    /// The Authorization header was not a case-sensitive Bearer credential.
    #[error("malformed bearer token")]
    Malformed,

    /// The token hash did not match any stored device token.
    #[error("unknown bearer token")]
    Unknown,

    /// The device token was revoked.
    #[error("revoked bearer token")]
    Revoked,

    /// Authentication failed due to an internal dependency error.
    #[error(transparent)]
    Internal(#[from] sqlx::Error),
}

impl<S> FromRequestParts<S> for AuthenticatedUser
where
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or(AuthError::Missing)
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, error) = match &self {
            Self::Missing => (StatusCode::UNAUTHORIZED, "missing_bearer"),
            Self::Malformed => (StatusCode::UNAUTHORIZED, "malformed_bearer"),
            Self::Unknown => (StatusCode::UNAUTHORIZED, "invalid_token"),
            Self::Revoked => (StatusCode::UNAUTHORIZED, "revoked_token"),
            Self::Internal(_) => {
                tracing::error!(error = %self, "auth middleware failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };

        (status, Json(AuthErrorResponse { error })).into_response()
    }
}

#[derive(Serialize)]
struct AuthErrorResponse {
    error: &'static str,
}

#[derive(Debug)]
struct TokenLookup {
    user_id: Id,
    id: Id,
    revoked_at: Option<Timestamp>,
}

pub(crate) fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Result<&str, AuthError> {
    let value = headers
        .get(header::AUTHORIZATION)
        .ok_or(AuthError::Missing)?
        .to_str()
        .map_err(|_| AuthError::Malformed)?;
    let token = value.strip_prefix("Bearer ").ok_or(AuthError::Malformed)?;

    if token.is_empty() {
        return Err(AuthError::Malformed);
    }

    Ok(token)
}

async fn lookup_token(db: &Db, hash: &str) -> Result<TokenLookup, AuthError> {
    let Some(row) = sqlx::query(
        r#"
        SELECT user_id, id, revoked_at
        FROM device_tokens
        WHERE hash = ?1
        "#,
    )
    .bind(hash)
    .fetch_optional(db.read_pool())
    .await?
    else {
        return Err(AuthError::Unknown);
    };

    Ok(TokenLookup {
        user_id: row.try_get("user_id")?,
        id: row.try_get("id")?,
        revoked_at: row.try_get("revoked_at")?,
    })
}

async fn lookup_user(db: &Db, user_id: Id) -> Result<User, AuthError> {
    let row = sqlx::query(
        r#"
        SELECT id, display_name, created_at
        FROM users
        WHERE id = ?1
        "#,
    )
    .bind(user_id)
    .fetch_one(db.read_pool())
    .await?;

    Ok(User {
        id: row.try_get("id")?,
        display_name: row.try_get("display_name")?,
        created_at: row.try_get("created_at")?,
    })
}

fn spawn_last_seen_update(db: Db, token_id: Id) {
    tokio::spawn(async move {
        let now = Timestamp::now();
        if let Err(err) = sqlx::query("UPDATE device_tokens SET last_seen_at = ?1 WHERE id = ?2")
            .bind(now)
            .bind(token_id)
            .execute(db.write_pool())
            .await
        {
            tracing::warn!(
                error = %err,
                %token_id,
                "failed to update device token last_seen_at"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_token_matches_sha256_hex() {
        assert_eq!(
            hash_token("kino-test-token"),
            "1071e42bcc1e292c066a0248d404345f30f43889aeadd474cbba31cb8c7f5491"
        );
    }
}
