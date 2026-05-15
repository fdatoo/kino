//! TMDB discover search proxy endpoints.

use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Query, State, rejection::QueryRejection},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use kino_fulfillment::tmdb::{
    self, TmdbClient, TmdbDiscoverCandidate, TmdbDiscoverKind, TmdbDiscoverPage,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::request::ErrorResponse;

const CACHE_TTL: Duration = Duration::from_secs(60);
const CACHE_CAPACITY: usize = 256;
const RATE_LIMIT_RETRY_AFTER_SECONDS: &str = "5";
const UNCONFIGURED_RETRY_AFTER_SECONDS: &str = "60";

/// Build the TMDB discover router.
pub(crate) fn router(tmdb_client: Option<TmdbClient>) -> Router {
    Router::new()
        .route("/api/v1/discover", get(discover))
        .with_state(DiscoverState {
            tmdb_client,
            cache: DiscoverCache::new(),
        })
}

#[derive(Clone)]
pub(crate) struct DiscoverState {
    tmdb_client: Option<TmdbClient>,
    cache: DiscoverCache,
}

/// Query parameters accepted by the discover endpoint.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiscoverQuery {
    /// Search query text.
    q: String,
    /// TMDB media kind to search.
    kind: DiscoverKind,
    /// One-indexed TMDB page number. Defaults to 1.
    #[param(minimum = 1)]
    page: Option<u32>,
}

/// Discover response body.
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub(crate) struct DiscoverResponse {
    /// Search candidates returned for the requested page.
    candidates: Vec<DiscoverCandidate>,
    /// TMDB response page number.
    #[schema(minimum = 1)]
    page: u32,
    /// Whether TMDB reports additional pages after this page.
    has_more: bool,
}

/// Discover candidate returned to HTTP clients.
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
pub(crate) struct DiscoverCandidate {
    /// TMDB id for the movie or TV series.
    tmdb_id: u32,
    /// TMDB media kind.
    kind: DiscoverKind,
    /// Display title.
    title: String,
    /// Release or first-air year when TMDB provided a parseable date.
    year: Option<i32>,
    /// TMDB overview text.
    overview: Option<String>,
    /// Original-size poster image URL when TMDB provided a poster path.
    poster_url: Option<String>,
    /// Original-size backdrop image URL when TMDB provided a backdrop path.
    backdrop_url: Option<String>,
    /// TMDB popularity score.
    popularity: f64,
}

/// Media kind accepted by discover.
#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DiscoverKind {
    /// Movie candidates.
    Movie,
    /// TV series candidates.
    Series,
}

impl Hash for DiscoverKind {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Movie => "movie".hash(state),
            Self::Series => "series".hash(state),
        }
    }
}

impl From<DiscoverKind> for TmdbDiscoverKind {
    fn from(kind: DiscoverKind) -> Self {
        match kind {
            DiscoverKind::Movie => Self::Movie,
            DiscoverKind::Series => Self::Series,
        }
    }
}

impl From<TmdbDiscoverKind> for DiscoverKind {
    fn from(kind: TmdbDiscoverKind) -> Self {
        match kind {
            TmdbDiscoverKind::Movie => Self::Movie,
            TmdbDiscoverKind::Series => Self::Series,
        }
    }
}

impl From<TmdbDiscoverPage> for DiscoverResponse {
    fn from(page: TmdbDiscoverPage) -> Self {
        Self {
            candidates: page.candidates.into_iter().map(Into::into).collect(),
            page: page.page,
            has_more: page.has_more,
        }
    }
}

impl From<TmdbDiscoverCandidate> for DiscoverCandidate {
    fn from(candidate: TmdbDiscoverCandidate) -> Self {
        Self {
            tmdb_id: candidate.tmdb_id,
            kind: candidate.kind.into(),
            title: candidate.title,
            year: candidate.year,
            overview: candidate.overview,
            poster_url: candidate.poster_url.map(Into::into),
            backdrop_url: candidate.backdrop_url.map(Into::into),
            popularity: candidate.popularity,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    query: String,
    kind: DiscoverKind,
    page: u32,
}

#[derive(Debug, Clone, Default)]
struct DiscoverCache {
    inner: Arc<Mutex<HashMap<CacheKey, (Instant, TmdbDiscoverPage)>>>,
}

impl DiscoverCache {
    fn new() -> Self {
        Self::default()
    }

    async fn lookup(&self, key: &CacheKey) -> Option<TmdbDiscoverPage> {
        let mut entries = self.inner.lock().await;
        let (stored_at, page) = entries.get(key)?;

        if stored_at.elapsed() >= CACHE_TTL {
            entries.remove(key);
            return None;
        }

        Some(page.clone())
    }

    async fn store(&self, key: CacheKey, value: TmdbDiscoverPage) {
        let mut entries = self.inner.lock().await;
        if entries.len() >= CACHE_CAPACITY && !entries.contains_key(&key) {
            // Typeahead should stay bounded even if clients send many distinct prefixes.
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, (stored_at, _))| *stored_at)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }

        entries.insert(key, (Instant::now(), value));
    }
}

/// Return TMDB candidates for a client search query.
#[utoipa::path(
    get,
    path = "/api/v1/discover",
    tag = "discover",
    params(DiscoverQuery),
    responses(
        (status = 200, description = "TMDB candidates", body = DiscoverResponse),
        (status = 400, description = "Invalid discover query", body = ErrorResponse),
        (status = 401, description = "Bearer token missing or invalid", body = ErrorResponse),
        (status = 500, description = "Discover configuration failed", body = ErrorResponse),
        (status = 502, description = "TMDB upstream failed", body = ErrorResponse),
        (status = 503, description = "TMDB unavailable or rate limited", body = ErrorResponse)
    )
)]
pub(crate) async fn discover(
    State(state): State<DiscoverState>,
    query: Result<Query<DiscoverQuery>, QueryRejection>,
) -> DiscoverResult<Json<DiscoverResponse>> {
    let Query(query) = query.map_err(|err| DiscoverApiError::InvalidQuery(err.to_string()))?;
    let page = query.page.unwrap_or(1);
    let normalized_query = query.q.trim().to_lowercase();
    if normalized_query.is_empty() {
        return Err(DiscoverApiError::Tmdb(tmdb::Error::EmptyQuery));
    }
    if page == 0 {
        return Err(DiscoverApiError::Tmdb(tmdb::Error::InvalidPage { page }));
    }

    let key = CacheKey {
        query: normalized_query,
        kind: query.kind,
        page,
    };
    if let Some(cached_page) = state.cache.lookup(&key).await {
        tracing::debug!(
            kind = ?query.kind,
            page = cached_page.page,
            cache_hit = true,
            "discover query handled"
        );
        return Ok(Json(cached_page.into()));
    }

    tracing::debug!(kind = ?query.kind, page, cache_hit = false, "discover query handled");
    let tmdb = state
        .tmdb_client
        .as_ref()
        .ok_or(DiscoverApiError::TmdbNotConfigured)?;
    let page = tmdb.discover(&query.q, query.kind.into(), page).await?;
    state.cache.store(key, page.clone()).await;

    Ok(Json(page.into()))
}

pub(crate) type DiscoverResult<T> = std::result::Result<T, DiscoverApiError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum DiscoverApiError {
    #[error("invalid discover query: {0}")]
    InvalidQuery(String),

    #[error("tmdb client is not configured")]
    TmdbNotConfigured,

    #[error(transparent)]
    Tmdb(#[from] tmdb::Error),
}

impl IntoResponse for DiscoverApiError {
    fn into_response(self) -> Response {
        let (status, retry_after) = match &self {
            Self::InvalidQuery(_) => (StatusCode::BAD_REQUEST, None),
            Self::TmdbNotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                Some(UNCONFIGURED_RETRY_AFTER_SECONDS),
            ),
            Self::Tmdb(tmdb::Error::HttpStatus { status, .. })
                if *status == StatusCode::TOO_MANY_REQUESTS =>
            {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some(RATE_LIMIT_RETRY_AFTER_SECONDS),
                )
            }
            Self::Tmdb(tmdb::Error::RateLimited { .. }) => (
                StatusCode::SERVICE_UNAVAILABLE,
                Some(RATE_LIMIT_RETRY_AFTER_SECONDS),
            ),
            Self::Tmdb(tmdb::Error::HttpStatus { .. } | tmdb::Error::Request(_)) => {
                (StatusCode::BAD_GATEWAY, None)
            }
            Self::Tmdb(
                tmdb::Error::InvalidResponse { .. }
                | tmdb::Error::InvalidRetryAfter { .. }
                | tmdb::Error::InvalidPage { .. }
                | tmdb::Error::EmptyQuery
                | tmdb::Error::InvalidYear { .. },
            ) => (StatusCode::BAD_REQUEST, None),
            Self::Tmdb(
                tmdb::Error::MissingApiKey
                | tmdb::Error::EmptyApiKey
                | tmdb::Error::InvalidRequestRate
                | tmdb::Error::InvalidBaseUrl { .. }
                | tmdb::Error::InvalidRequestPath { .. },
            ) => {
                tracing::error!(error = %self, "discover api failed");
                (StatusCode::INTERNAL_SERVER_ERROR, None)
            }
        };

        let mut response = (
            status,
            Json(ErrorResponse {
                error: self.to_string(),
            }),
        )
            .into_response();
        if let Some(retry_after) = retry_after {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                axum::http::HeaderValue::from_static(retry_after),
            );
        }

        response
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{
        collections::VecDeque,
        num::NonZeroU32,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use axum::{
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use kino_core::user::SEEDED_USER_ID;
    use kino_fulfillment::tmdb::TmdbClientConfig;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::Mutex as AsyncMutex,
    };
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn happy_path_returns_tmdb_candidates() -> Result<(), Box<dyn std::error::Error>> {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"page":1,"total_pages":2,"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","overview":"A dream heist.","poster_path":"/poster.jpg","backdrop_path":"/backdrop.jpg","popularity":83.1}]}"#,
        )])
        .await;
        let app = authenticated_app(Some(test_client(&server))).await?;

        let response = app
            .router
            .oneshot(authed_request(
                "/api/v1/discover?q=Inception&kind=movie&page=1",
                &app.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        let body: DiscoverResponse = read_json(response).await?;
        assert_eq!(body.page, 1);
        assert!(body.has_more);
        assert_eq!(body.candidates.len(), 1);
        assert_eq!(body.candidates[0].tmdb_id, 27205);
        assert_eq!(body.candidates[0].kind, DiscoverKind::Movie);
        assert_eq!(
            body.candidates[0].poster_url.as_deref(),
            Some("https://image.tmdb.org/t/p/original/poster.jpg")
        );
        assert_eq!(server.requests().await.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn cache_hits_and_misses_by_query_kind_and_page() -> Result<(), Box<dyn std::error::Error>>
    {
        let server = TestServer::new(vec![
            TestResponse::json(
                StatusCode::OK,
                r#"{"page":1,"total_pages":2,"results":[{"id":1,"title":"One","release_date":"2001-01-01","overview":null,"poster_path":null,"backdrop_path":null,"popularity":1.0}]}"#,
            ),
            TestResponse::json(
                StatusCode::OK,
                r#"{"page":2,"total_pages":2,"results":[{"id":2,"title":"Two","release_date":"2002-01-01","overview":null,"poster_path":null,"backdrop_path":null,"popularity":2.0}]}"#,
            ),
        ])
        .await;
        let app = authenticated_app(Some(test_client(&server))).await?;

        let first = app
            .router
            .clone()
            .oneshot(authed_request(
                "/api/v1/discover?q=Inception&kind=movie&page=1",
                &app.bearer,
            )?)
            .await?;
        assert_eq!(first.status(), StatusCode::OK);

        let second = app
            .router
            .clone()
            .oneshot(authed_request(
                "/api/v1/discover?q=inception&kind=movie&page=1",
                &app.bearer,
            )?)
            .await?;
        assert_eq!(second.status(), StatusCode::OK);

        let third = app
            .router
            .oneshot(authed_request(
                "/api/v1/discover?q=Inception&kind=movie&page=2",
                &app.bearer,
            )?)
            .await?;
        assert_eq!(third.status(), StatusCode::OK);

        assert_eq!(server.requests().await.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn upstream_failure_returns_bad_gateway() -> Result<(), Box<dyn std::error::Error>> {
        let server = TestServer::new(vec![TestResponse::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "oops",
        )])
        .await;
        let app = authenticated_app(Some(test_client(&server))).await?;

        let response = app
            .router
            .oneshot(authed_request(
                "/api/v1/discover?q=Inception&kind=movie",
                &app.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        Ok(())
    }

    #[tokio::test]
    async fn rate_limit_returns_service_unavailable_with_retry_after()
    -> Result<(), Box<dyn std::error::Error>> {
        let server = TestServer::new(vec![
            TestResponse::new(StatusCode::TOO_MANY_REQUESTS, "retry later")
                .with_header("Retry-After", "0"),
            TestResponse::new(StatusCode::TOO_MANY_REQUESTS, "retry later"),
        ])
        .await;
        let app = authenticated_app(Some(test_client(&server))).await?;

        let response = app
            .router
            .oneshot(authed_request(
                "/api/v1/discover?q=Inception&kind=movie",
                &app.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&axum::http::HeaderValue::from_static(
                RATE_LIMIT_RETRY_AFTER_SECONDS
            ))
        );
        Ok(())
    }

    #[tokio::test]
    async fn missing_bearer_returns_unauthorized() -> Result<(), Box<dyn std::error::Error>> {
        let server = TestServer::new(vec![]).await;
        let db = kino_db::test_db().await?;
        let app = crate::router_with_tmdb_client(db, test_client(&server));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/discover?q=Inception&kind=movie")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(server.requests().await.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn empty_query_returns_bad_request() -> Result<(), Box<dyn std::error::Error>> {
        let server = TestServer::new(vec![]).await;
        let app = authenticated_app(Some(test_client(&server))).await?;

        let response = app
            .router
            .oneshot(authed_request(
                "/api/v1/discover?q=%20&kind=movie",
                &app.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(server.requests().await.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn invalid_kind_returns_bad_request() -> Result<(), Box<dyn std::error::Error>> {
        let server = TestServer::new(vec![]).await;
        let app = authenticated_app(Some(test_client(&server))).await?;

        let response = app
            .router
            .oneshot(authed_request(
                "/api/v1/discover?q=Inception&kind=show",
                &app.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(server.requests().await.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn no_client_configured_returns_service_unavailable_with_retry_after()
    -> Result<(), Box<dyn std::error::Error>> {
        let app = authenticated_app(None).await?;

        let response = app
            .router
            .oneshot(authed_request(
                "/api/v1/discover?q=Inception&kind=movie",
                &app.bearer,
            )?)
            .await?;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers().get(header::RETRY_AFTER),
            Some(&axum::http::HeaderValue::from_static(
                UNCONFIGURED_RETRY_AFTER_SECONDS
            ))
        );
        Ok(())
    }

    struct AuthenticatedApp {
        router: Router,
        bearer: String,
    }

    async fn authenticated_app(
        tmdb_client: Option<TmdbClient>,
    ) -> Result<AuthenticatedApp, Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let minted = crate::token::mint_device_token(&db, SEEDED_USER_ID, "Discover test").await?;
        let router = match tmdb_client {
            Some(tmdb_client) => crate::router_with_tmdb_client(db, tmdb_client),
            None => crate::router(db),
        };

        Ok(AuthenticatedApp {
            router,
            bearer: minted.plaintext,
        })
    }

    fn authed_request(
        uri: &str,
        bearer: &str,
    ) -> Result<Request<Body>, Box<dyn std::error::Error>> {
        Ok(Request::builder()
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .body(Body::empty())?)
    }

    async fn read_json<T: serde::de::DeserializeOwned>(
        response: Response,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let bytes = to_bytes(response.into_body(), usize::MAX).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn test_client(server: &TestServer) -> TmdbClient {
        let config = TmdbClientConfig::new("test-api-key")
            .unwrap()
            .with_base_url(&server.base_url())
            .unwrap()
            .with_max_requests_per_second(NonZeroU32::new(50).unwrap())
            .with_max_retries(0);
        TmdbClient::new(config)
    }

    #[derive(Clone, Debug)]
    struct TestResponse {
        status: StatusCode,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl TestResponse {
        fn new(status: StatusCode, body: &str) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.to_owned(),
            }
        }

        fn json(status: StatusCode, body: &str) -> Self {
            Self::new(status, body).with_header("Content-Type", "application/json")
        }

        fn with_header(mut self, name: &str, value: &str) -> Self {
            self.headers.push((name.to_owned(), value.to_owned()));
            self
        }
    }

    #[derive(Debug)]
    struct TestServer {
        addr: std::net::SocketAddr,
        requests: Arc<AsyncMutex<Vec<String>>>,
        request_count: Arc<AtomicUsize>,
    }

    impl TestServer {
        async fn new(responses: Vec<TestResponse>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let responses = Arc::new(AsyncMutex::new(VecDeque::from(responses)));
            let requests = Arc::new(AsyncMutex::new(Vec::new()));
            let request_count = Arc::new(AtomicUsize::new(0));

            tokio::spawn(serve(
                listener,
                responses,
                requests.clone(),
                request_count.clone(),
            ));

            Self {
                addr,
                requests,
                request_count,
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}/3", self.addr)
        }

        async fn requests(&self) -> Vec<String> {
            let request_count = self.request_count.load(Ordering::SeqCst);
            let requests = self.requests.lock().await;
            assert_eq!(request_count, requests.len());
            requests.clone()
        }
    }

    async fn serve(
        listener: TcpListener,
        responses: Arc<AsyncMutex<VecDeque<TestResponse>>>,
        requests: Arc<AsyncMutex<Vec<String>>>,
        request_count: Arc<AtomicUsize>,
    ) {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let responses = responses.clone();
            let requests = requests.clone();
            let request_count = request_count.clone();
            tokio::spawn(async move {
                handle_connection(stream, responses, requests, request_count).await;
            });
        }
    }

    async fn handle_connection(
        mut stream: TcpStream,
        responses: Arc<AsyncMutex<VecDeque<TestResponse>>>,
        requests: Arc<AsyncMutex<Vec<String>>>,
        request_count: Arc<AtomicUsize>,
    ) {
        let mut buffer = [0_u8; 4096];
        let mut received = Vec::new();
        loop {
            let bytes_read = stream.read(&mut buffer).await.unwrap();
            if bytes_read == 0 {
                return;
            }
            received.extend_from_slice(&buffer[..bytes_read]);
            if received.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let request = String::from_utf8(received).unwrap();
        let request_target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap()
            .to_owned();
        requests.lock().await.push(request_target);
        request_count.fetch_add(1, Ordering::SeqCst);

        let response =
            responses.lock().await.pop_front().unwrap_or_else(|| {
                TestResponse::new(StatusCode::INTERNAL_SERVER_ERROR, "no response")
            });
        let reason = response.status.canonical_reason().unwrap_or("status");
        let mut raw = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            response.status.as_u16(),
            reason,
            response.body.len()
        );
        for (name, value) in response.headers {
            raw.push_str(&format!("{name}: {value}\r\n"));
        }
        raw.push_str("\r\n");
        raw.push_str(&response.body);
        stream.write_all(raw.as_bytes()).await.unwrap();
    }
}
