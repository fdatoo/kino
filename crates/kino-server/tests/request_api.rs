use axum::{
    body::{Body, to_bytes},
    http::{HeaderValue, Request as HttpRequest, StatusCode, header},
};
use kino_core::{CanonicalIdentityId, Id, Timestamp, TmdbId};
use kino_fulfillment::{
    FulfillmentPlanDecision, NewRequest, RequestDetail, RequestIdentityProvenance, RequestListPage,
    RequestService, RequestState, RequestTransition,
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

#[tokio::test]
async fn openapi_json_serves_valid_spec() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router_with_public_base_url(db, "https://kino.example.test");

    let response = app
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
    assert!(
        body["paths"]
            .get("/api/v1/library/items/{id}/images/{kind}")
            .is_some()
    );
    assert!(body["paths"].get("/api/v1/admin/tokens").is_some());
    assert!(body["paths"].get("/api/v1/playback/progress").is_some());
    assert!(
        body["paths"]["/api/v1/admin/tokens/{token_id}"]
            .get("delete")
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
    let app = kino_server::router(db);

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
    let app = kino_server::router(db);

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
async fn request_match_api_resolves_high_confidence_match() -> Result<(), Box<dyn std::error::Error>>
{
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db);
    let created = create_request(&app, &auth, "Inception (2010)").await?;
    let winner_id = identity(550);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/matches", created.request.id))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{
                        "message":"matched canonical media",
                        "candidates":[
                            {{
                                "canonical_identity_id":"{winner_id}",
                                "title":"Inception",
                                "year":2010,
                                "popularity":80.0
                            }},
                            {{
                                "canonical_identity_id":"{}",
                                "title":"Interstellar",
                                "year":2014,
                                "popularity":70.0
                            }}
                        ]
                    }}"#,
                    identity(157_336)
                )))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let resolved: RequestDetail = response_json(response).await?;
    assert_eq!(resolved.request.state, RequestState::Resolved);
    assert_eq!(
        resolved.request.target.canonical_identity_id,
        Some(winner_id)
    );
    assert!(resolved.candidates.is_empty());

    Ok(())
}

#[tokio::test]
async fn request_match_api_parks_low_confidence_match_with_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router(db);
    let created = create_request(&app, &auth, "Dune").await?;
    let newer_id = identity(438_631);
    let older_id = identity(841);

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/v1/requests/{}/matches", created.request.id))
                .bearer(&auth)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(
                    r#"{{
                        "message":"needs user choice",
                        "candidates":[
                            {{
                                "canonical_identity_id":"{older_id}",
                                "title":"Dune",
                                "year":1984,
                                "popularity":60.0
                            }},
                            {{
                                "canonical_identity_id":"{newer_id}",
                                "title":"Dune",
                                "year":2021,
                                "popularity":90.0
                            }}
                        ]
                    }}"#
                )))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let parked: RequestDetail = response_json(response).await?;
    assert_eq!(parked.request.state, RequestState::NeedsDisambiguation);
    assert_eq!(parked.candidates.len(), 2);
    assert_eq!(parked.candidates[0].canonical_identity_id, newer_id);

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
    assert_eq!(fetched.candidates, parked.candidates);

    Ok(())
}

#[tokio::test]
async fn re_resolution_api_records_versioned_identity_history()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
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
async fn manual_import_api_accepts_readable_file_and_starts_ingesting()
-> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
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
    assert_eq!(detail.request.state, RequestState::Ingesting);
    assert_eq!(
        detail
            .status_events
            .last()
            .and_then(|event| event.message.as_deref()),
        Some(expected_message.as_str())
    );

    tokio::fs::remove_file(path).await?;
    Ok(())
}

#[tokio::test]
async fn manual_import_api_surfaces_missing_path() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
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

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response_json(response).await?;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("path_not_found")),
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
    assert_eq!(detail.request.state, RequestState::Fulfilling);

    Ok(())
}

#[tokio::test]
async fn manual_import_api_rejects_invalid_request_state() -> Result<(), Box<dyn std::error::Error>>
{
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
    insert_source_file(&db, matrix, &matrix_path).await?;
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
        VALUES (?1, ?2, ?3, ?4, 'description', NULL, '', 'poster.jpg', '', 'backdrop.jpg', NULL, NULL, 'metadata.json', ?5, ?6)
        "#,
    )
    .bind(media_item_id)
    .bind(canonical_identity_id)
    .bind(canonical_identity_id.provider().as_str())
    .bind(title)
    .bind(now)
    .bind(now)
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
