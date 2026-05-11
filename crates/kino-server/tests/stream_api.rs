use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{Id, Timestamp};
use m3u8_rs::AlternativeMediaType;
use tempfile::TempDir;
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
async fn stream_source_file_serves_full_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/sourcefile/{}",
                    fixture.source_file_id
                ))
                .bearer(&fixture.auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::ACCEPT_RANGES),
        Some(&header::HeaderValue::from_static("bytes"))
    );
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static("video/x-matroska"))
    );
    assert!(response.headers().get(header::ETAG).is_some());
    assert!(response.headers().get(header::CONTENT_RANGE).is_none());

    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), fixture.bytes.as_slice());

    Ok(())
}

#[tokio::test]
async fn stream_source_file_serves_single_range() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/sourcefile/{}",
                    fixture.source_file_id
                ))
                .bearer(&fixture.auth)
                .header(header::RANGE, "bytes=0-99")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers().get(header::CONTENT_RANGE),
        Some(&header::HeaderValue::from_static("bytes 0-99/512"))
    );
    assert_eq!(
        response.headers().get(header::CONTENT_LENGTH),
        Some(&header::HeaderValue::from_static("100"))
    );

    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), &fixture.bytes[..100]);

    Ok(())
}

#[tokio::test]
async fn stream_transcode_output_serves_full_file() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/transcode/{}",
                    fixture.transcode_output_id
                ))
                .bearer(&fixture.auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static("video/mp4"))
    );

    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), fixture.transcode_bytes.as_slice());

    Ok(())
}

#[tokio::test]
async fn stream_master_playlist_describes_source_variant() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = hls_fixture(false).await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/items/{}/{}/master.m3u8",
                    fixture.media_item_id, fixture.source_file_id
                ))
                .bearer(&fixture.auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static(
            "application/vnd.apple.mpegurl"
        ))
    );

    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let playlist_text = String::from_utf8(body.to_vec())?;
    assert!(playlist_text.starts_with("#EXTM3U\n#EXT-X-VERSION:7\n"));
    assert!(playlist_text.contains("#EXT-X-MEDIA:TYPE=AUDIO"));
    assert!(playlist_text.contains("GROUP-ID=\"audio\""));
    assert!(playlist_text.contains("LANGUAGE=\"eng\""));
    assert!(playlist_text.contains(&format!(
        "URI=\"/api/v1/stream/items/{}/{}/media.m3u8?audio=1\"",
        fixture.media_item_id, fixture.source_file_id
    )));
    assert!(playlist_text.contains("#EXT-X-MEDIA:TYPE=SUBTITLES"));
    assert!(playlist_text.contains("FORCED=NO"));
    assert!(playlist_text.contains(&format!(
        "URI=\"/api/v1/stream/items/{}/subtitles/2.m3u8\"",
        fixture.media_item_id
    )));
    assert!(playlist_text.contains("#EXT-X-STREAM-INF:"));
    assert!(playlist_text.contains("CODECS=\"avc1,fLaC\""));
    assert!(playlist_text.contains("RESOLUTION=16x16"));
    assert!(playlist_text.ends_with(&format!(
        "/api/v1/stream/items/{}/{}/media.m3u8\n",
        fixture.media_item_id, fixture.source_file_id
    )));

    let parsed = match m3u8_rs::parse_master_playlist_res(playlist_text.as_bytes()) {
        Ok(playlist) => playlist,
        Err(error) => panic!("master playlist did not parse: {error:?}\n{playlist_text}"),
    };
    assert_eq!(parsed.version, Some(7));
    assert_eq!(parsed.variants.len(), 1);
    assert_eq!(parsed.variants[0].audio.as_deref(), Some("audio"));
    assert_eq!(parsed.variants[0].subtitles.as_deref(), Some("subs"));
    assert_eq!(
        parsed.variants[0].uri,
        format!(
            "/api/v1/stream/items/{}/{}/media.m3u8",
            fixture.media_item_id, fixture.source_file_id
        )
    );
    let audio = parsed
        .alternatives
        .iter()
        .find(|media| media.media_type == AlternativeMediaType::Audio)
        .expect("missing audio rendition");
    assert_eq!(audio.group_id, "audio");
    assert_eq!(audio.language.as_deref(), Some("eng"));
    let subtitles = parsed
        .alternatives
        .iter()
        .find(|media| media.media_type == AlternativeMediaType::Subtitles)
        .expect("missing subtitle rendition");
    assert_eq!(subtitles.group_id, "subs");
    assert_eq!(subtitles.language.as_deref(), Some("eng"));
    assert!(!subtitles.forced);

    Ok(())
}

#[tokio::test]
async fn stream_master_playlist_preserves_forced_subtitle_flag()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = hls_fixture(true).await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/items/{}/{}/master.m3u8",
                    fixture.media_item_id, fixture.source_file_id
                ))
                .bearer(&fixture.auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let playlist_text = String::from_utf8(body.to_vec())?;
    assert!(playlist_text.contains("FORCED=YES"));

    let parsed = match m3u8_rs::parse_master_playlist_res(playlist_text.as_bytes()) {
        Ok(playlist) => playlist,
        Err(error) => panic!("master playlist did not parse: {error:?}\n{playlist_text}"),
    };
    let subtitles = parsed
        .alternatives
        .iter()
        .find(|media| media.media_type == AlternativeMediaType::Subtitles)
        .expect("missing subtitle rendition");
    assert!(subtitles.forced);

    Ok(())
}

#[tokio::test]
async fn stream_source_file_serves_suffix_range() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/sourcefile/{}",
                    fixture.source_file_id
                ))
                .bearer(&fixture.auth)
                .header(header::RANGE, "bytes=-100")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers().get(header::CONTENT_RANGE),
        Some(&header::HeaderValue::from_static("bytes 412-511/512"))
    );

    let body = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(body.as_ref(), &fixture.bytes[412..]);

    Ok(())
}

#[tokio::test]
async fn stream_source_file_rejects_out_of_bounds_range() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = stream_fixture().await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/sourcefile/{}",
                    fixture.source_file_id
                ))
                .bearer(&fixture.auth)
                .header(header::RANGE, "bytes=99999999-")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        response.headers().get(header::CONTENT_RANGE),
        Some(&header::HeaderValue::from_static("bytes */512"))
    );

    Ok(())
}

#[tokio::test]
async fn stream_source_file_requires_auth() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/stream/sourcefile/{}", Id::new()))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

struct StreamFixture {
    app: axum::Router,
    auth: String,
    source_file_id: Id,
    transcode_output_id: Id,
    bytes: Vec<u8>,
    transcode_bytes: Vec<u8>,
    _temp_dir: TempDir,
}

struct HlsFixture {
    app: axum::Router,
    auth: String,
    media_item_id: Id,
    source_file_id: Id,
}

async fn hls_fixture(forced_subtitle: bool) -> Result<HlsFixture, Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let source_path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../kino-fulfillment/tests/fixtures/probe_sample.mkv");
    let source_file_id = insert_source_file(&db, media_item_id, &source_path).await?;
    insert_subtitle_sidecar(&db, media_item_id, "eng", 2, forced_subtitle).await?;
    let app = kino_server::router(db);

    Ok(HlsFixture {
        app,
        auth,
        media_item_id,
        source_file_id,
    })
}

async fn stream_fixture() -> Result<StreamFixture, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let source_path = temp_dir.path().join("source.mkv");
    let transcode_path = temp_dir.path().join("stream.mp4");
    let bytes = source_bytes();
    let transcode_bytes = b"transcoded bytes".to_vec();
    std::fs::write(&source_path, &bytes)?;
    std::fs::write(&transcode_path, &transcode_bytes)?;

    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let source_file_id = insert_source_file(&db, media_item_id, &source_path).await?;
    let transcode_output_id = insert_transcode_output(&db, source_file_id, &transcode_path).await?;
    let app = kino_server::router(db);

    Ok(StreamFixture {
        app,
        auth,
        source_file_id,
        transcode_output_id,
        bytes,
        transcode_bytes,
        _temp_dir: temp_dir,
    })
}

fn source_bytes() -> Vec<u8> {
    (0..512).map(|index| (index % 251) as u8).collect()
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

async fn insert_source_file(
    db: &kino_db::Db,
    media_item_id: Id,
    path: &std::path::Path,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();
    let path = path.to_string_lossy().into_owned();

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
    .bind(id)
    .bind(media_item_id)
    .bind(path)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(id)
}

async fn insert_transcode_output(
    db: &kino_db::Db,
    source_file_id: Id,
    path: &std::path::Path,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();
    let path = path.to_string_lossy().into_owned();

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

async fn insert_subtitle_sidecar(
    db: &kino_db::Db,
    media_item_id: Id,
    language: &str,
    track_index: u32,
    forced: bool,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();
    let path = format!("/subtitles/{id}.srt");

    sqlx::query(
        r#"
        INSERT INTO subtitle_sidecars (
            id,
            media_item_id,
            language,
            format,
            provenance,
            track_index,
            path,
            forced,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, 'srt', 'text', ?4, ?5, ?6, ?7, ?8)
        "#,
    )
    .bind(id)
    .bind(media_item_id)
    .bind(language)
    .bind(i64::from(track_index))
    .bind(path)
    .bind(if forced { 1_i64 } else { 0_i64 })
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(id)
}
