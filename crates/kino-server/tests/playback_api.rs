use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{Id, PlaybackProgress, PlaybackSession, Timestamp, user::SEEDED_USER_ID};
use serde::Deserialize;
use tower::util::ServiceExt;

mod common;

trait AuthRequestBuilder {
    fn bearer(self, token: &str) -> Self;
}

impl AuthRequestBuilder for axum::http::request::Builder {
    fn bearer(self, token: &str) -> Self {
        self.header(header::AUTHORIZATION, common::bearer(token))
    }
}

#[tokio::test]
async fn playback_progress_heartbeat_persists_max_position_from_same_device()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let (token, token_id) = common::issued_token_with_id(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let session_id = insert_active_session(&db, token_id, media_item_id).await?;
    let initial_last_seen_at: Timestamp =
        sqlx::query_scalar("SELECT last_seen_at FROM playback_sessions WHERE id = ?1")
            .bind(session_id)
            .fetch_one(db.read_pool())
            .await?;
    let app = kino_server::router(db.clone());

    assert_eq!(
        post_progress(&app, &token, media_item_id, 100).await?,
        StatusCode::NO_CONTENT
    );
    let row = playback_progress(&db, media_item_id).await?;
    assert_eq!(row.position_seconds, 100);
    assert_eq!(row.source_device_token_id, Some(token_id));

    let last_seen_at: Timestamp =
        sqlx::query_scalar("SELECT last_seen_at FROM playback_sessions WHERE id = ?1")
            .bind(session_id)
            .fetch_one(db.read_pool())
            .await?;
    assert!(last_seen_at > initial_last_seen_at);

    assert_eq!(
        post_progress(&app, &token, media_item_id, 50).await?,
        StatusCode::NO_CONTENT
    );
    let row_after_lower = playback_progress(&db, media_item_id).await?;
    assert_eq!(row_after_lower.position_seconds, 100);
    assert_eq!(row_after_lower.updated_at, row.updated_at);
    assert_eq!(row_after_lower.source_device_token_id, Some(token_id));

    assert_eq!(
        post_progress(&app, &token, media_item_id, 200).await?,
        StatusCode::NO_CONTENT
    );
    let row_after_higher = playback_progress(&db, media_item_id).await?;
    assert_eq!(row_after_higher.position_seconds, 200);
    assert!(row_after_higher.updated_at >= row_after_lower.updated_at);
    assert_eq!(row_after_higher.source_device_token_id, Some(token_id));

    Ok(())
}

#[tokio::test]
async fn playback_progress_heartbeat_persists_max_position_from_different_device()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let (first_token, first_token_id) = common::issued_token_with_id(&db).await?;
    let (second_token, second_token_id) = common::issued_token_with_id(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let app = kino_server::router(db.clone());

    assert_eq!(
        post_progress(&app, &first_token, media_item_id, 100).await?,
        StatusCode::NO_CONTENT
    );
    let row = playback_progress(&db, media_item_id).await?;
    assert_eq!(row.position_seconds, 100);
    assert_eq!(row.source_device_token_id, Some(first_token_id));

    assert_eq!(
        post_progress(&app, &second_token, media_item_id, 50).await?,
        StatusCode::NO_CONTENT
    );
    let row_after_lower = playback_progress(&db, media_item_id).await?;
    assert_eq!(row_after_lower.position_seconds, 100);
    assert_eq!(row_after_lower.updated_at, row.updated_at);
    assert_eq!(row_after_lower.source_device_token_id, Some(first_token_id));

    assert_eq!(
        post_progress(&app, &second_token, media_item_id, 200).await?,
        StatusCode::NO_CONTENT
    );
    let row_after_higher = playback_progress(&db, media_item_id).await?;
    assert_eq!(row_after_higher.position_seconds, 200);
    assert!(row_after_higher.updated_at >= row_after_lower.updated_at);
    assert_eq!(
        row_after_higher.source_device_token_id,
        Some(second_token_id)
    );

    Ok(())
}

#[tokio::test]
async fn playback_progress_heartbeat_rejects_negative_position()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let token = common::issued_token(&db).await?;
    let app = kino_server::router(db);

    let status = post_progress(&app, &token, Id::new(), -1).await?;

    assert_eq!(status, StatusCode::BAD_REQUEST);

    Ok(())
}

#[tokio::test]
async fn playback_progress_heartbeat_requires_auth() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/playback/progress")
                .header(header::CONTENT_TYPE, "application/json")
                .body(progress_body(Id::new(), 100))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn playback_progress_resume_returns_404_when_unwatched_without_progress()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let token = common::issued_token(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let app = kino_server::router(db);

    let response = get_progress(&app, &token, media_item_id).await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(body.is_empty());

    Ok(())
}

#[tokio::test]
async fn playback_progress_resume_returns_in_progress_position()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let token = common::issued_token(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let updated_at = insert_progress(&db, media_item_id, 180).await?;
    let app = kino_server::router(db);

    let response = get_progress(&app, &token, media_item_id).await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: PlaybackProgressResponse = response_json(response).await?;
    assert_eq!(body.position_seconds, 180);
    assert_eq!(body.updated_at, updated_at);
    assert!(!body.watched);

    Ok(())
}

#[tokio::test]
async fn playback_progress_resume_returns_watched_flag() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let token = common::issued_token(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let updated_at = insert_progress(&db, media_item_id, 240).await?;
    insert_watched(&db, media_item_id).await?;
    let app = kino_server::router(db);

    let response = get_progress(&app, &token, media_item_id).await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: PlaybackProgressResponse = response_json(response).await?;
    assert_eq!(body.position_seconds, 240);
    assert_eq!(body.updated_at, updated_at);
    assert!(body.watched);

    Ok(())
}

#[tokio::test]
async fn playback_progress_resume_requires_auth() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/playback/progress/{media_item_id}"))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

async fn post_progress(
    app: &axum::Router,
    auth_token: &str,
    media_item_id: Id,
    position_seconds: i64,
) -> Result<StatusCode, Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/playback/progress")
                .bearer(auth_token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(progress_body(media_item_id, position_seconds))?,
        )
        .await?;

    Ok(response.status())
}

async fn get_progress(
    app: &axum::Router,
    auth_token: &str,
    media_item_id: Id,
) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/playback/progress/{media_item_id}"))
                .bearer(auth_token)
                .body(Body::empty())?,
        )
        .await?;

    Ok(response)
}

fn progress_body(media_item_id: Id, position_seconds: i64) -> Body {
    Body::from(format!(
        r#"{{"media_item_id":"{media_item_id}","position_seconds":{position_seconds}}}"#
    ))
}

async fn insert_personal_media_item(db: &kino_db::Db) -> Result<Id, sqlx::Error> {
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

async fn insert_progress(
    db: &kino_db::Db,
    media_item_id: Id,
    position_seconds: i64,
) -> Result<Timestamp, Box<dyn std::error::Error>> {
    let updated_at = Timestamp::now();
    let progress = PlaybackProgress::new(
        SEEDED_USER_ID,
        media_item_id,
        position_seconds,
        updated_at,
        None,
    )?;

    sqlx::query(
        r#"
        INSERT INTO playback_progress (
            user_id,
            media_item_id,
            position_seconds,
            updated_at,
            source_device_token_id
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
    )
    .bind(progress.user_id)
    .bind(progress.media_item_id)
    .bind(progress.position_seconds)
    .bind(progress.updated_at)
    .bind(progress.source_device_token_id)
    .execute(db.write_pool())
    .await?;

    Ok(updated_at)
}

async fn insert_watched(db: &kino_db::Db, media_item_id: Id) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO watched (
            user_id,
            media_item_id,
            watched_at,
            source
        )
        VALUES (?1, ?2, ?3, 'manual')
        "#,
    )
    .bind(SEEDED_USER_ID)
    .bind(media_item_id)
    .bind(Timestamp::now())
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn insert_active_session(
    db: &kino_db::Db,
    token_id: Id,
    media_item_id: Id,
) -> Result<Id, Box<dyn std::error::Error>> {
    let started_at: Timestamp = "2026-01-01T00:00:00Z".parse()?;
    let session = PlaybackSession::active(
        Id::new(),
        SEEDED_USER_ID,
        token_id,
        media_item_id,
        Id::new().to_string(),
        started_at,
    );

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

    Ok(session.id)
}

async fn playback_progress(
    db: &kino_db::Db,
    media_item_id: Id,
) -> Result<PlaybackProgressRow, sqlx::Error> {
    let row: (i64, Timestamp, Option<Id>) = sqlx::query_as(
        r#"
        SELECT position_seconds, updated_at, source_device_token_id
        FROM playback_progress
        WHERE user_id = ?1 AND media_item_id = ?2
        "#,
    )
    .bind(SEEDED_USER_ID)
    .bind(media_item_id)
    .fetch_one(db.read_pool())
    .await?;

    Ok(PlaybackProgressRow {
        position_seconds: row.0,
        updated_at: row.1,
        source_device_token_id: row.2,
    })
}

struct PlaybackProgressRow {
    position_seconds: i64,
    updated_at: Timestamp,
    source_device_token_id: Option<Id>,
}

#[derive(Deserialize)]
struct PlaybackProgressResponse {
    position_seconds: i64,
    updated_at: Timestamp,
    watched: bool,
}

async fn response_json<T: serde::de::DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
