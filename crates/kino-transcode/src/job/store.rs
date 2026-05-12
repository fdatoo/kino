//! SQLx accessors for durable transcode jobs.

use std::time::Duration;

use kino_core::{Id, Timestamp};
use kino_db::Db;
use sqlx::{QueryBuilder, Row, Sqlite, sqlite::SqliteRow};

use super::{JobState, try_transition};
use crate::{Error, LaneId, Result};

const JOB_FIELDS: &str = r#"
    id,
    source_file_id,
    profile_json,
    profile_hash,
    state,
    lane,
    attempt,
    progress_pct,
    last_error,
    next_attempt_at,
    created_at,
    updated_at,
    started_at,
    completed_at
"#;

/// Row representation of a `transcode_jobs` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodeJob {
    /// Job id.
    pub id: Id,
    /// Source file this job transcodes.
    pub source_file_id: Id,
    /// Canonical JSON profile used to plan the transcode.
    pub profile_json: String,
    /// SHA-256 digest of `profile_json`.
    pub profile_hash: [u8; 32],
    /// Durable scheduler state.
    pub state: JobState,
    /// Resource lane this job must run on.
    pub lane: LaneId,
    /// Number of dispatch attempts recorded for this job.
    pub attempt: u32,
    /// Most recent runner progress percentage.
    pub progress_pct: Option<u8>,
    /// Last failure message recorded for operator visibility.
    pub last_error: Option<String>,
    /// Earliest time this job may be retried.
    pub next_attempt_at: Option<Timestamp>,
    /// Row creation timestamp.
    pub created_at: Timestamp,
    /// Last row update timestamp.
    pub updated_at: Timestamp,
    /// Time the current or most recent active attempt started.
    pub started_at: Option<Timestamp>,
    /// Terminal completion timestamp.
    pub completed_at: Option<Timestamp>,
}

/// Filter for `list_jobs`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListJobsFilter {
    /// Optional state equality filter.
    pub state: Option<JobState>,
    /// Optional lane equality filter.
    pub lane: Option<LaneId>,
    /// Optional source file equality filter.
    pub source_file_id: Option<Id>,
}

/// Insert payload for `insert_job_idempotent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    /// Source file this job transcodes.
    pub source_file_id: Id,
    /// Canonical JSON profile used to plan the transcode.
    pub profile_json: String,
    /// SHA-256 digest of `profile_json`.
    pub profile_hash: [u8; 32],
    /// Resource lane this job must run on.
    pub lane: LaneId,
}

/// Persistent transcode job query layer.
#[derive(Clone)]
pub struct JobStore {
    db: Db,
}

impl JobStore {
    /// Construct a job store backed by `db`.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub(crate) const fn db(&self) -> &Db {
        &self.db
    }

    /// Insert a planned job unless the source/profile pair already exists.
    pub async fn insert_job_idempotent(&self, new: &NewJob) -> Result<Option<Id>> {
        let id = Id::new();
        let now = Timestamp::now();

        let inserted = sqlx::query_scalar::<_, Id>(
            r#"
            INSERT INTO transcode_jobs (
                id,
                source_file_id,
                profile_json,
                profile_hash,
                state,
                lane,
                attempt,
                progress_pct,
                last_error,
                next_attempt_at,
                created_at,
                updated_at,
                started_at,
                completed_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, NULL, NULL, NULL, ?7, ?8, NULL, NULL)
            ON CONFLICT(source_file_id, profile_hash) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(id)
        .bind(new.source_file_id)
        .bind(&new.profile_json)
        .bind(new.profile_hash.as_slice())
        .bind(JobState::Planned.as_str())
        .bind(new.lane.as_str())
        .bind(now)
        .bind(now)
        .fetch_optional(self.db.write_pool())
        .await?;

        Ok(inserted)
    }

    /// Fetch a job by id.
    pub async fn fetch_job(&self, id: Id) -> Result<TranscodeJob> {
        let row = sqlx::query(&format!(
            "SELECT {JOB_FIELDS} FROM transcode_jobs WHERE id = ?1"
        ))
        .bind(id)
        .fetch_optional(self.db.read_pool())
        .await?;

        let Some(row) = row else {
            return Err(Error::JobNotFound { id });
        };

        job_from_row(&row)
    }

    /// List jobs matching all provided filters.
    pub async fn list_jobs(&self, filter: &ListJobsFilter) -> Result<Vec<TranscodeJob>> {
        let mut builder =
            QueryBuilder::<Sqlite>::new(format!("SELECT {JOB_FIELDS} FROM transcode_jobs"));
        let mut has_where = false;

        if let Some(state) = filter.state {
            push_filter_prefix(&mut builder, &mut has_where);
            builder.push("state = ");
            builder.push_bind(state.as_str());
        }

        if let Some(lane) = filter.lane {
            push_filter_prefix(&mut builder, &mut has_where);
            builder.push("lane = ");
            builder.push_bind(lane.as_str());
        }

        if let Some(source_file_id) = filter.source_file_id {
            push_filter_prefix(&mut builder, &mut has_where);
            builder.push("source_file_id = ");
            builder.push_bind(source_file_id);
        }

        builder.push(" ORDER BY created_at ASC");

        let rows = builder.build().fetch_all(self.db.read_pool()).await?;
        rows.iter().map(job_from_row).collect()
    }

    /// Atomically claim the oldest eligible planned job for a lane.
    pub async fn claim_next_for_lane(&self, lane: LaneId) -> Result<Option<TranscodeJob>> {
        let now = Timestamp::now();
        let row = sqlx::query(&format!(
            r#"
            UPDATE transcode_jobs
            SET state = 'running',
                attempt = attempt + 1,
                started_at = ?1,
                updated_at = ?1
            WHERE id = (
                SELECT id
                FROM transcode_jobs
                WHERE state = 'planned'
                  AND lane = ?2
                  AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)
                ORDER BY created_at
                LIMIT 1
            )
            RETURNING {JOB_FIELDS}
            "#
        ))
        .bind(now)
        .bind(lane.as_str())
        .fetch_optional(self.db.write_pool())
        .await?;

        row.as_ref().map(job_from_row).transpose()
    }

    /// Transition a job state with an optimistic current-state guard.
    pub async fn transition_state(
        &self,
        id: Id,
        from: JobState,
        to: JobState,
        error: Option<&str>,
    ) -> Result<TranscodeJob> {
        try_transition(from, to)?;

        let now = Timestamp::now();
        let row = sqlx::query(&format!(
            r#"
            UPDATE transcode_jobs
            SET state = ?1,
                updated_at = ?2,
                completed_at = CASE WHEN ?3 THEN ?2 ELSE completed_at END,
                last_error = ?4
            WHERE id = ?5
              AND state = ?6
            RETURNING {JOB_FIELDS}
            "#
        ))
        .bind(to.as_str())
        .bind(now)
        .bind(to.is_terminal())
        .bind(error)
        .bind(id)
        .bind(from.as_str())
        .fetch_optional(self.db.write_pool())
        .await?;

        let Some(row) = row else {
            return Err(Error::JobNotFound { id });
        };

        job_from_row(&row)
    }

    /// Reset a failed active job to planned with a retry delay.
    pub async fn reset_for_retry(
        &self,
        id: Id,
        error: &str,
        backoff: Duration,
    ) -> Result<TranscodeJob> {
        let now = Timestamp::now();
        let retry_at = timestamp_after(now, backoff)?;
        let row = sqlx::query(&format!(
            r#"
            UPDATE transcode_jobs
            SET state = 'planned',
                next_attempt_at = ?1,
                last_error = ?2,
                updated_at = ?3
            WHERE id = ?4
            RETURNING {JOB_FIELDS}
            "#
        ))
        .bind(retry_at)
        .bind(error)
        .bind(now)
        .bind(id)
        .fetch_optional(self.db.write_pool())
        .await?;

        let Some(row) = row else {
            return Err(Error::JobNotFound { id });
        };

        job_from_row(&row)
    }

    /// Reset all running jobs to planned after process startup recovery.
    pub async fn reset_running_to_planned(&self) -> Result<u64> {
        let now = Timestamp::now();
        let result = sqlx::query(
            r#"
            UPDATE transcode_jobs
            SET state = 'planned',
                attempt = attempt + 1,
                started_at = NULL,
                updated_at = ?1
            WHERE state = 'running'
            "#,
        )
        .bind(now)
        .execute(self.db.write_pool())
        .await?;

        Ok(result.rows_affected())
    }

    /// Update a job's most recent progress percentage.
    pub async fn update_progress(&self, id: Id, pct: u8) -> Result<()> {
        if pct > 100 {
            return Err(Error::InvalidProgressPct { pct });
        }

        let now = Timestamp::now();
        let result = sqlx::query(
            r#"
            UPDATE transcode_jobs
            SET progress_pct = ?1,
                updated_at = ?2
            WHERE id = ?3
            "#,
        )
        .bind(i64::from(pct))
        .bind(now)
        .bind(id)
        .execute(self.db.write_pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(Error::JobNotFound { id });
        }

        Ok(())
    }
}

fn timestamp_after(now: Timestamp, duration: Duration) -> Result<Timestamp> {
    let duration = time::Duration::try_from(duration).map_err(|_| Error::RetryBackoffTooLarge)?;
    let Some(timestamp) = now.as_offset().checked_add(duration) else {
        return Err(Error::RetryTimestampOutOfRange);
    };

    Ok(Timestamp::from_offset(timestamp))
}

fn push_filter_prefix(builder: &mut QueryBuilder<'_, Sqlite>, has_where: &mut bool) {
    if *has_where {
        builder.push(" AND ");
    } else {
        builder.push(" WHERE ");
        *has_where = true;
    }
}

fn job_from_row(row: &SqliteRow) -> Result<TranscodeJob> {
    let profile_hash: Vec<u8> = row.try_get("profile_hash")?;
    let profile_hash = profile_hash
        .try_into()
        .map_err(|hash: Vec<u8>| Error::InvalidProfileHashLength { len: hash.len() })?;
    let attempt =
        u32::try_from(row.try_get::<i64, _>("attempt")?).map_err(|_| Error::InvalidJobAttempt {
            value: row.get("attempt"),
        })?;
    let progress_pct = row
        .try_get::<Option<i64>, _>("progress_pct")?
        .map(|value| {
            u8::try_from(value)
                .ok()
                .filter(|pct| *pct <= 100)
                .ok_or(Error::InvalidJobProgress { value })
        })
        .transpose()?;
    let state = row.try_get::<String, _>("state")?.parse::<JobState>()?;
    let lane = row.try_get::<String, _>("lane")?.parse::<LaneId>()?;

    Ok(TranscodeJob {
        id: row.try_get("id")?,
        source_file_id: row.try_get("source_file_id")?,
        profile_json: row.try_get("profile_json")?,
        profile_hash,
        state,
        lane,
        attempt,
        progress_pct,
        last_error: row.try_get("last_error")?,
        next_attempt_at: row.try_get("next_attempt_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        started_at: row.try_get("started_at")?,
        completed_at: row.try_get("completed_at")?,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use kino_core::{Id, Timestamp};
    use kino_db::Db;

    use super::*;

    #[tokio::test]
    async fn insert_and_fetch_round_trips_job()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/one.mkv").await?;
        let store = JobStore::new(db);
        let new = new_job(source_file_id, 1, LaneId::Cpu);

        let inserted = store.insert_job_idempotent(&new).await?;
        let Some(id) = inserted else {
            panic!("first insert should create a job");
        };
        let fetched = store.fetch_job(id).await?;

        assert_eq!(fetched.id, id);
        assert_eq!(fetched.source_file_id, new.source_file_id);
        assert_eq!(fetched.profile_json, new.profile_json);
        assert_eq!(fetched.profile_hash, new.profile_hash);
        assert_eq!(fetched.state, JobState::Planned);
        assert_eq!(fetched.lane, new.lane);
        assert_eq!(fetched.attempt, 0);
        assert_eq!(fetched.progress_pct, None);
        assert_eq!(fetched.last_error, None);
        assert_eq!(fetched.next_attempt_at, None);
        assert_eq!(fetched.started_at, None);
        assert_eq!(fetched.completed_at, None);

        Ok(())
    }

    #[tokio::test]
    async fn insert_idempotency_returns_none_for_existing_source_and_hash()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/one.mkv").await?;
        let store = JobStore::new(db);
        let new = new_job(source_file_id, 1, LaneId::Cpu);

        assert!(store.insert_job_idempotent(&new).await?.is_some());
        assert_eq!(store.insert_job_idempotent(&new).await?, None);

        Ok(())
    }

    #[tokio::test]
    async fn list_jobs_applies_filters() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let first_source = insert_source_file(&db, "/library/one.mkv").await?;
        let second_source = insert_source_file(&db, "/library/two.mkv").await?;
        let store = JobStore::new(db);
        let first = insert_job(&store, new_job(first_source, 1, LaneId::Cpu)).await?;
        let _second = insert_job(&store, new_job(second_source, 2, LaneId::GpuIntel)).await?;
        let second = store
            .claim_next_for_lane(LaneId::GpuIntel)
            .await?
            .ok_or("expected gpu job")?;

        let planned = store
            .list_jobs(&ListJobsFilter {
                state: Some(JobState::Planned),
                lane: None,
                source_file_id: None,
            })
            .await?;
        assert_eq!(
            planned.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![first]
        );

        let gpu = store
            .list_jobs(&ListJobsFilter {
                state: None,
                lane: Some(LaneId::GpuIntel),
                source_file_id: None,
            })
            .await?;
        assert_eq!(
            gpu.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![second.id]
        );

        let by_source = store
            .list_jobs(&ListJobsFilter {
                state: None,
                lane: None,
                source_file_id: Some(second_source),
            })
            .await?;
        assert_eq!(
            by_source.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![second.id]
        );

        let combined = store
            .list_jobs(&ListJobsFilter {
                state: Some(JobState::Running),
                lane: Some(LaneId::GpuIntel),
                source_file_id: Some(second_source),
            })
            .await?;
        assert_eq!(
            combined.iter().map(|job| job.id).collect::<Vec<_>>(),
            vec![second.id]
        );

        Ok(())
    }

    #[tokio::test]
    async fn claim_next_for_lane_is_atomic() -> std::result::Result<(), Box<dyn std::error::Error>>
    {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/one.mkv").await?;
        let store = JobStore::new(db);
        let id = insert_job(&store, new_job(source_file_id, 1, LaneId::Cpu)).await?;

        let claimed = store
            .claim_next_for_lane(LaneId::Cpu)
            .await?
            .ok_or("expected claim")?;
        let second = store.claim_next_for_lane(LaneId::Cpu).await?;

        assert_eq!(claimed.id, id);
        assert_eq!(claimed.state, JobState::Running);
        assert_eq!(claimed.attempt, 1);
        assert!(claimed.started_at.is_some());
        assert_eq!(second, None);

        Ok(())
    }

    #[tokio::test]
    async fn transition_state_updates_valid_transition()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/one.mkv").await?;
        let store = JobStore::new(db);
        let id = insert_job(&store, new_job(source_file_id, 1, LaneId::Cpu)).await?;

        let running = store
            .transition_state(id, JobState::Planned, JobState::Running, None)
            .await?;
        let verifying = store
            .transition_state(id, JobState::Running, JobState::Verifying, None)
            .await?;
        let completed = store
            .transition_state(id, JobState::Verifying, JobState::Completed, None)
            .await?;

        assert_eq!(running.state, JobState::Running);
        assert_eq!(verifying.state, JobState::Verifying);
        assert_eq!(completed.state, JobState::Completed);
        assert!(completed.completed_at.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn transition_state_rejects_illegal_transition()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/one.mkv").await?;
        let store = JobStore::new(db);
        let id = insert_job(&store, new_job(source_file_id, 1, LaneId::Cpu)).await?;

        let result = store
            .transition_state(id, JobState::Planned, JobState::Completed, None)
            .await;

        assert!(matches!(
            result,
            Err(Error::InvalidTransition {
                from: JobState::Planned,
                to: JobState::Completed
            })
        ));

        Ok(())
    }

    #[tokio::test]
    async fn reset_for_retry_sets_planned_and_next_attempt()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/one.mkv").await?;
        let store = JobStore::new(db);
        let id = insert_job(&store, new_job(source_file_id, 1, LaneId::Cpu)).await?;
        let _claimed = store.claim_next_for_lane(LaneId::Cpu).await?;

        let reset = store
            .reset_for_retry(id, "encoder busy", Duration::from_secs(30))
            .await?;

        assert_eq!(reset.state, JobState::Planned);
        assert_eq!(reset.last_error.as_deref(), Some("encoder busy"));
        assert!(reset.next_attempt_at.is_some());

        Ok(())
    }

    #[tokio::test]
    async fn reset_running_to_planned_affects_only_running_rows()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let running_source = insert_source_file(&db, "/library/running.mkv").await?;
        let planned_source = insert_source_file(&db, "/library/planned.mkv").await?;
        let store = JobStore::new(db);
        let running_id = insert_job(&store, new_job(running_source, 1, LaneId::Cpu)).await?;
        let planned_id = insert_job(&store, new_job(planned_source, 2, LaneId::GpuIntel)).await?;
        let _claimed = store.claim_next_for_lane(LaneId::Cpu).await?;

        let reset_count = store.reset_running_to_planned().await?;
        let running = store.fetch_job(running_id).await?;
        let planned = store.fetch_job(planned_id).await?;

        assert_eq!(reset_count, 1);
        assert_eq!(running.state, JobState::Planned);
        assert_eq!(running.attempt, 2);
        assert_eq!(running.started_at, None);
        assert_eq!(planned.state, JobState::Planned);
        assert_eq!(planned.attempt, 0);

        Ok(())
    }

    #[tokio::test]
    async fn update_progress_persists_percent()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/one.mkv").await?;
        let store = JobStore::new(db);
        let id = insert_job(&store, new_job(source_file_id, 1, LaneId::Cpu)).await?;

        store.update_progress(id, 42).await?;
        let fetched = store.fetch_job(id).await?;

        assert_eq!(fetched.progress_pct, Some(42));

        Ok(())
    }

    async fn insert_job(store: &JobStore, new: NewJob) -> Result<Id> {
        let Some(id) = store.insert_job_idempotent(&new).await? else {
            return Err(Error::JobNotFound {
                id: new.source_file_id,
            });
        };

        Ok(id)
    }

    fn new_job(source_file_id: Id, seed: u8, lane: LaneId) -> NewJob {
        NewJob {
            source_file_id,
            profile_json: format!(r#"{{"variant":{seed}}}"#),
            profile_hash: [seed; 32],
            lane,
        }
    }

    async fn insert_source_file(db: &Db, path: &str) -> std::result::Result<Id, sqlx::Error> {
        let media_item_id = Id::new();
        let source_file_id = Id::new();
        let now = Timestamp::now();

        sqlx::query(
            r#"
            INSERT INTO media_items (
                id,
                media_kind,
                canonical_identity_id,
                created_at,
                updated_at
            )
            VALUES (?1, 'personal', NULL, ?2, ?3)
            "#,
        )
        .bind(media_item_id)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        sqlx::query(
            r#"
            INSERT INTO source_files (
                id,
                media_item_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(source_file_id)
        .bind(media_item_id)
        .bind(path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(source_file_id)
    }
}
