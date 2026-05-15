//! Background pairing reaping.

use std::{future::Future, time::Duration};

use kino_core::{PairingStatus, Timestamp};
use kino_db::Db;
use tokio::{
    sync::oneshot,
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};
use tracing::{debug, error, info};

use crate::PairingTokenStore;

/// Background task that expires and prunes pairing rows.
#[derive(Clone)]
pub struct PairingReaper {
    /// Database handle used for pairing updates.
    pub db: Db,
    /// In-memory token store used for approved pairing one-shot tokens.
    pub token_store: PairingTokenStore,
    /// Timing thresholds used by the reaper loop.
    pub config: PairingReaperConfig,
}

/// Timing thresholds for pairing reaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairingReaperConfig {
    /// Duration between background reaper ticks.
    pub tick_interval: Duration,
    /// Time after terminal pairing expiry before the row is deleted.
    pub retention: Duration,
}

/// Number of rows and staged tokens changed by one reaper pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionCount {
    /// Pending pairing rows changed to expired.
    pub expired_rows: u64,
    /// Terminal pairing rows deleted after retention.
    pub deleted_rows: u64,
    /// In-memory staged tokens removed after expiry.
    pub purged_tokens: u64,
}

/// Errors produced by the pairing reaper.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configured threshold was too large to convert into Kino's timestamp
    /// representation.
    #[error("pairing reaper duration {field} is too large")]
    DurationTooLarge {
        /// Duration field that could not be represented.
        field: &'static str,
    },

    /// A configured threshold cannot be subtracted from the current timestamp.
    #[error("pairing reaper cutoff {field} is out of range")]
    CutoffOutOfRange {
        /// Duration field that produced an out-of-range cutoff.
        field: &'static str,
    },

    /// Updating pairings failed.
    #[error("pairing reaper database update failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Crate-local result alias for pairing reaping.
pub type Result<T> = std::result::Result<T, Error>;

impl Default for PairingReaperConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(60),
            retention: Duration::from_secs(24 * 60 * 60),
        }
    }
}

impl From<kino_core::config::PairingReaperConfig> for PairingReaperConfig {
    fn from(config: kino_core::config::PairingReaperConfig) -> Self {
        Self {
            tick_interval: config.tick_interval,
            retention: config.retention,
        }
    }
}

impl PairingReaper {
    /// Build a reaper with the provided database, token store, and timing
    /// configuration.
    pub fn new(db: Db, token_store: PairingTokenStore, config: PairingReaperConfig) -> Self {
        Self {
            db,
            token_store,
            config,
        }
    }

    /// Run one transition pass using the current UTC timestamp.
    pub async fn transition_once(&self) -> Result<TransitionCount> {
        self.transition_once_at(Timestamp::now()).await
    }

    /// Run one transition pass using an explicit timestamp.
    pub async fn transition_once_at(&self, now: Timestamp) -> Result<TransitionCount> {
        let retention_cutoff = timestamp_before(now, self.config.retention, "retention")?;

        let result = sqlx::query(
            r#"
            UPDATE pairings
            SET status = ?1
            WHERE status = ?2 AND julianday(expires_at) <= julianday(?3)
            "#,
        )
        .bind(PairingStatus::Expired.as_str())
        .bind(PairingStatus::Pending.as_str())
        .bind(now)
        .execute(self.db.write_pool())
        .await?;

        let deleted_rows = delete_expired_pairings(&self.db, retention_cutoff).await?;
        let purged_tokens = self.token_store.purge_expired(now).await;

        Ok(TransitionCount {
            expired_rows: result.rows_affected(),
            deleted_rows,
            purged_tokens: purged_tokens as u64,
        })
    }

    async fn run_until_shutdown<S>(self, shutdown: S)
    where
        S: Future<Output = ()> + Send,
    {
        self.run_until_shutdown_with_clock(shutdown, Timestamp::now)
            .await;
    }

    async fn run_until_shutdown_with_clock<S, F>(self, shutdown: S, mut now: F)
    where
        S: Future<Output = ()> + Send,
        F: FnMut() -> Timestamp + Send,
    {
        if self.config.tick_interval.is_zero() {
            error!("pairing reaper tick interval is zero");
            return;
        }

        info!("pairing reaper started");
        let mut shutdown = std::pin::pin!(shutdown);
        let mut interval = interval(self.config.tick_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    info!("pairing reaper stopped");
                    break;
                }
                _ = interval.tick() => {
                    self.transition_once_and_log(now()).await;
                }
            }
        }
    }

    async fn transition_once_and_log(&self, now: Timestamp) {
        match self.transition_once_at(now).await {
            Ok(count) => {
                debug!(
                    expired = count.expired_rows,
                    deleted = count.deleted_rows,
                    purged = count.purged_tokens,
                    "pairing reaper pass complete"
                );
            }
            Err(err) => {
                error!(error = %err, "pairing reaper pass failed");
            }
        }
    }
}

/// Start the pairing reaper background loop.
pub fn spawn(
    db: Db,
    token_store: PairingTokenStore,
    config: PairingReaperConfig,
) -> JoinHandle<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _shutdown_tx = shutdown_tx;
        PairingReaper::new(db, token_store, config)
            .run_until_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    })
}

#[cfg(test)]
fn spawn_with_shutdown_and_clock<F>(
    db: Db,
    token_store: PairingTokenStore,
    config: PairingReaperConfig,
    shutdown: oneshot::Receiver<()>,
    now: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Timestamp + Send + 'static,
{
    tokio::spawn(async move {
        PairingReaper::new(db, token_store, config)
            .run_until_shutdown_with_clock(
                async move {
                    let _ = shutdown.await;
                },
                now,
            )
            .await;
    })
}

async fn delete_expired_pairings(db: &Db, older_than: Timestamp) -> Result<u64> {
    kino_db::pairings::delete_expired(db, older_than)
        .await
        .map_err(pairing_db_error)
}

fn pairing_db_error(err: kino_db::Error) -> Error {
    match err {
        kino_db::Error::Sqlx(err) => Error::Sqlx(err),
        err => Error::Sqlx(sqlx::Error::Protocol(err.to_string())),
    }
}

fn timestamp_before(now: Timestamp, duration: Duration, field: &'static str) -> Result<Timestamp> {
    let duration =
        time::Duration::try_from(duration).map_err(|_| Error::DurationTooLarge { field })?;
    let Some(cutoff) = now.as_offset().checked_sub(duration) else {
        return Err(Error::CutoffOutOfRange { field });
    };

    Ok(Timestamp::from_offset(cutoff))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::PendingToken;
    use kino_core::{
        Id, Pairing, PairingPlatform, PairingStatus, Timestamp, device_token::DeviceToken,
        user::SEEDED_USER_ID,
    };

    #[tokio::test]
    async fn transition_once_at_expires_and_deletes_eligible_pairings()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let store = PairingTokenStore::new();
        let config = PairingReaperConfig {
            tick_interval: Duration::from_secs(60),
            retention: Duration::from_secs(24 * 60 * 60),
        };
        let now: Timestamp = "2026-05-14T02:00:00Z".parse()?;
        let old_created_at: Timestamp = "2026-05-12T00:00:00Z".parse()?;
        let old_expires_at: Timestamp = "2026-05-12T01:00:00Z".parse()?;
        let pending_expires_at: Timestamp = "2026-05-14T01:59:00Z".parse()?;
        let fresh_expires_at: Timestamp = "2026-05-14T02:05:00Z".parse()?;
        let approved_at: Timestamp = "2026-05-12T00:05:00Z".parse()?;

        let pending_fresh =
            insert_pairing(&db, "100000", "Pending fresh", now, fresh_expires_at).await?;
        let pending_expired = insert_pairing(
            &db,
            "100001",
            "Pending expired",
            old_created_at,
            pending_expires_at,
        )
        .await?;
        let expired_old =
            insert_pairing(&db, "100002", "Expired old", old_created_at, old_expires_at).await?;
        let consumed_old = insert_pairing(
            &db,
            "100003",
            "Consumed old",
            old_created_at,
            old_expires_at,
        )
        .await?;
        let approved_old = insert_pairing(
            &db,
            "100004",
            "Approved old",
            old_created_at,
            old_expires_at,
        )
        .await?;

        kino_db::pairings::update_status(&db, expired_old.id, PairingStatus::Expired, None).await?;
        let consumed_token = insert_device_token(&db, "Consumed old", old_created_at).await?;
        kino_db::pairings::approve(&db, consumed_old.id, consumed_token, approved_at).await?;
        kino_db::pairings::mark_consumed(&db, consumed_old.id).await?;
        let approved_token = insert_device_token(&db, "Approved old", old_created_at).await?;
        kino_db::pairings::approve(&db, approved_old.id, approved_token, approved_at).await?;

        let count = PairingReaper::new(db.clone(), store, config)
            .transition_once_at(now)
            .await?;

        assert_eq!(
            count,
            TransitionCount {
                expired_rows: 1,
                deleted_rows: 2,
                purged_tokens: 0,
            }
        );
        assert_pairing_status(&db, pending_fresh.id, Some(PairingStatus::Pending)).await?;
        assert_pairing_status(&db, pending_expired.id, Some(PairingStatus::Expired)).await?;
        assert_pairing_status(&db, expired_old.id, None).await?;
        assert_pairing_status(&db, consumed_old.id, None).await?;
        assert_pairing_status(&db, approved_old.id, Some(PairingStatus::Approved)).await?;
        assert_eq!(device_token_count(&db).await?, 2);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn transition_once_at_purges_expired_staged_tokens()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let store = PairingTokenStore::new();
        let expired_id = Id::new();
        let fresh_id = Id::new();
        let expired_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let fresh_expires_at: Timestamp = "2026-05-14T01:10:00Z".parse()?;
        let now: Timestamp = "2026-05-14T01:05:00Z".parse()?;

        store
            .insert(expired_id, pending_token("expired-token", expired_at))
            .await;
        store
            .insert(fresh_id, pending_token("fresh-token", fresh_expires_at))
            .await;

        let count = PairingReaper::new(db.clone(), store.clone(), PairingReaperConfig::default())
            .transition_once_at(now)
            .await?;

        assert_eq!(count.purged_tokens, 1);
        assert!(store.take(expired_id).await.is_none());
        assert!(store.take(fresh_id).await.is_some());

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn loop_reaps_on_tick_and_stops_cleanly()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let store = PairingTokenStore::new();
        let config = PairingReaperConfig {
            tick_interval: Duration::from_millis(10),
            retention: Duration::from_secs(180),
        };
        let first_tick: Timestamp = "2026-05-14T01:01:00Z".parse()?;
        let second_tick: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let pairing = insert_pairing(
            &db,
            "100000",
            "Pending expired",
            "2026-05-14T01:00:00Z".parse()?,
            "2026-05-14T01:00:00Z".parse()?,
        )
        .await?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let clock = Arc::new(Mutex::new(first_tick));
        let handle = spawn_with_shutdown_and_clock(db.clone(), store, config, shutdown_rx, {
            let clock = Arc::clone(&clock);
            move || {
                *clock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            }
        });

        wait_for_status(&db, pairing.id, PairingStatus::Expired).await?;
        yield_reaper().await;

        *clock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = second_tick;
        tokio::time::sleep(Duration::from_millis(10)).await;
        yield_reaper().await;
        wait_for_deleted(&db, pairing.id).await?;

        assert!(shutdown_tx.send(()).is_ok());
        handle.await?;
        db.close().await;
        Ok(())
    }

    async fn insert_pairing(
        db: &Db,
        code: &str,
        device_name: &str,
        created_at: Timestamp,
        expires_at: Timestamp,
    ) -> std::result::Result<Pairing, kino_db::Error> {
        let pairing = Pairing::new(
            Id::new(),
            code,
            device_name,
            PairingPlatform::Ios,
            created_at,
            expires_at,
        );
        kino_db::pairings::insert(db, &pairing).await?;
        Ok(pairing)
    }

    async fn insert_device_token(
        db: &Db,
        label: &str,
        created_at: Timestamp,
    ) -> std::result::Result<Id, sqlx::Error> {
        let token = DeviceToken::new(
            Id::new(),
            SEEDED_USER_ID,
            label,
            format!("hash-{}", Id::new()),
            created_at,
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

        Ok(token.id)
    }

    async fn assert_pairing_status(
        db: &Db,
        id: Id,
        expected: Option<PairingStatus>,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let actual = pairing_status(db, id).await?;
        assert_eq!(actual, expected);
        Ok(())
    }

    async fn wait_for_status(
        db: &Db,
        id: Id,
        status: PairingStatus,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        for _ in 0..100 {
            if pairing_status(db, id).await? == Some(status) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_pairing_status(db, id, Some(status)).await
    }

    async fn wait_for_deleted(
        db: &Db,
        id: Id,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        for _ in 0..100 {
            if pairing_status(db, id).await?.is_none() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert_pairing_status(db, id, None).await
    }

    async fn pairing_status(
        db: &Db,
        id: Id,
    ) -> std::result::Result<Option<PairingStatus>, Box<dyn std::error::Error>> {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM pairings WHERE id = ?1")
                .bind(id)
                .fetch_optional(db.read_pool())
                .await?;
        Ok(status
            .as_deref()
            .map(str::parse::<PairingStatus>)
            .transpose()?)
    }

    async fn device_token_count(db: &Db) -> std::result::Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COUNT(*) FROM device_tokens")
            .fetch_one(db.read_pool())
            .await
    }

    fn pending_token(token: &str, expires_at: Timestamp) -> PendingToken {
        PendingToken {
            token: token.to_owned(),
            token_id: Id::new(),
            user_id: SEEDED_USER_ID,
            expires_at,
        }
    }

    async fn yield_reaper() {
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
    }
}
