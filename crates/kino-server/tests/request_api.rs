use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_fulfillment::{RequestDetail, RequestState};
use kino_server::ListRequestsResponse;
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
                .body(Body::from(r#"{"message":"requested from curl"}"#))?,
        )
        .await?;
    assert_eq!(create_response.status(), StatusCode::CREATED);
    let created: RequestDetail = response_json(create_response).await?;
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
    let listed: ListRequestsResponse = response_json(list_response).await?;
    assert_eq!(listed.requests.len(), 1);
    assert_eq!(listed.requests[0].id, created.request.id);
    assert_eq!(listed.requests[0].state, RequestState::Pending);

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
async fn create_request_accepts_empty_body() -> Result<(), Box<dyn std::error::Error>> {
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

    assert_eq!(response.status(), StatusCode::CREATED);
    let created: RequestDetail = response_json(response).await?;
    assert_eq!(created.request.state, RequestState::Pending);
    assert_eq!(created.status_events.len(), 1);
    assert_eq!(created.status_events[0].message, None);

    Ok(())
}

async fn response_json<T: DeserializeOwned>(
    response: axum::response::Response,
) -> Result<T, Box<dyn std::error::Error>> {
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    Ok(serde_json::from_slice(&bytes)?)
}
