use axum::{
    body::{Body, to_bytes},
    http::{Request as HttpRequest, StatusCode, header},
};
use kino_core::{
    Config,
    config::{LogFormat, ServerConfig, TmdbConfig},
};
use serde_json::Value;
use tower::util::ServiceExt;

mod common;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn config_api_returns_resolved_config_with_masked_secrets() -> TestResult {
    let temp = tempfile::tempdir()?;
    let library_root = temp.path().join("library");
    std::fs::create_dir(&library_root)?;

    let db = kino_db::test_db().await?;
    let auth = common::issued_token(&db).await?;
    let app = kino_server::router_with_config(
        db,
        Config {
            database_path: temp.path().join("kino.db"),
            library_root: library_root.clone(),
            library: kino_core::LibraryConfig::default(),
            server: ServerConfig {
                public_base_url: "https://kino.example.test".to_owned(),
                cors_allowed_origins: vec!["https://tools.example.test".to_owned()],
                ..ServerConfig::default()
            },
            tmdb: TmdbConfig {
                api_key: Some("super-secret-tmdb-key".to_owned()),
                max_requests_per_second: 7,
            },
            ocr: kino_core::OcrConfig::default(),
            providers: kino_core::config::ProvidersConfig::default(),
            log_level: "debug,kino=trace".to_owned(),
            log_format: LogFormat::Json,
        },
    );

    let response = app
        .oneshot(
            HttpRequest::builder()
                .method("GET")
                .uri("/api/v1/admin/config")
                .header(header::AUTHORIZATION, common::bearer(&auth))
                .body(Body::empty())?,
        )
        .await?;

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX).await?;
    let body: Value = serde_json::from_slice(&bytes)?;

    assert_eq!(
        body.pointer("/library/root/value"),
        Some(&Value::String(library_root.display().to_string()))
    );
    assert_eq!(
        body.pointer("/tmdb/api_key/value"),
        Some(&Value::String("***".to_owned()))
    );
    assert_eq!(
        body.pointer("/tmdb/max_requests_per_second/value"),
        Some(&Value::Number(7.into()))
    );
    assert_eq!(
        body.pointer("/log/format/value"),
        Some(&Value::String("json".to_owned()))
    );
    assert_eq!(
        body.pointer("/server/cors_allowed_origins/value/0"),
        Some(&Value::String("https://tools.example.test".to_owned()))
    );
    assert!(
        !std::str::from_utf8(&bytes)?.contains("super-secret-tmdb-key"),
        "config response must not expose secret values"
    );
    assert!(
        body.pointer("/tmdb/api_key/source").is_some(),
        "config values include source provenance"
    );

    Ok(())
}
