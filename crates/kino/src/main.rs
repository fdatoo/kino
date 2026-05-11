use std::{
    error::Error as StdError,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use kino_core::Config;
use kino_db::Db;
use tracing::warn;

#[tokio::main]
async fn main() -> ExitCode {
    match start().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report_error(&err);
            ExitCode::FAILURE
        }
    }
}

async fn start() -> Result<(), Error> {
    let config = Config::load()?;
    kino_core::tracing::init(&config)?;
    warn_if_library_root_non_empty(&config.library_root)?;
    run(config).await
}

async fn run(config: Config) -> Result<(), Error> {
    let db = Db::open(&config).await?;
    kino_server::serve(&config, db).await?;
    Ok(())
}

fn report_error(err: &Error) {
    eprintln!("error: {err}");

    let mut source = err.source();
    while let Some(err) = source {
        eprintln!("caused by: {err}");
        source = err.source();
    }
}

fn warn_if_library_root_non_empty(path: &Path) -> Result<(), Error> {
    if library_root_contains_entries(path)? {
        warn!(
            library_root = %path.display(),
            "this directory will be owned by Kino; existing contents will be treated as Kino-managed storage"
        );
    }

    Ok(())
}

fn library_root_contains_entries(path: &Path) -> Result<bool, Error> {
    let mut entries = std::fs::read_dir(path).map_err(|source| Error::LibraryRootRead {
        path: path.to_path_buf(),
        source,
    })?;

    match entries.next() {
        Some(Ok(_)) => Ok(true),
        Some(Err(source)) => Err(Error::LibraryRootRead {
            path: path.to_path_buf(),
            source,
        }),
        None => Ok(false),
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Config(#[from] kino_core::config::ConfigError),

    #[error(transparent)]
    Tracing(#[from] kino_core::tracing::Error),

    #[error(transparent)]
    Db(#[from] kino_db::Error),

    #[error(transparent)]
    Server(#[from] kino_server::Error),

    #[error("reading library_root {path}: {source}", path = .path.display())]
    LibraryRootRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kino_core::config::ServerConfig;

    use super::*;

    #[tokio::test]
    async fn database_startup_applies_migrations() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let database_path = dir.path().join("kino.db");
        let config = Config {
            database_path: database_path.clone(),
            library_root: PathBuf::from("/srv/media"),
            library: Default::default(),
            server: ServerConfig::default(),
            tmdb: Default::default(),
            providers: Default::default(),
            log_level: "info".into(),
            log_format: Default::default(),
        };

        let db = Db::open(&config).await?;
        db.close().await;

        let db = Db::open(&Config {
            database_path,
            library_root: PathBuf::from("/srv/media"),
            library: Default::default(),
            server: ServerConfig::default(),
            tmdb: Default::default(),
            providers: Default::default(),
            log_level: "info".into(),
            log_format: Default::default(),
        })
        .await?;
        let applied: Vec<(i64, String)> =
            sqlx::query_as("SELECT version, description FROM schema_migrations ORDER BY version")
                .fetch_all(db.write_pool())
                .await?;

        assert_eq!(
            applied,
            vec![
                (1, String::from("initial")),
                (2, String::from("request status events")),
                (3, String::from("request list index")),
                (4, String::from("request model fields")),
                (5, String::from("request match candidates")),
                (6, String::from("request identity versions")),
                (7, String::from("canonical identities")),
                (8, String::from("request fulfillment plans")),
                (9, String::from("minimal media items")),
                (10, String::from("subtitle sidecars")),
                (11, String::from("metadata cache")),
                (12, String::from("source files")),
                (13, String::from("core catalog schemas")),
                (14, String::from("users")),
                (15, String::from("device tokens")),
            ]
        );

        db.close().await;
        Ok(())
    }

    #[test]
    fn library_root_entry_check_reports_non_empty_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        assert!(!library_root_contains_entries(dir.path())?);

        std::fs::write(dir.path().join("existing.mkv"), b"existing")?;

        assert!(library_root_contains_entries(dir.path())?);
        Ok(())
    }
}
