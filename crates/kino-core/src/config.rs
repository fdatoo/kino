//! Configuration: a TOML file plus environment-variable overrides.

use std::{
    fs::{self, OpenOptions},
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;

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

    /// HTTP/gRPC server settings. Optional; defaults documented on
    /// [`ServerConfig`].
    #[serde(default)]
    pub server: ServerConfig,

    /// TMDB API client settings. Optional; defaults documented on
    /// [`TmdbConfig`].
    #[serde(default)]
    pub tmdb: TmdbConfig,

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

/// HTTP/gRPC server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the server binds to. Defaults to `127.0.0.1:7777`.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
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

fn default_listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777)
}

fn default_tmdb_max_requests_per_second() -> u32 {
    20
}

fn default_log_level() -> String {
    "info".into()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: default_listen(),
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

    /// The configured TMDB client settings are invalid.
    #[error("invalid tmdb config: {reason}")]
    InvalidTmdbConfig {
        /// Human-readable validation failure.
        reason: &'static str,
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
        validate_tmdb_config(&self.tmdb)?;
        Ok(self)
    }
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
        format!(
            r#"
                database_path = "{}"
                library_root = "{}"
                log_level = "debug"

                [server]
                listen = "0.0.0.0:9000"

                [tmdb]
                api_key = "test-api-key"
                max_requests_per_second = 10
            "#,
            database_path.display(),
            library_root.display()
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
            assert_eq!(cfg.log_level, "debug");
            assert_eq!(cfg.log_format, LogFormat::Pretty);
            assert_eq!(cfg.tmdb.api_key.as_deref(), Some("test-api-key"));
            assert_eq!(cfg.tmdb.max_requests_per_second, 10);
            assert_eq!(
                cfg.server.listen,
                "0.0.0.0:9000".parse::<SocketAddr>().unwrap()
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
            assert_eq!(cfg.tmdb.api_key, None);
            assert_eq!(cfg.tmdb.max_requests_per_second, 20);
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
}
