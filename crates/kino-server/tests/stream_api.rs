use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{Id, PlaybackSessionStatus, Timestamp};
use kino_library::{
    OcrSubtitleExtractionInput, OcrSubtitleTrack, ProbedSubtitleFormat, ProbedSubtitleTrack,
    SubtitleExtractionInput, SubtitleService, subtitle_ocr,
};
use m3u8_rs::{AlternativeMediaType, MediaPlaylistType, Playlist};
use serde_json::Value;
use std::time::Duration;
use tempfile::TempDir;
use tower::util::ServiceExt;
use webvtt_parser::Vtt;

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
async fn media_playlist_uses_source_file_byte_ranges() -> Result<(), Box<dyn std::error::Error>> {
    let file_size = 60 * 1024 * 1024;
    let fixture = media_playlist_fixture(60, file_size).await?;

    let response = fixture
        .app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/items/{}/{}/media.m3u8",
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
    let playlist_text = std::str::from_utf8(&body)?;
    assert!(playlist_text.contains("#EXT-X-ENDLIST"));
    assert!(!playlist_text.contains("#EXT-X-PLAYLIST-TYPE:EVENT"));

    let playlist = parse_media_playlist(playlist_text)?;
    assert_eq!(playlist.target_duration, 6);
    assert_eq!(playlist.playlist_type, Some(MediaPlaylistType::Vod));
    assert!(playlist.end_list);
    assert_eq!(playlist.segments.len(), 10);

    let mut offset = 0;
    let mut total_length = 0;
    for segment in &playlist.segments {
        assert_eq!(segment.duration, 6.0);
        assert_eq!(
            segment.uri,
            format!("/api/v1/stream/sourcefile/{}", fixture.source_file_id)
        );
        let Some(byte_range) = &segment.byte_range else {
            return Err("segment missing byte range".into());
        };
        assert_eq!(byte_range.offset, Some(offset));
        offset += byte_range.length;
        total_length += byte_range.length;
    }
    assert_eq!(total_length, file_size);

    Ok(())
}

#[tokio::test]
async fn media_playlist_requires_auth() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/items/{}/{}/media.m3u8",
                    Id::new(),
                    Id::new()
                ))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn media_playlist_returns_probe_data_missing_when_duration_is_null()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let source_path = temp_dir.path().join("source.mkv");
    std::fs::write(&source_path, b"source bytes")?;

    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let source_file_id = insert_source_file(&db, media_item_id, &source_path).await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/items/{media_item_id}/{source_file_id}/media.m3u8"
                ))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await?;
    let error: Value = serde_json::from_slice(&body)?;
    assert_eq!(error["error"], "probe_data_missing");
    assert_eq!(error["source_file_id"], source_file_id.to_string());

    Ok(())
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
async fn stream_request_opens_playback_session() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let response = fixture
        .app
        .clone()
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
    let session = active_session(&fixture.db).await?;
    assert_eq!(session.token_id, fixture.token_id);
    assert_eq!(session.media_item_id, fixture.media_item_id);
    assert_eq!(session.variant_id, fixture.source_file_id.to_string());
    assert_eq!(session.status, PlaybackSessionStatus::Active);
    assert_eq!(session.ended_at, None);

    Ok(())
}

#[tokio::test]
async fn second_stream_request_replaces_prior_active_session()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let source_response = fixture
        .app
        .clone()
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
    assert_eq!(source_response.status(), StatusCode::OK);
    let source_session = active_session(&fixture.db).await?;

    let transcode_response = fixture
        .app
        .clone()
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

    assert_eq!(transcode_response.status(), StatusCode::OK);
    let old_session = session_by_id(&fixture.db, source_session.id).await?;
    assert_eq!(old_session.status, PlaybackSessionStatus::Ended);
    assert!(old_session.ended_at.is_some());
    let active_session = active_session(&fixture.db).await?;
    assert_eq!(
        active_session.variant_id,
        fixture.transcode_output_id.to_string()
    );
    assert_eq!(active_session.status, PlaybackSessionStatus::Active);

    Ok(())
}

#[tokio::test]
async fn repeated_stream_request_heartbeats_existing_session()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let first_response = fixture
        .app
        .clone()
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
    assert_eq!(first_response.status(), StatusCode::OK);
    let first_session = active_session(&fixture.db).await?;

    let second_response = fixture
        .app
        .clone()
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

    assert_eq!(second_response.status(), StatusCode::PARTIAL_CONTENT);
    let second_session = active_session(&fixture.db).await?;
    assert_eq!(second_session.id, first_session.id);
    assert!(second_session.last_seen_at >= first_session.last_seen_at);
    assert_eq!(playback_session_count(&fixture.db).await?, 1);

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

#[tokio::test]
async fn subtitle_srt_sidecar_serves_webvtt() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;
    let track_id = seed_text_subtitle(
        &fixture,
        2,
        "eng",
        ProbedSubtitleFormat::Srt,
        "1\n00:00:01,000 --> 00:00:02,500\nHello, Kino\n",
    )
    .await?;

    let response = fetch_subtitle(
        &fixture,
        fixture.media_item_id,
        track_id,
        Some(&fixture.auth),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&header::HeaderValue::from_static("text/vtt"))
    );
    let body = response_text(response).await?;
    let parsed = parse_webvtt(&body)?;
    assert_eq!(parsed.cues.len(), 1);
    assert_eq!(parsed.cues[0].start.as_milliseconds(), 1_000);
    assert_eq!(parsed.cues[0].end.as_milliseconds(), 2_500);
    assert_eq!(parsed.cues[0].text, "Hello, Kino");

    Ok(())
}

#[tokio::test]
async fn subtitle_ass_sidecar_serves_webvtt() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;
    let track_id = seed_text_subtitle(
        &fixture,
        3,
        "spa",
        ProbedSubtitleFormat::Ass,
        "[Script Info]\nTitle: Kino\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:03.50,0:00:05.75,Default,,0,0,0,,{\\i1}Hola, Kino\n",
    )
    .await?;

    let response = fetch_subtitle(
        &fixture,
        fixture.media_item_id,
        track_id,
        Some(&fixture.auth),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await?;
    let parsed = parse_webvtt(&body)?;
    assert_eq!(parsed.cues.len(), 1);
    assert_eq!(parsed.cues[0].start.as_milliseconds(), 3_500);
    assert_eq!(parsed.cues[0].end.as_milliseconds(), 5_750);
    assert_eq!(parsed.cues[0].text, "Hola, Kino");

    Ok(())
}

#[tokio::test]
async fn subtitle_ocr_sidecar_serves_webvtt() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;
    let track_id = seed_ocr_subtitle(&fixture).await?;

    let response = fetch_subtitle(
        &fixture,
        fixture.media_item_id,
        track_id,
        Some(&fixture.auth),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response_text(response).await?;
    let parsed = parse_webvtt(&body)?;
    assert_eq!(parsed.cues.len(), 1);
    assert_eq!(parsed.cues[0].start.as_milliseconds(), 6_250);
    assert_eq!(parsed.cues[0].end.as_milliseconds(), 7_750);
    assert_eq!(parsed.cues[0].text, "OCR KINO");

    Ok(())
}

#[tokio::test]
async fn subtitle_unknown_track_returns_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;

    let response = fetch_subtitle(
        &fixture,
        fixture.media_item_id,
        Id::new(),
        Some(&fixture.auth),
    )
    .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn subtitle_track_requires_auth() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = stream_fixture().await?;
    let track_id = seed_ocr_subtitle(&fixture).await?;

    let response = fetch_subtitle(&fixture, fixture.media_item_id, track_id, None).await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

struct StreamFixture {
    app: axum::Router,
    db: kino_db::Db,
    auth: String,
    token_id: Id,
    media_item_id: Id,
    source_file_id: Id,
    transcode_output_id: Id,
    bytes: Vec<u8>,
    transcode_bytes: Vec<u8>,
    temp_dir: TempDir,
}

struct HlsFixture {
    app: axum::Router,
    auth: String,
    media_item_id: Id,
    source_file_id: Id,
}

struct MediaPlaylistFixture {
    app: axum::Router,
    auth: String,
    media_item_id: Id,
    source_file_id: Id,
    _temp_dir: TempDir,
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

async fn media_playlist_fixture(
    duration_seconds: i64,
    file_size: u64,
) -> Result<MediaPlaylistFixture, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let source_path = temp_dir.path().join("source.mkv");
    let source_file = std::fs::File::create(&source_path)?;
    source_file.set_len(file_size)?;

    let db = kino_db::test_db().await?;
    let (auth, _) = common::issued_token_with_id(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let source_file_id =
        insert_source_file_with_duration(&db, media_item_id, &source_path, Some(duration_seconds))
            .await?;
    let app = kino_server::router(db);

    Ok(MediaPlaylistFixture {
        app,
        auth,
        media_item_id,
        source_file_id,
        _temp_dir: temp_dir,
    })
}

fn parse_media_playlist(
    playlist_text: &str,
) -> Result<m3u8_rs::MediaPlaylist, Box<dyn std::error::Error>> {
    match m3u8_rs::parse_playlist_res(playlist_text.as_bytes()) {
        Ok(Playlist::MediaPlaylist(playlist)) => Ok(playlist),
        Ok(Playlist::MasterPlaylist(_)) => Err("expected media playlist, got master".into()),
        Err(error) => Err(format!("playlist parse failed: {error:?}").into()),
    }
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
    let (auth, token_id) = common::issued_token_with_id(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let source_file_id = insert_source_file(&db, media_item_id, &source_path).await?;
    let transcode_output_id = insert_transcode_output(&db, source_file_id, &transcode_path).await?;
    let app = kino_server::router(db.clone());

    Ok(StreamFixture {
        app,
        db,
        auth,
        token_id,
        media_item_id,
        source_file_id,
        transcode_output_id,
        bytes,
        transcode_bytes,
        temp_dir,
    })
}

async fn seed_text_subtitle(
    fixture: &StreamFixture,
    track_index: u32,
    language: &str,
    format: ProbedSubtitleFormat,
    text: &str,
) -> Result<Id, Box<dyn std::error::Error>> {
    let service = SubtitleService::new(fixture.db.clone());
    let result = service
        .extract_text_subtitles(SubtitleExtractionInput::new(
            fixture.media_item_id,
            fixture.temp_dir.path().join("sidecars"),
            vec![ProbedSubtitleTrack::new(
                track_index,
                language,
                format,
                text,
            )],
        ))
        .await?;

    let Some(sidecar) = result.sidecars.first() else {
        return Err("subtitle sidecar was not created".into());
    };
    Ok(sidecar.id)
}

async fn seed_ocr_subtitle(fixture: &StreamFixture) -> Result<Id, Box<dyn std::error::Error>> {
    let service = SubtitleService::new(fixture.db.clone());
    let result = service
        .extract_ocr_subtitles(OcrSubtitleExtractionInput::new(
            fixture.media_item_id,
            fixture.temp_dir.path().join("sidecars"),
            vec![OcrSubtitleTrack::new(
                4,
                "jpn",
                vec![subtitle_ocr::OcrCue {
                    start: Duration::from_millis(6_250),
                    end: Duration::from_millis(7_750),
                    text: String::from("OCR KINO"),
                    confidence: 94.5,
                }],
            )],
        ))
        .await?;

    let Some(sidecar) = result.sidecars.first() else {
        return Err("subtitle sidecar was not created".into());
    };
    Ok(sidecar.id)
}

async fn fetch_subtitle(
    fixture: &StreamFixture,
    media_item_id: Id,
    track_id: Id,
    auth: Option<&str>,
) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
    let mut request = HttpRequest::builder().method("GET").uri(format!(
        "/api/v1/stream/items/{media_item_id}/subtitles/{track_id}.vtt"
    ));
    if let Some(token) = auth {
        request = request.bearer(token);
    }

    Ok(fixture
        .app
        .clone()
        .oneshot(request.body(Body::empty())?)
        .await?)
}

async fn response_text(
    response: axum::response::Response,
) -> Result<String, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(std::str::from_utf8(&bytes)?.to_owned())
}

fn parse_webvtt(body: &str) -> Result<Vtt<'_>, Box<dyn std::error::Error>> {
    Vtt::parse(body).map_err(|error| format!("webvtt parse failed: {error:?}").into())
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

async fn active_session(
    db: &kino_db::Db,
) -> Result<PlaybackSessionRow, Box<dyn std::error::Error>> {
    let row = sqlx::query_as(
        r#"
        SELECT id, token_id, media_item_id, variant_id, last_seen_at, ended_at, status
        FROM playback_sessions
        WHERE status = 'active'
        "#,
    )
    .fetch_one(db.read_pool())
    .await?;

    playback_session_from_row(row)
}

async fn session_by_id(
    db: &kino_db::Db,
    id: Id,
) -> Result<PlaybackSessionRow, Box<dyn std::error::Error>> {
    let row = sqlx::query_as(
        r#"
        SELECT id, token_id, media_item_id, variant_id, last_seen_at, ended_at, status
        FROM playback_sessions
        WHERE id = ?1
        "#,
    )
    .bind(id)
    .fetch_one(db.read_pool())
    .await?;

    playback_session_from_row(row)
}

async fn playback_session_count(db: &kino_db::Db) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM playback_sessions")
        .fetch_one(db.read_pool())
        .await
}

fn playback_session_from_row(
    row: (Id, Id, Id, String, Timestamp, Option<Timestamp>, String),
) -> Result<PlaybackSessionRow, Box<dyn std::error::Error>> {
    let Some(status) = PlaybackSessionStatus::parse(&row.6) else {
        return Err(format!("unknown playback session status {}", row.6).into());
    };

    Ok(PlaybackSessionRow {
        id: row.0,
        token_id: row.1,
        media_item_id: row.2,
        variant_id: row.3,
        last_seen_at: row.4,
        ended_at: row.5,
        status,
    })
}

struct PlaybackSessionRow {
    id: Id,
    token_id: Id,
    media_item_id: Id,
    variant_id: String,
    last_seen_at: Timestamp,
    ended_at: Option<Timestamp>,
    status: PlaybackSessionStatus,
}

async fn insert_source_file(
    db: &kino_db::Db,
    media_item_id: Id,
    path: &std::path::Path,
) -> Result<Id, sqlx::Error> {
    insert_source_file_with_duration(db, media_item_id, path, None).await
}

async fn insert_source_file_with_duration(
    db: &kino_db::Db,
    media_item_id: Id,
    path: &std::path::Path,
    probe_duration_seconds: Option<i64>,
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
            probe_duration_seconds,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )
    .bind(id)
    .bind(media_item_id)
    .bind(path)
    .bind(probe_duration_seconds)
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
