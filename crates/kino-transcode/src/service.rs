//! Transcode service entry point for ingestion and admin workflows.

use std::sync::Arc;

use kino_core::{Id, ProbeResult};

use crate::{
    JobStore, NewJob, OutputPolicy, Result, Scheduler, SourceContext, SourceFile, TranscodeFuture,
    TranscodeHandOff, TranscodeProfile, TranscodeReceipt,
};

/// Phase 3 transcode service: replaces the Phase 1 `NoopTranscodeHandOff`.
///
/// Holds the configured [`OutputPolicy`], [`JobStore`], and [`Scheduler`].
/// Ingestion calls `submit` with a freshly probed source and the service plans
/// and idempotently enqueues the policy variants.
pub struct TranscodeService {
    store: JobStore,
    policy: Box<dyn OutputPolicy>,
    scheduler: Arc<Scheduler>,
}

impl TranscodeService {
    /// Construct a transcode service from its durable store, policy, and scheduler.
    pub fn new(store: JobStore, policy: Box<dyn OutputPolicy>, scheduler: Arc<Scheduler>) -> Self {
        Self {
            store,
            policy,
            scheduler,
        }
    }

    /// Plan and enqueue all policy variants for a source file.
    pub async fn submit(&self, source: SourceContext) -> Result<Vec<Id>> {
        self.submit_planned(source).await
    }

    /// Cancel a queued or running transcode job.
    pub async fn cancel(&self, job_id: Id) -> Result<()> {
        self.scheduler.cancel_job(job_id).await
    }

    /// Re-run planning for a source file and insert only newly missing variants.
    pub async fn replan(&self, source: SourceContext) -> Result<Vec<Id>> {
        self.submit_planned(source).await
    }

    /// Delete existing outputs and jobs for the source, then enqueue a fresh plan.
    pub async fn retranscode(&self, source: SourceContext) -> Result<Vec<Id>> {
        let source_file_id = source.source_file_id;
        let mut tx = self.store.db().write_pool().begin().await?;
        sqlx::query("DELETE FROM transcode_outputs WHERE source_file_id = ?1")
            .bind(source_file_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM transcode_jobs WHERE source_file_id = ?1")
            .bind(source_file_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        self.submit_planned(source).await
    }

    async fn submit_planned(&self, source: SourceContext) -> Result<Vec<Id>> {
        let mut inserted = Vec::new();

        for variant in self.policy.plan(&source) {
            let profile = TranscodeProfile::from_variant(source.source_file_id, &variant);
            let id = self
                .store
                .insert_job_idempotent(&NewJob {
                    source_file_id: source.source_file_id,
                    profile_json: profile.profile_json(),
                    profile_hash: profile.profile_hash(),
                    lane: self.scheduler.lane_for_variant(&variant)?,
                })
                .await?;

            if let Some(id) = id {
                inserted.push(id);
            }
        }

        Ok(inserted)
    }
}

impl TranscodeHandOff for TranscodeService {
    fn submit<'a>(&'a self, source_file: SourceFile) -> TranscodeFuture<'a, TranscodeReceipt> {
        Box::pin(async move {
            let source = source_context(source_file.id, source_file.probe.clone())?;
            let inserted = TranscodeService::submit(self, source).await?;
            let message = if inserted.is_empty() {
                "transcode jobs already planned".to_owned()
            } else {
                format!("enqueued {} transcode jobs", inserted.len())
            };
            let receipt_id = inserted.first().copied().unwrap_or_else(Id::new);

            Ok(TranscodeReceipt::new(receipt_id, source_file, message))
        })
    }
}

fn source_context(source_file_id: Id, probe: Option<ProbeResult>) -> Result<SourceContext> {
    let Some(probe) = probe else {
        return Err(crate::Error::MissingSourceProbe { id: source_file_id });
    };

    Ok(SourceContext {
        source_file_id,
        probe,
    })
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use kino_core::{ProbeResult, Timestamp};
    use kino_db::Db;

    use super::*;
    use crate::{
        DefaultPolicy, DetectionConfig, ListJobsFilter, PipelineRunner, SchedulerConfig,
        available_encoders,
    };

    #[tokio::test]
    async fn submit_plans_policy_variants_idempotently()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let store = JobStore::new(db.clone());
        let service = service(store.clone()).await?;
        let source = source_context_for(source_file_id, "/library/source.mkv");

        let first = service.submit(source.clone()).await?;
        let second = service.submit(source).await?;
        let jobs = store
            .list_jobs(&ListJobsFilter {
                source_file_id: Some(source_file_id),
                ..ListJobsFilter::default()
            })
            .await?;

        assert_eq!(first.len(), 3);
        assert!(second.is_empty());
        assert_eq!(jobs.len(), 3);
        assert!(jobs.iter().any(|job| job.lane == crate::LaneId::Cpu));

        Ok(())
    }

    #[tokio::test]
    async fn retranscode_removes_existing_outputs_and_jobs()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = kino_db::test_db().await?;
        let source_file_id = insert_source_file(&db, "/library/source.mkv").await?;
        let store = JobStore::new(db.clone());
        let service = service(store).await?;
        let source = source_context_for(source_file_id, "/library/source.mkv");

        let _ = service.submit(source.clone()).await?;
        insert_transcode_output(&db, source_file_id, "/library/source.1080p.mp4").await?;

        let inserted = service.retranscode(source).await?;
        let output_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM transcode_outputs WHERE source_file_id = ?1")
                .bind(source_file_id)
                .fetch_one(db.read_pool())
                .await?;
        let job_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM transcode_jobs WHERE source_file_id = ?1")
                .bind(source_file_id)
                .fetch_one(db.read_pool())
                .await?;

        assert_eq!(inserted.len(), 3);
        assert_eq!(output_count, 0);
        assert_eq!(job_count, 3);

        Ok(())
    }

    async fn service(store: JobStore) -> Result<TranscodeService> {
        let registry = available_encoders(&DetectionConfig::default()).await?;
        let scheduler = Arc::new(Scheduler::new(
            store.clone(),
            Arc::new(registry),
            Arc::new(PipelineRunner::new()),
            SchedulerConfig::default(),
        ));
        Ok(TranscodeService::new(
            store,
            Box::new(DefaultPolicy::default()),
            scheduler,
        ))
    }

    fn source_context_for(source_file_id: Id, path: &str) -> SourceContext {
        SourceContext {
            source_file_id,
            probe: ProbeResult {
                source_path: PathBuf::from(path),
                container: None,
                title: None,
                duration: None,
                video_streams: Vec::new(),
                audio_streams: Vec::new(),
                subtitle_streams: Vec::new(),
            },
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

    async fn insert_transcode_output(
        db: &Db,
        source_file_id: Id,
        path: &str,
    ) -> std::result::Result<(), sqlx::Error> {
        let now = Timestamp::now();
        sqlx::query(
            r#"
            INSERT INTO transcode_outputs (
                id,
                source_file_id,
                path,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
        )
        .bind(Id::new())
        .bind(source_file_id)
        .bind(path)
        .bind(now)
        .bind(now)
        .execute(db.write_pool())
        .await?;

        Ok(())
    }

    #[test]
    fn source_file_handoff_requires_probe_data() {
        let source_file_id = Id::new();
        let error = source_context(source_file_id, None).err();

        assert!(matches!(
            error,
            Some(crate::Error::MissingSourceProbe { id }) if id == source_file_id
        ));
    }
}
