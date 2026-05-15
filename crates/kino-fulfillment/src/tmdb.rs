//! TMDB HTTP client for request resolution.

use std::{collections::HashMap, num::NonZeroU32, sync::Arc, time::Duration};

use kino_core::{CanonicalIdentityId, CanonicalIdentityKind};
use kino_library::{
    MetadataAsset, MetadataCastMember, MetadataFuture, TmdbMetadata, TmdbMetadataProvider,
};
use reqwest::{StatusCode, Url, header};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};

use crate::{
    movie::{TmdbMovieId, TmdbMovieSearchResult, release_year_from_date},
    tv::{TmdbSeriesId, TmdbTvSearchResult, first_air_year_from_date},
};

const DEFAULT_BASE_URL: &str = "https://api.themoviedb.org/3/";
const DEFAULT_IMAGE_BASE_URL: &str = "https://image.tmdb.org/t/p/original/";
const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_MAX_REQUESTS_PER_SECOND: NonZeroU32 = match NonZeroU32::new(20) {
    Some(value) => value,
    None => unreachable!(),
};

/// Errors produced by the TMDB HTTP client.
#[derive(Debug, Error)]
pub enum Error {
    /// The TMDB client cannot authenticate without an API key.
    #[error("tmdb api key is missing")]
    MissingApiKey,

    /// The TMDB API key was configured as an empty string.
    #[error("tmdb api key is empty")]
    EmptyApiKey,

    /// A search query did not contain usable text.
    #[error("tmdb search query is empty")]
    EmptyQuery,

    /// A year parameter cannot be represented by TMDB search endpoints.
    #[error("tmdb year {year} is invalid")]
    InvalidYear {
        /// Invalid year value.
        year: i32,
    },

    /// A page parameter cannot be represented by TMDB search endpoints.
    #[error("tmdb page {page} is invalid")]
    InvalidPage {
        /// Invalid page value.
        page: u32,
    },

    /// A configured URL could not be parsed.
    #[error("invalid tmdb base url {value}: {source}")]
    InvalidBaseUrl {
        /// Configured base URL value.
        value: String,
        /// URL parse failure.
        #[source]
        source: url::ParseError,
    },

    /// The configured request rate was zero.
    #[error("tmdb max_requests_per_second must be positive")]
    InvalidRequestRate,

    /// A request URL could not be built from the configured base URL.
    #[error("invalid tmdb request path {path}: {source}")]
    InvalidRequestPath {
        /// Relative request path.
        path: String,
        /// URL parse failure.
        #[source]
        source: url::ParseError,
    },

    /// The HTTP request failed before a response was available.
    #[error("tmdb http request failed: {0}")]
    Request(#[from] reqwest::Error),

    /// TMDB returned a non-success HTTP response.
    #[error("tmdb request failed with status {status}: {body}")]
    HttpStatus {
        /// HTTP status code returned by TMDB.
        status: StatusCode,
        /// Response body returned by TMDB.
        body: String,
    },

    /// TMDB kept rate limiting after all retry attempts were used.
    #[error("tmdb request remained rate limited after {attempts} attempts")]
    RateLimited {
        /// Number of attempts made, including the initial request.
        attempts: u32,
    },

    /// TMDB returned a malformed `Retry-After` header.
    #[error("tmdb retry-after header is invalid: {value}")]
    InvalidRetryAfter {
        /// Header value returned by TMDB.
        value: String,
    },

    /// TMDB returned a response that could not be mapped to Kino types.
    #[error("tmdb response is invalid: {reason}")]
    InvalidResponse {
        /// Human-readable validation failure.
        reason: &'static str,
    },
}

/// Result alias for TMDB client operations.
pub type Result<T> = std::result::Result<T, Error>;

/// TMDB HTTP client configuration.
#[derive(Debug, Clone)]
pub struct TmdbClientConfig {
    /// TMDB v3 API key used for application-level authentication.
    pub api_key: String,
    /// Base API URL. Defaults to `https://api.themoviedb.org/3/`.
    pub base_url: Url,
    /// Base URL for original-size image assets.
    pub image_base_url: Url,
    /// Maximum request rate enforced by this client instance.
    pub max_requests_per_second: NonZeroU32,
    /// Maximum number of retries after `429 Too Many Requests`.
    pub max_retries: u32,
}

impl TmdbClientConfig {
    /// Construct TMDB client configuration with production defaults.
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into().trim().to_owned();
        if api_key.is_empty() {
            return Err(Error::EmptyApiKey);
        }

        let base_url = parse_base_url(DEFAULT_BASE_URL)?;
        let image_base_url = parse_base_url(DEFAULT_IMAGE_BASE_URL)?;
        Ok(Self {
            api_key,
            base_url,
            image_base_url,
            max_requests_per_second: DEFAULT_MAX_REQUESTS_PER_SECOND,
            max_retries: DEFAULT_MAX_RETRIES,
        })
    }

    /// Construct TMDB client configuration from Kino's core config.
    pub fn from_core(config: &kino_core::config::TmdbConfig) -> Result<Self> {
        let api_key = config.api_key.clone().ok_or(Error::MissingApiKey)?;
        let request_rate =
            NonZeroU32::new(config.max_requests_per_second).ok_or(Error::InvalidRequestRate)?;
        Ok(Self::new(api_key)?.with_max_requests_per_second(request_rate))
    }

    /// Return configuration using a different base URL.
    pub fn with_base_url(mut self, value: &str) -> Result<Self> {
        self.base_url = parse_base_url(value)?;
        Ok(self)
    }

    /// Return configuration using a different image asset base URL.
    pub fn with_image_base_url(mut self, value: &str) -> Result<Self> {
        self.image_base_url = parse_base_url(value)?;
        Ok(self)
    }

    /// Return configuration using a different maximum request rate.
    pub fn with_max_requests_per_second(mut self, value: NonZeroU32) -> Self {
        self.max_requests_per_second = value;
        self
    }

    /// Return configuration using a different retry budget.
    pub const fn with_max_retries(mut self, value: u32) -> Self {
        self.max_retries = value;
        self
    }
}

/// Top-level TMDB movie details used by fulfillment.
#[derive(Debug, Clone, PartialEq)]
pub struct TmdbMovieDetails {
    /// TMDB movie id.
    pub movie_id: TmdbMovieId,
    /// TMDB display title.
    pub title: String,
    /// Release year derived from TMDB `release_date`, when present.
    pub release_year: Option<i32>,
    /// TMDB overview text.
    pub overview: Option<String>,
    /// TMDB runtime in minutes, when provided.
    pub runtime_minutes: Option<u32>,
    /// TMDB popularity value.
    pub popularity: f64,
}

/// Top-level TMDB TV series details used by fulfillment.
#[derive(Debug, Clone, PartialEq)]
pub struct TmdbTvSeriesDetails {
    /// TMDB series id.
    pub series_id: TmdbSeriesId,
    /// TMDB display name.
    pub name: String,
    /// First-air year derived from TMDB `first_air_date`, when present.
    pub first_air_year: Option<i32>,
    /// TMDB overview text.
    pub overview: Option<String>,
    /// TMDB popularity value.
    pub popularity: f64,
}

/// TMDB media kind accepted by the discover endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TmdbDiscoverKind {
    /// Movie search candidates.
    Movie,
    /// TV series search candidates.
    Series,
}

/// Paged TMDB discover response.
#[derive(Debug, Clone, PartialEq)]
pub struct TmdbDiscoverPage {
    /// Search candidates returned for the requested page.
    pub candidates: Vec<TmdbDiscoverCandidate>,
    /// TMDB response page number.
    pub page: u32,
    /// Whether TMDB reports additional pages after this page.
    pub has_more: bool,
}

/// Candidate returned by TMDB discover search.
#[derive(Debug, Clone, PartialEq)]
pub struct TmdbDiscoverCandidate {
    /// TMDB id for the movie or TV series.
    pub tmdb_id: u32,
    /// TMDB media kind.
    pub kind: TmdbDiscoverKind,
    /// Display title.
    pub title: String,
    /// Release or first-air year when TMDB provided a parseable date.
    pub year: Option<i32>,
    /// TMDB overview text.
    pub overview: Option<String>,
    /// Original-size poster image URL when TMDB provided a poster path.
    pub poster_url: Option<Url>,
    /// Original-size backdrop image URL when TMDB provided a backdrop path.
    pub backdrop_url: Option<Url>,
    /// TMDB popularity score.
    pub popularity: f64,
}

/// HTTP client for TMDB movie and TV endpoints.
#[derive(Clone, Debug)]
pub struct TmdbClient {
    http: reqwest::Client,
    config: Arc<TmdbClientConfig>,
    rate_gate: Arc<RateGate>,
    cache: Arc<TmdbCache>,
}

impl TmdbClient {
    /// Construct a TMDB client from explicit client configuration.
    pub fn new(config: TmdbClientConfig) -> Self {
        Self {
            rate_gate: Arc::new(RateGate::new(config.max_requests_per_second)),
            config: Arc::new(config),
            http: reqwest::Client::new(),
            cache: Arc::new(TmdbCache::default()),
        }
    }

    /// Construct a TMDB client from Kino's core config.
    pub fn from_core(config: &kino_core::config::TmdbConfig) -> Result<Self> {
        TmdbClientConfig::from_core(config).map(Self::new)
    }

    /// Search TMDB movies by text and optional release year.
    pub async fn search_movies(
        &self,
        query: &str,
        release_year: Option<i32>,
    ) -> Result<Vec<TmdbMovieSearchResult>> {
        let query = validate_query(query)?;
        if let Some(year) = release_year {
            validate_year(year)?;
        }

        let mut params = vec![("query", query.to_owned())];
        if let Some(year) = release_year {
            params.push(("primary_release_year", year.to_string()));
        }

        let response: SearchResponse<MovieSearchItem> =
            self.get_json("search/movie", &params).await?;
        response
            .results
            .into_iter()
            .map(MovieSearchItem::try_into_result)
            .collect()
    }

    /// Search TMDB TV series by text and optional first-air year.
    pub async fn search_tv(
        &self,
        query: &str,
        first_air_year: Option<i32>,
    ) -> Result<Vec<TmdbTvSearchResult>> {
        let query = validate_query(query)?;
        if let Some(year) = first_air_year {
            validate_year(year)?;
        }

        let mut params = vec![("query", query.to_owned())];
        if let Some(year) = first_air_year {
            params.push(("first_air_date_year", year.to_string()));
        }

        let response: SearchResponse<TvSearchItem> = self.get_json("search/tv", &params).await?;
        response
            .results
            .into_iter()
            .map(TvSearchItem::try_into_result)
            .collect()
    }

    /// Discover TMDB movies or TV series by query and page.
    pub async fn discover(
        &self,
        query: &str,
        kind: TmdbDiscoverKind,
        page: u32,
    ) -> Result<TmdbDiscoverPage> {
        let query = validate_query(query)?;
        validate_page(page)?;

        let params = [("query", query.to_owned()), ("page", page.to_string())];
        match kind {
            TmdbDiscoverKind::Movie => {
                let response: SearchResponse<MovieDiscoverItem> =
                    self.get_json("search/movie", &params).await?;
                let candidates = response
                    .results
                    .into_iter()
                    .map(|item| item.try_into_candidate(self))
                    .collect::<Result<Vec<_>>>()?;
                Ok(TmdbDiscoverPage {
                    candidates,
                    page: response.page,
                    has_more: response.page < response.total_pages,
                })
            }
            TmdbDiscoverKind::Series => {
                let response: SearchResponse<TvDiscoverItem> =
                    self.get_json("search/tv", &params).await?;
                let candidates = response
                    .results
                    .into_iter()
                    .map(|item| item.try_into_candidate(self))
                    .collect::<Result<Vec<_>>>()?;
                Ok(TmdbDiscoverPage {
                    candidates,
                    page: response.page,
                    has_more: response.page < response.total_pages,
                })
            }
        }
    }

    /// Fetch top-level TMDB movie details, using the in-session cache by movie id.
    pub async fn movie_details(&self, movie_id: TmdbMovieId) -> Result<TmdbMovieDetails> {
        if let Some(cached) = self.cache.movies.read().await.get(&movie_id).cloned() {
            return Ok(cached);
        }

        let path = format!("movie/{}", movie_id.get());
        let raw: MovieDetailsResponse = self.get_json(&path, &[]).await?;
        let details = raw.try_into_details()?;
        if details.movie_id != movie_id {
            return Err(Error::InvalidResponse {
                reason: "movie details id does not match requested id",
            });
        }

        let mut movies = self.cache.movies.write().await;
        let details = movies.entry(movie_id).or_insert(details).clone();
        Ok(details)
    }

    /// Fetch top-level TMDB TV series details, using the in-session cache by series id.
    pub async fn tv_series_details(&self, series_id: TmdbSeriesId) -> Result<TmdbTvSeriesDetails> {
        if let Some(cached) = self.cache.tv_series.read().await.get(&series_id).cloned() {
            return Ok(cached);
        }

        let path = format!("tv/{}", series_id.get());
        let raw: TvDetailsResponse = self.get_json(&path, &[]).await?;
        let details = raw.try_into_details()?;
        if details.series_id != series_id {
            return Err(Error::InvalidResponse {
                reason: "tv details id does not match requested id",
            });
        }

        let mut tv_series = self.cache.tv_series.write().await;
        let details = tv_series.entry(series_id).or_insert(details).clone();
        Ok(details)
    }

    async fn fetch_metadata_payload(
        &self,
        identity_id: CanonicalIdentityId,
    ) -> Result<TmdbMetadata> {
        match identity_id.kind() {
            CanonicalIdentityKind::Movie => {
                let raw: MovieMetadataResponse = self
                    .get_json(
                        &format!("movie/{}", identity_id.tmdb_id().get()),
                        &[("append_to_response", String::from("credits,images"))],
                    )
                    .await?;
                raw.into_metadata(self).await
            }
            CanonicalIdentityKind::TvSeries => {
                let raw: TvMetadataResponse = self
                    .get_json(
                        &format!("tv/{}", identity_id.tmdb_id().get()),
                        &[("append_to_response", String::from("credits,images"))],
                    )
                    .await?;
                raw.into_metadata(self).await
            }
        }
    }

    async fn download_image_asset(&self, path: &str) -> Result<MetadataAsset> {
        let extension = image_extension(path).ok_or(Error::InvalidResponse {
            reason: "image path does not include an extension",
        })?;
        let url = self
            .config
            .image_base_url
            .join(path.trim_start_matches('/'))
            .map_err(|source| Error::InvalidRequestPath {
                path: path.to_owned(),
                source,
            })?;
        let response = self.http.get(url.clone()).send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await?;
            return Err(Error::HttpStatus { status, body });
        }

        Ok(
            MetadataAsset::new(extension, response.bytes().await?.to_vec())
                .with_source_url(url.to_string()),
        )
    }

    async fn get_json<T>(&self, path: &str, params: &[(&str, String)]) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut attempts = 0;
        loop {
            attempts += 1;
            self.rate_gate.wait().await;

            let mut url =
                self.config
                    .base_url
                    .join(path)
                    .map_err(|source| Error::InvalidRequestPath {
                        path: path.to_owned(),
                        source,
                    })?;
            {
                let mut query = url.query_pairs_mut();
                query.append_pair("api_key", &self.config.api_key);
                for (key, value) in params {
                    query.append_pair(key, value);
                }
            }

            let response = self.http.get(url).send().await?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS {
                if attempts > self.config.max_retries + 1 {
                    return Err(Error::RateLimited { attempts });
                }

                let backoff = retry_after(response.headers())?
                    .unwrap_or_else(|| fallback_backoff(attempts.saturating_sub(1)));
                tokio::time::sleep(backoff).await;
                continue;
            }

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await?;
                return Err(Error::HttpStatus { status, body });
            }

            return response.json::<T>().await.map_err(Error::Request);
        }
    }
}

impl TmdbMetadataProvider for TmdbClient {
    fn fetch_metadata<'a>(
        &'a self,
        identity_id: CanonicalIdentityId,
    ) -> MetadataFuture<'a, TmdbMetadata> {
        Box::pin(async move {
            self.fetch_metadata_payload(identity_id)
                .await
                .map_err(|error| kino_library::Error::MetadataProvider {
                    reason: error.to_string(),
                })
        })
    }
}

#[derive(Debug)]
struct RateGate {
    next_request: Mutex<tokio::time::Instant>,
    interval: Duration,
}

impl RateGate {
    fn new(max_requests_per_second: NonZeroU32) -> Self {
        let interval = Duration::from_secs_f64(1.0 / f64::from(max_requests_per_second.get()));
        Self {
            next_request: Mutex::new(tokio::time::Instant::now()),
            interval,
        }
    }

    async fn wait(&self) {
        let now = tokio::time::Instant::now();
        let scheduled = {
            let mut next_request = self.next_request.lock().await;
            let scheduled = (*next_request).max(now);
            *next_request = scheduled + self.interval;
            scheduled
        };

        if scheduled > now {
            tokio::time::sleep_until(scheduled).await;
        }
    }
}

#[derive(Debug, Default)]
struct TmdbCache {
    movies: RwLock<HashMap<TmdbMovieId, TmdbMovieDetails>>,
    tv_series: RwLock<HashMap<TmdbSeriesId, TmdbTvSeriesDetails>>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse<T> {
    #[serde(default)]
    page: u32,
    #[serde(default)]
    total_pages: u32,
    results: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct MovieSearchItem {
    id: u32,
    title: String,
    #[serde(default)]
    release_date: Option<String>,
    popularity: f64,
}

impl MovieSearchItem {
    fn try_into_result(self) -> Result<TmdbMovieSearchResult> {
        let movie_id = TmdbMovieId::new(self.id).ok_or(Error::InvalidResponse {
            reason: "movie id is not positive",
        })?;
        Ok(TmdbMovieSearchResult::from_release_date(
            movie_id,
            self.title,
            self.release_date.as_deref(),
            self.popularity,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct TvSearchItem {
    id: u32,
    name: String,
    #[serde(default)]
    first_air_date: Option<String>,
    popularity: f64,
}

impl TvSearchItem {
    fn try_into_result(self) -> Result<TmdbTvSearchResult> {
        let series_id = TmdbSeriesId::new(self.id).ok_or(Error::InvalidResponse {
            reason: "series id is not positive",
        })?;
        Ok(TmdbTvSearchResult::from_first_air_date(
            series_id,
            self.name,
            self.first_air_date.as_deref(),
            self.popularity,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct MovieDiscoverItem {
    id: u32,
    title: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    backdrop_path: Option<String>,
    popularity: f64,
}

impl MovieDiscoverItem {
    fn try_into_candidate(self, client: &TmdbClient) -> Result<TmdbDiscoverCandidate> {
        if self.id == 0 {
            return Err(Error::InvalidResponse {
                reason: "movie id is not positive",
            });
        }

        Ok(TmdbDiscoverCandidate {
            tmdb_id: self.id,
            kind: TmdbDiscoverKind::Movie,
            title: self.title,
            year: self
                .release_date
                .as_deref()
                .and_then(release_year_from_date),
            overview: self.overview,
            poster_url: optional_image_url(client, self.poster_path)?,
            backdrop_url: optional_image_url(client, self.backdrop_path)?,
            popularity: self.popularity,
        })
    }
}

#[derive(Debug, Deserialize)]
struct TvDiscoverItem {
    id: u32,
    name: String,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    backdrop_path: Option<String>,
    popularity: f64,
}

impl TvDiscoverItem {
    fn try_into_candidate(self, client: &TmdbClient) -> Result<TmdbDiscoverCandidate> {
        if self.id == 0 {
            return Err(Error::InvalidResponse {
                reason: "series id is not positive",
            });
        }

        Ok(TmdbDiscoverCandidate {
            tmdb_id: self.id,
            kind: TmdbDiscoverKind::Series,
            title: self.name,
            year: self
                .first_air_date
                .as_deref()
                .and_then(first_air_year_from_date),
            overview: self.overview,
            poster_url: optional_image_url(client, self.poster_path)?,
            backdrop_url: optional_image_url(client, self.backdrop_path)?,
            popularity: self.popularity,
        })
    }
}

#[derive(Debug, Deserialize)]
struct MovieDetailsResponse {
    id: u32,
    title: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    runtime: Option<u32>,
    popularity: f64,
}

impl MovieDetailsResponse {
    fn try_into_details(self) -> Result<TmdbMovieDetails> {
        let movie_id = TmdbMovieId::new(self.id).ok_or(Error::InvalidResponse {
            reason: "movie id is not positive",
        })?;
        Ok(TmdbMovieDetails {
            movie_id,
            title: self.title,
            release_year: self
                .release_date
                .as_deref()
                .and_then(release_year_from_date),
            overview: self.overview,
            runtime_minutes: self.runtime,
            popularity: self.popularity,
        })
    }
}

#[derive(Debug, Deserialize)]
struct TvDetailsResponse {
    id: u32,
    name: String,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    overview: Option<String>,
    popularity: f64,
}

impl TvDetailsResponse {
    fn try_into_details(self) -> Result<TmdbTvSeriesDetails> {
        let series_id = TmdbSeriesId::new(self.id).ok_or(Error::InvalidResponse {
            reason: "series id is not positive",
        })?;
        Ok(TmdbTvSeriesDetails {
            series_id,
            name: self.name,
            first_air_year: self
                .first_air_date
                .as_deref()
                .and_then(first_air_year_from_date),
            overview: self.overview,
            popularity: self.popularity,
        })
    }
}

#[derive(Debug, Deserialize)]
struct MovieMetadataResponse {
    title: String,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    backdrop_path: Option<String>,
    #[serde(default)]
    credits: CreditsResponse,
    #[serde(default)]
    images: ImagesResponse,
}

impl MovieMetadataResponse {
    async fn into_metadata(self, client: &TmdbClient) -> Result<TmdbMetadata> {
        let poster_path = required_image_path(self.poster_path, "movie poster_path")?;
        let backdrop_path = required_image_path(self.backdrop_path, "movie backdrop_path")?;

        Ok(TmdbMetadata::new(
            self.title,
            self.overview.unwrap_or_default(),
            self.release_date,
            client.download_image_asset(&poster_path).await?,
            client.download_image_asset(&backdrop_path).await?,
            optional_logo_asset(client, self.images).await?,
            cast_members(self.credits.cast),
        ))
    }
}

#[derive(Debug, Deserialize)]
struct TvMetadataResponse {
    name: String,
    #[serde(default)]
    overview: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
    #[serde(default)]
    backdrop_path: Option<String>,
    #[serde(default)]
    credits: CreditsResponse,
    #[serde(default)]
    images: ImagesResponse,
}

impl TvMetadataResponse {
    async fn into_metadata(self, client: &TmdbClient) -> Result<TmdbMetadata> {
        let poster_path = required_image_path(self.poster_path, "tv poster_path")?;
        let backdrop_path = required_image_path(self.backdrop_path, "tv backdrop_path")?;

        Ok(TmdbMetadata::new(
            self.name,
            self.overview.unwrap_or_default(),
            self.first_air_date,
            client.download_image_asset(&poster_path).await?,
            client.download_image_asset(&backdrop_path).await?,
            optional_logo_asset(client, self.images).await?,
            cast_members(self.credits.cast),
        ))
    }
}

#[derive(Debug, Default, Deserialize)]
struct CreditsResponse {
    #[serde(default)]
    cast: Vec<CastResponse>,
}

#[derive(Debug, Deserialize)]
struct CastResponse {
    #[serde(default)]
    order: u32,
    name: String,
    #[serde(default)]
    character: Option<String>,
    #[serde(default)]
    profile_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ImagesResponse {
    #[serde(default)]
    logos: Vec<ImageResponse>,
}

#[derive(Debug, Deserialize)]
struct ImageResponse {
    file_path: String,
}

fn parse_base_url(value: &str) -> Result<Url> {
    let normalized = if value.ends_with('/') {
        value.to_owned()
    } else {
        format!("{value}/")
    };
    Url::parse(&normalized).map_err(|source| Error::InvalidBaseUrl {
        value: value.to_owned(),
        source,
    })
}

fn required_image_path(value: Option<String>, field: &'static str) -> Result<String> {
    value
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
        .ok_or(Error::InvalidResponse { reason: field })
}

fn optional_image_url(client: &TmdbClient, value: Option<String>) -> Result<Option<Url>> {
    let Some(path) = value
        .map(|path| path.trim().to_owned())
        .filter(|path| !path.is_empty())
    else {
        return Ok(None);
    };

    client
        .config
        .image_base_url
        .join(path.trim_start_matches('/'))
        .map(Some)
        .map_err(|source| Error::InvalidRequestPath { path, source })
}

async fn optional_logo_asset(
    client: &TmdbClient,
    images: ImagesResponse,
) -> Result<Option<MetadataAsset>> {
    let Some(logo) = images.logos.into_iter().next() else {
        return Ok(None);
    };

    client.download_image_asset(&logo.file_path).await.map(Some)
}

fn cast_members(cast: Vec<CastResponse>) -> Vec<MetadataCastMember> {
    cast.into_iter()
        .map(|member| {
            MetadataCastMember::new(
                member.order,
                member.name,
                member.character.unwrap_or_default(),
                member.profile_path,
            )
        })
        .collect()
}

fn image_extension(path: &str) -> Option<&str> {
    path.rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| !extension.is_empty())
}

fn validate_query(query: &str) -> Result<&str> {
    let query = query.trim();
    if query.is_empty() {
        Err(Error::EmptyQuery)
    } else {
        Ok(query)
    }
}

fn validate_year(year: i32) -> Result<()> {
    if (1000..=9999).contains(&year) {
        Ok(())
    } else {
        Err(Error::InvalidYear { year })
    }
}

fn validate_page(page: u32) -> Result<()> {
    if page == 0 {
        Err(Error::InvalidPage { page })
    } else {
        Ok(())
    }
}

fn retry_after(headers: &header::HeaderMap) -> Result<Option<Duration>> {
    let Some(value) = headers.get(header::RETRY_AFTER) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| Error::InvalidRetryAfter {
        value: "<non-utf8>".to_owned(),
    })?;
    let seconds = value.parse::<u64>().map_err(|_| Error::InvalidRetryAfter {
        value: value.to_owned(),
    })?;
    Ok(Some(Duration::from_secs(seconds)))
}

fn fallback_backoff(retry_attempt: u32) -> Duration {
    let multiplier = 2_u32.saturating_pow(retry_attempt.saturating_sub(1));
    Duration::from_millis(100 * u64::from(multiplier))
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

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::Mutex as AsyncMutex,
    };

    use super::*;

    #[tokio::test]
    async fn search_movies_sends_api_key_and_maps_results() {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","popularity":83.1}]}"#,
        )])
        .await;
        let client = test_client(&server);

        let results = client
            .search_movies(" Inception ", Some(2010))
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].movie_id.get(), 27205);
        assert_eq!(results[0].title, "Inception");
        assert_eq!(results[0].release_year, Some(2010));

        let requests = server.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("/3/search/movie?"));
        assert!(requests[0].contains("api_key=test-api-key"));
        assert!(requests[0].contains("query=Inception"));
        assert!(requests[0].contains("primary_release_year=2010"));
    }

    #[tokio::test]
    async fn search_tv_sends_api_key_and_maps_results() {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"results":[{"id":1399,"name":"Game of Thrones","first_air_date":"2011-04-17","popularity":126.5}]}"#,
        )])
        .await;
        let client = test_client(&server);

        let results = client
            .search_tv("Game of Thrones", Some(2011))
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].series_id.get(), 1399);
        assert_eq!(results[0].name, "Game of Thrones");
        assert_eq!(results[0].first_air_year, Some(2011));

        let requests = server.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("/3/search/tv?"));
        assert!(requests[0].contains("api_key=test-api-key"));
        assert!(requests[0].contains("query=Game+of+Thrones"));
        assert!(requests[0].contains("first_air_date_year=2011"));
    }

    #[tokio::test]
    async fn discover_movies_maps_paged_candidates_and_image_urls() {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"page":1,"total_pages":2,"results":[{"id":27205,"title":"Inception","release_date":"2010-07-15","overview":"A dream heist.","poster_path":"/poster.jpg","backdrop_path":"/backdrop.jpg","popularity":83.1}]}"#,
        )])
        .await;
        let client = test_client(&server);

        let page = client
            .discover(" Inception ", TmdbDiscoverKind::Movie, 1)
            .await
            .unwrap();

        assert_eq!(page.page, 1);
        assert!(page.has_more);
        assert_eq!(page.candidates.len(), 1);
        let candidate = &page.candidates[0];
        assert_eq!(candidate.tmdb_id, 27205);
        assert_eq!(candidate.kind, TmdbDiscoverKind::Movie);
        assert_eq!(candidate.title, "Inception");
        assert_eq!(candidate.year, Some(2010));
        assert_eq!(candidate.overview.as_deref(), Some("A dream heist."));
        assert_eq!(
            candidate.poster_url.as_ref().map(Url::as_str),
            Some("https://image.tmdb.org/t/p/original/poster.jpg")
        );
        assert_eq!(
            candidate.backdrop_url.as_ref().map(Url::as_str),
            Some("https://image.tmdb.org/t/p/original/backdrop.jpg")
        );
        assert_eq!(candidate.popularity, 83.1);

        let requests = server.requests().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("/3/search/movie?"));
        assert!(requests[0].contains("api_key=test-api-key"));
        assert!(requests[0].contains("query=Inception"));
        assert!(requests[0].contains("page=1"));
    }

    #[tokio::test]
    async fn discover_series_maps_missing_optional_fields_to_none() {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"page":1,"total_pages":1,"results":[{"id":1399,"name":"Game of Thrones","first_air_date":"2011-04-17","overview":null,"poster_path":"","backdrop_path":null,"popularity":126.5}]}"#,
        )])
        .await;
        let client = test_client(&server);

        let page = client
            .discover("Game of Thrones", TmdbDiscoverKind::Series, 1)
            .await
            .unwrap();

        assert_eq!(page.page, 1);
        assert!(!page.has_more);
        assert_eq!(page.candidates.len(), 1);
        let candidate = &page.candidates[0];
        assert_eq!(candidate.tmdb_id, 1399);
        assert_eq!(candidate.kind, TmdbDiscoverKind::Series);
        assert_eq!(candidate.title, "Game of Thrones");
        assert_eq!(candidate.year, Some(2011));
        assert_eq!(candidate.overview, None);
        assert_eq!(candidate.poster_url, None);
        assert_eq!(candidate.backdrop_url, None);
    }

    #[tokio::test]
    async fn discover_page_past_total_pages_has_no_more_results() {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"page":5,"total_pages":3,"results":[]}"#,
        )])
        .await;
        let client = test_client(&server);

        let page = client
            .discover("Inception", TmdbDiscoverKind::Movie, 5)
            .await
            .unwrap();

        assert_eq!(page.page, 5);
        assert!(!page.has_more);
        assert!(page.candidates.is_empty());
    }

    #[tokio::test]
    async fn movie_details_are_cached_by_id() {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"id":11,"title":"Star Wars","release_date":"1977-05-25","overview":"A space opera.","popularity":70.0}"#,
        )])
        .await;
        let client = test_client(&server);
        let movie_id = TmdbMovieId::new(11).unwrap();

        let first = client.movie_details(movie_id).await.unwrap();
        let second = client.movie_details(movie_id).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(first.release_year, Some(1977));
        assert_eq!(server.requests().await.len(), 1);
    }

    #[tokio::test]
    async fn tv_details_are_cached_by_id() {
        let server = TestServer::new(vec![TestResponse::json(
            StatusCode::OK,
            r#"{"id":1396,"name":"Breaking Bad","first_air_date":"2008-01-20","overview":"A chemistry teacher.","popularity":90.0}"#,
        )])
        .await;
        let client = test_client(&server);
        let series_id = TmdbSeriesId::new(1396).unwrap();

        let first = client.tv_series_details(series_id).await.unwrap();
        let second = client.tv_series_details(series_id).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(first.first_air_year, Some(2008));
        assert_eq!(server.requests().await.len(), 1);
    }

    #[tokio::test]
    async fn rate_limit_response_backs_off_and_retries() {
        let server = TestServer::new(vec![
            TestResponse::new(StatusCode::TOO_MANY_REQUESTS, "retry later")
                .with_header("Retry-After", "0"),
            TestResponse::json(
                StatusCode::OK,
                r#"{"id":11,"title":"Star Wars","release_date":"1977-05-25","overview":null,"popularity":70.0}"#,
            ),
        ])
        .await;
        let client = test_client(&server);

        let details = client
            .movie_details(TmdbMovieId::new(11).unwrap())
            .await
            .unwrap();

        assert_eq!(details.movie_id.get(), 11);
        assert_eq!(server.requests().await.len(), 2);
    }

    #[tokio::test]
    async fn non_success_status_returns_body() {
        let server =
            TestServer::new(vec![TestResponse::new(StatusCode::UNAUTHORIZED, "bad key")]).await;
        let client = test_client(&server);

        let err = client.search_movies("Inception", None).await.unwrap_err();

        match err {
            Error::HttpStatus { status, body } => {
                assert_eq!(status, StatusCode::UNAUTHORIZED);
                assert_eq!(body, "bad key");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_api_key_is_explicit() {
        let config = kino_core::config::TmdbConfig::default();
        let err = TmdbClientConfig::from_core(&config).unwrap_err();

        assert!(matches!(err, Error::MissingApiKey));
    }

    #[tokio::test]
    async fn empty_query_is_rejected_before_http() {
        let server = TestServer::new(vec![]).await;
        let client = test_client(&server);

        let err = client.search_movies(" ", None).await.unwrap_err();

        assert!(matches!(err, Error::EmptyQuery));
        assert_eq!(server.requests().await.len(), 0);
    }

    fn test_client(server: &TestServer) -> TmdbClient {
        let config = TmdbClientConfig::new("test-api-key")
            .unwrap()
            .with_base_url(&server.base_url())
            .unwrap()
            .with_max_requests_per_second(NonZeroU32::new(50).unwrap());
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
