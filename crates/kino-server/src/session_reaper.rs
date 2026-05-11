//! Background playback session reaping.

use std::{future::Future, time::Duration};

use kino_core::{Timestamp, playback_session::PlaybackSessionStatus};
use kino_db::Db;
use tokio::{
    sync::oneshot,
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};
use tracing::{debug, error};

/// Background task that transitions stale playback sessions.
#[derive(Clone)]
pub struct SessionReaper {
    /// Database handle used for session updates.
    pub db: Db,
    /// Timing thresholds used by the reaper loop.
    pub config: SessionReaperConfig,
}

/// Timing thresholds for playback session reaping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionReaperConfig {
    /// Duration between background reaper ticks.
    pub tick_interval: Duration,
    /// Time since last heartbeat before an active session becomes idle.
    pub active_to_idle: Duration,
    /// Time since last heartbeat before an idle session becomes ended.
    pub idle_to_ended: Duration,
}

/// Number of session rows transitioned by one reaper pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionCount {
    /// Rows changed by the transition query.
    pub rows_affected: u64,
}

/// Errors produced by the session reaper.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configured threshold was too large to convert into Kino's timestamp
    /// representation.
    #[error("session reaper duration {field} is too large")]
    DurationTooLarge {
        /// Duration field that could not be represented.
        field: &'static str,
    },

    /// A configured threshold cannot be subtracted from the current timestamp.
    #[error("session reaper cutoff {field} is out of range")]
    CutoffOutOfRange {
        /// Duration field that produced an out-of-range cutoff.
        field: &'static str,
    },

    /// Updating playback sessions failed.
    #[error("session reaper database update failed: {0}")]
    Sqlx(#[from] sqlx::Error),
}

/// Crate-local result alias for session reaping.
pub type Result<T> = std::result::Result<T, Error>;

impl Default for SessionReaperConfig {
    fn default() -> Self {
        Self {
            tick_interval: Duration::from_secs(30),
            active_to_idle: Duration::from_secs(60),
            idle_to_ended: Duration::from_secs(300),
        }
    }
}

impl From<kino_core::config::SessionReaperConfig> for SessionReaperConfig {
    fn from(config: kino_core::config::SessionReaperConfig) -> Self {
        Self {
            tick_interval: config.tick_interval,
            active_to_idle: config.active_to_idle,
            idle_to_ended: config.idle_to_ended,
        }
    }
}

impl SessionReaper {
    /// Build a reaper with the provided database and timing configuration.
    pub fn new(db: Db, config: SessionReaperConfig) -> Self {
        Self { db, config }
    }

    /// Run one transition pass using the current UTC timestamp.
    pub async fn transition_once(&self) -> Result<TransitionCount> {
        self.transition_once_at(Timestamp::now()).await
    }

    /// Run one transition pass using an explicit timestamp.
    pub async fn transition_once_at(&self, now: Timestamp) -> Result<TransitionCount> {
        let active_cutoff = timestamp_before(now, self.config.active_to_idle, "active_to_idle")?;
        let idle_cutoff = timestamp_before(now, self.config.idle_to_ended, "idle_to_ended")?;

        let result = sqlx::query(
            r#"
            UPDATE playback_sessions
            SET
                status = CASE
                    WHEN status = ?1 AND julianday(last_seen_at) < julianday(?2) THEN ?3
                    WHEN status = ?4 AND julianday(last_seen_at) < julianday(?5) THEN ?6
                    ELSE status
                END,
                ended_at = CASE
                    WHEN status = ?4 AND julianday(last_seen_at) < julianday(?5) THEN ?7
                    ELSE ended_at
                END
            WHERE
                (status = ?1 AND julianday(last_seen_at) < julianday(?2))
                OR (status = ?4 AND julianday(last_seen_at) < julianday(?5))
            "#,
        )
        .bind(PlaybackSessionStatus::Active.as_str())
        .bind(active_cutoff)
        .bind(PlaybackSessionStatus::Idle.as_str())
        .bind(PlaybackSessionStatus::Idle.as_str())
        .bind(idle_cutoff)
        .bind(PlaybackSessionStatus::Ended.as_str())
        .bind(now)
        .execute(self.db.write_pool())
        .await?;

        Ok(TransitionCount {
            rows_affected: result.rows_affected(),
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
            error!("session reaper tick interval is zero");
            return;
        }

        let mut shutdown = std::pin::pin!(shutdown);
        let mut interval = interval(self.config.tick_interval);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = &mut shutdown => {
                    debug!("session reaper stopped");
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
                    rows_affected = count.rows_affected,
                    "session reaper pass complete"
                );
            }
            Err(err) => {
                error!(error = %err, "session reaper pass failed");
            }
        }
    }
}

/// Start the playback session reaper background loop.
pub fn spawn(db: Db, config: SessionReaperConfig) -> JoinHandle<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let _shutdown_tx = shutdown_tx;
        SessionReaper::new(db, config)
            .run_until_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await;
    })
}

#[cfg(test)]
fn spawn_with_shutdown_and_clock<F>(
    db: Db,
    config: SessionReaperConfig,
    shutdown: oneshot::Receiver<()>,
    now: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Timestamp + Send + 'static,
{
    tokio::spawn(async move {
        SessionReaper::new(db, config)
            .run_until_shutdown_with_clock(
                async move {
                    let _ = shutdown.await;
                },
                now,
            )
            .await;
    })
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
    use kino_core::{
        Id, PlaybackSession, PlaybackSessionStatus, device_token::DeviceToken, user::SEEDED_USER_ID,
    };

    #[tokio::test]
    async fn transition_once_moves_stale_sessions()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let config = SessionReaperConfig {
            tick_interval: Duration::from_secs(30),
            active_to_idle: Duration::from_secs(60),
            idle_to_ended: Duration::from_secs(300),
        };
        let now: Timestamp = "2026-05-11T01:10:00Z".parse()?;
        let active_session = insert_session(
            &db,
            PlaybackSessionStatus::Active,
            "2026-05-11T01:08:59Z".parse()?,
            None,
        )
        .await?;
        let fresh_active_session = insert_session(
            &db,
            PlaybackSessionStatus::Active,
            "2026-05-11T01:09:30Z".parse()?,
            None,
        )
        .await?;
        let idle_session = insert_session(
            &db,
            PlaybackSessionStatus::Idle,
            "2026-05-11T01:04:59Z".parse()?,
            None,
        )
        .await?;

        let count = SessionReaper::new(db.clone(), config)
            .transition_once_at(now)
            .await?;

        assert_eq!(count.rows_affected, 2);
        assert_session(&db, active_session.id, PlaybackSessionStatus::Idle, None).await?;
        assert_session(
            &db,
            fresh_active_session.id,
            PlaybackSessionStatus::Active,
            None,
        )
        .await?;
        assert_session(
            &db,
            idle_session.id,
            PlaybackSessionStatus::Ended,
            Some(now),
        )
        .await?;

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn loop_reaps_on_tick_and_stops_cleanly()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let config = SessionReaperConfig {
            tick_interval: Duration::from_secs(1),
            active_to_idle: Duration::from_secs(60),
            idle_to_ended: Duration::from_secs(300),
        };
        let first_tick: Timestamp = "2026-05-11T01:01:01Z".parse()?;
        let second_tick: Timestamp = "2026-05-11T01:05:01Z".parse()?;
        let active_session = insert_session(
            &db,
            PlaybackSessionStatus::Active,
            "2026-05-11T01:00:00Z".parse()?,
            None,
        )
        .await?;
        tokio::time::pause();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let clock = Arc::new(Mutex::new(first_tick));
        let handle = spawn_with_shutdown_and_clock(db.clone(), config, shutdown_rx, {
            let clock = Arc::clone(&clock);
            move || {
                *clock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            }
        });

        wait_for_status(&db, active_session.id, PlaybackSessionStatus::Idle).await?;
        yield_reaper().await;

        *clock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = second_tick;
        tokio::time::advance(Duration::from_secs(1)).await;
        yield_reaper().await;
        wait_for_status(&db, active_session.id, PlaybackSessionStatus::Ended).await?;
        assert!(session_status(&db, active_session.id).await?.1.is_some());

        assert!(shutdown_tx.send(()).is_ok());
        handle.await?;
        db.close().await;
        Ok(())
    }

    async fn insert_session(
        db: &Db,
        status: PlaybackSessionStatus,
        last_seen_at: Timestamp,
        ended_at: Option<Timestamp>,
    ) -> std::result::Result<PlaybackSession, sqlx::Error> {
        let now = Timestamp::now();
        let token = DeviceToken::new(
            Id::new(),
            SEEDED_USER_ID,
            format!("test token {}", Id::new()),
            format!("hash-{}", Id::new()),
            now,
        );
        let media_item_id = Id::new();
        let mut session = PlaybackSession::active(
            Id::new(),
            SEEDED_USER_ID,
            token.id,
            media_item_id,
            Id::new().to_string(),
            last_seen_at,
        );
        session.last_seen_at = last_seen_at;
        session.status = status;
        session.ended_at = ended_at;

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

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                season_number,
                episode_number,
                created_at,
                updated_at
            )
            VALUES (?1, 'personal', NULL, NULL, NULL, ?2, ?3)
            "#,
        )
        .bind(media_item_id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO playback_sessions (
                id,
                user_id,
                token_id,
                media_item_id,
                variant_id,
                started_at,
                last_seen_at,
                ended_at,
                status
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
        )
        .bind(session.id)
        .bind(session.user_id)
        .bind(session.token_id)
        .bind(session.media_item_id)
        .bind(&session.variant_id)
        .bind(session.started_at)
        .bind(session.last_seen_at)
        .bind(session.ended_at)
        .bind(session.status.as_str())
        .execute(db.write_pool())
        .await?;

        Ok(session)
    }

    async fn wait_for_status(
        db: &Db,
        id: Id,
        status: PlaybackSessionStatus,
    ) -> std::result::Result<(), sqlx::Error> {
        for _ in 0..10 {
            let actual = session_status(db, id).await?;
            if PlaybackSessionStatus::parse(&actual.0) == Some(status) {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }

        let actual = session_status(db, id).await?;
        assert_eq!(PlaybackSessionStatus::parse(&actual.0), Some(status));
        Ok(())
    }

    async fn yield_reaper() {
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
    }

    async fn assert_session(
        db: &Db,
        id: Id,
        status: PlaybackSessionStatus,
        ended_at: Option<Timestamp>,
    ) -> std::result::Result<(), sqlx::Error> {
        let actual = session_status(db, id).await?;
        assert_eq!(PlaybackSessionStatus::parse(&actual.0), Some(status));
        assert_eq!(actual.1, ended_at);
        Ok(())
    }

    async fn session_status(
        db: &Db,
        id: Id,
    ) -> std::result::Result<(String, Option<Timestamp>), sqlx::Error> {
        sqlx::query_as("SELECT status, ended_at FROM playback_sessions WHERE id = ?1")
            .bind(id)
            .fetch_one(db.read_pool())
            .await
    }
}
