use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::Id;
use kino_fulfillment::{
    NewRequest, RequestDetail, RequestIdentityProvenance, RequestListPage, RequestService,
    RequestState, RequestTransition,
};
use serde::de::DeserializeOwned;
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
    let winner_id = Id::new();

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
                    Id::new()
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
    let newer_id = Id::new();
    let older_id = Id::new();

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
    let first_identity = Id::new();
    let second_identity = Id::new();
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
