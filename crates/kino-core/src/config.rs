//! Configuration: a TOML file plus environment-variable overrides.

use std::{
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

    /// Logging filter. Accepts any tracing-subscriber `EnvFilter` expression.
    /// Defaults to `"info"`.
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

/// HTTP/gRPC server settings.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Address the server binds to. Defaults to `127.0.0.1:7777`.
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
}

fn default_listen() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 7777)
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
        fig.merge(Env::prefixed("KINO_").split("__").ignore(&["CONFIG"]))
            .extract::<Config>()
            .map_err(|e| ConfigError::Invalid(Box::new(e)))
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
            .merge(Env::prefixed("KINO_").split("__").ignore(&["CONFIG"]))
            .extract::<Config>()
            .map_err(|e| ConfigError::Invalid(Box::new(e)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[allow(clippy::result_large_err)]
mod tests {
    use super::*;
    use figment::Jail;

    const FULL_TOML: &str = r#"
        database_path = "/var/lib/kino/kino.db"
        library_root = "/srv/media"
        log_level = "debug"

        [server]
        listen = "0.0.0.0:9000"
    "#;

    const REQUIRED_ONLY_TOML: &str = r#"
        database_path = "/db"
        library_root = "/lib"
    "#;

    #[test]
    fn happy_path_full_toml() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", FULL_TOML)?;
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/var/lib/kino/kino.db"));
            assert_eq!(cfg.library_root, PathBuf::from("/srv/media"));
            assert_eq!(cfg.log_level, "debug");
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
            jail.create_file("kino.toml", REQUIRED_ONLY_TOML)?;
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.log_level, "info");
            assert_eq!(
                cfg.server.listen,
                "127.0.0.1:7777".parse::<SocketAddr>().unwrap()
            );
            Ok(())
        });
    }

    #[test]
    fn missing_required_field_is_invalid_error() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", r#"library_root = "/lib""#)?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            assert!(err.to_string().contains("database_path"), "got: {err}");
            Ok(())
        });
    }

    #[test]
    fn deny_unknown_fields_rejects_typos() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "kino.toml",
                r#"
                    databse_path = "/typo"
                    library_root = "/lib"
                "#,
            )?;
            let err = Config::load().unwrap_err();
            assert!(matches!(err, ConfigError::Invalid(_)), "got: {err:?}");
            Ok(())
        });
    }

    #[test]
    fn env_override_supplies_required_field() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", r#"library_root = "/lib""#)?;
            jail.set_env("KINO_DATABASE_PATH", "/from-env");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/from-env"));
            Ok(())
        });
    }

    #[test]
    fn nested_env_override_for_server_listen() {
        Jail::expect_with(|jail| {
            jail.create_file("kino.toml", REQUIRED_ONLY_TOML)?;
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
    fn kino_config_selects_explicit_file() {
        Jail::expect_with(|jail| {
            jail.create_file("elsewhere.toml", REQUIRED_ONLY_TOML)?;
            jail.set_env("KINO_CONFIG", "elsewhere.toml");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/db"));
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
            jail.set_env("KINO_DATABASE_PATH", "/d");
            jail.set_env("KINO_LIBRARY_ROOT", "/l");
            let cfg = Config::load().map_err(|e| e.to_string())?;
            assert_eq!(cfg.database_path, PathBuf::from("/d"));
            assert_eq!(cfg.library_root, PathBuf::from("/l"));
            Ok(())
        });
    }
}
