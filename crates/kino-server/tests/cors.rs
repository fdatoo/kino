use std::path::Path;

use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{
    Config, LibraryConfig, OcrConfig,
    config::{LogFormat, ProvidersConfig, ServerConfig, TmdbConfig},
};
use tower::util::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn permissive_cors_handles_openapi_preflight() -> TestResult {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(preflight_request(
            "/api/openapi.json",
            "http://localhost:3000",
        )?)
        .await?;

    assert!(matches!(
        response.status(),
        StatusCode::OK | StatusCode::NO_CONTENT
    ));
    assert_eq!(
        header_value(response.headers(), header::ACCESS_CONTROL_ALLOW_ORIGIN)?,
        "*"
    );
    assert_header_contains(
        response.headers(),
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        "authorization",
    )?;
    assert_header_contains(
        response.headers(),
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "GET",
    )?;
    assert_header_contains(
        response.headers(),
        header::ACCESS_CONTROL_ALLOW_METHODS,
        "POST",
    )?;
    assert_eq!(
        header_value(response.headers(), header::ACCESS_CONTROL_MAX_AGE)?,
        "600"
    );

    Ok(())
}

#[tokio::test]
async fn permissive_cors_handles_protected_api_preflight_without_auth() -> TestResult {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(preflight_request(
            "/api/v1/library/items",
            "http://localhost:3000",
        )?)
        .await?;

    assert!(matches!(
        response.status(),
        StatusCode::OK | StatusCode::NO_CONTENT
    ));
    assert_eq!(
        header_value(response.headers(), header::ACCESS_CONTROL_ALLOW_ORIGIN)?,
        "*"
    );

    Ok(())
}

#[tokio::test]
async fn permissive_cors_marks_cross_origin_openapi_get() -> TestResult {
    let db = kino_db::test_db().await?;
    let app = kino_server::router(db);

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/openapi.json")
                .header(header::ORIGIN, "http://localhost:3000")
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_value(response.headers(), header::ACCESS_CONTROL_ALLOW_ORIGIN)?,
        "*"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    assert!(
        !bytes.is_empty(),
        "OpenAPI response body should not be empty"
    );

    Ok(())
}

#[tokio::test]
async fn restricted_cors_allows_matching_origin_preflight() -> TestResult {
    let temp = tempfile::tempdir()?;
    let db = kino_db::test_db().await?;
    let app = kino_server::router_with_config(
        db,
        config_with_cors(temp.path(), vec!["http://example.com".to_owned()]),
    );

    let response = app
        .oneshot(preflight_request(
            "/api/openapi.json",
            "http://example.com",
        )?)
        .await?;

    assert!(matches!(
        response.status(),
        StatusCode::OK | StatusCode::NO_CONTENT
    ));
    assert_eq!(
        header_value(response.headers(), header::ACCESS_CONTROL_ALLOW_ORIGIN)?,
        "http://example.com"
    );

    Ok(())
}

#[tokio::test]
async fn restricted_cors_omits_allow_origin_for_other_preflight() -> TestResult {
    let temp = tempfile::tempdir()?;
    let db = kino_db::test_db().await?;
    let app = kino_server::router_with_config(
        db,
        config_with_cors(temp.path(), vec!["http://example.com".to_owned()]),
    );

    let response = app
        .oneshot(preflight_request("/api/openapi.json", "http://other.com")?)
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .is_none(),
        "disallowed preflight should not include access-control-allow-origin"
    );

    Ok(())
}

fn preflight_request(uri: &str, origin: &str) -> Result<HttpRequest<Body>, axum::http::Error> {
    HttpRequest::builder()
        .method("OPTIONS")
        .uri(uri)
        .header(header::ORIGIN, origin)
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .header(
            header::ACCESS_CONTROL_REQUEST_HEADERS,
            "Authorization, Content-Type",
        )
        .body(Body::empty())
}

fn config_with_cors(temp: &Path, cors_allowed_origins: Vec<String>) -> Config {
    Config {
        database_path: temp.join("kino.db"),
        library_root: temp.join("library"),
        library: LibraryConfig::default(),
        server: ServerConfig {
            cors_allowed_origins,
            ..ServerConfig::default()
        },
        tmdb: TmdbConfig::default(),
        ocr: OcrConfig::default(),
        providers: ProvidersConfig::default(),
        log_level: "info".to_owned(),
        log_format: LogFormat::Pretty,
    }
}

fn header_value(
    headers: &axum::http::HeaderMap,
    name: header::HeaderName,
) -> Result<&str, Box<dyn std::error::Error>> {
    Ok(headers
        .get(name)
        .ok_or("expected response header missing")?
        .to_str()?)
}

fn assert_header_contains(
    headers: &axum::http::HeaderMap,
    name: header::HeaderName,
    needle: &str,
) -> TestResult {
    let value = header_value(headers, name)?.to_ascii_lowercase();
    assert!(value.contains(&needle.to_ascii_lowercase()), "got: {value}");
    Ok(())
}
