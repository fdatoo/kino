//! Playback session lifecycle writes.

use kino_core::{
    Id, PlaybackSession, PlaybackSessionStatus, Timestamp,
    playback_session::PlaybackSessionStatus as Status,
};
use kino_db::Db;
use sqlx::{Row, sqlite::SqliteRow};

/// Errors produced by playback session lifecycle writes.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The database contained a playback session status outside the known enum.
    #[error("unknown playback session status {value}")]
    UnknownStatus {
        /// Persisted status value that could not be decoded.
        value: String,
    },

    /// Reading or writing playback sessions failed.
    #[error("playback session database update failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Crate-local result alias for playback session lifecycle writes.
pub type Result<T> = std::result::Result<T, Error>;

/// Open a new active playback session for a user and device token.
///
/// Opening a stream is a replacement boundary: any current active session for
/// the same `(user_id, token_id)` is ended before the new active row is
/// inserted. Idle sessions are left for the reaper, which owns `idle -> ended`.
pub async fn open_session(
    db: &Db,
    user_id: Id,
    token_id: Id,
    media_item_id: Id,
    variant_id: String,
) -> Result<PlaybackSession> {
    let now = Timestamp::now();
    let session =
        PlaybackSession::active(Id::new(), user_id, token_id, media_item_id, variant_id, now);
    let mut tx = db.write_pool().begin().await?;

    sqlx::query(
        r#"
        UPDATE playback_sessions
        SET status = ?1,
            ended_at = ?2
        WHERE user_id = ?3
            AND token_id = ?4
            AND status = ?5
        "#,
    )
    .bind(Status::Ended.as_str())
    .bind(now)
    .bind(user_id)
    .bind(token_id)
    .bind(Status::Active.as_str())
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO playback_sessions (
            id,
            user_id,
            token_id,
            media_item_id,
            variant_id,
            started_at,
            last_seen_at,
            ended_at,
            status
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
    )
    .bind(session.id)
    .bind(session.user_id)
    .bind(session.token_id)
    .bind(session.media_item_id)
    .bind(&session.variant_id)
    .bind(session.started_at)
    .bind(session.last_seen_at)
    .bind(session.ended_at)
    .bind(session.status.as_str())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(session)
}

/// Refresh an active or idle playback session heartbeat.
///
/// Heartbeats keep `active` sessions active and revive `idle -> active`.
/// `ended` is terminal for client heartbeats, so ended sessions are a no-op.
pub async fn heartbeat_session(db: &Db, session_id: Id) -> Result<()> {
    heartbeat_session_at(db, session_id, Timestamp::now()).await
}

/// Refresh an existing stream session or open one if none is resumable.
///
/// Segment fetches are heartbeat boundaries for the same media variant. A
/// request for a different variant or media item opens a replacement session,
/// which ends any prior active session on the same user and device token.
pub async fn heartbeat_or_open_session(
    db: &Db,
    user_id: Id,
    token_id: Id,
    media_item_id: Id,
    variant_id: String,
) -> Result<PlaybackSession> {
    if let Some(session_id) =
        active_or_idle_session_id(db, user_id, token_id, media_item_id, &variant_id).await?
    {
        heartbeat_stream_session(db, session_id, user_id, token_id).await?;
        return fetch_session(db, session_id).await;
    }

    open_session(db, user_id, token_id, media_item_id, variant_id).await
}

async fn heartbeat_stream_session(
    db: &Db,
    session_id: Id,
    user_id: Id,
    token_id: Id,
) -> Result<()> {
    let now = Timestamp::now();
    let mut tx = db.write_pool().begin().await?;

    sqlx::query(
        r#"
        UPDATE playback_sessions
        SET last_seen_at = ?1,
            status = ?2
        WHERE id = ?3
            AND status IN (?2, ?4)
        "#,
    )
    .bind(now)
    .bind(Status::Active.as_str())
    .bind(session_id)
    .bind(Status::Idle.as_str())
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        UPDATE playback_sessions
        SET status = ?1,
            ended_at = ?2
        WHERE user_id = ?3
            AND token_id = ?4
            AND id != ?5
            AND status = ?6
        "#,
    )
    .bind(Status::Ended.as_str())
    .bind(now)
    .bind(user_id)
    .bind(token_id)
    .bind(session_id)
    .bind(Status::Active.as_str())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

async fn heartbeat_session_at(db: &Db, session_id: Id, now: Timestamp) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE playback_sessions
        SET last_seen_at = ?1,
            status = ?2
        WHERE id = ?3
            AND status IN (?2, ?4)
        "#,
    )
    .bind(now)
    .bind(Status::Active.as_str())
    .bind(session_id)
    .bind(Status::Idle.as_str())
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn active_or_idle_session_id(
    db: &Db,
    user_id: Id,
    token_id: Id,
    media_item_id: Id,
    variant_id: &str,
) -> Result<Option<Id>> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT id
        FROM playback_sessions
        WHERE user_id = ?1
            AND token_id = ?2
            AND media_item_id = ?3
            AND variant_id = ?4
            AND status IN (?5, ?6)
        ORDER BY started_at DESC
        LIMIT 1
        "#,
    )
    .bind(user_id)
    .bind(token_id)
    .bind(media_item_id)
    .bind(variant_id)
    .bind(Status::Active.as_str())
    .bind(Status::Idle.as_str())
    .fetch_optional(db.read_pool())
    .await?)
}

async fn fetch_session(db: &Db, session_id: Id) -> Result<PlaybackSession> {
    let row = sqlx::query(
        r#"
        SELECT
            id,
            user_id,
            token_id,
            media_item_id,
            variant_id,
            started_at,
            last_seen_at,
            ended_at,
            status
        FROM playback_sessions
        WHERE id = ?1
        "#,
    )
    .bind(session_id)
    .fetch_one(db.read_pool())
    .await?;

    session_from_row(&row)
}

fn session_from_row(row: &SqliteRow) -> Result<PlaybackSession> {
    let status: String = row.try_get("status")?;
    let Some(status) = PlaybackSessionStatus::parse(&status) else {
        return Err(Error::UnknownStatus { value: status });
    };

    Ok(PlaybackSession {
        id: row.try_get("id")?,
        user_id: row.try_get("user_id")?,
        token_id: row.try_get("token_id")?,
        media_item_id: row.try_get("media_item_id")?,
        variant_id: row.try_get("variant_id")?,
        started_at: row.try_get("started_at")?,
        last_seen_at: row.try_get("last_seen_at")?,
        ended_at: row.try_get("ended_at")?,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kino_core::{Timestamp, device_token::DeviceToken, user::SEEDED_USER_ID};

    #[tokio::test]
    async fn open_session_without_prior_session_inserts_active_row()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let unrelated =
            insert_session(&db, Status::Active, "2026-05-11T01:00:00Z".parse()?, None).await?;
        let token_id = insert_device_token(&db).await?;
        let media_item_id = insert_personal_media_item(&db).await?;

        let session = open_session(
            &db,
            SEEDED_USER_ID,
            token_id,
            media_item_id,
            "source".to_owned(),
        )
        .await?;

        assert_eq!(session.user_id, SEEDED_USER_ID);
        assert_eq!(session.token_id, token_id);
        assert_eq!(session.media_item_id, media_item_id);
        assert_eq!(session.variant_id, "source");
        assert_eq!(session.status, Status::Active);
        assert_eq!(session.started_at, session.last_seen_at);
        assert_eq!(session.ended_at, None);
        let unrelated = fetch_session(&db, unrelated.id).await?;
        assert_eq!(unrelated.status, Status::Active);
        assert_eq!(unrelated.ended_at, None);
        assert_eq!(session_count(&db).await?, 2);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn open_session_replaces_prior_active_session_for_same_user_and_token()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let token_id = insert_device_token(&db).await?;
        let first_media_item_id = insert_personal_media_item(&db).await?;
        let second_media_item_id = insert_personal_media_item(&db).await?;
        let prior = open_session(
            &db,
            SEEDED_USER_ID,
            token_id,
            first_media_item_id,
            "first".to_owned(),
        )
        .await?;

        let replacement = open_session(
            &db,
            SEEDED_USER_ID,
            token_id,
            second_media_item_id,
            "second".to_owned(),
        )
        .await?;

        let prior = fetch_session(&db, prior.id).await?;
        assert_eq!(prior.status, Status::Ended);
        assert!(prior.ended_at.is_some());
        assert_eq!(replacement.status, Status::Active);
        assert_eq!(replacement.ended_at, None);
        assert_eq!(session_count(&db).await?, 2);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_session_updates_active_last_seen()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let session =
            insert_session(&db, Status::Active, "2026-05-11T01:00:00Z".parse()?, None).await?;
        let heartbeat_at: Timestamp = "2026-05-11T01:00:10Z".parse()?;

        heartbeat_session_at(&db, session.id, heartbeat_at).await?;

        let session = fetch_session(&db, session.id).await?;
        assert_eq!(session.status, Status::Active);
        assert_eq!(session.last_seen_at, heartbeat_at);
        assert_eq!(session.ended_at, None);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_session_revives_idle_session()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let session =
            insert_session(&db, Status::Idle, "2026-05-11T01:00:00Z".parse()?, None).await?;
        let heartbeat_at: Timestamp = "2026-05-11T01:00:10Z".parse()?;

        heartbeat_session_at(&db, session.id, heartbeat_at).await?;

        let session = fetch_session(&db, session.id).await?;
        assert_eq!(session.status, Status::Active);
        assert_eq!(session.last_seen_at, heartbeat_at);
        assert_eq!(session.ended_at, None);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_session_ignores_ended_session()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let ended_at: Timestamp = "2026-05-11T01:05:00Z".parse()?;
        let session = insert_session(
            &db,
            Status::Ended,
            "2026-05-11T01:00:00Z".parse()?,
            Some(ended_at),
        )
        .await?;
        let heartbeat_at: Timestamp = "2026-05-11T01:00:10Z".parse()?;

        heartbeat_session_at(&db, session.id, heartbeat_at).await?;

        let session = fetch_session(&db, session.id).await?;
        assert_eq!(session.status, Status::Ended);
        assert_eq!(session.last_seen_at, "2026-05-11T01:00:00Z".parse()?);
        assert_eq!(session.ended_at, Some(ended_at));

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn heartbeat_or_open_session_revives_idle_variant_and_ends_other_active_session()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let token_id = insert_device_token(&db).await?;
        let first_media_item_id = insert_personal_media_item(&db).await?;
        let second_media_item_id = insert_personal_media_item(&db).await?;
        let first = open_session(
            &db,
            SEEDED_USER_ID,
            token_id,
            first_media_item_id,
            "first".to_owned(),
        )
        .await?;
        sqlx::query("UPDATE playback_sessions SET status = ?1 WHERE id = ?2")
            .bind(Status::Idle.as_str())
            .bind(first.id)
            .execute(db.write_pool())
            .await?;
        let second = open_session(
            &db,
            SEEDED_USER_ID,
            token_id,
            second_media_item_id,
            "second".to_owned(),
        )
        .await?;

        let revived = heartbeat_or_open_session(
            &db,
            SEEDED_USER_ID,
            token_id,
            first_media_item_id,
            "first".to_owned(),
        )
        .await?;

        let second = fetch_session(&db, second.id).await?;
        assert_eq!(revived.id, first.id);
        assert_eq!(revived.status, Status::Active);
        assert_eq!(second.status, Status::Ended);
        assert!(second.ended_at.is_some());

        db.close().await;
        Ok(())
    }

    async fn insert_session(
        db: &Db,
        status: Status,
        last_seen_at: Timestamp,
        ended_at: Option<Timestamp>,
    ) -> std::result::Result<PlaybackSession, sqlx::Error> {
        let token_id = insert_device_token(db).await?;
        let media_item_id = insert_personal_media_item(db).await?;
        let mut session = PlaybackSession::active(
            Id::new(),
            SEEDED_USER_ID,
            token_id,
            media_item_id,
            Id::new().to_string(),
            last_seen_at,
        );
        session.status = status;
        session.ended_at = ended_at;

        sqlx::query(
            r#"
            INSERT INTO playback_sessions (
                id,
                user_id,
                token_id,
                media_item_id,
                variant_id,
                started_at,
                last_seen_at,
                ended_at,
                status
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(session.id)
        .bind(session.user_id)
        .bind(session.token_id)
        .bind(session.media_item_id)
        .bind(&session.variant_id)
        .bind(session.started_at)
        .bind(session.last_seen_at)
        .bind(session.ended_at)
        .bind(session.status.as_str())
        .execute(db.write_pool())
        .await?;

        Ok(session)
    }

    async fn insert_device_token(db: &Db) -> std::result::Result<Id, sqlx::Error> {
        let now = Timestamp::now();
        let token = DeviceToken::new(
            Id::new(),
            SEEDED_USER_ID,
            format!("test token {}", Id::new()),
            format!("hash-{}", Id::new()),
            now,
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

    async fn insert_personal_media_item(db: &Db) -> std::result::Result<Id, sqlx::Error> {
        let id = Id::new();
        let now = Timestamp::now();

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                created_at,
                updated_at
            )
            VALUES (?1, 'personal', NULL, ?2, ?3)
            "#,
        )
        .bind(id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(id)
    }

    async fn session_count(db: &Db) -> std::result::Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COUNT(*) FROM playback_sessions")
            .fetch_one(db.read_pool())
            .await
    }
}
