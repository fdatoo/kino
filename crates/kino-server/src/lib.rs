//! HTTP API server for Kino.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    Router,
    http::{HeaderValue, Method, header},
    middleware,
};
use kino_core::{
    Config,
    config::{LogFormat, ServerConfig},
};
use kino_db::Db;
use kino_fulfillment::tmdb::TmdbClient;
use kino_library::SubtitleReocrService;
use kino_transcode::{
    DefaultPolicy, DetectionConfig, EncoderRegistry, EphemeralStore, EvictionConfig,
    EvictionSweeper, JobStore, OutputPolicy, PipelineRunner, Scheduler, SchedulerConfig,
    TranscodeHandOff, TranscodeService, available_encoders, encoder::SoftwareEncoder,
};
use tower_http::cors::{AllowOrigin, CorsLayer};

mod admin_config;
pub mod auth;
mod catalog_admin;
mod ingestion_orchestrator;
mod openapi;
mod pairing;
mod playback;
mod request;
mod session_admin;
pub mod session_reaper;
pub mod session_service;
mod stream;
mod token;
mod transcode_admin;
pub mod variant_select;
pub mod watch_folder_intake;

pub use pairing::{PairingTokenStore, PendingToken};

/// Errors produced by `kino-server`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Binding or serving the HTTP listener failed.
    #[error("http server error: {0}")]
    Io(#[from] std::io::Error),
    /// Transcode service initialization failed.
    #[error(transparent)]
    Transcode(#[from] kino_transcode::Error),
}

/// Crate-local `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;

/// Build the Kino HTTP router.
pub fn router(db: Db) -> Router {
    router_with_library_root(db, PathBuf::from("."))
}

/// Build the Kino HTTP router with an explicit library root.
pub fn router_with_library_root(db: Db, library_root: impl Into<PathBuf>) -> Router {
    router_with_library_root_and_public_base_url(
        db,
        library_root,
        kino_core::config::ServerConfig::default().public_base_url,
    )
}

/// Build the Kino HTTP router with an explicit public base URL for OpenAPI.
pub fn router_with_public_base_url(db: Db, public_base_url: impl Into<String>) -> Router {
    router_with_library_root_and_public_base_url(db, PathBuf::from("."), public_base_url)
}

/// Build the Kino HTTP router with explicit library and OpenAPI settings.
pub fn router_with_library_root_and_public_base_url(
    db: Db,
    library_root: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Router {
    let library_root = library_root.into();
    let artwork_cache_dir = kino_core::config::default_artwork_cache_dir(&library_root);
    router_with_library_root_artwork_cache_and_public_base_url(
        db,
        library_root,
        artwork_cache_dir,
        public_base_url,
    )
}

/// Build the Kino HTTP router with a fully resolved configuration.
pub fn router_with_config(db: Db, config: Config) -> Router {
    let subtitle_reocr = SubtitleReocrService::with_default_tools(db.clone(), &config.library_root);
    router_with_config_and_reocr(db, config, subtitle_reocr)
}

/// Build the Kino HTTP router with a provided pairing token store.
pub fn router_with_config_and_token_store(
    db: Db,
    config: Config,
    token_store: PairingTokenStore,
) -> Router {
    let subtitle_reocr = SubtitleReocrService::with_default_tools(db.clone(), &config.library_root);
    router_with_config_reocr_and_token_store(db, config, subtitle_reocr, token_store)
}

fn router_with_config_and_reocr(
    db: Db,
    config: Config,
    subtitle_reocr: SubtitleReocrService,
) -> Router {
    let tmdb_client = TmdbClient::from_core(&config.tmdb).ok();
    let transcode = local_transcode_runtime(db.clone(), &config);
    router_with_config_reocr_tmdb_and_transcode(db, config, subtitle_reocr, tmdb_client, transcode)
}

fn router_with_config_reocr_and_token_store(
    db: Db,
    config: Config,
    subtitle_reocr: SubtitleReocrService,
    token_store: PairingTokenStore,
) -> Router {
    let tmdb_client = TmdbClient::from_core(&config.tmdb).ok();
    let transcode = local_transcode_runtime(db.clone(), &config);
    router_with_config_reocr_tmdb_transcode_and_token_store(
        db,
        config,
        subtitle_reocr,
        tmdb_client,
        transcode,
        token_store,
    )
}

fn router_with_config_reocr_and_tmdb(
    db: Db,
    config: Config,
    subtitle_reocr: SubtitleReocrService,
    tmdb_client: Option<TmdbClient>,
) -> Router {
    let transcode = local_transcode_runtime(db.clone(), &config);
    router_with_config_reocr_tmdb_and_transcode(db, config, subtitle_reocr, tmdb_client, transcode)
}

struct TranscodeRuntime {
    service: Arc<TranscodeService>,
    encoders: Arc<EncoderRegistry>,
}

fn router_with_config_reocr_tmdb_and_transcode(
    db: Db,
    config: Config,
    subtitle_reocr: SubtitleReocrService,
    tmdb_client: Option<TmdbClient>,
    transcode: TranscodeRuntime,
) -> Router {
    router_with_config_reocr_tmdb_transcode_and_token_store(
        db,
        config,
        subtitle_reocr,
        tmdb_client,
        transcode,
        PairingTokenStore::new(),
    )
}

fn router_with_config_reocr_tmdb_transcode_and_token_store(
    db: Db,
    config: Config,
    subtitle_reocr: SubtitleReocrService,
    tmdb_client: Option<TmdbClient>,
    transcode: TranscodeRuntime,
    token_store: PairingTokenStore,
) -> Router {
    let auth_state = auth::AuthState { db: db.clone() };
    let public_base_url = config.server.public_base_url.clone();
    let library_root = config.library_root.clone();
    let artwork_cache_dir = config.artwork_cache_dir();
    let canonical_transfer = config.library.canonical_transfer;
    let cors = cors_layer(&config.server);
    let live_stream = live_stream_state(db.clone(), &config);
    let transcode_handoff: Arc<dyn TranscodeHandOff> = transcode.service.clone();
    let protected_api = Router::new()
        .merge(request::router_with_canonical_transfer(
            db.clone(),
            library_root,
            artwork_cache_dir,
            subtitle_reocr,
            tmdb_client,
            canonical_transfer,
            transcode_handoff,
        ))
        .merge(catalog_admin::router(
            db.clone(),
            config.library_root.clone(),
        ))
        .merge(stream::router_with_live(db.clone(), live_stream))
        .merge(token::router(db.clone()))
        .merge(pairing::admin_router(db.clone(), token_store.clone()))
        .merge(playback::router(db.clone()))
        .merge(session_admin::router(db.clone()))
        .merge(admin_config::router(config))
        .merge(transcode_admin::router(
            db.clone(),
            transcode.service,
            transcode.encoders,
        ))
        .route_layer(middleware::from_fn_with_state(
            auth_state,
            auth::require_auth,
        ));

    Router::new()
        .merge(openapi::router(public_base_url))
        .merge(pairing::router(db, token_store))
        .merge(protected_api)
        .merge(kino_admin::router())
        .layer(cors)
}

fn live_stream_state(db: Db, config: &Config) -> Option<stream::LiveStreamState> {
    let live = stream::LiveStreamState::new(db.clone(), &config.transcode.ephemeral);
    if config.transcode.ephemeral.enabled && tokio::runtime::Handle::try_current().is_ok() {
        EvictionSweeper::new(
            EphemeralStore::new(db),
            EvictionConfig::from(&config.transcode.ephemeral),
        )
        .spawn();
    }
    Some(live)
}

/// Build the Kino HTTP router with an explicit TMDB client.
pub fn router_with_tmdb_client(db: Db, tmdb_client: TmdbClient) -> Router {
    let library_root = PathBuf::from(".");
    let subtitle_reocr = SubtitleReocrService::with_default_tools(db.clone(), &library_root);
    router_with_config_reocr_and_tmdb(
        db,
        Config {
            database_path: PathBuf::from("kino.db"),
            library_root,
            library: kino_core::LibraryConfig::default(),
            server: ServerConfig::default(),
            tmdb: kino_core::config::TmdbConfig::default(),
            ocr: kino_core::OcrConfig::default(),
            providers: kino_core::config::ProvidersConfig::default(),
            transcode: kino_core::TranscodeConfig::default(),
            log_level: "info".to_owned(),
            log_format: LogFormat::Pretty,
        },
        subtitle_reocr,
        Some(tmdb_client),
    )
}

/// Build the Kino HTTP router with explicit library root and TMDB client.
pub fn router_with_library_root_and_tmdb_client(
    db: Db,
    library_root: impl Into<PathBuf>,
    artwork_cache_dir: impl Into<PathBuf>,
    tmdb_client: TmdbClient,
) -> Router {
    let library_root = library_root.into();
    let artwork_cache_dir = artwork_cache_dir.into();
    let subtitle_reocr = SubtitleReocrService::with_default_tools(db.clone(), &library_root);
    router_with_config_reocr_and_tmdb(
        db,
        Config {
            database_path: PathBuf::from("kino.db"),
            library_root,
            library: kino_core::LibraryConfig {
                artwork_cache_dir: Some(artwork_cache_dir),
                ..kino_core::LibraryConfig::default()
            },
            server: ServerConfig::default(),
            tmdb: kino_core::config::TmdbConfig::default(),
            ocr: kino_core::OcrConfig::default(),
            providers: kino_core::config::ProvidersConfig::default(),
            transcode: kino_core::TranscodeConfig::default(),
            log_level: "info".to_owned(),
            log_format: LogFormat::Pretty,
        },
        subtitle_reocr,
        Some(tmdb_client),
    )
}

/// Serve the Kino HTTP API until the listener exits.
pub async fn serve(config: &Config, db: Db) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(config.server.listen).await?;
    let local_addr = listener.local_addr()?;
    let transcode = build_transcode_service(db.clone(), config).await?;
    let subtitle_reocr = SubtitleReocrService::with_default_tools(db.clone(), &config.library_root);
    let tmdb_client = TmdbClient::from_core(&config.tmdb).ok();
    let intake = config.providers.watch_folder.as_ref().map(|provider| {
        watch_folder_intake::spawn(
            watch_folder_intake::WatchFolderIntakeDeps {
                db: db.clone(),
                library_root: config.library_root.clone(),
                artwork_cache_dir: config.artwork_cache_dir(),
                canonical_transfer: config.library.canonical_transfer,
                subtitle_reocr: subtitle_reocr.clone(),
                tmdb_client: tmdb_client.clone(),
                transcode: transcode.service.clone(),
            },
            provider.clone(),
        )
    });

    tracing::info!(listen = %local_addr, "server listening");
    let serve_result = axum::serve(
        listener,
        router_with_config_reocr_tmdb_and_transcode(
            db,
            config.clone(),
            subtitle_reocr,
            tmdb_client,
            transcode,
        ),
    )
    .await;

    if let Some(intake) = intake {
        intake.shutdown().await;
    }
    serve_result?;
    Ok(())
}

/// Serve the Kino HTTP API on an explicit socket address.
pub async fn serve_on(listen: SocketAddr, db: Db) -> Result<()> {
    serve_with_library_root(listen, db, PathBuf::from(".")).await
}

fn cors_layer(config: &ServerConfig) -> CorsLayer {
    let origins = config.cors_allowed_origins.clone();
    let allow_origin = if origins.is_empty() {
        AllowOrigin::any()
    } else {
        AllowOrigin::predicate(move |origin: &HeaderValue, _request_parts| {
            origins
                .iter()
                .any(|allowed| origin.as_bytes() == allowed.as_bytes())
        })
    };

    CorsLayer::new()
        .allow_origin(allow_origin)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(std::time::Duration::from_secs(600))
}

async fn build_transcode_service(db: Db, config: &Config) -> Result<TranscodeRuntime> {
    let store = JobStore::new(db);
    let policy: Box<dyn OutputPolicy> = Box::new(DefaultPolicy::from_config(config)?);
    let registry = Arc::new(available_encoders(&DetectionConfig::default()).await?);
    let runner = Arc::new(PipelineRunner::new());
    let scheduler_config = SchedulerConfig::from(&config.transcode.scheduler);
    let scheduler = Arc::new(Scheduler::new(
        store.clone(),
        Arc::clone(&registry),
        runner,
        scheduler_config,
    ));

    if scheduler_config.recovery_on_boot {
        scheduler.recover_on_boot().await?;
    }

    Arc::clone(&scheduler).spawn();

    Ok(TranscodeRuntime {
        service: Arc::new(TranscodeService::new(store, policy, scheduler)),
        encoders: registry,
    })
}

fn local_transcode_runtime(db: Db, config: &Config) -> TranscodeRuntime {
    let store = JobStore::new(db);
    let policy: Box<dyn OutputPolicy> = match DefaultPolicy::from_config(config) {
        Ok(policy) => Box::new(policy),
        Err(err) => panic!("invalid transcode policy config: {err}"),
    };
    let mut registry = EncoderRegistry::new();
    registry.register(Box::new(SoftwareEncoder::new()));
    let registry = Arc::new(registry);
    let scheduler = Arc::new(Scheduler::new(
        store.clone(),
        Arc::clone(&registry),
        Arc::new(PipelineRunner::new()),
        SchedulerConfig::from(&config.transcode.scheduler),
    ));

    TranscodeRuntime {
        service: Arc::new(TranscodeService::new(store, policy, scheduler)),
        encoders: registry,
    }
}

/// Serve the Kino HTTP API on an explicit socket address and library root.
pub async fn serve_with_library_root(
    listen: SocketAddr,
    db: Db,
    library_root: impl Into<PathBuf>,
) -> Result<()> {
    serve_with_library_root_and_public_base_url(
        listen,
        db,
        library_root,
        kino_core::config::ServerConfig::default().public_base_url,
    )
    .await
}

/// Serve the Kino HTTP API with explicit library and OpenAPI settings.
pub async fn serve_with_library_root_and_public_base_url(
    listen: SocketAddr,
    db: Db,
    library_root: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Result<()> {
    let library_root = library_root.into();
    let artwork_cache_dir = kino_core::config::default_artwork_cache_dir(&library_root);
    serve_with_library_root_artwork_cache_and_public_base_url(
        listen,
        db,
        library_root,
        artwork_cache_dir,
        public_base_url,
    )
    .await
}

/// Serve the Kino HTTP API with explicit library, artwork cache, and OpenAPI settings.
pub async fn serve_with_library_root_artwork_cache_and_public_base_url(
    listen: SocketAddr,
    db: Db,
    library_root: impl Into<PathBuf>,
    artwork_cache_dir: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Result<()> {
    let library_root = library_root.into();
    let artwork_cache_dir = artwork_cache_dir.into();
    let config = Config {
        database_path: PathBuf::from("kino.db"),
        library_root: library_root.clone(),
        library: kino_core::LibraryConfig {
            artwork_cache_dir: Some(artwork_cache_dir),
            ..kino_core::LibraryConfig::default()
        },
        server: ServerConfig {
            public_base_url: public_base_url.into(),
            listen,
            ..ServerConfig::default()
        },
        tmdb: kino_core::config::TmdbConfig::default(),
        ocr: kino_core::OcrConfig::default(),
        providers: kino_core::config::ProvidersConfig::default(),
        transcode: kino_core::TranscodeConfig::default(),
        log_level: "info".to_owned(),
        log_format: LogFormat::Pretty,
    };
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let local_addr = listener.local_addr()?;
    let transcode = build_transcode_service(db.clone(), &config).await?;
    let subtitle_reocr = SubtitleReocrService::with_default_tools(db.clone(), &library_root);
    tracing::info!(listen = %local_addr, "server listening");
    axum::serve(
        listener,
        router_with_config_reocr_tmdb_and_transcode(db, config, subtitle_reocr, None, transcode),
    )
    .await?;
    Ok(())
}

/// Build the Kino HTTP router with explicit library, artwork cache, and OpenAPI settings.
pub fn router_with_library_root_artwork_cache_and_public_base_url(
    db: Db,
    library_root: impl Into<PathBuf>,
    artwork_cache_dir: impl Into<PathBuf>,
    public_base_url: impl Into<String>,
) -> Router {
    let library_root = library_root.into();
    let subtitle_reocr = SubtitleReocrService::with_default_tools(db.clone(), &library_root);
    router_with_library_root_artwork_cache_reocr_and_public_base_url(
        db,
        library_root,
        artwork_cache_dir,
        subtitle_reocr,
        public_base_url,
    )
}

/// Build the Kino HTTP router with explicit library, artwork, re-OCR, and OpenAPI settings.
pub fn router_with_library_root_artwork_cache_reocr_and_public_base_url(
    db: Db,
    library_root: impl Into<PathBuf>,
    artwork_cache_dir: impl Into<PathBuf>,
    subtitle_reocr: SubtitleReocrService,
    public_base_url: impl Into<String>,
) -> Router {
    let server = ServerConfig {
        public_base_url: public_base_url.into(),
        ..ServerConfig::default()
    };
    router_with_config_and_reocr(
        db,
        Config {
            database_path: PathBuf::from("kino.db"),
            library_root: library_root.into(),
            library: kino_core::LibraryConfig {
                artwork_cache_dir: Some(artwork_cache_dir.into()),
                ..kino_core::LibraryConfig::default()
            },
            server,
            tmdb: kino_core::config::TmdbConfig::default(),
            ocr: kino_core::OcrConfig::default(),
            providers: kino_core::config::ProvidersConfig::default(),
            transcode: kino_core::TranscodeConfig::default(),
            log_level: "info".to_owned(),
            log_format: LogFormat::Pretty,
        },
        subtitle_reocr,
    )
}
