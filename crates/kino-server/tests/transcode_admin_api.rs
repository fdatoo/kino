use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{Id, Timestamp};
use serde_json::Value;
use tower::util::ServiceExt;

mod common;

#[tokio::test]
async fn transcode_admin_lists_filters_and_shows_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
    let job_id = insert_transcode_job(&db, source_file_id, "planned", "cpu", 7).await?;
    let output_id = insert_transcode_output(&db, source_file_id, "/library/out/media.m3u8").await?;
    insert_downgrade(&db, output_id, "hdr10_to_sdr").await?;
    let app = kino_server::router(db);

    let list_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/admin/transcodes/jobs?state=planned&lane=cpu&source_file_id={source_file_id}"
                ))
                .header(header::AUTHORIZATION, common::bearer(&auth))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(list_response.status(), StatusCode::OK);
    let list: Value = response_json(list_response).await?;
    assert_eq!(list.as_array().map(Vec::len), Some(1));
    assert_eq!(
        list.pointer("/0/id"),
        Some(&Value::String(job_id.to_string()))
    );
    assert_eq!(list.pointer("/0/attempts"), Some(&Value::from(0)));

    let detail_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/admin/transcodes/jobs/{job_id}"))
                .header(header::AUTHORIZATION, common::bearer(&auth))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(detail_response.status(), StatusCode::OK);
    let detail: Value = response_json(detail_response).await?;
    assert_eq!(
        detail.pointer("/downgrades/0/transcode_output_id"),
        Some(&Value::String(output_id.to_string()))
    );
    assert_eq!(
        detail.pointer("/downgrades/0/kind"),
        Some(&Value::String("hdr10_to_sdr".to_owned()))
    );

    Ok(())
}

#[tokio::test]
async fn transcode_admin_cancels_planned_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
    let job_id = insert_transcode_job(&db, source_file_id, "planned", "cpu", 8).await?;
    let app = kino_server::router(db.clone());

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/admin/transcodes/jobs/{job_id}/cancel"))
                .header(header::AUTHORIZATION, common::bearer(&auth))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let state: String = sqlx::query_scalar("SELECT state FROM transcode_jobs WHERE id = ?1")
        .bind(job_id)
        .fetch_one(db.read_pool())
        .await?;
    assert_eq!(state, "cancelled");

    Ok(())
}

#[tokio::test]
async fn transcode_admin_reports_cache_stats_and_purges_ephemeral()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let temp = tempfile::tempdir()?;
    let durable_dir = temp.path().join("durable");
    let ephemeral_dir = temp.path().join("ephemeral");
    tokio::fs::create_dir_all(&durable_dir).await?;
    tokio::fs::create_dir_all(&ephemeral_dir).await?;
    tokio::fs::write(durable_dir.join("segment.m4s"), [0_u8; 11]).await?;
    tokio::fs::write(ephemeral_dir.join("segment.m4s"), [0_u8; 13]).await?;
    let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
    insert_transcode_output_with_dir(&db, source_file_id, "/library/out/media.m3u8", &durable_dir)
        .await?;
    insert_ephemeral_transcode(&db, source_file_id, &ephemeral_dir, 13).await?;
    let app = kino_server::router(db.clone());

    let stats_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/admin/transcodes/cache")
                .header(header::AUTHORIZATION, common::bearer(&auth))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(stats_response.status(), StatusCode::OK);
    let stats: Value = response_json(stats_response).await?;
    assert_eq!(stats.pointer("/durable/row_count"), Some(&Value::from(1)));
    assert_eq!(
        stats.pointer("/durable/total_bytes"),
        Some(&Value::from(11))
    );
    assert_eq!(stats.pointer("/ephemeral/row_count"), Some(&Value::from(1)));
    assert_eq!(
        stats.pointer("/ephemeral/total_bytes"),
        Some(&Value::from(13))
    );

    let purge_response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/admin/transcodes/cache/purge")
                .header(header::AUTHORIZATION, common::bearer(&auth))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(purge_response.status(), StatusCode::OK);
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM ephemeral_transcodes")
        .fetch_one(db.read_pool())
        .await?;
    assert_eq!(rows, 0);
    assert!(!ephemeral_dir.exists());

    Ok(())
}

async fn response_json<T: serde::de::DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn insert_source_file(db: &kino_db::Db, path: &str) -> Result<Id, sqlx::Error> {
    let media_item_id = Id::new();
    let source_file_id = Id::new();
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
    .bind(media_item_id)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    sqlx::query(
        r#"
        INSERT INTO source_files (
            id,
            media_item_id,
            path,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
    )
    .bind(source_file_id)
    .bind(media_item_id)
    .bind(path)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(source_file_id)
}

async fn insert_transcode_job(
    db: &kino_db::Db,
    source_file_id: Id,
    state: &str,
    lane: &str,
    seed: u8,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();
    sqlx::query(
        r#"
        INSERT INTO transcode_jobs (
            id,
            source_file_id,
            profile_json,
            profile_hash,
            state,
            lane,
            attempt,
            progress_pct,
            last_error,
            next_attempt_at,
            created_at,
            updated_at,
            started_at,
            completed_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, NULL, NULL, NULL, ?7, ?8, NULL, NULL)
        "#,
    )
    .bind(id)
    .bind(source_file_id)
    .bind(format!(r#"{{"seed":{seed}}}"#))
    .bind([seed; 32].as_slice())
    .bind(state)
    .bind(lane)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(id)
}

async fn insert_transcode_output(
    db: &kino_db::Db,
    source_file_id: Id,
    path: &str,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();
    sqlx::query(
        r#"
        INSERT INTO transcode_outputs (
            id,
            source_file_id,
            path,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
    )
    .bind(id)
    .bind(source_file_id)
    .bind(path)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(id)
}

async fn insert_transcode_output_with_dir(
    db: &kino_db::Db,
    source_file_id: Id,
    path: &str,
    directory_path: &std::path::Path,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();
    sqlx::query(
        r#"
        INSERT INTO transcode_outputs (
            id,
            source_file_id,
            path,
            created_at,
            updated_at,
            directory_path,
            playlist_filename,
            init_filename,
            encode_metadata_json
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'media.m3u8', 'init.mp4', '{}')
        "#,
    )
    .bind(id)
    .bind(source_file_id)
    .bind(path)
    .bind(now)
    .bind(now)
    .bind(directory_path.display().to_string())
    .execute(db.write_pool())
    .await?;

    Ok(id)
}

async fn insert_downgrade(db: &kino_db::Db, output_id: Id, kind: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO transcode_color_downgrades (
            transcode_output_id,
            kind,
            note,
            created_at
        )
        VALUES (?1, ?2, NULL, ?3)
        "#,
    )
    .bind(output_id)
    .bind(kind)
    .bind(Timestamp::now())
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn insert_ephemeral_transcode(
    db: &kino_db::Db,
    source_file_id: Id,
    directory_path: &std::path::Path,
    size_bytes: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = Timestamp::now();
    sqlx::query(
        r#"
        INSERT INTO ephemeral_transcodes (
            id,
            source_file_id,
            profile_hash,
            profile_json,
            directory_path,
            size_bytes,
            created_at,
            last_access_at
        )
        VALUES (?1, ?2, ?3, '{}', ?4, ?5, ?6, ?7)
        "#,
    )
    .bind(Id::new())
    .bind(source_file_id)
    .bind([1_u8; 32].as_slice())
    .bind(directory_path.display().to_string())
    .bind(i64::try_from(size_bytes)?)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(())
}
