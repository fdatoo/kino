use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{Id, PlaybackSession, PlaybackSessionStatus, Timestamp, user::SEEDED_USER_ID};
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
async fn admin_sessions_list_defaults_to_active_and_idle() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let (auth, token_id) = common::issued_token_with_id(&db).await?;
    let active = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Active,
        "2026-05-11T01:00:00Z".parse()?,
        None,
    )
    .await?;
    let idle = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Idle,
        "2026-05-11T01:01:00Z".parse()?,
        None,
    )
    .await?;
    let ended = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Ended,
        "2026-05-11T01:02:00Z".parse()?,
        Some("2026-05-11T01:03:00Z".parse()?),
    )
    .await?;
    let app = kino_server::router(db);

    let response = get_sessions(&app, &auth, None).await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Vec<SessionResponse> = response_json(response).await?;
    assert_session_ids(&body, &[active.id, idle.id]);
    assert!(!body.iter().any(|session| session.id == ended.id));

    Ok(())
}

#[tokio::test]
async fn admin_sessions_list_filters_each_status() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let (auth, token_id) = common::issued_token_with_id(&db).await?;
    let active = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Active,
        "2026-05-11T01:00:00Z".parse()?,
        None,
    )
    .await?;
    let idle = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Idle,
        "2026-05-11T01:01:00Z".parse()?,
        None,
    )
    .await?;
    let ended_at = "2026-05-11T01:03:00Z".parse()?;
    let ended = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Ended,
        "2026-05-11T01:02:00Z".parse()?,
        Some(ended_at),
    )
    .await?;
    let app = kino_server::router(db);

    let active_body: Vec<SessionResponse> =
        response_json(get_sessions(&app, &auth, Some("active")).await?).await?;
    let idle_body: Vec<SessionResponse> =
        response_json(get_sessions(&app, &auth, Some("idle")).await?).await?;
    let ended_body: Vec<SessionResponse> =
        response_json(get_sessions(&app, &auth, Some("ended")).await?).await?;

    assert_session_ids(&active_body, &[active.id]);
    assert_eq!(active_body[0].status, PlaybackSessionStatus::Active);
    assert_eq!(active_body[0].ended_at, None);

    assert_session_ids(&idle_body, &[idle.id]);
    assert_eq!(idle_body[0].status, PlaybackSessionStatus::Idle);
    assert_eq!(idle_body[0].ended_at, None);

    assert_session_ids(&ended_body, &[ended.id]);
    assert_eq!(ended_body[0].status, PlaybackSessionStatus::Ended);
    assert_eq!(ended_body[0].ended_at, Some(ended_at));
    assert_eq!(ended_body[0].user_id, SEEDED_USER_ID);
    assert_eq!(ended_body[0].token_id, token_id);
    assert_eq!(ended_body[0].media_item_id, ended.media_item_id);
    assert_eq!(ended_body[0].variant_id, ended.variant_id);
    assert_eq!(ended_body[0].started_at, ended.started_at);
    assert_eq!(ended_body[0].last_seen_at, ended.last_seen_at);

    Ok(())
}

#[tokio::test]
async fn admin_sessions_list_accepts_comma_separated_statuses()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let (auth, token_id) = common::issued_token_with_id(&db).await?;
    let active = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Active,
        "2026-05-11T01:00:00Z".parse()?,
        None,
    )
    .await?;
    let idle = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Idle,
        "2026-05-11T01:01:00Z".parse()?,
        None,
    )
    .await?;
    let ended = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Ended,
        "2026-05-11T01:02:00Z".parse()?,
        Some("2026-05-11T01:03:00Z".parse()?),
    )
    .await?;
    let app = kino_server::router(db);

    let body: Vec<SessionResponse> =
        response_json(get_sessions(&app, &auth, Some("active,idle")).await?).await?;

    assert_session_ids(&body, &[active.id, idle.id]);
    assert!(!body.iter().any(|session| session.id == ended.id));

    Ok(())
}

#[tokio::test]
async fn admin_sessions_list_includes_known_progress_position()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let (auth, token_id) = common::issued_token_with_id(&db).await?;
    let session = insert_session(
        &db,
        token_id,
        PlaybackSessionStatus::Active,
        "2026-05-11T01:00:00Z".parse()?,
        None,
    )
    .await?;
    insert_progress(&db, session.media_item_id, token_id, 372).await?;
    let app = kino_server::router(db);

    let body: Vec<SessionResponse> =
        response_json(get_sessions(&app, &auth, Some("active")).await?).await?;

    assert_session_ids(&body, &[session.id]);
    assert_eq!(body[0].position_seconds, Some(372));

    Ok(())
}

#[tokio::test]
async fn admin_sessions_list_requires_auth() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let _auth = common::issued_token(&db).await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/admin/sessions")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn admin_sessions_list_rejects_unknown_status() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db);

    let response = get_sessions(&app, &auth, Some("paused")).await?;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResponse = response_json(response).await?;
    assert_eq!(body.error, "invalid playback session status filter: paused");

    Ok(())
}

async fn get_sessions(
    app: &axum::Router,
    auth_token: &str,
    status: Option<&str>,
) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
    let uri = match status {
        Some(status) => format!("/api/v1/admin/sessions?status={status}"),
        None => "/api/v1/admin/sessions".to_owned(),
    };

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(uri)
                .bearer(auth_token)
                .body(Body::empty())?,
        )
        .await?;

    Ok(response)
}

async fn insert_session(
    db: &kino_db::Db,
    token_id: Id,
    status: PlaybackSessionStatus,
    last_seen_at: Timestamp,
    ended_at: Option<Timestamp>,
) -> Result<PlaybackSession, Box<dyn std::error::Error>> {
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
    token_id: Id,
    position_seconds: i64,
) -> Result<(), sqlx::Error> {
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
    .bind(SEEDED_USER_ID)
    .bind(media_item_id)
    .bind(position_seconds)
    .bind(Timestamp::now())
    .bind(token_id)
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn response_json<T: serde::de::DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn assert_session_ids(sessions: &[SessionResponse], expected: &[Id]) {
    let mut actual = sessions
        .iter()
        .map(|session| session.id)
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = expected.to_vec();
    expected.sort();
    assert_eq!(actual, expected);
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    id: Id,
    user_id: Id,
    token_id: Id,
    media_item_id: Id,
    variant_id: String,
    position_seconds: Option<i64>,
    status: PlaybackSessionStatus,
    started_at: Timestamp,
    last_seen_at: Timestamp,
    ended_at: Option<Timestamp>,
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: String,
}
