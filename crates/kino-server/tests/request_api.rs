use std::{future::Future, num::NonZeroU32, path::Path, pin::Pin, sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{HeaderValue, Request as HttpRequest, StatusCode, header},
};
use kino_core::{
    CanonicalIdentityId, Id, RequestFailureReason, Timestamp, TmdbId, user::SEEDED_USER_ID,
};
use kino_fulfillment::{
    FulfillmentPlanDecision, NewRequest, RequestDetail, RequestIdentityProvenance, RequestListPage,
    RequestService, RequestState, RequestTransition,
    tmdb::{TmdbClient, TmdbClientConfig},
};
use kino_library::{
    CatalogService, ImageSubtitleExtraction, ImageSubtitleExtractionInput, ImageSubtitleFrame,
    OcrSubtitleExtractionInput, OcrSubtitleTrack, ProbeSubtitleKind, SubtitleReocrService,
    SubtitleService, TranscodeCapabilities,
    subtitle_ocr::{OcrEngine, OcrFrameResult},
};
use serde::de::DeserializeOwned;
use serde_json::Value;
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

struct FakeSubtitleExtractor {
    frames: Vec<ImageSubtitleFrame>,
}

impl ImageSubtitleExtraction for FakeSubtitleExtractor {
    fn extract_image_subtitle_track<'a>(
        &'a self,
        input: ImageSubtitleExtractionInput,
    ) -> Pin<Box<dyn Future<Output = kino_library::Result<Vec<ImageSubtitleFrame>>> + Send + 'a>>
    {
        Box::pin(async move {
            assert_eq!(input.stream_index, 4);
            assert_eq!(input.kind, ProbeSubtitleKind::ImagePgs);
            Ok(self.frames.clone())
        })
    }
}

struct FakeOcrEngine;

impl OcrEngine for FakeOcrEngine {
    fn ocr(&self, _image_path: &Path) -> kino_library::Result<OcrFrameResult> {
        Ok(OcrFrameResult {
            text: String::from("UPDATED OCR"),
            avg_confidence: 94.0,
        })
    }
}

struct TmdbTestServer {
    base_url: String,
    image_base_url: String,
    requests: Arc<tokio::sync::Mutex<Vec<String>>>,
}

impl TmdbTestServer {
    async fn new(responses: Vec<TmdbTestResponse>) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let requests = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let request_log = Arc::clone(&requests);

        tokio::spawn(async move {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = vec![0_u8; 4096];
                let Ok(bytes_read) = tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await
                else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..bytes_read]);
                if let Some(line) = request.lines().next()
                    && let Some(target) = line.split_whitespace().nth(1)
                {
                    request_log.lock().await.push(target.to_owned());
                }
                let body = response.body.as_bytes();
                let header = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    response.status.as_u16(),
                    body.len()
                );
                if tokio::io::AsyncWriteExt::write_all(&mut stream, header.as_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
                if tokio::io::AsyncWriteExt::write_all(&mut stream, body)
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });

        Ok(Self {
            base_url: format!("http://{address}/3/"),
            image_base_url: format!("http://{address}/images/"),
            requests,
        })
    }

    fn client(&self) -> Result<TmdbClient, Box<dyn std::error::Error>> {
        let config = TmdbClientConfig::new("test-api-key")?
            .with_base_url(&self.base_url)?
            .with_image_base_url(&self.image_base_url)?
            .with_max_requests_per_second(
                NonZeroU32::new(50).ok_or("test TMDB request rate must be positive")?,
            );
        Ok(TmdbClient::new(config))
    }

    async fn requests(&self) -> Vec<String> {
        self.requests.lock().await.clone()
    }
}

struct TmdbTestResponse {
    status: StatusCode,
    body: String,
}

impl TmdbTestResponse {
    fn json(status: StatusCode, body: &str) -> Self {
        Self {
            status,
            body: body.to_owned(),
        }
    }
}

#[tokio::test]
async fn openapi_json_serves_valid_spec() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router_with_public_base_url(db, "https://kino.example.test");

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/openapi.json")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .ok_or("content-type header missing")?
        .to_str()?;
    assert!(
        content_type.starts_with("application/json"),
        "got: {content_type}"
    );

    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(body["openapi"], "3.1.0");
    assert_eq!(body["info"]["title"], "Kino API");
    assert_eq!(body["info"]["version"], "0.1.0-phase-2");
    assert_eq!(body["servers"][0]["url"], "https://kino.example.test");
    assert!(body["paths"].get("/api/v1/library/items").is_some());
    assert!(body["paths"].get("/api/v1/requests/{id}/cancel").is_some());
    let catalog_params = body["paths"]["/api/v1/library/items"]["get"]["parameters"]
        .as_array()
        .ok_or("catalog list parameters should be an array")?;
    for param in ["kind", "year", "watched", "sort", "cursor"] {
        assert!(
            catalog_params
                .iter()
                .any(|candidate| candidate["name"] == param),
            "missing catalog list query parameter: {param}"
        );
    }
    assert!(body["paths"].get("/api/v1/admin/config").is_some());
    assert!(
        body["paths"]
            .get("/api/v1/admin/library/items/{id}")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/stream/sourcefile/{id}/file.{ext}")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/stream/items/{id}/subtitles/{track}.vtt")
            .is_some()
    );
    assert!(body["paths"].get("/api/v1/stream/transcode/{id}").is_some());
    assert!(
        body["paths"]
            .get("/api/v1/stream/items/{id}/master.m3u8")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/stream/transcodes/{transcode_output_id}/media.m3u8")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/stream/transcodes/{transcode_output_id}/init.mp4")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/stream/transcodes/{transcode_output_id}/seg-{segment}.m4s")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/stream/items/{id}/{variant_id}/master.m3u8")
            .is_none()
    );
    assert!(body["paths"].get("/api/v1/library/items/{id}").is_some());
    assert!(
        body["paths"]
            .get("/api/v1/library/items/{id}/images/{kind}")
            .is_some()
    );
    assert!(body["paths"].get("/api/v1/admin/tokens").is_some());
    assert!(body["paths"].get("/api/v1/admin/sessions").is_some());
    assert!(body["paths"].get("/api/v1/playback/progress").is_some());
    assert!(
        body["paths"]
            .get("/api/v1/playback/watched/{media_item_id}")
            .is_some()
    );
    assert!(
        body["paths"]["/api/v1/admin/tokens/{token_id}"]
            .get("delete")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/admin/items/{id}/subtitles/{track}/re-ocr")
            .is_some()
    );
    assert!(
        body["paths"]
            .get("/api/v1/admin/source-files/{id}/reprobe")
            .is_some()
    );

    let json = std::str::from_utf8(&bytes)?;
    let spec = oas3::from_json(json)?;
    spec.validate_version()?;

    Ok(())
}

#[tokio::test]
async fn request_api_exercises_happy_path_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db.clone());

    let create_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/requests")
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"target":"Inception (2010)","message":"requested from curl"}"#,
                ))?,
        )
        .await?;
    assert_eq!(create_response.status(), StatusCode::CREATED);
    let created: RequestDetail = response_json(create_response).await?;
    assert_eq!(created.request.target.raw_query, "Inception (2010)");
    assert_eq!(created.request.state, RequestState::Pending);
    assert_eq!(created.status_events.len(), 1);
    assert_eq!(
        created.status_events[0].message.as_deref(),
        Some("requested from curl")
    );

    let get_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/requests/{}", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    let fetched: RequestDetail = response_json(get_response).await?;
    assert_eq!(fetched.request.id, created.request.id);
    assert_eq!(fetched.status_events.len(), 1);

    let list_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/requests")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(list_response.status(), StatusCode::OK);
    let listed: RequestListPage = response_json(list_response).await?;
    assert_eq!(listed.requests.len(), 1);
    assert_eq!(listed.requests[0].id, created.request.id);
    assert_eq!(listed.requests[0].state, RequestState::Pending);
    assert_eq!(listed.next_offset, None);

    let delete_response = app
        .oneshot(
            HttpRequest::builder()
                .method("DELETE")
                .uri(format!("/api/v1/requests/{}", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(delete_response.status(), StatusCode::OK);
    let cancelled: RequestDetail = response_json(delete_response).await?;
    assert_eq!(cancelled.request.id, created.request.id);
    assert_eq!(cancelled.request.state, RequestState::Cancelled);
    assert_eq!(cancelled.status_events.len(), 2);
    assert_eq!(
        cancelled.status_events[1].from_state,
        Some(RequestState::Pending)
    );
    assert_eq!(cancelled.status_events[1].to_state, RequestState::Cancelled);

    Ok(())
}

#[tokio::test]
async fn list_request_api_accepts_filter_and_pagination() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db.clone());

    let first = create_request(&app, &auth, "first").await?;
    let second = create_request(&app, &auth, "second").await?;
    create_request(&app, &auth, "third").await?;

    let first_page_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/requests?state=pending&limit=2&offset=0")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(first_page_response.status(), StatusCode::OK);
    let first_page: RequestListPage = response_json(first_page_response).await?;
    assert_eq!(
        first_page
            .requests
            .iter()
            .map(|request| request.id)
            .collect::<Vec<_>>(),
        vec![first.request.id, second.request.id]
    );
    assert_eq!(first_page.next_offset, Some(2));

    let bad_limit_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/requests?limit=0")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(bad_limit_response.status(), StatusCode::BAD_REQUEST);

    Ok(())
}

#[tokio::test]
async fn request_resolve_api_fetches_and_scores_tmdb_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let tmdb = TmdbTestServer::new(vec![
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, r#"{"results":[]}"#),
    ])
    .await?;
    let app = kino_server::router_with_tmdb_client(db, tmdb.client()?);
    let created = create_request(&app, &auth, "Inception (2010)").await?;
    let winner_id = identity(27_205);

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/resolve", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response_json(response).await?;
    assert_eq!(
        body["candidates"][0]["canonical_identity_id"],
        "tmdb:movie:27205"
    );
    assert_eq!(body["candidates"][0]["title"], "Inception");
    assert_eq!(body["candidates"][0]["year"], 2010);
    assert_eq!(body["candidates"][0]["score"], 1.0);

    let resolved_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/requests/{}", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resolved_response.status(), StatusCode::OK);
    let resolved: RequestDetail = response_json(resolved_response).await?;
    assert_eq!(
        resolved.request.target.canonical_identity_id,
        Some(winner_id)
    );
    assert_eq!(resolved.request.state, RequestState::Resolved);

    let requests = tmdb.requests().await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("/3/search/movie?"));
    assert!(requests[1].starts_with("/3/search/tv?"));

    Ok(())
}

#[tokio::test]
async fn request_match_api_returns_not_found() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db.clone());
    let created = create_request(&app, &auth, "Dune").await?;

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/matches", created.request.id))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"candidates":[]}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn re_resolution_api_records_versioned_identity_history()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db.clone());
    let first_identity = identity(550);
    let second_identity = identity(551);
    let created = service
        .create(NewRequest::anonymous("Inception (2010)"))
        .await?;
    service
        .resolve_identity(
            created.request.id,
            first_identity,
            RequestIdentityProvenance::Manual,
            None,
            Some("initial choice"),
        )
        .await?;
    service
        .transition(
            created.request.id,
            RequestTransition::StartPlanning,
            None,
            None,
        )
        .await?;

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/requests/{}/re-resolution",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{
                        "canonical_identity_id":"{second_identity}",
                        "message":"admin selected a better match"
                    }}"#
                )))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let detail: RequestDetail = response_json(response).await?;
    assert_eq!(detail.request.state, RequestState::Resolved);
    assert_eq!(
        detail.request.target.canonical_identity_id,
        Some(second_identity)
    );
    assert_eq!(detail.identity_versions.len(), 2);
    assert_eq!(detail.identity_versions[0].version, 1);
    assert_eq!(
        detail.identity_versions[0].canonical_identity_id,
        first_identity
    );
    assert_eq!(detail.identity_versions[1].version, 2);
    assert_eq!(
        detail.identity_versions[1].canonical_identity_id,
        second_identity
    );
    assert_eq!(
        detail
            .status_events
            .last()
            .and_then(|event| event.message.as_deref()),
        Some("admin selected a better match")
    );

    Ok(())
}

#[tokio::test]
async fn request_plan_api_records_current_plan_and_history()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
    let created = service
        .create(NewRequest::anonymous("Inception (2010)"))
        .await?;
    service
        .resolve_identity(
            created.request.id,
            identity(550),
            RequestIdentityProvenance::Manual,
            None,
            None,
        )
        .await?;
    service
        .transition(
            created.request.id,
            RequestTransition::StartPlanning,
            None,
            None,
        )
        .await?;

    let first_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/plans", created.request.id))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{
                        "decision":"needs_provider",
                        "summary":"watch-folder provider can satisfy this request"
                    }"#,
                ))?,
        )
        .await?;
    assert_eq!(first_response.status(), StatusCode::OK);
    let first: RequestDetail = response_json(first_response).await?;
    assert_eq!(first.plan_history.len(), 1);
    assert_eq!(
        first.current_plan.as_ref().map(|plan| plan.decision),
        Some(FulfillmentPlanDecision::NeedsProvider)
    );

    let second_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/plans", created.request.id))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{
                        "decision":"needs_user_input",
                        "summary":"provider candidates require a user choice"
                    }"#,
                ))?,
        )
        .await?;
    assert_eq!(second_response.status(), StatusCode::OK);
    let second: RequestDetail = response_json(second_response).await?;
    assert_eq!(second.plan_history.len(), 2);
    assert_eq!(second.plan_history[0].version, 1);
    assert_eq!(second.plan_history[1].version, 2);
    assert_eq!(
        second.current_plan.as_ref().map(|plan| plan.id),
        Some(second.plan_history[1].id)
    );

    let get_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/requests/{}", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    let fetched: RequestDetail = response_json(get_response).await?;
    assert_eq!(fetched.plan_history, second.plan_history);
    assert_eq!(fetched.current_plan, second.current_plan);

    Ok(())
}

#[tokio::test]
async fn manual_import_walks_state_from_resolved() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let library_root = tempfile::tempdir()?;
    let artwork_cache = tempfile::tempdir()?;
    let tmdb = TmdbTestServer::new(vec![
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, r#"{"results":[]}"#),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{
                "id":27205,
                "title":"Inception",
                "release_date":"2010-07-15",
                "overview":"A thief enters dreams.",
                "runtime":1,
                "popularity":83.1,
                "poster_path":"/poster.jpg",
                "backdrop_path":"/backdrop.jpg",
                "credits":{"cast":[{"order":0,"name":"Leonardo DiCaprio","character":"Cobb","profile_path":null}]},
                "images":{"logos":[]}
            }"#,
        ),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{
                "id":27205,
                "title":"Inception",
                "release_date":"2010-07-15",
                "overview":"A thief enters dreams.",
                "runtime":1,
                "poster_path":"/poster.jpg",
                "backdrop_path":"/backdrop.jpg",
                "credits":{"cast":[{"order":0,"name":"Leonardo DiCaprio","character":"Cobb","profile_path":null}]},
                "images":{"logos":[]}
            }"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, "poster-bytes"),
        TmdbTestResponse::json(StatusCode::OK, "backdrop-bytes"),
    ])
    .await?;
    let app = kino_server::router_with_library_root_and_tmdb_client(
        db.clone(),
        library_root.path(),
        artwork_cache.path(),
        tmdb.client()?,
    );
    let created = create_request(&app, &auth, "Inception (2010)").await?;

    let resolve_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/resolve", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resolve_response.status(), StatusCode::OK);

    let incoming = tempfile::tempdir()?;
    let path = incoming.path().join("inception.mkv");
    write_sample_video(&path).await?;

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{
                        "path":"{}",
                        "message":"operator selected source"
                    }}"#,
                    path.display()
                )))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response_json(response).await?;
    assert_eq!(body["provider_id"], "manual-import");
    assert_eq!(body["path"], path.display().to_string());

    let job_id = body["job_id"]
        .as_str()
        .ok_or("manual import job id missing")?;
    let expected_message = format!(
        "operator selected source; manual import {} accepted as {job_id}",
        path.display()
    );
    let detail: RequestDetail = serde_json::from_value(body["request"].clone())?;
    assert_eq!(detail.request.state, RequestState::Satisfied);
    assert_eq!(detail.request.failure_reason, None);
    assert_eq!(
        detail.current_plan.as_ref().map(|plan| plan.decision),
        Some(FulfillmentPlanDecision::NeedsProvider)
    );
    assert_eq!(
        detail
            .current_plan
            .as_ref()
            .map(|plan| plan.summary.as_str()),
        Some("operator selected source")
    );
    assert_eq!(
        detail
            .status_events
            .iter()
            .map(|event| (event.from_state, event.to_state))
            .collect::<Vec<_>>(),
        vec![
            (None, RequestState::Pending),
            (Some(RequestState::Pending), RequestState::Resolved),
            (Some(RequestState::Resolved), RequestState::Planning),
            (Some(RequestState::Planning), RequestState::Fulfilling),
            (Some(RequestState::Fulfilling), RequestState::Ingesting),
            (Some(RequestState::Ingesting), RequestState::Satisfied),
        ]
    );
    assert_eq!(
        detail.status_events[4].message.as_deref(),
        Some(expected_message.as_str())
    );

    let canonical_path = library_root
        .path()
        .join("Movies")
        .join("Inception (2010)")
        .join("Inception (2010).mkv");
    assert!(tokio::fs::try_exists(&canonical_path).await?);
    assert!(tokio::fs::try_exists(&path).await?);

    let catalog_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?has_source_file=true")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(catalog_response.status(), StatusCode::OK);
    let catalog: Value = response_json(catalog_response).await?;
    assert_eq!(catalog["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        catalog["items"][0]["canonical_identity_id"],
        "tmdb:movie:27205"
    );
    assert_eq!(
        catalog["items"][0]["source_files"][0]["path"],
        canonical_path.display().to_string()
    );
    assert_eq!(
        catalog["items"][0]["source_files"][0]["probe"]["video"]["resolution"],
        "240p"
    );
    let source_file_id: Id = catalog["items"][0]["source_files"][0]["id"]
        .as_str()
        .ok_or("source file id missing")?
        .parse()?;
    let (probe_duration_seconds,): (Option<i64>,) =
        sqlx::query_as("SELECT probe_duration_seconds FROM source_files WHERE id = ?1")
            .bind(source_file_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(probe_duration_seconds, Some(1));

    let media_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/stream/items/{}/{}/media.m3u8",
                    catalog["items"][0]["id"]
                        .as_str()
                        .ok_or("media item id missing")?,
                    source_file_id
                ))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(media_response.status(), StatusCode::OK);
    let media_bytes = to_bytes(media_response.into_body(), usize::MAX).await?;
    let media_playlist = std::str::from_utf8(&media_bytes)?;
    assert!(media_playlist.contains("#EXT-X-ENDLIST"));

    let (poster_local_path,): (String,) =
        sqlx::query_as("SELECT poster_local_path FROM media_metadata_cache LIMIT 1")
            .fetch_one(db.read_pool())
            .await?;
    assert!(tokio::fs::try_exists(artwork_cache.path().join(poster_local_path)).await?);

    let second = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;
    assert_eq!(second.status(), StatusCode::CONFLICT);

    Ok(())
}

#[tokio::test]
async fn delete_catalog_item_removes_rows_and_canonical_files()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let (auth, token_id) = common::issued_token_with_id(&db).await?;
    let library_root = tempfile::tempdir()?;
    let artwork_cache = tempfile::tempdir()?;
    let tmdb = TmdbTestServer::new(vec![
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, r#"{"results":[]}"#),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{
                "id":27205,
                "title":"Inception",
                "release_date":"2010-07-15",
                "overview":"A thief enters dreams.",
                "runtime":1,
                "popularity":83.1,
                "poster_path":"/poster.jpg",
                "backdrop_path":"/backdrop.jpg",
                "credits":{"cast":[{"order":0,"name":"Leonardo DiCaprio","character":"Cobb","profile_path":null}]},
                "images":{"logos":[]}
            }"#,
        ),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{
                "id":27205,
                "title":"Inception",
                "release_date":"2010-07-15",
                "overview":"A thief enters dreams.",
                "runtime":1,
                "poster_path":"/poster.jpg",
                "backdrop_path":"/backdrop.jpg",
                "credits":{"cast":[{"order":0,"name":"Leonardo DiCaprio","character":"Cobb","profile_path":null}]},
                "images":{"logos":[]}
            }"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, "poster-bytes"),
        TmdbTestResponse::json(StatusCode::OK, "backdrop-bytes"),
    ])
    .await?;
    let app = kino_server::router_with_library_root_and_tmdb_client(
        db.clone(),
        library_root.path(),
        artwork_cache.path(),
        tmdb.client()?,
    );
    let created = create_request(&app, &auth, "Inception (2010)").await?;

    let resolve_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/resolve", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resolve_response.status(), StatusCode::OK);

    let incoming = tempfile::tempdir()?;
    let path = incoming.path().join("inception.mkv");
    write_sample_video(&path).await?;
    let import_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;
    assert_eq!(import_response.status(), StatusCode::OK);

    let catalog_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?has_source_file=true")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(catalog_response.status(), StatusCode::OK);
    let catalog: Value = response_json(catalog_response).await?;
    let media_item_id: Id = catalog["items"][0]["id"]
        .as_str()
        .ok_or("media item id missing")?
        .parse()?;
    let source_file_id: Id = catalog["items"][0]["source_files"][0]["id"]
        .as_str()
        .ok_or("source file id missing")?
        .parse()?;
    let canonical_path = Path::new(
        catalog["items"][0]["source_files"][0]["path"]
            .as_str()
            .ok_or("source file path missing")?,
    )
    .to_path_buf();
    let item_dir = canonical_path
        .parent()
        .ok_or("canonical source should have a parent")?
        .to_path_buf();

    let transcode_path = item_dir.join("Inception (2010).1080p.mp4");
    tokio::fs::write(&transcode_path, b"transcoded").await?;
    CatalogService::new(db.clone())
        .register_transcode_output(
            media_item_id,
            TranscodeCapabilities {
                codec: "h264".to_owned(),
                container: "mp4".to_owned(),
                resolution: Some("1080p".to_owned()),
                hdr: None,
            },
            &transcode_path,
        )
        .await?;

    let sidecars = SubtitleService::new(db.clone())
        .extract_ocr_subtitles(OcrSubtitleExtractionInput::new(
            media_item_id,
            &item_dir,
            vec![OcrSubtitleTrack::new(
                4,
                "eng",
                vec![kino_library::subtitle_ocr::OcrCue {
                    start: Duration::from_millis(250),
                    end: Duration::from_millis(750),
                    text: String::from("HELLO"),
                    confidence: 99.0,
                }],
            )],
        ))
        .await?
        .sidecars;
    let sidecar_path = sidecars[0].path.clone();

    let now = Timestamp::now();
    sqlx::query(
        r#"
        INSERT INTO playback_progress (
            user_id,
            media_item_id,
            position_seconds,
            updated_at,
            source_device_token_id
        )
        VALUES (?1, ?2, 42, ?3, ?4)
        "#,
    )
    .bind(SEEDED_USER_ID)
    .bind(media_item_id)
    .bind(now)
    .bind(token_id)
    .execute(db.write_pool())
    .await?;
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
    .bind(now)
    .execute(db.write_pool())
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
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 'active')
        "#,
    )
    .bind(Id::new())
    .bind(SEEDED_USER_ID)
    .bind(token_id)
    .bind(media_item_id)
    .bind(source_file_id.to_string())
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    let delete_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("DELETE")
                .uri(format!("/api/v1/admin/library/items/{media_item_id}"))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);

    assert_deleted_catalog_rows(&db, media_item_id, source_file_id).await?;
    assert!(!tokio::fs::try_exists(&canonical_path).await?);
    assert!(!tokio::fs::try_exists(&transcode_path).await?);
    assert!(!tokio::fs::try_exists(&sidecar_path).await?);
    assert!(!tokio::fs::try_exists(&item_dir).await?);

    let list_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(list_response.status(), StatusCode::OK);
    let listed: Value = response_json(list_response).await?;
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(0));

    let get_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/library/items/{media_item_id}"))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn delete_catalog_item_returns_not_found_for_missing_item()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let library_root = tempfile::tempdir()?;
    let app = kino_server::router_with_library_root(db, library_root.path());

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("DELETE")
                .uri(format!("/api/v1/admin/library/items/{}", Id::new()))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn manual_import_api_fails_when_probe_rejects_source()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db.clone());
    let request = fulfilling_request(&service).await?;
    let path = std::env::temp_dir().join(format!("kino-manual-import-api-{}.mkv", request));
    tokio::fs::write(&path, b"movie").await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/admin/requests/{request}/manual-import"))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{
                        "path":"{}",
                        "message":"operator selected source"
                    }}"#,
                    path.display()
                )))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response_json(response).await?;
    assert_eq!(body["provider_id"], "manual-import");
    assert_eq!(body["path"], path.display().to_string());

    let job_id = body["job_id"]
        .as_str()
        .ok_or("manual import job id missing")?;
    let expected_message = format!(
        "operator selected source; manual import {} accepted as {job_id}",
        path.display()
    );
    let detail: RequestDetail = serde_json::from_value(body["request"].clone())?;
    assert_eq!(detail.request.state, RequestState::Failed);
    assert_eq!(
        detail.request.failure_reason,
        Some(kino_core::RequestFailureReason::IngestFailed)
    );
    assert_eq!(
        detail
            .status_events
            .iter()
            .find(|event| event.to_state == RequestState::Ingesting)
            .and_then(|event| event.message.as_deref()),
        Some(expected_message.as_str())
    );
    assert!(
        detail
            .status_events
            .last()
            .and_then(|event| event.message.as_deref())
            .is_some_and(|message| message.contains("ffprobe")),
        "got: {detail:?}"
    );
    assert_eq!(media_item_count(&db).await?, 0);
    assert!(tokio::fs::try_exists(&path).await?);

    tokio::fs::remove_file(path).await?;
    Ok(())
}

#[tokio::test]
async fn manual_import_api_returns_failed_detail_when_probe_mismatches()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let library_root = tempfile::tempdir()?;
    let artwork_cache = tempfile::tempdir()?;
    let tmdb = TmdbTestServer::new(vec![
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, r#"{"results":[]}"#),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{
                "id":27205,
                "title":"Inception",
                "release_date":"2010-07-15",
                "runtime":10,
                "popularity":83.1
            }"#,
        ),
    ])
    .await?;
    let app = kino_server::router_with_library_root_and_tmdb_client(
        db.clone(),
        library_root.path(),
        artwork_cache.path(),
        tmdb.client()?,
    );
    let created = create_request(&app, &auth, "Inception (2010)").await?;

    let resolve_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/resolve", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resolve_response.status(), StatusCode::OK);

    let incoming = tempfile::tempdir()?;
    let path = incoming.path().join("short-inception.mkv");
    write_sample_video(&path).await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response_json(response).await?;
    assert_eq!(body["request"]["request"]["state"], "failed");
    assert_eq!(
        body["request"]["request"]["failure_reason"],
        "ingest_failed"
    );
    let detail: RequestDetail = serde_json::from_value(body["request"].clone())?;
    assert_eq!(detail.request.state, RequestState::Failed);
    assert_eq!(
        detail.request.failure_reason,
        Some(RequestFailureReason::IngestFailed)
    );
    assert!(
        detail
            .status_events
            .last()
            .and_then(|event| event.message.as_deref())
            .is_some_and(|message| message.contains("duration mismatch")),
        "got: {detail:?}"
    );
    assert_eq!(media_item_count(&db).await?, 0);
    assert!(tokio::fs::try_exists(&path).await?);

    Ok(())
}

#[tokio::test]
async fn manual_import_api_surfaces_missing_path() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db.clone());
    let request = fulfilling_request(&service).await?;
    let path = std::env::temp_dir().join(format!("kino-manual-import-missing-{request}.mkv"));

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/admin/requests/{request}/manual-import"))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response_json(response).await?;
    let detail: RequestDetail = serde_json::from_value(body["request"].clone())?;
    assert_eq!(detail.request.state, RequestState::Failed);
    assert_eq!(
        detail.request.failure_reason,
        Some(kino_core::RequestFailureReason::IngestFailed)
    );
    assert!(
        detail
            .status_events
            .last()
            .and_then(|event| event.message.as_deref())
            .is_some_and(|message| message.contains("No such file")),
        "got: {body}"
    );

    let get_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/requests/{request}"))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    let detail: RequestDetail = response_json(get_response).await?;
    assert_eq!(detail.request.state, RequestState::Failed);
    assert_eq!(media_item_count(&db).await?, 0);

    Ok(())
}

#[tokio::test]
async fn request_reset_allows_failed_manual_import_retry() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let library_root = tempfile::tempdir()?;
    let artwork_cache = tempfile::tempdir()?;
    let tmdb = TmdbTestServer::new(vec![
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, r#"{"results":[]}"#),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, r#"{"results":[]}"#),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{
                "id":27205,
                "title":"Inception",
                "release_date":"2010-07-15",
                "runtime":1,
                "popularity":83.1
            }"#,
        ),
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{
                "id":27205,
                "title":"Inception",
                "release_date":"2010-07-15",
                "overview":"A thief enters dreams.",
                "runtime":1,
                "poster_path":"/poster.jpg",
                "backdrop_path":"/backdrop.jpg",
                "popularity":83.1,
                "credits":{"cast":[{"order":0,"name":"Leonardo DiCaprio","character":"Cobb","profile_path":null}]},
                "images":{"logos":[]}
            }"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, "poster-bytes"),
        TmdbTestResponse::json(StatusCode::OK, "backdrop-bytes"),
    ])
    .await?;
    let app = kino_server::router_with_library_root_and_tmdb_client(
        db.clone(),
        library_root.path(),
        artwork_cache.path(),
        tmdb.client()?,
    );
    let created = create_request(&app, &auth, "Inception (2010)").await?;

    let resolve_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/resolve", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resolve_response.status(), StatusCode::OK);

    let missing_path = std::env::temp_dir().join(format!(
        "kino-manual-import-reset-missing-{}.mkv",
        created.request.id
    ));
    let failed_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"path":"{}","message":"bad source path"}}"#,
                    missing_path.display()
                )))?,
        )
        .await?;
    assert_eq!(failed_response.status(), StatusCode::OK);
    let failed_body: Value = response_json(failed_response).await?;
    assert_eq!(failed_body["request"]["request"]["state"], "failed");
    assert_eq!(
        failed_body["request"]["request"]["failure_reason"],
        "ingest_failed"
    );

    let reset_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/reset", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(reset_response.status(), StatusCode::OK);
    let reset: RequestDetail = response_json(reset_response).await?;
    assert_eq!(reset.request.state, RequestState::Pending);
    assert_eq!(reset.request.failure_reason, None);
    assert_eq!(reset.request.target.canonical_identity_id, None);
    assert_eq!(reset.request.plan_id, None);
    assert_eq!(reset.current_plan, None);
    assert_eq!(reset.identity_versions.len(), 1);
    assert!(!reset.status_events.is_empty());
    let reset_event = reset
        .status_events
        .last()
        .ok_or("reset should write a status event")?;
    assert_eq!(reset_event.from_state, Some(RequestState::Failed));
    assert_eq!(reset_event.to_state, RequestState::Pending);
    assert_eq!(reset_event.message.as_deref(), Some("reset by operator"));

    let second_resolve_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/resolve", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(second_resolve_response.status(), StatusCode::OK);
    let resolved_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/requests/{}", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    let resolved: RequestDetail = response_json(resolved_response).await?;
    assert_eq!(resolved.request.state, RequestState::Resolved);

    let incoming = tempfile::tempdir()?;
    let path = incoming.path().join("inception.mkv");
    write_sample_video(&path).await?;
    let satisfied_response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"path":"{}","message":"retry with valid source"}}"#,
                    path.display()
                )))?,
        )
        .await?;
    assert_eq!(satisfied_response.status(), StatusCode::OK);
    let satisfied_body: Value = response_json(satisfied_response).await?;
    let satisfied: RequestDetail = serde_json::from_value(satisfied_body["request"].clone())?;
    assert_eq!(satisfied.request.state, RequestState::Satisfied);
    assert_eq!(satisfied.request.failure_reason, None);
    assert_eq!(satisfied.identity_versions.len(), 2);
    assert_eq!(media_item_count(&db).await?, 1);

    Ok(())
}

#[tokio::test]
async fn request_reset_rejects_satisfied_request() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db.clone());
    let created = service
        .create(NewRequest::anonymous("Inception (2010)"))
        .await?;
    service
        .resolve_identity(
            created.request.id,
            identity(550),
            RequestIdentityProvenance::Manual,
            None,
            None,
        )
        .await?;
    service
        .transition(
            created.request.id,
            RequestTransition::StartPlanning,
            None,
            None,
        )
        .await?;
    service
        .transition(created.request.id, RequestTransition::Satisfy, None, None)
        .await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/reset", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);

    Ok(())
}

#[tokio::test]
async fn request_cancel_api_cancels_pending_and_rejects_terminal()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db.clone());
    let pending = create_request(&app, &auth, "Inception (2010)").await?;

    let cancel_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/cancel", pending.request.id))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"message":"operator stopped it"}"#))?,
        )
        .await?;
    assert_eq!(cancel_response.status(), StatusCode::OK);
    let cancelled: RequestDetail = response_json(cancel_response).await?;
    assert_eq!(cancelled.request.state, RequestState::Cancelled);
    assert_eq!(
        cancelled
            .status_events
            .last()
            .and_then(|event| event.message.as_deref()),
        Some("operator stopped it")
    );

    let satisfied = service
        .create(NewRequest::anonymous("Satisfied Movie"))
        .await?;
    service
        .resolve_identity(
            satisfied.request.id,
            identity(550),
            RequestIdentityProvenance::Manual,
            None,
            None,
        )
        .await?;
    service
        .transition(
            satisfied.request.id,
            RequestTransition::StartPlanning,
            None,
            None,
        )
        .await?;
    service
        .transition(satisfied.request.id, RequestTransition::Satisfy, None, None)
        .await?;

    let satisfied_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/cancel", satisfied.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(satisfied_response.status(), StatusCode::CONFLICT);

    let missing_response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/cancel", Id::new()))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(missing_response.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn request_cancel_reset_then_resolve_composes() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let tmdb = TmdbTestServer::new(vec![
        TmdbTestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        ),
        TmdbTestResponse::json(StatusCode::OK, r#"{"results":[]}"#),
    ])
    .await?;
    let app = kino_server::router_with_tmdb_client(db, tmdb.client()?);
    let created = create_request(&app, &auth, "Inception (2010)").await?;

    let cancel_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/cancel", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(cancel_response.status(), StatusCode::OK);
    let cancelled: RequestDetail = response_json(cancel_response).await?;
    assert_eq!(cancelled.request.state, RequestState::Cancelled);

    let reset_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/reset", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(reset_response.status(), StatusCode::OK);
    let reset: RequestDetail = response_json(reset_response).await?;
    assert_eq!(reset.request.state, RequestState::Pending);

    let resolve_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/resolve", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(resolve_response.status(), StatusCode::OK);

    let fetched_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/requests/{}", created.request.id))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    let fetched: RequestDetail = response_json(fetched_response).await?;
    assert_eq!(fetched.request.state, RequestState::Resolved);
    assert_eq!(
        fetched.request.target.canonical_identity_id,
        Some(identity(27_205))
    );

    Ok(())
}

#[tokio::test]
async fn request_reset_returns_not_found_for_unknown_request()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db.clone());
    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/reset", Id::new()))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn manual_import_rejects_pending_request() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db);
    let created = create_request(&app, &auth, "Inception (2010)").await?;
    let path = std::env::temp_dir().join(format!(
        "kino-manual-import-invalid-state-{}.mkv",
        created.request.id
    ));
    tokio::fs::write(&path, b"movie").await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let body: Value = response_json(response).await?;
    assert_eq!(
        body["error"],
        "request must be resolved before manual import"
    );

    tokio::fs::remove_file(path).await?;
    Ok(())
}

#[tokio::test]
async fn manual_import_rejects_satisfied_request() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
    let created = service
        .create(NewRequest::anonymous("Inception (2010)"))
        .await?;
    service
        .resolve_identity(
            created.request.id,
            identity(550),
            RequestIdentityProvenance::Manual,
            None,
            None,
        )
        .await?;
    service
        .transition(
            created.request.id,
            RequestTransition::StartPlanning,
            None,
            None,
        )
        .await?;
    service
        .transition(created.request.id, RequestTransition::Satisfy, None, None)
        .await?;
    let path = std::env::temp_dir().join(format!(
        "kino-manual-import-satisfied-{}.mkv",
        created.request.id
    ));
    tokio::fs::write(&path, b"movie").await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);

    tokio::fs::remove_file(path).await?;
    Ok(())
}

#[tokio::test]
async fn manual_import_rejects_cancelled_request() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
    let created = service
        .create(NewRequest::anonymous("Inception (2010)"))
        .await?;
    service
        .transition(created.request.id, RequestTransition::Cancel, None, None)
        .await?;
    let path = std::env::temp_dir().join(format!(
        "kino-manual-import-cancelled-{}.mkv",
        created.request.id
    ));
    tokio::fs::write(&path, b"movie").await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);

    tokio::fs::remove_file(path).await?;
    Ok(())
}

#[tokio::test]
async fn admin_library_scan_reports_orphans_and_missing_files()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let library_root = tempfile::tempdir()?;
    let orphan = library_root
        .path()
        .join("Movies")
        .join("Orphan (2002)")
        .join("Orphan (2002).mkv");
    let missing = library_root
        .path()
        .join("Movies")
        .join("Missing (2003)")
        .join("Missing (2003).mkv");
    let orphan_parent = orphan.parent().ok_or("orphan path should have parent")?;
    tokio::fs::create_dir_all(orphan_parent).await?;
    tokio::fs::write(&orphan, b"orphan").await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let source_file_id = insert_source_file(&db, media_item_id, &missing).await?;
    let app = kino_server::router_with_library_root(db, library_root.path());

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/admin/library/scan")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response_json(response).await?;
    assert_eq!(body["orphans"][0]["path"], orphan.display().to_string());
    assert_eq!(body["orphans"][0]["kind"], "movie");
    assert_eq!(
        body["missing"][0]["source_file"]["id"],
        source_file_id.to_string()
    );
    assert_eq!(
        body["missing"][0]["source_file"]["media_item_id"],
        media_item_id.to_string()
    );
    assert_eq!(
        body["missing"][0]["source_file"]["path"],
        missing.display().to_string()
    );
    assert_eq!(body["layout_violations"], serde_json::json!([]));

    Ok(())
}

#[tokio::test]
async fn admin_library_scan_rejects_unauthenticated() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/admin/library/scan")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn admin_source_file_reprobe_updates_probe_duration() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let media_item_id = insert_personal_media_item(&db).await?;
    let temp_dir = tempfile::tempdir()?;
    let source_path = temp_dir.path().join("source.mkv");
    write_sample_video(&source_path).await?;
    let source_file_id = insert_source_file(&db, media_item_id, &source_path).await?;
    let app = kino_server::router(db.clone());

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/source-files/{source_file_id}/reprobe"
                ))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response_json(response).await?;
    assert_eq!(body["source_file_id"], source_file_id.to_string());
    assert_eq!(body["probe_duration_seconds"], 1);
    let (probe_duration_seconds,): (Option<i64>,) =
        sqlx::query_as("SELECT probe_duration_seconds FROM source_files WHERE id = ?1")
            .bind(source_file_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(probe_duration_seconds, Some(1));
    let (probe_rows,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM source_file_probes WHERE source_file_id = ?1")
            .bind(source_file_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(probe_rows, 1);

    Ok(())
}

#[tokio::test]
async fn catalog_api_lists_filters_and_gets_items() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let matrix_identity = identity(603);
    let fight_club_identity = identity(550);
    let matrix = insert_tmdb_media_item(&db, matrix_identity).await?;
    let fight_club = insert_tmdb_media_item(&db, fight_club_identity).await?;
    insert_catalog_title(&db, matrix, matrix_identity, "The Matrix").await?;
    insert_catalog_title(&db, fight_club, fight_club_identity, "Fight Club").await?;
    let matrix_path =
        std::path::PathBuf::from("/library/Movies/The Matrix (1999)/The Matrix (1999).mkv");
    let matrix_source_file = insert_source_file(&db, matrix, &matrix_path).await?;
    insert_subtitle_sidecar(
        &db,
        matrix,
        "eng",
        "srt",
        "text",
        2,
        "/subtitles/matrix.eng.srt",
    )
    .await?;
    insert_subtitle_sidecar(
        &db,
        matrix,
        "jpn",
        "json",
        "ocr",
        4,
        "/subtitles/matrix.jpn.json",
    )
    .await?;
    let app = kino_server::router(db);

    let list_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?type=movie&title_contains=matrix&has_source_file=true&limit=1")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(list_response.status(), StatusCode::OK);
    let listed: Value = response_json(list_response).await?;
    assert_eq!(listed["items"][0]["id"], matrix.to_string());
    assert_eq!(listed["items"][0]["media_kind"], "movie");
    assert_eq!(listed["items"][0]["title"], "The Matrix");
    assert_eq!(
        listed["items"][0]["source_files"][0]["path"],
        matrix_path.display().to_string()
    );
    assert_eq!(
        listed["items"][0]["variants"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(
        listed["items"][0]["variants"][0]["variant_id"],
        matrix_source_file.to_string()
    );
    assert_eq!(listed["items"][0]["variants"][0]["kind"], "source");
    assert_eq!(
        listed["items"][0]["variants"][0]["capabilities"]["container"],
        "mkv"
    );
    assert_eq!(
        listed["items"][0]["variants"][0]["stream_url"],
        format!("/api/v1/stream/sourcefile/{matrix_source_file}/file.mkv")
    );
    assert_eq!(listed["next_offset"], Value::Null);

    let search_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?search=matr")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(search_response.status(), StatusCode::OK);
    let searched: Value = response_json(search_response).await?;
    assert_eq!(searched["items"][0]["id"], matrix.to_string());

    let paged_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?limit=1")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(paged_response.status(), StatusCode::OK);
    let paged: Value = response_json(paged_response).await?;
    assert_eq!(paged["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(paged["next_offset"], 1);

    let get_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/library/items/{matrix}"))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    let fetched: Value = response_json(get_response).await?;
    assert_eq!(fetched["id"], matrix.to_string());
    assert_eq!(
        fetched["canonical_identity_id"],
        matrix_identity.to_string()
    );
    assert_eq!(fetched["source_files"].as_array().map(Vec::len), Some(1));
    assert_eq!(fetched["variants"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        fetched["variants"][0]["stream_url"],
        format!("/api/v1/stream/sourcefile/{matrix_source_file}/file.mkv")
    );
    assert_eq!(fetched["subtitle_tracks"][0]["language"], "eng");
    assert_eq!(fetched["subtitle_tracks"][0]["label"], "ENG");
    assert_eq!(fetched["subtitle_tracks"][0]["format"], "srt");
    assert_eq!(fetched["subtitle_tracks"][0]["provenance"], "text");
    assert_eq!(fetched["subtitle_tracks"][0]["forced"], false);
    assert_eq!(fetched["subtitle_tracks"][1]["language"], "jpn");
    assert_eq!(fetched["subtitle_tracks"][1]["label"], "JPN (OCR)");
    assert_eq!(fetched["subtitle_tracks"][1]["format"], "json");
    assert_eq!(fetched["subtitle_tracks"][1]["provenance"], "ocr");
    assert_eq!(fetched["subtitle_tracks"][1]["forced"], false);

    Ok(())
}

#[tokio::test]
async fn catalog_item_detail_includes_multiple_source_file_probe_tracks()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let identity_id = identity(603);
    let media_item_id = insert_tmdb_media_item(&db, identity_id).await?;
    insert_catalog_title(&db, media_item_id, identity_id, "The Matrix").await?;
    update_catalog_release_date(&db, media_item_id, "1999-03-31").await?;
    insert_catalog_cast_member(&db, media_item_id, 0, "Keanu Reeves", "Neo").await?;
    let main_path = std::path::PathBuf::from("/library/Movies/The Matrix/01-main.mkv");
    let bonus_path = std::path::PathBuf::from("/library/Movies/The Matrix/02-bonus.mp4");
    let main_source_file = insert_source_file(&db, media_item_id, &main_path).await?;
    let bonus_source_file = insert_source_file(&db, media_item_id, &bonus_path).await?;
    insert_source_file_probe(
        &db,
        main_source_file,
        SourceFileProbeSeed {
            container: "matroska",
            video_codec: "h265",
            video_width: 3840,
            video_height: 2160,
            video_hdr: Some("hdr10"),
        },
    )
    .await?;
    insert_source_file_audio_track(&db, main_source_file, 1, "truehd", Some("eng"), Some(8))
        .await?;
    insert_source_file_audio_track(&db, main_source_file, 2, "aac", Some("jpn"), Some(2)).await?;
    insert_source_file_subtitle_track(&db, main_source_file, 3, "srt", "text", "eng", false)
        .await?;
    insert_source_file_subtitle_track(&db, main_source_file, 4, "json", "ocr", "jpn", true).await?;
    insert_source_file_probe(
        &db,
        bonus_source_file,
        SourceFileProbeSeed {
            container: "mp4",
            video_codec: "h264",
            video_width: 1920,
            video_height: 1080,
            video_hdr: None,
        },
    )
    .await?;
    insert_source_file_audio_track(&db, bonus_source_file, 1, "aac", Some("eng"), Some(2)).await?;
    insert_source_file_subtitle_track(&db, bonus_source_file, 2, "ass", "text", "spa", false)
        .await?;
    insert_subtitle_sidecar(
        &db,
        media_item_id,
        "eng",
        "srt",
        "text",
        3,
        "/subtitles/matrix.eng.srt",
    )
    .await?;
    let forced_sidecar = insert_subtitle_sidecar(
        &db,
        media_item_id,
        "jpn",
        "json",
        "ocr",
        4,
        "/subtitles/matrix.jpn.json",
    )
    .await?;
    mark_subtitle_sidecar_forced(&db, forced_sidecar).await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/library/items/{media_item_id}"))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let fetched: Value = response_json(response).await?;

    assert_eq!(fetched["title"], "The Matrix");
    assert_eq!(fetched["description"], "description");
    assert_eq!(fetched["release_date"], "1999-03-31");
    assert_eq!(fetched["year"], 1999);
    assert_eq!(fetched["cast"][0]["name"], "Keanu Reeves");
    assert_eq!(fetched["source_files"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        fetched["source_files"][0]["id"],
        main_source_file.to_string()
    );
    assert_eq!(
        fetched["source_files"][0]["path"],
        main_path.display().to_string()
    );
    assert_eq!(fetched["source_files"][0]["probe"]["container"], "matroska");
    assert_eq!(
        fetched["source_files"][0]["probe"]["video"]["codec"],
        "h265"
    );
    assert_eq!(
        fetched["source_files"][0]["probe"]["video"]["resolution"],
        "2160p"
    );
    assert_eq!(fetched["source_files"][0]["probe"]["video"]["hdr"], "hdr10");
    assert_eq!(
        fetched["source_files"][0]["audio_tracks"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        fetched["source_files"][0]["audio_tracks"][0]["language"],
        "eng"
    );
    assert_eq!(fetched["source_files"][0]["audio_tracks"][0]["channels"], 8);
    assert_eq!(
        fetched["source_files"][0]["subtitle_tracks"][1]["provenance"],
        "ocr"
    );
    assert_eq!(
        fetched["source_files"][0]["subtitle_tracks"][1]["forced"],
        true
    );
    assert_eq!(
        fetched["source_files"][1]["id"],
        bonus_source_file.to_string()
    );
    assert_eq!(
        fetched["source_files"][1]["probe"]["video"]["resolution"],
        "1080p"
    );
    assert_eq!(
        fetched["source_files"][1]["subtitle_tracks"][0]["format"],
        "ass"
    );
    assert_eq!(fetched["variants"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        fetched["variants"][0]["variant_id"],
        main_source_file.to_string()
    );
    assert_eq!(fetched["variants"][0]["capabilities"]["codec"], "h265");
    assert_eq!(
        fetched["variants"][0]["capabilities"]["container"],
        "matroska"
    );
    assert_eq!(
        fetched["variants"][0]["capabilities"]["resolution"],
        "2160p"
    );
    assert_eq!(fetched["variants"][0]["capabilities"]["hdr"], "hdr10");
    assert_eq!(
        fetched["variants"][0]["stream_url"],
        format!("/api/v1/stream/sourcefile/{main_source_file}/file.mkv")
    );
    assert_eq!(fetched["variants"][1]["capabilities"]["codec"], "h264");
    assert_eq!(fetched["variants"][1]["capabilities"]["container"], "mp4");
    assert_eq!(fetched["subtitle_tracks"][0]["forced"], false);
    assert_eq!(fetched["subtitle_tracks"][1]["provenance"], "ocr");
    assert_eq!(fetched["subtitle_tracks"][1]["forced"], true);

    Ok(())
}

#[tokio::test]
async fn catalog_api_filters_sorts_and_cursor_paginates() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let alpha_identity = identity(1001);
    let bravo_identity = identity(1002);
    let charlie_identity = identity(1003);
    let show_identity = tv_identity(1004);
    let alpha = insert_tmdb_media_item(&db, alpha_identity).await?;
    let bravo = insert_tmdb_media_item(&db, bravo_identity).await?;
    let charlie = insert_tmdb_media_item(&db, charlie_identity).await?;
    let show = insert_tmdb_media_item(&db, show_identity).await?;
    insert_catalog_title_with_release_date(&db, alpha, alpha_identity, "Alpha", Some("2020-01-01"))
        .await?;
    insert_catalog_title_with_release_date(&db, bravo, bravo_identity, "Bravo", Some("2021-01-01"))
        .await?;
    insert_catalog_title_with_release_date(
        &db,
        charlie,
        charlie_identity,
        "Charlie",
        Some("2022-01-01"),
    )
    .await?;
    insert_catalog_title_with_release_date(&db, show, show_identity, "Show", Some("2020-02-01"))
        .await?;
    insert_watched(&db, alpha).await?;
    let app = kino_server::router(db);

    let movies_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?kind=movie")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(movies_response.status(), StatusCode::OK);
    let movies: Value = response_json(movies_response).await?;
    let movie_items = movies["items"]
        .as_array()
        .ok_or("movie items should be an array")?;
    assert_eq!(movie_items.len(), 3);
    assert!(movie_items.iter().all(|item| item["media_kind"] == "movie"));

    let year_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?year=2020&sort=title")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(year_response.status(), StatusCode::OK);
    let released_in_2020: Value = response_json(year_response).await?;
    assert_eq!(
        released_in_2020["items"]
            .as_array()
            .ok_or("year items should be an array")?
            .iter()
            .map(|item| item["id"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::String(alpha.to_string()),
            Value::String(show.to_string())
        ]
    );

    let watched_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?watched=true")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(watched_response.status(), StatusCode::OK);
    let watched: Value = response_json(watched_response).await?;
    assert_eq!(watched["items"][0]["id"], Value::String(alpha.to_string()));
    assert_eq!(watched["items"].as_array().map(Vec::len), Some(1));

    let first_page_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?sort=title&limit=2")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(first_page_response.status(), StatusCode::OK);
    let first_page: Value = response_json(first_page_response).await?;
    assert_eq!(
        first_page["items"]
            .as_array()
            .ok_or("first page items should be an array")?
            .iter()
            .map(|item| item["id"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::String(alpha.to_string()),
            Value::String(bravo.to_string())
        ]
    );
    let cursor = first_page["next_cursor"]
        .as_str()
        .ok_or("first page should include next cursor")?;

    let second_page_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/library/items?sort=title&limit=2&cursor={cursor}"
                ))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(second_page_response.status(), StatusCode::OK);
    let second_page: Value = response_json(second_page_response).await?;
    assert_eq!(
        second_page["items"]
            .as_array()
            .ok_or("second page items should be an array")?
            .iter()
            .map(|item| item["id"].clone())
            .collect::<Vec<_>>(),
        vec![
            Value::String(charlie.to_string()),
            Value::String(show.to_string())
        ]
    );
    assert_eq!(second_page["next_cursor"], Value::Null);

    Ok(())
}

#[tokio::test]
async fn admin_reocr_api_archives_old_sidecar_and_catalog_reports_new_current()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let library_root = tempfile::tempdir()?;
    let artwork_cache = tempfile::tempdir()?;
    let matrix_identity = identity(603);
    let media_item_id = insert_tmdb_media_item(&db, matrix_identity).await?;
    insert_catalog_title(&db, media_item_id, matrix_identity, "The Matrix").await?;
    let source_path = library_root.path().join("matrix.mkv");
    tokio::fs::write(&source_path, b"media bytes").await?;
    insert_source_file(&db, media_item_id, &source_path).await?;
    let sidecar_dir = library_root.path().join(".kino").join("sidecars");
    tokio::fs::create_dir_all(&sidecar_dir).await?;
    let old_path = sidecar_dir.join("matrix.jpn.json");
    tokio::fs::write(&old_path, br#"{"provenance":"ocr","cues":[]}"#).await?;
    let old_path_text = old_path.to_string_lossy().into_owned();
    let old_sidecar_id =
        insert_subtitle_sidecar(&db, media_item_id, "jpn", "json", "ocr", 4, &old_path_text)
            .await?;
    let reocr = SubtitleReocrService::new(
        db.clone(),
        Arc::new(FakeSubtitleExtractor {
            frames: vec![ImageSubtitleFrame::new(
                Duration::from_secs(3),
                Duration::from_secs(4),
                library_root.path().join("frame.png"),
            )],
        }),
        Arc::new(FakeOcrEngine),
    );
    let app = kino_server::router_with_library_root_artwork_cache_reocr_and_public_base_url(
        db.clone(),
        library_root.path(),
        artwork_cache.path(),
        reocr,
        "https://kino.example.test",
    );

    let reocr_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/admin/items/{media_item_id}/subtitles/4/re-ocr"
                ))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(reocr_response.status(), StatusCode::ACCEPTED);
    let accepted: Value = response_json(reocr_response).await?;
    let job_id = accepted["job_id"]
        .as_str()
        .ok_or("job_id should be a string")?;

    let archived_at: Option<Timestamp> =
        sqlx::query_scalar("SELECT archived_at FROM subtitle_sidecars WHERE id = ?1")
            .bind(old_sidecar_id)
            .fetch_one(db.read_pool())
            .await?;
    assert!(archived_at.is_some());

    let catalog_response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/library/items/{media_item_id}"))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(catalog_response.status(), StatusCode::OK);
    let fetched: Value = response_json(catalog_response).await?;
    assert_eq!(fetched["subtitle_tracks"].as_array().map(Vec::len), Some(1));
    assert_eq!(fetched["subtitle_tracks"][0]["id"], job_id);
    assert_eq!(fetched["subtitle_tracks"][0]["language"], "jpn");
    assert_eq!(fetched["subtitle_tracks"][0]["label"], "JPN (OCR)");
    assert_eq!(fetched["subtitle_tracks"][0]["provenance"], "ocr");

    Ok(())
}

#[tokio::test]
async fn catalog_api_reports_invalid_filters_and_missing_items()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db);

    let invalid_filter = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?type=episode")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(invalid_filter.status(), StatusCode::BAD_REQUEST);

    let unknown_param = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items?foo=bar")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(unknown_param.status(), StatusCode::BAD_REQUEST);
    let error: Value = response_json(unknown_param).await?;
    assert!(
        error["error"]
            .as_str()
            .is_some_and(|message| message.contains("foo"))
    );

    let missing = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!("/api/v1/library/items/{}", Id::new()))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    Ok(())
}

#[tokio::test]
async fn catalog_image_api_serves_cached_artwork() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let library_root = tempfile::tempdir()?;
    let artwork_cache = tempfile::tempdir()?;
    let identity_id = identity(550);
    let media_item_id = insert_tmdb_media_item(&db, identity_id).await?;
    let local_path = std::path::PathBuf::from("aa/bb/poster.jpg");
    insert_catalog_artwork(&db, media_item_id, identity_id, &local_path).await?;
    let cached_file = artwork_cache.path().join(&local_path);
    let cached_parent = cached_file
        .parent()
        .ok_or("cached file should have parent")?;
    tokio::fs::create_dir_all(cached_parent).await?;
    tokio::fs::write(&cached_file, b"poster-bytes").await?;
    let app = kino_server::router_with_library_root_artwork_cache_and_public_base_url(
        db,
        library_root.path(),
        artwork_cache.path(),
        "https://kino.example.test",
    );

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/library/items/{media_item_id}/images/poster"
                ))
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static("image/jpeg"))
    );
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL),
        Some(&HeaderValue::from_static(
            "public, max-age=31536000, immutable",
        ))
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    assert_eq!(bytes.as_ref(), b"poster-bytes");

    let missing_auth = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri(format!(
                    "/api/v1/library/items/{media_item_id}/images/poster"
                ))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(missing_auth.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

#[tokio::test]
async fn catalog_api_rejects_unauthenticated_list() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/library/items")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

async fn create_request(
    app: &axum::Router,
    auth_token: &str,
    message: &str,
) -> Result<RequestDetail, Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/requests")
                .bearer(auth_token)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{"target":"{message}","message":"{message}"}}"#
                )))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CREATED);
    response_json(response).await
}

async fn write_sample_video(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = tokio::process::Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("color=c=black:s=320x240:d=1")
        .arg("-pix_fmt")
        .arg("yuv420p")
        .arg(path)
        .status()
        .await?;
    if !status.success() {
        return Err(format!("ffmpeg failed with {status}").into());
    }

    Ok(())
}

async fn media_item_count(db: &kino_db::Db) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(db.read_pool())
        .await
}

async fn assert_deleted_catalog_rows(
    db: &kino_db::Db,
    media_item_id: Id,
    source_file_id: Id,
) -> Result<(), Box<dyn std::error::Error>> {
    let media_items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE id = ?1")
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;
    assert_eq!(media_items, 0);

    let source_files: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM source_files WHERE media_item_id = ?1")
            .bind(media_item_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(source_files, 0);

    let subtitle_sidecars: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM subtitle_sidecars WHERE media_item_id = ?1")
            .bind(media_item_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(subtitle_sidecars, 0);

    let transcode_outputs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transcode_outputs WHERE source_file_id = ?1")
            .bind(source_file_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(transcode_outputs, 0);

    let playback_progress: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playback_progress WHERE media_item_id = ?1")
            .bind(media_item_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(playback_progress, 0);

    let watched: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM watched WHERE media_item_id = ?1")
        .bind(media_item_id)
        .fetch_one(db.read_pool())
        .await?;
    assert_eq!(watched, 0);

    let playback_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM playback_sessions WHERE media_item_id = ?1")
            .bind(media_item_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(playback_sessions, 0);

    let fts_source: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items_fts_source WHERE media_item_id = ?1")
            .bind(media_item_id)
            .fetch_one(db.read_pool())
            .await?;
    assert_eq!(fts_source, 0);

    Ok(())
}

async fn fulfilling_request(
    service: &RequestService,
) -> Result<kino_core::Id, Box<dyn std::error::Error>> {
    let created = service
        .create(NewRequest::anonymous("Inception (2010)"))
        .await?;
    service
        .resolve_identity(
            created.request.id,
            identity(550),
            RequestIdentityProvenance::Manual,
            None,
            None,
        )
        .await?;
    service
        .transition(
            created.request.id,
            RequestTransition::StartPlanning,
            None,
            None,
        )
        .await?;
    service
        .record_plan(
            created.request.id,
            kino_fulfillment::NewFulfillmentPlan::new(
                FulfillmentPlanDecision::NeedsProvider,
                "manual import will provide a source file",
            ),
        )
        .await?;
    service
        .transition(
            created.request.id,
            RequestTransition::StartFulfilling,
            None,
            None,
        )
        .await?;

    Ok(created.request.id)
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

struct SourceFileProbeSeed<'a> {
    container: &'a str,
    video_codec: &'a str,
    video_width: u32,
    video_height: u32,
    video_hdr: Option<&'a str>,
}

async fn insert_source_file_probe(
    db: &kino_db::Db,
    source_file_id: Id,
    probe: SourceFileProbeSeed<'_>,
) -> Result<(), sqlx::Error> {
    let now = Timestamp::now();

    sqlx::query(
        r#"
        INSERT INTO source_file_probes (
            source_file_id,
            container,
            video_codec,
            video_width,
            video_height,
            video_hdr,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        "#,
    )
    .bind(source_file_id)
    .bind(probe.container)
    .bind(probe.video_codec)
    .bind(i64::from(probe.video_width))
    .bind(i64::from(probe.video_height))
    .bind(probe.video_hdr)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn insert_source_file_audio_track(
    db: &kino_db::Db,
    source_file_id: Id,
    track_index: u32,
    codec: &str,
    language: Option<&str>,
    channels: Option<u32>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO source_file_audio_tracks (
            source_file_id,
            track_index,
            codec,
            language,
            channels
        )
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
    )
    .bind(source_file_id)
    .bind(i64::from(track_index))
    .bind(codec)
    .bind(language)
    .bind(channels.map(i64::from))
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn insert_source_file_subtitle_track(
    db: &kino_db::Db,
    source_file_id: Id,
    track_index: u32,
    format: &str,
    provenance: &str,
    language: &str,
    forced: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO source_file_subtitle_tracks (
            source_file_id,
            track_index,
            format,
            provenance,
            language,
            forced
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
    )
    .bind(source_file_id)
    .bind(i64::from(track_index))
    .bind(format)
    .bind(provenance)
    .bind(language)
    .bind(if forced { 1_i64 } else { 0_i64 })
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn insert_subtitle_sidecar(
    db: &kino_db::Db,
    media_item_id: Id,
    language: &str,
    format: &str,
    provenance: &str,
    track_index: u32,
    path: &str,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();

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
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
    )
    .bind(id)
    .bind(media_item_id)
    .bind(language)
    .bind(format)
    .bind(provenance)
    .bind(i64::from(track_index))
    .bind(path)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(id)
}

async fn mark_subtitle_sidecar_forced(db: &kino_db::Db, id: Id) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE subtitle_sidecars SET forced = 1 WHERE id = ?1")
        .bind(id)
        .execute(db.write_pool())
        .await?;

    Ok(())
}

async fn insert_tmdb_media_item(
    db: &kino_db::Db,
    canonical_identity_id: CanonicalIdentityId,
) -> Result<Id, sqlx::Error> {
    let id = Id::new();
    let now = Timestamp::now();

    sqlx::query(
        r#"
        INSERT INTO canonical_identities (
            id,
            provider,
            media_kind,
            tmdb_id,
            source,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, 'manual', ?5, ?6)
        "#,
    )
    .bind(canonical_identity_id)
    .bind(canonical_identity_id.provider().as_str())
    .bind(canonical_identity_id.kind().as_str())
    .bind(i64::from(canonical_identity_id.tmdb_id().get()))
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    sqlx::query(
        r#"
        INSERT INTO media_items (
            id,
            media_kind,
            canonical_identity_id,
            season_number,
            episode_number,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
    )
    .bind(id)
    .bind(media_item_kind_for_identity(canonical_identity_id).as_str())
    .bind(canonical_identity_id)
    .bind(media_item_season_number(canonical_identity_id))
    .bind(media_item_episode_number(canonical_identity_id))
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(id)
}

async fn insert_catalog_title(
    db: &kino_db::Db,
    media_item_id: Id,
    canonical_identity_id: CanonicalIdentityId,
    title: &str,
) -> Result<(), sqlx::Error> {
    insert_catalog_title_with_release_date(db, media_item_id, canonical_identity_id, title, None)
        .await
}

async fn insert_catalog_title_with_release_date(
    db: &kino_db::Db,
    media_item_id: Id,
    canonical_identity_id: CanonicalIdentityId,
    title: &str,
    release_date: Option<&str>,
) -> Result<(), sqlx::Error> {
    let now = Timestamp::now();

    sqlx::query(
        r#"
        INSERT INTO media_metadata_cache (
            media_item_id,
            canonical_identity_id,
            provider,
            title,
            description,
            release_date,
            poster_path,
            poster_local_path,
            backdrop_path,
            backdrop_local_path,
            logo_path,
            logo_local_path,
            metadata_path,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, ?4, 'description', ?5, '', 'poster.jpg', '', 'backdrop.jpg', NULL, NULL, 'metadata.json', ?6, ?7)
        "#,
    )
    .bind(media_item_id)
    .bind(canonical_identity_id)
    .bind(canonical_identity_id.provider().as_str())
    .bind(title)
    .bind(release_date)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn insert_watched(db: &kino_db::Db, media_item_id: Id) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO watched (user_id, media_item_id, watched_at, source)
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

async fn update_catalog_release_date(
    db: &kino_db::Db,
    media_item_id: Id,
    release_date: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE media_metadata_cache SET release_date = ?1 WHERE media_item_id = ?2")
        .bind(release_date)
        .bind(media_item_id)
        .execute(db.write_pool())
        .await?;

    Ok(())
}

async fn insert_catalog_cast_member(
    db: &kino_db::Db,
    media_item_id: Id,
    position: u32,
    name: &str,
    character: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO media_metadata_cast_members (
            media_item_id,
            position,
            name,
            character,
            profile_path
        )
        VALUES (?1, ?2, ?3, ?4, NULL)
        "#,
    )
    .bind(media_item_id)
    .bind(i64::from(position))
    .bind(name)
    .bind(character)
    .execute(db.write_pool())
    .await?;

    Ok(())
}

async fn insert_catalog_artwork(
    db: &kino_db::Db,
    media_item_id: Id,
    canonical_identity_id: CanonicalIdentityId,
    poster_local_path: &std::path::Path,
) -> Result<(), sqlx::Error> {
    let now = Timestamp::now();
    let poster_local_path = poster_local_path.to_string_lossy().into_owned();

    sqlx::query(
        r#"
        INSERT INTO media_metadata_cache (
            media_item_id,
            canonical_identity_id,
            provider,
            title,
            description,
            release_date,
            poster_path,
            poster_local_path,
            backdrop_path,
            backdrop_local_path,
            logo_path,
            logo_local_path,
            metadata_path,
            created_at,
            updated_at
        )
        VALUES (?1, ?2, ?3, 'Fight Club', 'description', NULL, 'https://image.tmdb.org/poster.jpg', ?4, '', NULL, '', NULL, 'metadata.json', ?5, ?6)
        "#,
    )
    .bind(media_item_id)
    .bind(canonical_identity_id)
    .bind(canonical_identity_id.provider().as_str())
    .bind(poster_local_path)
    .bind(now)
    .bind(now)
    .execute(db.write_pool())
    .await?;

    Ok(())
}

fn identity(tmdb_id: u32) -> CanonicalIdentityId {
    match TmdbId::new(tmdb_id) {
        Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
        None => panic!("test tmdb id must be positive"),
    }
}

fn tv_identity(tmdb_id: u32) -> CanonicalIdentityId {
    match TmdbId::new(tmdb_id) {
        Some(tmdb_id) => CanonicalIdentityId::tmdb_tv_series(tmdb_id),
        None => panic!("test tmdb id must be positive"),
    }
}

fn media_item_kind_for_identity(
    canonical_identity_id: CanonicalIdentityId,
) -> kino_core::MediaItemKind {
    match canonical_identity_id.kind() {
        kino_core::CanonicalIdentityKind::Movie => kino_core::MediaItemKind::Movie,
        kino_core::CanonicalIdentityKind::TvSeries => kino_core::MediaItemKind::TvEpisode,
    }
}

fn media_item_season_number(canonical_identity_id: CanonicalIdentityId) -> Option<i64> {
    match canonical_identity_id.kind() {
        kino_core::CanonicalIdentityKind::Movie => None,
        kino_core::CanonicalIdentityKind::TvSeries => Some(1),
    }
}

fn media_item_episode_number(canonical_identity_id: CanonicalIdentityId) -> Option<i64> {
    match canonical_identity_id.kind() {
        kino_core::CanonicalIdentityKind::Movie => None,
        kino_core::CanonicalIdentityKind::TvSeries => Some(1),
    }
}

#[tokio::test]
async fn create_request_requires_target() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/requests")
                .bearer(&auth)
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    Ok(())
}

#[tokio::test]
async fn request_api_rejects_unauthenticated_create() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/v1/requests")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"target":"Inception (2010)"}"#))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    Ok(())
}

async fn response_json<T: DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
