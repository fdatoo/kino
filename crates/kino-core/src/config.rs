//! Configuration: a TOML file plus environment-variable overrides.

use std::{
    fs::{self, OpenOptions},
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::{
    Deserialize, Deserializer,
    de::{SeqAccess, Visitor},
};

/// Kino's startup configuration.
///
/// Loaded from a TOML file (path resolved by [`Config::load`]) with
/// environment-variable overrides under the `KINO_` prefix. See
/// `kino.toml.example` at the repo root for a full reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Path to the SQLite database file. Required.
    pub database_path: PathBuf,

    /// Root directory of the on-disk media library. Required.
    pub library_root: PathBuf,

    /// Library behavior settings. Optional; defaults documented on
    /// [`LibraryConfig`].
    #[serde(default)]
    pub library: LibraryConfig,

    /// HTTP/gRPC server settings. Optional; defaults documented on
    /// [`ServerConfig`].
    #[serde(default)]
    pub server: ServerConfig,

    /// TMDB API client settings. Optional; defaults documented on
    /// [`TmdbConfig`].
    #[serde(default)]
    pub tmdb: TmdbConfig,

    /// OCR engine settings. Optional; defaults documented on [`OcrConfig`].
    #[serde(default)]
    pub ocr: OcrConfig,

    /// Fulfillment provider settings. Optional; no provider is configured by
    /// default.
    #[serde(default)]
    pub providers: ProvidersConfig,

    /// Logging filter. Accepts any tracing-subscriber `EnvFilter` expression.
    /// Defaults to `"info"`.
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Logging output format. Defaults to [`LogFormat::Pretty`].
    #[serde(default)]
    pub log_format: LogFormat,
}

/// Supported tracing subscriber output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable formatter for local development.
    #[default]
    Pretty,

    /// Newline-delimited JSON formatter for production.
    Json,
}

/// Library behavior settings.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryConfig {
    /// Filesystem operation used when placing canonical media files. Defaults
    /// to [`CanonicalLayoutTransfer::HardLink`].
    #[serde(default)]
    pub canonical_transfer: CanonicalLayoutTransfer,

    /// Directory used for content-addressed artwork caching. Defaults to
    /// `<library_root>/.kino/artwork` when omitted.
    #[serde(default)]
    pub artwork_cache_dir: Option<PathBuf>,

    /// Directory used for image-subtitle OCR staging. Defaults to
    /// `<library_root>/.kino/subtitles` when omitted.
    #[serde(default)]
    pub subtitle_staging_dir: Option<PathBuf>,
}

/// Filesystem operation used by the canonical layout writer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalLayoutTransfer {
    /// Create a hard link at the canonical path and preserve the original.
    #[default]
    HardLink,

    /// Move the source file into the canonical path.
    Move,
}

/// HTTP/gRPC server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the server binds to. Defaults to `127.0.0.1:7777`.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,

    /// Public base URL advertised in generated OpenAPI documents. Defaults to
    /// `http://127.0.0.1:8080`.
    #[serde(default = "default_public_base_url")]
    pub public_base_url: String,

    /// Origins allowed to make browser cross-origin API requests. Empty means
    /// any origin is allowed for non-credentialed requests.
    #[serde(default, deserialize_with = "comma_separated_strings")]
    pub cors_allowed_origins: Vec<String>,

    /// Playback session background reaper settings. Defaults documented on
    /// [`SessionReaperConfig`].
    #[serde(default)]
    pub session_reaper: SessionReaperConfig,
}

/// Playback session background reaper settings.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionReaperConfig {
    /// Interval between reaper sweeps. Defaults to 30 seconds.
    #[serde(
        default = "default_session_reaper_tick_interval",
        rename = "tick_seconds",
        deserialize_with = "duration_seconds"
    )]
    pub tick_interval: Duration,

    /// Age after which active sessions become idle. Defaults to 60 seconds.
    #[serde(
        default = "default_session_reaper_active_to_idle",
        rename = "active_to_idle_seconds",
        deserialize_with = "duration_seconds"
    )]
    pub active_to_idle: Duration,

    /// Age after which idle sessions become ended. Defaults to 300 seconds.
    #[serde(
        default = "default_session_reaper_idle_to_ended",
        rename = "idle_to_ended_seconds",
        deserialize_with = "duration_seconds"
    )]
    pub idle_to_ended: Duration,
}

/// TMDB API client settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TmdbConfig {
    /// TMDB v3 API key used for application-level authentication.
    #[serde(default)]
    pub api_key: Option<String>,

    /// Maximum client-side request rate. Defaults to 20 requests per second.
    #[serde(default = "default_tmdb_max_requests_per_second")]
    pub max_requests_per_second: u32,
}

/// OCR engine settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OcrConfig {
    /// Tesseract binary path. Defaults to `tesseract`, resolved through `PATH`.
    #[serde(default = "default_tesseract_path")]
    pub tesseract_path: PathBuf,

    /// Tesseract language code. Defaults to `eng`.
    #[serde(default = "default_ocr_language")]
    pub language: String,
}

/// Fulfillment provider configuration sections.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvidersConfig {
    /// Disc-rip import provider settings.
    #[serde(default)]
    pub disc_rip: Option<DiscRipProviderConfig>,

    /// Watch-folder provider settings.
    #[serde(default)]
    pub watch_folder: Option<WatchFolderProviderConfig>,
}

/// Configuration for the first-party disc-rip import provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscRipProviderConfig {
    /// Directory containing MakeMKV-style output files.
    pub path: PathBuf,

    /// User preference used when ranking matching providers.
    #[serde(default)]
    pub preference: i32,
}

/// Configuration for the first-party watch-folder fulfillment provider.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchFolderProviderConfig {
    /// Directory the provider watches for user-supplied media files.
    pub path: PathBuf,

    /// User preference used when ranking matching providers.
    #[serde(default)]
    pub preference: i32,

    /// Seconds a file size must remain unchanged before it can be ingested.
    #[serde(default = "default_watch_folder_stability_seconds")]
    pub stability_seconds: u64,
}

fn default_listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777)
}

fn default_public_base_url() -> String {
    "http://127.0.0.1:8080".into()
}

fn default_tmdb_max_requests_per_second() -> u32 {
    20
}

fn default_watch_folder_stability_seconds() -> u64 {
    5
}

fn default_session_reaper_tick_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_session_reaper_active_to_idle() -> Duration {
    Duration::from_secs(60)
}

fn default_session_reaper_idle_to_ended() -> Duration {
    Duration::from_secs(300)
}

fn default_tesseract_path() -> PathBuf {
    PathBuf::from("tesseract")
}

fn default_ocr_language() -> String {
    "eng".into()
}

fn default_log_level() -> String {
    "info".into()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            public_base_url: default_public_base_url(),
            cors_allowed_origins: Vec::new(),
            session_reaper: SessionReaperConfig::default(),
        }
    }
}

impl Default for SessionReaperConfig {
    fn default() -> Self {
        Self {
            tick_interval: default_session_reaper_tick_interval(),
            active_to_idle: default_session_reaper_active_to_idle(),
            idle_to_ended: default_session_reaper_idle_to_ended(),
        }
    }
}

impl Default for TmdbConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            max_requests_per_second: default_tmdb_max_requests_per_second(),
        }
    }
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            tesseract_path: default_tesseract_path(),
            language: default_ocr_language(),
        }
    }
}

/// Errors produced while loading [`Config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The config file could not be read.
    #[error("reading config file {path}: {source}", path = .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The config file or environment overrides failed to parse or validate.
    ///
    /// Includes missing required fields, unknown fields, and type mismatches.
    /// The wrapped figment error carries source location.
    #[error("invalid config: {0}")]
    Invalid(#[from] Box<figment::Error>),

    /// The configured library root is missing, inaccessible, or not a directory.
    #[error("invalid library_root {path}: {source}", path = .path.display())]
    InvalidLibraryRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The configured database path cannot be created in its parent directory.
    #[error("invalid database_path {path}: {source}", path = .path.display())]
    InvalidDatabasePath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The configured server settings are invalid.
    #[error("invalid server config {field}: {source}")]
    InvalidServerConfig {
        /// Server config field.
        field: &'static str,
        /// Underlying parse error.
        #[source]
        source: url::ParseError,
    },

    /// The configured session reaper settings are invalid.
    #[error("invalid session reaper config {field}: {reason}")]
    InvalidSessionReaperConfig {
        /// Session reaper config field.
        field: &'static str,
        /// Human-readable validation failure.
        reason: &'static str,
    },

    /// The configured TMDB client settings are invalid.
    #[error("invalid tmdb config: {reason}")]
    InvalidTmdbConfig {
        /// Human-readable validation failure.
        reason: &'static str,
    },

    /// The configured OCR settings are invalid.
    #[error("invalid ocr config: {reason}")]
    InvalidOcrConfig {
        /// Human-readable validation failure.
        reason: &'static str,
    },

    /// A configured fulfillment provider has invalid scalar settings.
    #[error("invalid provider config {provider}: {reason}")]
    InvalidProviderConfig {
        /// Stable provider config section.
        provider: &'static str,
        /// Human-readable validation failure.
        reason: &'static str,
    },

    /// A configured fulfillment provider path is missing or not a directory.
    #[error("invalid provider config {provider} path {path}: {source}", path = .path.display())]
    InvalidProviderPath {
        /// Stable provider config section.
        provider: &'static str,
        /// Invalid path.
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl Config {
    /// Load configuration from disk and the environment.
    ///
    /// Resolves the file path in this order:
    /// 1. The `KINO_CONFIG` environment variable, if set.
    /// 2. `./kino.toml` in the current working directory (used only if it
    ///    exists; absent file is not an error).
    ///
    /// Then layers `KINO_`-prefixed environment variables on top, with `__`
    /// separating nested sections (e.g. `KINO_SERVER__LISTEN`).
    ///
    /// Returns [`ConfigError::Io`] when `KINO_CONFIG` points at an unreadable
    /// path. Returns [`ConfigError::Invalid`] for parse errors, missing
    /// required fields, unknown fields, or invalid values.
    pub fn load() -> Result<Self, ConfigError> {
        let explicit = std::env::var_os("KINO_CONFIG").map(PathBuf::from);
        let path = explicit
            .clone()
            .unwrap_or_else(|| PathBuf::from("kino.toml"));

        let mut fig = Figment::new();
        if explicit.is_some() || path.exists() {
            std::fs::metadata(&path).map_err(|e| ConfigError::Io {
                path: path.clone(),
                source: e,
            })?;
            fig = fig.merge(Toml::file(&path));
        }
        fig.merge(
            Env::prefixed("KINO_")
                .split("__")
                .ignore(&["CONFIG", "LOG"]),
        )
        .extract::<Config>()
        .map_err(|e| ConfigError::Invalid(Box::new(e)))
        .and_then(Config::validate)
    }

    /// Load configuration from an explicit file path (skips `KINO_CONFIG`).
    /// Useful for tests and the CLI's `--config` flag.
    pub fn load_from(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        std::fs::metadata(path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        Figment::new()
            .merge(Toml::file(path))
            .merge(
                Env::prefixed("KINO_")
                    .split("__")
                    .ignore(&["CONFIG", "LOG"]),
            )
            .extract::<Config>()
            .map_err(|e| ConfigError::Invalid(Box::new(e)))
            .and_then(Config::validate)
    }

    fn validate(self) -> Result<Self, ConfigError> {
        validate_library_root(&self.library_root)?;
        validate_database_path(&self.database_path)?;
        validate_server_config(&self.server)?;
        validate_tmdb_config(&self.tmdb)?;
        validate_ocr_config(&self.ocr)?;
        validate_provider_configs(&self.providers)?;
        Ok(self)
    }

    /// Return the artwork cache directory, applying the library-root default.
    pub fn artwork_cache_dir(&self) -> PathBuf {
        self.library
            .artwork_cache_dir
            .clone()
            .unwrap_or_else(|| default_artwork_cache_dir(&self.library_root))
    }
}

/// Return Kino's default content-addressed artwork cache directory.
pub fn default_artwork_cache_dir(library_root: &Path) -> PathBuf {
    library_root.join(".kino").join("artwork")
}

fn validate_server_config(config: &ServerConfig) -> Result<(), ConfigError> {
    url::Url::parse(&config.public_base_url).map_err(|source| {
        ConfigError::InvalidServerConfig {
            field: "public_base_url",
            source,
        }
    })?;
    validate_session_reaper_config(&config.session_reaper)?;
    Ok(())
}

fn validate_session_reaper_config(config: &SessionReaperConfig) -> Result<(), ConfigError> {
    if config.tick_interval.is_zero() {
        return Err(ConfigError::InvalidSessionReaperConfig {
            field: "tick_seconds",
            reason: "must be positive",
        });
    }

    if config.active_to_idle.is_zero() {
        return Err(ConfigError::InvalidSessionReaperConfig {
            field: "active_to_idle_seconds",
            reason: "must be positive",
        });
    }

    if config.idle_to_ended.is_zero() {
        return Err(ConfigError::InvalidSessionReaperConfig {
            field: "idle_to_ended_seconds",
            reason: "must be positive",
        });
    }

    Ok(())
}

fn duration_seconds<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let seconds = u64::deserialize(deserializer)?;
    Ok(Duration::from_secs(seconds))
}

fn comma_separated_strings<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringListVisitor;

    impl<'de> Visitor<'de> for StringListVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string list or a comma-separated string")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(split_string_list(value))
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(split_string_list(&value))
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_any(StringListVisitor)
}

fn split_string_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn validate_tmdb_config(config: &TmdbConfig) -> Result<(), ConfigError> {
    if config
        .api_key
        .as_ref()
        .is_some_and(|api_key| api_key.trim().is_empty())
    {
        return Err(ConfigError::InvalidTmdbConfig {
            reason: "api_key is empty",
        });
    }

    if config.max_requests_per_second == 0 {
        return Err(ConfigError::InvalidTmdbConfig {
            reason: "max_requests_per_second must be positive",
        });
    }

    if config.max_requests_per_second > 50 {
        return Err(ConfigError::InvalidTmdbConfig {
            reason: "max_requests_per_second must be at most 50",
        });
    }

    Ok(())
}

fn validate_ocr_config(config: &OcrConfig) -> Result<(), ConfigError> {
    if config.tesseract_path.as_os_str().is_empty() {
        return Err(ConfigError::InvalidOcrConfig {
            reason: "tesseract_path is empty",
        });
    }

    if config.language.trim().is_empty() {
        return Err(ConfigError::InvalidOcrConfig {
            reason: "language is empty",
        });
    }

    Ok(())
}

fn validate_provider_configs(config: &ProvidersConfig) -> Result<(), ConfigError> {
    if let Some(disc_rip) = &config.disc_rip {
        validate_provider_directory("disc_rip", &disc_rip.path)?;
    }
    if let Some(watch_folder) = &config.watch_folder {
        validate_watch_folder_provider_config(watch_folder)?;
    }

    Ok(())
}

fn validate_watch_folder_provider_config(
    config: &WatchFolderProviderConfig,
) -> Result<(), ConfigError> {
    const PROVIDER: &str = "watch_folder";

    validate_provider_directory(PROVIDER, &config.path)?;

    if config.stability_seconds == 0 {
        return Err(ConfigError::InvalidProviderConfig {
            provider: PROVIDER,
            reason: "stability_seconds must be positive",
        });
    }

    Ok(())
}

fn validate_provider_directory(provider: &'static str, path: &Path) -> Result<(), ConfigError> {
    if path.as_os_str().is_empty() {
        return Err(ConfigError::InvalidProviderConfig {
            provider,
            reason: "path is empty",
        });
    }

    let metadata = fs::metadata(path).map_err(|source| ConfigError::InvalidProviderPath {
        provider,
        path: path.to_path_buf(),
        source,
    })?;

    if !metadata.is_dir() {
        return Err(ConfigError::InvalidProviderPath {
            provider,
            path: path.to_path_buf(),
            source: io::Error::other("path is not a directory"),
        });
    }

    Ok(())
}

fn validate_library_root(path: &Path) -> Result<(), ConfigError> {
    let metadata = fs::metadata(path).map_err(|source| ConfigError::InvalidLibraryRoot {
        path: path.to_path_buf(),
        source,
    })?;

    if !metadata.is_dir() {
        return Err(ConfigError::InvalidLibraryRoot {
            path: path.to_path_buf(),
            source: io::Error::other("path is not a directory"),
        });
    }

    Ok(())
}

fn validate_database_path(path: &Path) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let metadata = fs::metadata(parent).map_err(|source| ConfigError::InvalidDatabasePath {
        path: path.to_path_buf(),
        source,
    })?;

    if !metadata.is_dir() {
        return Err(ConfigError::InvalidDatabasePath {
            path: path.to_path_buf(),
            source: io::Error::other("parent path is not a directory"),
        });
    }

    let probe = parent.join(format!(".kino-config-write-test-{}", crate::id::Id::new()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|source| ConfigError::InvalidDatabasePath {
            path: path.to_path_buf(),
            source,
        })?;
    drop(file);

    fs::remove_file(&probe).map_err(|source| ConfigError::InvalidDatabasePath {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use figment::Jail;
    use tempfile::TempDir;

    struct ConfigFixture {
        dir: TempDir,
        database_path: PathBuf,
        database_dir: PathBuf,
        library_root: PathBuf,
    }

    impl ConfigFixture {
        fn new() -> Result<Self, figment::Error> {
            let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
            let database_dir = dir.path().join("db");
            let library_root = dir.path().join("library");

            fs::create_dir(&database_dir).map_err(|e| e.to_string())?;
            fs::create_dir(&library_root).map_err(|e| e.to_string())?;
            fs::create_dir(library_root.join("incoming")).map_err(|e| e.to_string())?;
            fs::create_dir(library_root.join("rips")).map_err(|e| e.to_string())?;

            Ok(Self {
                database_path: database_dir.join("kino.db"),
                database_dir,
                library_root,
                dir,
            })
        }

        fn required_only_toml(&self) -> String {
            required_only_toml(&self.database_path, &self.library_root)
        }
    }

    fn required_only_toml(database_path: &Path, library_root: &Path) -> String {
        format!(
            r#"
                database_path = "{}"
                library_root = "{}"
            "#,
            database_path.display(),
            library_root.display()
        )
    }

    fn full_toml(database_path: &Path, library_root: &Path) -> String {
        let disc_rip = library_root.join("rips");
        let watch_folder = library_root.join("incoming");
        format!(
            r#"
                database_path = "{}"
                library_root = "{}"
                log_level = "debug"

                [library]
                canonical_transfer = "move"
                artwork_cache_dir = "{}/artwork-cache"
                subtitle_staging_dir = "{}/subtitle-staging"

                [server]
                listen = "0.0.0.0:9000"
                public_base_url = "https://kino.example.test"

                [server.session_reaper]
                tick_seconds = 5
                active_to_idle_seconds = 10
                idle_to_ended_seconds = 20

                [tmdb]
                api_key = "test-api-key"
                max_requests_per_second = 10

                [ocr]
                tesseract_path = "/usr/local/bin/tesseract"
                language = "jpn"

                [providers.disc_rip]
                path = "{}"
                preference = 30

                [providers.watch_folder]
                path = "{}"
                preference = 25
                stability_seconds = 7
            "#,
            database_path.display(),
            library_root.display(),
            library_root.display(),
            library_root.display(),
            disc_rip.display(),
            watch_folder.display()
        )
    }

    #[test]
    fn happy_path_full_toml() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &full_toml(&fixture.database_path, &fixture.library_root),
            )?;
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, fixture.database_path);
            assert_eq!(cfg.library_root, fixture.library_root);
            assert_eq!(
                cfg.library.canonical_transfer,
                CanonicalLayoutTransfer::Move
            );
            assert_eq!(
                cfg.library.artwork_cache_dir,
                Some(fixture.library_root.join("artwork-cache"))
            );
            assert_eq!(
                cfg.library.subtitle_staging_dir,
                Some(fixture.library_root.join("subtitle-staging"))
            );
            assert_eq!(cfg.log_level, "debug");
            assert_eq!(cfg.log_format, LogFormat::Pretty);
            assert_eq!(cfg.tmdb.api_key.as_deref(), Some("test-api-key"));
            assert_eq!(cfg.tmdb.max_requests_per_second, 10);
            assert_eq!(
                cfg.ocr.tesseract_path,
                PathBuf::from("/usr/local/bin/tesseract")
            );
            assert_eq!(cfg.ocr.language, "jpn");
            let disc_rip = cfg.providers.disc_rip.expect("disc rip should parse");
            assert_eq!(disc_rip.path, fixture.library_root.join("rips"));
            assert_eq!(disc_rip.preference, 30);
            let watch_folder = cfg
                .providers
                .watch_folder
                .expect("watch folder should parse");
            assert_eq!(watch_folder.path, fixture.library_root.join("incoming"));
            assert_eq!(watch_folder.preference, 25);
            assert_eq!(watch_folder.stability_seconds, 7);
            assert_eq!(
                cfg.server.listen,
                "0.0.0.0:9000".parse::<SocketAddr>().unwrap()
            );
            assert_eq!(cfg.server.public_base_url, "https://kino.example.test");
            assert_eq!(
                cfg.server.session_reaper.tick_interval,
                Duration::from_secs(5)
            );
            assert_eq!(
                cfg.server.session_reaper.active_to_idle,
                Duration::from_secs(10)
            );
            assert_eq!(
                cfg.server.session_reaper.idle_to_ended,
                Duration::from_secs(20)
            );
            Ok(())
        });
    }

    #[test]
    fn defaults_apply_when_optional_fields_omitted() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.log_level, "info");
            assert_eq!(cfg.log_format, LogFormat::Pretty);
            assert_eq!(
                cfg.server.listen,
                "127.0.0.1:7777".parse::<SocketAddr>().unwrap()
            );
            assert_eq!(cfg.server.public_base_url, "http://127.0.0.1:8080");
            assert_eq!(
                cfg.server.session_reaper.tick_interval,
                Duration::from_secs(30)
            );
            assert_eq!(
                cfg.server.session_reaper.active_to_idle,
                Duration::from_secs(60)
            );
            assert_eq!(
                cfg.server.session_reaper.idle_to_ended,
                Duration::from_secs(300)
            );
            assert_eq!(cfg.tmdb.api_key, None);
            assert_eq!(cfg.tmdb.max_requests_per_second, 20);
            assert_eq!(cfg.ocr.tesseract_path, PathBuf::from("tesseract"));
            assert_eq!(cfg.ocr.language, "eng");
            assert_eq!(
                cfg.library.canonical_transfer,
                CanonicalLayoutTransfer::HardLink
            );
            assert_eq!(cfg.library.artwork_cache_dir, None);
            assert_eq!(
                cfg.artwork_cache_dir(),
                fixture.library_root.join(".kino").join("artwork")
            );
            assert_eq!(cfg.library.subtitle_staging_dir, None);
            assert!(cfg.providers.disc_rip.is_none());
            assert!(cfg.providers.watch_folder.is_none());
            Ok(())
        });
    }

    #[test]
    fn missing_required_field_is_invalid_error() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(r#"library_root = "{}""#, fixture.library_root.display()),
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            assert!(err.to_string().contains("database_path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn deny_unknown_fields_rejects_typos() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        databse_path = "{}"
                        library_root = "{}"
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            Ok(())
        });
    }

    #[test]
    fn env_override_supplies_required_field() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(r#"library_root = "{}""#, fixture.library_root.display()),
            )?;
            jail.set_env("KINO_DATABASE_PATH", fixture.database_path.display());
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, fixture.database_path);
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_server_listen() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_SERVER__LISTEN", "0.0.0.0:8080");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(
                cfg.server.listen,
                "0.0.0.0:8080".parse::<SocketAddr>().unwrap()
            );
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_server_public_base_url() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_SERVER__PUBLIC_BASE_URL", "https://kino.example.test");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.server.public_base_url, "https://kino.example.test");
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_server_cors_allowed_origins() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env(
                "KINO_SERVER__CORS_ALLOWED_ORIGINS",
                "http://localhost:3000, https://tools.example.test",
            );
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(
                cfg.server.cors_allowed_origins,
                vec![
                    "http://localhost:3000".to_owned(),
                    "https://tools.example.test".to_owned()
                ]
            );
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_session_reaper() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_SERVER__SESSION_REAPER__TICK_SECONDS", "2");
            jail.set_env("KINO_SERVER__SESSION_REAPER__ACTIVE_TO_IDLE_SECONDS", "3");
            jail.set_env("KINO_SERVER__SESSION_REAPER__IDLE_TO_ENDED_SECONDS", "4");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(
                cfg.server.session_reaper.tick_interval,
                Duration::from_secs(2)
            );
            assert_eq!(
                cfg.server.session_reaper.active_to_idle,
                Duration::from_secs(3)
            );
            assert_eq!(
                cfg.server.session_reaper.idle_to_ended,
                Duration::from_secs(4)
            );
            Ok(())
        });
    }

    #[test]
    fn invalid_server_public_base_url_is_rejected() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [server]
                        public_base_url = "not a url"
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidServerConfig { .. }),
                "got: {err:?}"
            );
            Ok(())
        });
    }

    #[test]
    fn invalid_session_reaper_zero_interval_is_rejected() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [server.session_reaper]
                        tick_seconds = 0
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidSessionReaperConfig { .. }),
                "got: {err:?}"
            );
            Ok(())
        });
    }

    #[test]
    fn env_override_selects_log_format() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_LOG_FORMAT", "json");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.log_format, LogFormat::Json);
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_tmdb_api_key() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_TMDB__API_KEY", "env-api-key");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.tmdb.api_key.as_deref(), Some("env-api-key"));
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_ocr_config() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_OCR__TESSERACT_PATH", "/opt/bin/tesseract");
            jail.set_env("KINO_OCR__LANGUAGE", "spa");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.ocr.tesseract_path, PathBuf::from("/opt/bin/tesseract"));
            assert_eq!(cfg.ocr.language, "spa");
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_library_canonical_transfer() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_LIBRARY__CANONICAL_TRANSFER", "move");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(
                cfg.library.canonical_transfer,
                CanonicalLayoutTransfer::Move
            );
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_library_artwork_cache_dir() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let cache = fixture.library_root.join("artwork-cache-env");

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_LIBRARY__ARTWORK_CACHE_DIR", cache.display());
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.library.artwork_cache_dir, Some(cache.clone()));
            assert_eq!(cfg.artwork_cache_dir(), cache);
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_library_subtitle_staging_dir() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let staging = fixture.library_root.join("subtitle-staging-env");

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_LIBRARY__SUBTITLE_STAGING_DIR", staging.display());
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.library.subtitle_staging_dir, Some(staging));
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_disc_rip_provider() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let disc_rip = fixture.library_root.join("env-rips");
            fs::create_dir(&disc_rip).map_err(|e| e.to_string())?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_PROVIDERS__DISC_RIP__PATH", disc_rip.display());
            jail.set_env("KINO_PROVIDERS__DISC_RIP__PREFERENCE", "12");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            let provider = cfg
                .providers
                .disc_rip
                .expect("disc rip should be configured");
            assert_eq!(provider.path, disc_rip);
            assert_eq!(provider.preference, 12);
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_watch_folder_provider() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let watch_folder = fixture.library_root.join("env-incoming");
            fs::create_dir(&watch_folder).map_err(|e| e.to_string())?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_PROVIDERS__WATCH_FOLDER__PATH", watch_folder.display());
            jail.set_env("KINO_PROVIDERS__WATCH_FOLDER__PREFERENCE", "9");
            jail.set_env("KINO_PROVIDERS__WATCH_FOLDER__STABILITY_SECONDS", "11");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            let provider = cfg
                .providers
                .watch_folder
                .expect("watch folder should be configured");
            assert_eq!(provider.path, watch_folder);
            assert_eq!(provider.preference, 9);
            assert_eq!(provider.stability_seconds, 11);
            Ok(())
        });
    }

    #[test]
    fn watch_folder_provider_stability_seconds_defaults() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let watch_folder = fixture.library_root.join("incoming");

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        {}

                        [providers.watch_folder]
                        path = "{}"
                    "#,
                    fixture.required_only_toml(),
                    watch_folder.display()
                ),
            )?;

            let cfg = Config::load().map_err(|e| e.to_string())?;
            let provider = cfg
                .providers
                .watch_folder
                .expect("watch folder should be configured");
            assert_eq!(provider.stability_seconds, 5);
            Ok(())
        });
    }

    #[test]
    fn rejects_disc_rip_provider_without_path() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [providers.disc_rip]
                        preference = 9
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            assert!(err.to_string().contains("disc_rip"), "got: {err}");
            assert!(err.to_string().contains("path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_disc_rip_provider_path_that_is_not_directory() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let provider_file = fixture.library_root.join("provider-file");
            fs::write(&provider_file, b"not a directory").map_err(|e| e.to_string())?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [providers.disc_rip]
                        path = "{}"
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display(),
                    provider_file.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidProviderPath { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("disc_rip"), "got: {err}");
            assert!(err.to_string().contains("path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_empty_disc_rip_provider_path() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [providers.disc_rip]
                        path = ""
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidProviderConfig { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("disc_rip"), "got: {err}");
            assert!(err.to_string().contains("path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_watch_folder_provider_without_path() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [providers.watch_folder]
                        preference = 9
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            assert!(err.to_string().contains("watch_folder"), "got: {err}");
            assert!(err.to_string().contains("path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_watch_folder_provider_path_that_is_not_directory() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let provider_file = fixture.library_root.join("provider-file");
            fs::write(&provider_file, b"not a directory").map_err(|e| e.to_string())?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [providers.watch_folder]
                        path = "{}"
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display(),
                    provider_file.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidProviderPath { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("watch_folder"), "got: {err}");
            assert!(err.to_string().contains("path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_empty_watch_folder_provider_path() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [providers.watch_folder]
                        path = ""
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidProviderConfig { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("watch_folder"), "got: {err}");
            assert!(err.to_string().contains("path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_empty_tmdb_api_key() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [tmdb]
                        api_key = " "
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::InvalidTmdbConfig { .. }));
            assert!(err.to_string().contains("api_key"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_zero_tmdb_request_rate() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [tmdb]
                        max_requests_per_second = 0
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::InvalidTmdbConfig { .. }));
            assert!(
                err.to_string().contains("max_requests_per_second"),
                "got: {err}"
            );
            Ok(())
        });
    }

    #[test]
    fn rejects_empty_ocr_language() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        database_path = "{}"
                        library_root = "{}"

                        [ocr]
                        language = " "
                    "#,
                    fixture.database_path.display(),
                    fixture.library_root.display()
                ),
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::InvalidOcrConfig { .. }));
            assert!(err.to_string().contains("language"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn kino_log_is_runtime_filter_not_config_field() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_LOG", "debug");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.log_level, "info");
            Ok(())
        });
    }

    #[test]
    fn kino_config_selects_explicit_file() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.create_file("elsewhere.toml", &fixture.required_only_toml())?;
            jail.set_env("KINO_CONFIG", "elsewhere.toml");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, fixture.database_path);
            Ok(())
        });
    }

    #[test]
    fn kino_config_pointing_at_missing_file_is_io_error() {
        Jail::expect_with(|jail| {
            jail.set_env("KINO_CONFIG", "does-not-exist.toml");
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Io { .. }), "got: {err:?}");
            Ok(())
        });
    }

    #[test]
    fn load_from_missing_path_is_io_error() {
        let err = Config::load_from("/definitely/not/a/real/path.toml").unwrap_err();
        assert!(matches!(err, ConfigError::Io { .. }), "got: {err:?}");
    }

    #[test]
    fn load_works_with_no_file_when_env_supplies_required_fields() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;

            jail.set_env("KINO_DATABASE_PATH", fixture.database_path.display());
            jail.set_env("KINO_LIBRARY_ROOT", fixture.library_root.display());
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, fixture.database_path);
            assert_eq!(cfg.library_root, fixture.library_root);
            Ok(())
        });
    }

    #[test]
    fn rejects_missing_library_root() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let missing_library_root = fixture.dir.path().join("missing-library");

            jail.create_file(
                "kino.toml",
                &required_only_toml(&fixture.database_path, &missing_library_root),
            )?;
            let err = Config::load().unwrap_err();

            assert!(
                matches!(err, ConfigError::InvalidLibraryRoot { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("library_root"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_library_root_that_is_not_a_directory() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let library_file = fixture.dir.path().join("library-file");

            fs::write(&library_file, b"not a directory").map_err(|e| e.to_string())?;
            jail.create_file(
                "kino.toml",
                &required_only_toml(&fixture.database_path, &library_file),
            )?;
            let err = Config::load().unwrap_err();

            assert!(
                matches!(err, ConfigError::InvalidLibraryRoot { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("library_root"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_missing_database_parent() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let database_path = fixture.dir.path().join("missing-db-dir").join("kino.db");

            jail.create_file(
                "kino.toml",
                &required_only_toml(&database_path, &fixture.library_root),
            )?;
            let err = Config::load().unwrap_err();

            assert!(
                matches!(err, ConfigError::InvalidDatabasePath { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("database_path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_database_parent_that_is_not_a_directory() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let database_parent = fixture.dir.path().join("db-parent-file");
            let database_path = database_parent.join("kino.db");

            fs::write(&database_parent, b"not a directory").map_err(|e| e.to_string())?;
            jail.create_file(
                "kino.toml",
                &required_only_toml(&database_path, &fixture.library_root),
            )?;
            let err = Config::load().unwrap_err();

            assert!(
                matches!(err, ConfigError::InvalidDatabasePath { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("database_path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_database_parent_that_is_not_writable() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let original_permissions = fs::metadata(&fixture.database_dir)
                .map_err(|e| e.to_string())?
                .permissions();
            let mut readonly_permissions = original_permissions.clone();
            readonly_permissions.set_readonly(true);

            jail.create_file("kino.toml", &fixture.required_only_toml())?;
            fs::set_permissions(&fixture.database_dir, readonly_permissions)
                .map_err(|e| e.to_string())?;
            let result = Config::load();
            fs::set_permissions(&fixture.database_dir, original_permissions)
                .map_err(|e| e.to_string())?;

            let err = result.unwrap_err();
            assert!(
                matches!(err, ConfigError::InvalidDatabasePath { .. }),
                "got: {err:?}"
            );
            assert!(err.to_string().contains("database_path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn rejects_zero_watch_folder_stability_seconds() {
        Jail::expect_with(|jail| {
            let fixture = ConfigFixture::new()?;
            let watch_folder = fixture.library_root.join("incoming");

            jail.create_file(
                "kino.toml",
                &format!(
                    r#"
                        {}

                        [providers.watch_folder]
                        path = "{}"
                        stability_seconds = 0
                    "#,
                    fixture.required_only_toml(),
                    watch_folder.display()
                ),
            )?;

            let err = Config::load().expect_err("zero stability should fail validation");
            assert!(err.to_string().contains("stability_seconds"), "got: {err}");
            Ok(())
        });
    }
}
