use std::sync::atomic::{AtomicU64, Ordering};

use kino_core::{Id, Timestamp, device_token::DeviceToken, user::SEEDED_USER_ID};
use sha2::{Digest, Sha256};

static TOKEN_COUNTER: AtomicU64 = AtomicU64::new(1);

#[allow(dead_code)]
pub(crate) async fn issued_token(db: &kino_db::Db) -> Result<String, sqlx::Error> {
    let (plaintext, _) = issued_token_with_id(db).await?;
    Ok(plaintext)
}

pub(crate) async fn issued_token_with_id(db: &kino_db::Db) -> Result<(String, Id), sqlx::Error> {
    let sequence = TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let plaintext = format!("kino-test-token-{}-{sequence}", Id::new());
    let hash = format!("{:x}", Sha256::digest(plaintext.as_bytes()));
    let id = Id::new();
    let token = DeviceToken::new(
        id,
        SEEDED_USER_ID,
        format!("Test auth token {sequence}"),
        hash,
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

    Ok((plaintext, token.id))
}

pub(crate) fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}
