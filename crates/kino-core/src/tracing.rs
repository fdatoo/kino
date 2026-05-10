//! Tracing subscriber setup for Kino processes.

use std::ffi::OsString;

use tracing_subscriber::{EnvFilter, fmt};

use crate::{Config, config::LogFormat};

/// Errors produced while initializing tracing.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An environment variable used for logging was not valid Unicode.
    #[error("invalid logging env var {name}: {value:?}")]
    InvalidEnv {
        /// Environment variable name.
        name: &'static str,

        /// Raw non-Unicode value.
        value: OsString,
    },

    /// The selected log filter could not be parsed.
    #[error("invalid log filter {value:?}: {source}")]
    InvalidFilter {
        /// Filter expression from `RUST_LOG`, `KINO_LOG`, or config.
        value: String,

        /// Parser error from `tracing-subscriber`.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// A global tracing subscriber was already installed or could not be set.
    #[error("installing tracing subscriber: {source}")]
    Install {
        /// Subscriber installation error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

/// Install the process-wide tracing subscriber.
///
/// Formatter selection comes from [`Config::log_format`]. Filter precedence is
/// `RUST_LOG`, then `KINO_LOG`, then [`Config::log_level`].
pub fn init(config: &Config) -> Result<(), Error> {
    let filter = env_filter(config)?;
    match config.log_format {
        LogFormat::Pretty => fmt()
            .with_env_filter(filter)
            .pretty()
            .try_init()
            .map_err(|source| Error::Install { source }),
        LogFormat::Json => fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .json()
            .try_init()
            .map_err(|source| Error::Install { source }),
    }
}

fn env_filter(config: &Config) -> Result<EnvFilter, Error> {
    let rust_log = read_env_var("RUST_LOG")?;
    let kino_log = read_env_var("KINO_LOG")?;
    env_filter_from(config, rust_log, kino_log)
}

fn env_filter_from(
    config: &Config,
    rust_log: Option<String>,
    kino_log: Option<String>,
) -> Result<EnvFilter, Error> {
    let value = rust_log
        .or(kino_log)
        .unwrap_or_else(|| config.log_level.clone());
    EnvFilter::try_new(&value).map_err(|source| Error::InvalidFilter {
        value,
        source: Box::new(source),
    })
}

fn read_env_var(name: &'static str) -> Result<Option<String>, Error> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(value)) => Ok(Some(
            value
                .into_string()
                .map_err(|value| Error::InvalidEnv { name, value })?,
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::{
        io::{self, Write},
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use serde_json::Value;
    use tracing::Dispatch;
    use tracing_subscriber::fmt::MakeWriter;

    use super::*;
    use crate::config::{ServerConfig, TmdbConfig};

    #[test]
    fn pretty_format_produces_parseable_utf8_output() {
        let output = emit_with_local_subscriber(LogFormat::Pretty, "info", None, None).unwrap();
        assert!(output.contains("subscriber test event"), "got: {output}");
    }

    #[test]
    fn json_format_produces_parseable_json_output() {
        let output = emit_with_local_subscriber(LogFormat::Json, "info", None, None).unwrap();
        let line = output.lines().find(|line| !line.is_empty()).unwrap();
        let json: Value = serde_json::from_str(line).unwrap();

        assert_eq!(json["level"], "INFO");
        assert_eq!(json["fields"]["message"], "subscriber test event");
        assert_eq!(json["fields"]["answer"], 42);
    }

    #[test]
    fn kino_log_controls_filter_when_rust_log_is_absent() {
        let output =
            emit_with_local_subscriber(LogFormat::Pretty, "error", None, Some("info")).unwrap();
        assert!(output.contains("subscriber test event"), "got: {output}");
    }

    #[test]
    fn rust_log_takes_precedence_over_kino_log() {
        let output =
            emit_with_local_subscriber(LogFormat::Pretty, "info", Some("error"), Some("info"))
                .unwrap();
        assert!(output.is_empty(), "got: {output}");
    }

    fn emit_with_local_subscriber(
        format: LogFormat,
        log_level: &str,
        rust_log: Option<&str>,
        kino_log: Option<&str>,
    ) -> Result<String, Error> {
        let config = config(format, log_level);
        let writer = Buffer::default();
        let filter = env_filter_from(
            &config,
            rust_log.map(ToOwned::to_owned),
            kino_log.map(ToOwned::to_owned),
        )?;

        match format {
            LogFormat::Pretty => {
                let subscriber = fmt()
                    .with_env_filter(filter)
                    .with_writer(writer.clone())
                    .with_ansi(false)
                    .without_time()
                    .pretty()
                    .finish();
                let dispatch = Dispatch::new(subscriber);
                tracing::dispatcher::with_default(&dispatch, emit_event);
            }
            LogFormat::Json => {
                let subscriber = fmt()
                    .with_env_filter(filter)
                    .with_writer(writer.clone())
                    .with_ansi(false)
                    .without_time()
                    .json()
                    .finish();
                let dispatch = Dispatch::new(subscriber);
                tracing::dispatcher::with_default(&dispatch, emit_event);
            }
        }

        Ok(writer.output())
    }

    fn emit_event() {
        tracing::info!(answer = 42, "subscriber test event");
    }

    fn config(log_format: LogFormat, log_level: &str) -> Config {
        Config {
            database_path: PathBuf::from("/db"),
            library_root: PathBuf::from("/lib"),
            server: ServerConfig::default(),
            tmdb: TmdbConfig::default(),
            providers: Default::default(),
            log_level: log_level.into(),
            log_format,
        }
    }

    #[derive(Clone, Default)]
    struct Buffer {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Buffer {
        fn output(&self) -> String {
            let bytes = self.bytes.lock().unwrap().clone();
            String::from_utf8(bytes).unwrap()
        }
    }

    impl<'writer> MakeWriter<'writer> for Buffer {
        type Writer = BufferWriter;

        fn make_writer(&'writer self) -> Self::Writer {
            BufferWriter {
                bytes: Arc::clone(&self.bytes),
            }
        }
    }

    struct BufferWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
    }

    impl Write for BufferWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut bytes = self
                .bytes
                .lock()
                .map_err(|_| io::Error::other("writer lock poisoned"))?;
            bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
}
