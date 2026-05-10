use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{CanonicalIdentityId, TmdbId};
use kino_fulfillment::{
    FulfillmentPlanDecision, NewRequest, RequestDetail, RequestIdentityProvenance, RequestListPage,
    RequestService, RequestState, RequestTransition,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use tower::util::ServiceExt;

#[tokio::test]
async fn request_api_exercises_happy_path_end_to_end() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let create_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/requests")
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
                .uri(format!("/api/requests/{}", created.request.id))
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
                .uri("/api/requests")
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
                .uri(format!("/api/requests/{}", created.request.id))
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
    let app = kino_server::router(db);

    let first = create_request(&app, "first").await?;
    let second = create_request(&app, "second").await?;
    create_request(&app, "third").await?;

    let first_page_response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/requests?state=pending&limit=2&offset=0")
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
                .uri("/api/requests?limit=0")
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
    let app = kino_server::router(db);
    let created = create_request(&app, "Inception (2010)").await?;
    let winner_id = identity(550);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/requests/{}/matches", created.request.id))
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
    let app = kino_server::router(db);
    let created = create_request(&app, "Dune").await?;
    let newer_id = identity(438_631);
    let older_id = identity(841);

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/requests/{}/matches", created.request.id))
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
                .uri(format!("/api/requests/{}", created.request.id))
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
                    "/api/requests/{}/re-resolution",
                    created.request.id
                ))
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
                .uri(format!("/api/requests/{}/plans", created.request.id))
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
                .uri(format!("/api/requests/{}/plans", created.request.id))
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
                .uri(format!("/api/requests/{}", created.request.id))
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
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
    let request = fulfilling_request(&service).await?;
    let path = std::env::temp_dir().join(format!("kino-manual-import-api-{}.mkv", request));
    tokio::fs::write(&path, b"movie").await?;

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/admin/requests/{request}/manual-import"))
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
    let service = RequestService::new(db.clone());
    let app = kino_server::router(db);
    let request = fulfilling_request(&service).await?;
    let path = std::env::temp_dir().join(format!("kino-manual-import-missing-{request}.mkv"));

    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri(format!("/api/admin/requests/{request}/manual-import"))
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
                .uri(format!("/api/requests/{request}"))
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
    let app = kino_server::router(db);
    let created = create_request(&app, "Inception (2010)").await?;
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
                    "/api/admin/requests/{}/manual-import",
                    created.request.id
                ))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!(r#"{{"path":"{}"}}"#, path.display())))?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::CONFLICT);

    tokio::fs::remove_file(path).await?;
    Ok(())
}

async fn create_request(
    app: &axum::Router,
    message: &str,
) -> Result<RequestDetail, Box<dyn std::error::Error>> {
    let response = app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/requests")
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

fn identity(tmdb_id: u32) -> CanonicalIdentityId {
    match TmdbId::new(tmdb_id) {
        Some(tmdb_id) => CanonicalIdentityId::tmdb_movie(tmdb_id),
        None => panic!("test tmdb id must be positive"),
    }
}

#[tokio::test]
async fn create_request_requires_target() -> Result<(), Box<dyn std::error::Error>> {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("POST")
                .uri("/api/requests")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);

    Ok(())
}

async fn response_json<T: DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
