use std::{
    error::Error as StdError,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use kino_core::{Config, DeviceToken, Id, Timestamp, user::SEEDED_USER_ID};
use kino_db::Db;
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

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
    ensure_bootstrap_device_token(&db).await?;
    let _session_reaper =
        kino_server::session_reaper::spawn(db.clone(), config.server.session_reaper.into());
    kino_server::serve(&config, db).await?;
    Ok(())
}

async fn ensure_bootstrap_device_token(db: &Db) -> Result<Option<String>, Error> {
    let token_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_tokens")
        .fetch_one(db.read_pool())
        .await?;

    if token_count > 0 {
        return Ok(None);
    }

    let plaintext = generate_plaintext_token()?;
    let token = DeviceToken::new(
        Id::new(),
        SEEDED_USER_ID,
        "bootstrap",
        hash_token(&plaintext),
        Timestamp::now(),
    );

    sqlx::query(
        r#"
        INSERT INTO device_tokens (
            id,
            user_id,
            label,
            hash,
            last_seen_at,
            revoked_at,
            created_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        "#,
    )
    .bind(token.id)
    .bind(token.user_id)
    .bind(&token.label)
    .bind(&token.hash)
    .bind(token.last_seen_at)
    .bind(token.revoked_at)
    .bind(token.created_at)
    .execute(db.write_pool())
    .await?;

    info!(token = %plaintext, "bootstrap token issued");

    Ok(Some(plaintext))
}

fn generate_plaintext_token() -> Result<String, Error> {
    let mut bytes = [0_u8; 32];
    OsRng.try_fill_bytes(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn hash_token(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
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

    #[error("secure random token generation failed: {0}")]
    Random(#[from] rand::Error),

    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),

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
            ocr: Default::default(),
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
            ocr: Default::default(),
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
                (16, String::from("playback state")),
                (17, String::from("playback sessions")),
                (18, String::from("subtitle provenance")),
                (19, String::from("catalog fts")),
                (20, String::from("metadata artwork")),
                (21, String::from("subtitle archive")),
                (22, String::from("watched transitions")),
            ]
        );

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn bootstrap_device_token_is_minted_once() -> Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;

        let first_plaintext = ensure_bootstrap_device_token(&db)
            .await?
            .ok_or("bootstrap token was not minted")?;
        let token_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_tokens")
            .fetch_one(db.read_pool())
            .await?;
        let stored: (String, String) =
            sqlx::query_as("SELECT label, hash FROM device_tokens LIMIT 1")
                .fetch_one(db.read_pool())
                .await?;

        assert_eq!(token_count, 1);
        assert_eq!(stored.0, "bootstrap");
        assert_ne!(stored.1, first_plaintext);
        assert_eq!(stored.1.len(), 64);

        let second_plaintext = ensure_bootstrap_device_token(&db).await?;
        let token_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM device_tokens")
            .fetch_one(db.read_pool())
            .await?;

        assert_eq!(second_plaintext, None);
        assert_eq!(token_count, 1);

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
