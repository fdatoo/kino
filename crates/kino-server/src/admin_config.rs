use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use axum::{Json, Router, extract::State, routing::get};
use figment::{
    Figment, Metadata,
    providers::{Env, Format, Toml},
};
use kino_core::{
    CanonicalLayoutTransfer, Config,
    config::{
        DiscRipProviderConfig, LibraryConfig, LogFormat, OcrConfig, ProvidersConfig, ServerConfig,
        SessionReaperConfig, TmdbConfig, WatchFolderProviderConfig,
    },
};
use serde::Serialize;

#[derive(Clone)]
pub(crate) struct AdminConfigState {
    config: Config,
    sources: ConfigSources,
}

#[derive(Debug, Clone, Copy, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ConfigSource {
    Env,
    File,
    Default,
    Unknown,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct StringConfigValue {
    value: String,
    source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct StringListConfigValue {
    value: Vec<String>,
    source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct OptionalStringConfigValue {
    value: Option<String>,
    source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct U32ConfigValue {
    value: u32,
    source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct U64ConfigValue {
    value: u64,
    source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct I32ConfigValue {
    value: i32,
    source: ConfigSource,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminConfigResponse {
    database_path: StringConfigValue,
    server: AdminServerConfig,
    library: AdminLibraryConfig,
    providers: AdminProvidersConfig,
    tmdb: AdminTmdbConfig,
    ocr: AdminOcrConfig,
    log: AdminLogConfig,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminServerConfig {
    listen: StringConfigValue,
    public_base_url: StringConfigValue,
    cors_allowed_origins: StringListConfigValue,
    session_reaper: AdminSessionReaperConfig,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminSessionReaperConfig {
    tick_seconds: U64ConfigValue,
    active_to_idle_seconds: U64ConfigValue,
    idle_to_ended_seconds: U64ConfigValue,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminLibraryConfig {
    root: StringConfigValue,
    canonical_transfer: StringConfigValue,
    subtitle_staging_dir: OptionalStringConfigValue,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminProvidersConfig {
    disc_rip: Option<AdminDiscRipProviderConfig>,
    watch_folder: Option<AdminWatchFolderProviderConfig>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminDiscRipProviderConfig {
    path: StringConfigValue,
    preference: I32ConfigValue,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminWatchFolderProviderConfig {
    path: StringConfigValue,
    preference: I32ConfigValue,
    stability_seconds: U64ConfigValue,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminTmdbConfig {
    api_key: OptionalStringConfigValue,
    max_requests_per_second: U32ConfigValue,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminOcrConfig {
    tesseract_path: StringConfigValue,
    language: StringConfigValue,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct AdminLogConfig {
    level: StringConfigValue,
    format: StringConfigValue,
}

#[derive(Debug, Clone)]
struct ConfigSources {
    database_path: ConfigSource,
    library_root: ConfigSource,
    library: LibraryConfigSources,
    server: ServerConfigSources,
    tmdb: TmdbConfigSources,
    ocr: OcrConfigSources,
    providers: ProvidersConfigSources,
    log_level: ConfigSource,
    log_format: ConfigSource,
}

#[derive(Debug, Clone)]
struct LibraryConfigSources {
    canonical_transfer: ConfigSource,
    subtitle_staging_dir: ConfigSource,
}

#[derive(Debug, Clone)]
struct ServerConfigSources {
    listen: ConfigSource,
    public_base_url: ConfigSource,
    cors_allowed_origins: ConfigSource,
    session_reaper: SessionReaperConfigSources,
}

#[derive(Debug, Clone)]
struct SessionReaperConfigSources {
    tick_seconds: ConfigSource,
    active_to_idle_seconds: ConfigSource,
    idle_to_ended_seconds: ConfigSource,
}

#[derive(Debug, Clone)]
struct TmdbConfigSources {
    api_key: ConfigSource,
    max_requests_per_second: ConfigSource,
}

#[derive(Debug, Clone)]
struct OcrConfigSources {
    tesseract_path: ConfigSource,
    language: ConfigSource,
}

#[derive(Debug, Clone)]
struct ProvidersConfigSources {
    disc_rip: DiscRipProviderConfigSources,
    watch_folder: WatchFolderProviderConfigSources,
}

#[derive(Debug, Clone)]
struct DiscRipProviderConfigSources {
    path: ConfigSource,
    preference: ConfigSource,
}

#[derive(Debug, Clone)]
struct WatchFolderProviderConfigSources {
    path: ConfigSource,
    preference: ConfigSource,
    stability_seconds: ConfigSource,
}

pub(crate) fn router(config: Config) -> Router {
    Router::new()
        .route("/api/v1/admin/config", get(get_config))
        .with_state(AdminConfigState {
            config,
            sources: ConfigSources::current(),
        })
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/config",
    tag = "admin",
    responses(
        (status = 200, description = "Resolved server configuration", body = AdminConfigResponse)
    )
)]
pub(crate) async fn get_config(State(state): State<AdminConfigState>) -> Json<AdminConfigResponse> {
    Json(AdminConfigResponse::from_config(
        &state.config,
        &state.sources,
    ))
}

impl AdminConfigResponse {
    fn from_config(config: &Config, sources: &ConfigSources) -> Self {
        Self {
            database_path: string_value(path_string(&config.database_path), sources.database_path),
            server: AdminServerConfig::from_config(&config.server, &sources.server),
            library: AdminLibraryConfig::from_config(
                &config.library_root,
                &config.library,
                &sources.library,
                sources.library_root,
            ),
            providers: AdminProvidersConfig::from_config(&config.providers, &sources.providers),
            tmdb: AdminTmdbConfig::from_config(&config.tmdb, &sources.tmdb),
            ocr: AdminOcrConfig::from_config(&config.ocr, &sources.ocr),
            log: AdminLogConfig {
                level: string_value(config.log_level.clone(), sources.log_level),
                format: string_value(log_format(&config.log_format), sources.log_format),
            },
        }
    }
}

impl AdminServerConfig {
    fn from_config(config: &ServerConfig, sources: &ServerConfigSources) -> Self {
        Self {
            listen: socket_value(config.listen, sources.listen),
            public_base_url: string_value(config.public_base_url.clone(), sources.public_base_url),
            cors_allowed_origins: string_list_value(
                config.cors_allowed_origins.clone(),
                sources.cors_allowed_origins,
            ),
            session_reaper: AdminSessionReaperConfig::from_config(
                &config.session_reaper,
                &sources.session_reaper,
            ),
        }
    }
}

impl AdminSessionReaperConfig {
    fn from_config(config: &SessionReaperConfig, sources: &SessionReaperConfigSources) -> Self {
        Self {
            tick_seconds: u64_value(duration_seconds(config.tick_interval), sources.tick_seconds),
            active_to_idle_seconds: u64_value(
                duration_seconds(config.active_to_idle),
                sources.active_to_idle_seconds,
            ),
            idle_to_ended_seconds: u64_value(
                duration_seconds(config.idle_to_ended),
                sources.idle_to_ended_seconds,
            ),
        }
    }
}

impl AdminLibraryConfig {
    fn from_config(
        library_root: &Path,
        config: &LibraryConfig,
        sources: &LibraryConfigSources,
        library_root_source: ConfigSource,
    ) -> Self {
        Self {
            root: string_value(path_string(library_root), library_root_source),
            canonical_transfer: string_value(
                canonical_transfer(&config.canonical_transfer),
                sources.canonical_transfer,
            ),
            subtitle_staging_dir: optional_path_value(
                config.subtitle_staging_dir.as_deref(),
                sources.subtitle_staging_dir,
            ),
        }
    }
}

impl AdminProvidersConfig {
    fn from_config(config: &ProvidersConfig, sources: &ProvidersConfigSources) -> Self {
        Self {
            disc_rip: config.disc_rip.as_ref().map(|provider| {
                AdminDiscRipProviderConfig::from_config(provider, &sources.disc_rip)
            }),
            watch_folder: config.watch_folder.as_ref().map(|provider| {
                AdminWatchFolderProviderConfig::from_config(provider, &sources.watch_folder)
            }),
        }
    }
}

impl AdminDiscRipProviderConfig {
    fn from_config(config: &DiscRipProviderConfig, sources: &DiscRipProviderConfigSources) -> Self {
        Self {
            path: string_value(path_string(&config.path), sources.path),
            preference: i32_value(config.preference, sources.preference),
        }
    }
}

impl AdminWatchFolderProviderConfig {
    fn from_config(
        config: &WatchFolderProviderConfig,
        sources: &WatchFolderProviderConfigSources,
    ) -> Self {
        Self {
            path: string_value(path_string(&config.path), sources.path),
            preference: i32_value(config.preference, sources.preference),
            stability_seconds: u64_value(config.stability_seconds, sources.stability_seconds),
        }
    }
}

impl AdminTmdbConfig {
    fn from_config(config: &TmdbConfig, sources: &TmdbConfigSources) -> Self {
        Self {
            api_key: OptionalStringConfigValue {
                value: config.api_key.as_ref().map(|_| "***".to_owned()),
                source: sources.api_key,
            },
            max_requests_per_second: u32_value(
                config.max_requests_per_second,
                sources.max_requests_per_second,
            ),
        }
    }
}

impl AdminOcrConfig {
    fn from_config(config: &OcrConfig, sources: &OcrConfigSources) -> Self {
        Self {
            tesseract_path: string_value(
                path_string(&config.tesseract_path),
                sources.tesseract_path,
            ),
            language: string_value(config.language.clone(), sources.language),
        }
    }
}

impl ConfigSources {
    fn current() -> Self {
        let figment = current_figment();
        Self {
            database_path: source_or(&figment, "database_path", ConfigSource::Unknown),
            library_root: source_or(&figment, "library_root", ConfigSource::Unknown),
            library: LibraryConfigSources {
                canonical_transfer: source_or(
                    &figment,
                    "library.canonical_transfer",
                    ConfigSource::Default,
                ),
                subtitle_staging_dir: source_or(
                    &figment,
                    "library.subtitle_staging_dir",
                    ConfigSource::Default,
                ),
            },
            server: ServerConfigSources {
                listen: source_or(&figment, "server.listen", ConfigSource::Default),
                public_base_url: source_or(
                    &figment,
                    "server.public_base_url",
                    ConfigSource::Default,
                ),
                cors_allowed_origins: source_or(
                    &figment,
                    "server.cors_allowed_origins",
                    ConfigSource::Default,
                ),
                session_reaper: SessionReaperConfigSources {
                    tick_seconds: source_or(
                        &figment,
                        "server.session_reaper.tick_seconds",
                        ConfigSource::Default,
                    ),
                    active_to_idle_seconds: source_or(
                        &figment,
                        "server.session_reaper.active_to_idle_seconds",
                        ConfigSource::Default,
                    ),
                    idle_to_ended_seconds: source_or(
                        &figment,
                        "server.session_reaper.idle_to_ended_seconds",
                        ConfigSource::Default,
                    ),
                },
            },
            tmdb: TmdbConfigSources {
                api_key: source_or(&figment, "tmdb.api_key", ConfigSource::Default),
                max_requests_per_second: source_or(
                    &figment,
                    "tmdb.max_requests_per_second",
                    ConfigSource::Default,
                ),
            },
            ocr: OcrConfigSources {
                tesseract_path: source_or(&figment, "ocr.tesseract_path", ConfigSource::Default),
                language: source_or(&figment, "ocr.language", ConfigSource::Default),
            },
            providers: ProvidersConfigSources {
                disc_rip: DiscRipProviderConfigSources {
                    path: source_or(&figment, "providers.disc_rip.path", ConfigSource::Unknown),
                    preference: source_or(
                        &figment,
                        "providers.disc_rip.preference",
                        ConfigSource::Default,
                    ),
                },
                watch_folder: WatchFolderProviderConfigSources {
                    path: source_or(
                        &figment,
                        "providers.watch_folder.path",
                        ConfigSource::Unknown,
                    ),
                    preference: source_or(
                        &figment,
                        "providers.watch_folder.preference",
                        ConfigSource::Default,
                    ),
                    stability_seconds: source_or(
                        &figment,
                        "providers.watch_folder.stability_seconds",
                        ConfigSource::Default,
                    ),
                },
            },
            log_level: source_or(&figment, "log_level", ConfigSource::Default),
            log_format: source_or(&figment, "log_format", ConfigSource::Default),
        }
    }
}

fn current_figment() -> Figment {
    let explicit = std::env::var_os("KINO_CONFIG").map(PathBuf::from);
    let path = explicit
        .clone()
        .unwrap_or_else(|| PathBuf::from("kino.toml"));

    let mut figment = Figment::new();
    if explicit.is_some() || path.exists() {
        figment = figment.merge(Toml::file(&path));
    }

    figment.merge(
        Env::prefixed("KINO_")
            .split("__")
            .ignore(&["CONFIG", "LOG"]),
    )
}

fn source_or(figment: &Figment, path: &str, missing: ConfigSource) -> ConfigSource {
    figment
        .find_metadata(path)
        .map(classify_metadata)
        .unwrap_or(missing)
}

fn classify_metadata(metadata: &Metadata) -> ConfigSource {
    if metadata.name.contains("environment") {
        ConfigSource::Env
    } else if metadata.name.contains("TOML") {
        ConfigSource::File
    } else {
        ConfigSource::Unknown
    }
}

fn string_value(value: String, source: ConfigSource) -> StringConfigValue {
    StringConfigValue { value, source }
}

fn string_list_value(value: Vec<String>, source: ConfigSource) -> StringListConfigValue {
    StringListConfigValue { value, source }
}

fn optional_path_value(value: Option<&Path>, source: ConfigSource) -> OptionalStringConfigValue {
    OptionalStringConfigValue {
        value: value.map(path_string),
        source,
    }
}

fn socket_value(value: SocketAddr, source: ConfigSource) -> StringConfigValue {
    string_value(value.to_string(), source)
}

fn u32_value(value: u32, source: ConfigSource) -> U32ConfigValue {
    U32ConfigValue { value, source }
}

fn u64_value(value: u64, source: ConfigSource) -> U64ConfigValue {
    U64ConfigValue { value, source }
}

fn i32_value(value: i32, source: ConfigSource) -> I32ConfigValue {
    I32ConfigValue { value, source }
}

fn path_string(path: &Path) -> String {
    path.display().to_string()
}

fn duration_seconds(duration: Duration) -> u64 {
    duration.as_secs()
}

fn canonical_transfer(value: &CanonicalLayoutTransfer) -> String {
    match value {
        CanonicalLayoutTransfer::HardLink => "hard_link",
        CanonicalLayoutTransfer::Move => "move",
    }
    .to_owned()
}

fn log_format(value: &LogFormat) -> String {
    match value {
        LogFormat::Pretty => "pretty",
        LogFormat::Json => "json",
    }
    .to_owned()
}
