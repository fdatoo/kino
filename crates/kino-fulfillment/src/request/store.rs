//! Request persistence owned by fulfillment.

use kino_core::{
    CanonicalIdentity, CanonicalIdentityId, CanonicalIdentitySource, Id, Request,
    RequestFailureReason, RequestRequester, RequestState, RequestTarget, Timestamp,
};
use kino_db::Db;
use sqlx::{QueryBuilder, Row, Sqlite, sqlite::SqliteRow};

use super::{
    Error, NewRequest, RequestDetail, RequestEventActor, RequestIdentityProvenance,
    RequestIdentityVersion, RequestListQuery, RequestMatchCandidate, RequestStatusEvent, Result,
};

#[derive(Clone)]
pub(super) struct RequestStore {
    db: Db,
}

pub(super) struct NewStatusEvent<'a> {
    pub id: Id,
    pub request_id: Id,
    pub from_state: Option<RequestState>,
    pub to_state: RequestState,
    pub occurred_at: Timestamp,
    pub message: Option<&'a str>,
    pub actor: Option<RequestEventActor>,
}

pub(super) struct NewIdentityVersion {
    pub request_id: Id,
    pub version: u32,
    pub canonical_identity_id: CanonicalIdentityId,
    pub provenance: RequestIdentityProvenance,
    pub status_event_id: Option<Id>,
    pub created_at: Timestamp,
    pub actor: Option<RequestEventActor>,
}

impl RequestStore {
    pub(super) fn new(db: Db) -> Self {
        Self { db }
    }

    pub(super) async fn begin(&self) -> Result<sqlx::Transaction<'_, Sqlite>> {
        Ok(self.db.write_pool().begin().await?)
    }

    pub(super) async fn create_pending(
        &self,
        id: Id,
        request: NewRequest<'_>,
        event: NewStatusEvent<'_>,
    ) -> Result<()> {
        let mut tx = self.begin().await?;

        sqlx::query(
            r#"
            INSERT INTO requests (
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
            )
            VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, NULL, NULL)
            "#,
        )
        .bind(id)
        .bind(request.requester.kind())
        .bind(request.requester.id())
        .bind(request.target_raw_query)
        .bind(RequestState::Pending.as_str())
        .bind(event.occurred_at)
        .bind(event.occurred_at)
        .execute(&mut *tx)
        .await?;

        self.insert_status_event(&mut tx, event).await?;
        tx.commit().await?;
        Ok(())
    }

    pub(super) async fn get(&self, id: Id) -> Result<RequestDetail> {
        let request_row = sqlx::query(
            r#"
            SELECT
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
            FROM requests
            WHERE id = ?1
            "#,
        )
        .bind(id)
        .fetch_optional(self.db.read_pool())
        .await?;

        let Some(request_row) = request_row else {
            return Err(Error::RequestNotFound { id });
        };

        let request = request_from_row(&request_row)?;
        let event_rows = sqlx::query(
            r#"
            SELECT id, request_id, from_state, to_state, occurred_at, message, actor_kind, actor_id
            FROM request_status_events
            WHERE request_id = ?1
            ORDER BY occurred_at, id
            "#,
        )
        .bind(id)
        .fetch_all(self.db.read_pool())
        .await?;
        let status_events = event_rows
            .iter()
            .map(status_event_from_row)
            .collect::<Result<Vec<_>>>()?;
        let identity_rows = sqlx::query(
            r#"
            SELECT
                request_id,
                version,
                canonical_identity_id,
                provenance,
                status_event_id,
                created_at,
                actor_kind,
                actor_id
            FROM request_identity_versions
            WHERE request_id = ?1
            ORDER BY version
            "#,
        )
        .bind(id)
        .fetch_all(self.db.read_pool())
        .await?;
        let identity_versions = identity_rows
            .iter()
            .map(identity_version_from_row)
            .collect::<Result<Vec<_>>>()?;
        let candidate_rows = sqlx::query(
            r#"
            SELECT rank, canonical_identity_id, title, year, popularity, score
            FROM request_match_candidates
            WHERE request_id = ?1
            ORDER BY rank
            "#,
        )
        .bind(id)
        .fetch_all(self.db.read_pool())
        .await?;
        let candidates = candidate_rows
            .iter()
            .map(match_candidate_from_row)
            .collect::<Result<Vec<_>>>()?;

        Ok(RequestDetail {
            request,
            status_events,
            identity_versions,
            candidates,
        })
    }

    pub(super) async fn list(&self, query: RequestListQuery) -> Result<Vec<Request>> {
        let mut builder = QueryBuilder::<Sqlite>::new(
            r#"
            SELECT
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
            FROM requests
            "#,
        );

        if let Some(state) = query.state {
            builder.push(" WHERE state = ");
            builder.push_bind(state.as_str());
        }

        builder.push(" ORDER BY created_at, id LIMIT ");
        builder.push_bind(i64::from(query.limit) + 1);
        builder.push(" OFFSET ");
        builder.push_bind(
            i64::try_from(query.offset).map_err(|_| Error::InvalidListOffset {
                offset: query.offset,
            })?,
        );

        let rows = builder.build().fetch_all(self.db.read_pool()).await?;
        rows.iter().map(request_from_row).collect()
    }

    pub(super) async fn update_model_links(
        &self,
        request_id: Id,
        plan_id: Option<Id>,
        updated_at: Timestamp,
    ) -> Result<u64> {
        let result = sqlx::query(
            r#"
            UPDATE requests
            SET plan_id = ?2,
                updated_at = ?3
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .bind(plan_id)
        .bind(updated_at)
        .execute(self.db.write_pool())
        .await?;

        Ok(result.rows_affected())
    }

    pub(super) async fn request_in_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
    ) -> Result<Request> {
        let row = sqlx::query(
            r#"
            SELECT
                id,
                requester_kind,
                requester_id,
                target_raw_query,
                canonical_identity_id,
                state,
                created_at,
                updated_at,
                plan_id,
                failure_reason
            FROM requests
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .fetch_optional(&mut **tx)
        .await?;

        row.as_ref()
            .map(request_from_row)
            .transpose()?
            .ok_or(Error::RequestNotFound { id: request_id })
    }

    pub(super) async fn update_resolved(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
        canonical_identity_id: CanonicalIdentityId,
        state: RequestState,
        updated_at: Timestamp,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE requests
            SET canonical_identity_id = ?2,
                state = ?3,
                updated_at = ?4,
                failure_reason = NULL
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .bind(canonical_identity_id)
        .bind(state.as_str())
        .bind(updated_at)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub(super) async fn delete_match_candidates(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
    ) -> Result<()> {
        sqlx::query("DELETE FROM request_match_candidates WHERE request_id = ?1")
            .bind(request_id)
            .execute(&mut **tx)
            .await?;

        Ok(())
    }

    pub(super) async fn refresh_disambiguation(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
        updated_at: Timestamp,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE requests
            SET canonical_identity_id = NULL,
                updated_at = ?2,
                failure_reason = NULL
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .bind(updated_at)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub(super) async fn move_to_disambiguation(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
        state: RequestState,
        updated_at: Timestamp,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE requests
            SET canonical_identity_id = NULL,
                state = ?2,
                updated_at = ?3,
                failure_reason = NULL
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .bind(state.as_str())
        .bind(updated_at)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub(super) async fn update_state(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
        state: RequestState,
        updated_at: Timestamp,
        failure_reason: Option<RequestFailureReason>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE requests
            SET state = ?2,
                updated_at = ?3,
                failure_reason = ?4
            WHERE id = ?1
            "#,
        )
        .bind(request_id)
        .bind(state.as_str())
        .bind(updated_at)
        .bind(failure_reason.map(RequestFailureReason::as_str))
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub(super) async fn insert_status_event(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        event: NewStatusEvent<'_>,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO request_status_events (
                id,
                request_id,
                from_state,
                to_state,
                occurred_at,
                message,
                actor_kind,
                actor_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(event.id)
        .bind(event.request_id)
        .bind(event.from_state.map(RequestState::as_str))
        .bind(event.to_state.as_str())
        .bind(event.occurred_at)
        .bind(event.message)
        .bind(event.actor.map(RequestEventActor::kind))
        .bind(event.actor.and_then(RequestEventActor::id))
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub(super) async fn next_identity_version(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
    ) -> Result<u32> {
        let version = sqlx::query_scalar::<_, Option<i64>>(
            r#"
            SELECT MAX(version)
            FROM request_identity_versions
            WHERE request_id = ?1
            "#,
        )
        .bind(request_id)
        .fetch_one(&mut **tx)
        .await?
        .unwrap_or(0)
            + 1;

        version
            .try_into()
            .map_err(|_| Error::InvalidIdentityVersion { version })
    }

    pub(super) async fn insert_identity_version(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        version: NewIdentityVersion,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO request_identity_versions (
                request_id,
                version,
                canonical_identity_id,
                provenance,
                status_event_id,
                created_at,
                actor_kind,
                actor_id
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(version.request_id)
        .bind(i64::from(version.version))
        .bind(version.canonical_identity_id)
        .bind(version.provenance.as_str())
        .bind(version.status_event_id)
        .bind(version.created_at)
        .bind(version.actor.map(RequestEventActor::kind))
        .bind(version.actor.and_then(RequestEventActor::id))
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub(super) async fn ensure_canonical_identity(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        id: CanonicalIdentityId,
        source: CanonicalIdentitySource,
        now: Timestamp,
    ) -> Result<()> {
        let identity = CanonicalIdentity::new(id, source, now, now);

        sqlx::query(
            r#"
            INSERT INTO canonical_identities (
                id,
                provider,
                media_kind,
                tmdb_id,
                source,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(id) DO NOTHING
            "#,
        )
        .bind(identity.id)
        .bind(identity.provider.as_str())
        .bind(identity.kind.as_str())
        .bind(i64::from(identity.tmdb_id.get()))
        .bind(identity.source.as_str())
        .bind(identity.created_at)
        .bind(identity.updated_at)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }

    pub(super) async fn insert_match_candidate(
        &self,
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        request_id: Id,
        candidate: &RequestMatchCandidate,
        created_at: Timestamp,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO request_match_candidates (
                request_id,
                rank,
                canonical_identity_id,
                title,
                year,
                popularity,
                score,
                created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
        )
        .bind(request_id)
        .bind(i64::from(candidate.rank))
        .bind(candidate.canonical_identity_id)
        .bind(candidate.title.as_str())
        .bind(candidate.year)
        .bind(candidate.popularity)
        .bind(candidate.score)
        .bind(created_at)
        .execute(&mut **tx)
        .await?;

        Ok(())
    }
}

fn request_from_row(row: &SqliteRow) -> Result<Request> {
    let failure_reason = row
        .try_get::<Option<&str>, _>("failure_reason")?
        .map(parse_failure_reason)
        .transpose()?;
    let requester = RequestRequester::from_parts(
        row.try_get("requester_kind")?,
        row.try_get::<Option<Id>, _>("requester_id")?,
    )
    .ok_or(Error::InvalidRequester)?;

    Ok(Request {
        id: row.try_get("id")?,
        requester,
        target: RequestTarget {
            raw_query: row.try_get("target_raw_query")?,
            canonical_identity_id: row.try_get("canonical_identity_id")?,
        },
        state: parse_request_state(row.try_get("state")?)?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
        plan_id: row.try_get("plan_id")?,
        failure_reason,
    })
}

fn match_candidate_from_row(row: &SqliteRow) -> Result<RequestMatchCandidate> {
    let canonical_identity_id = row.try_get("canonical_identity_id")?;
    let rank =
        row.try_get::<i64, _>("rank")?
            .try_into()
            .map_err(|_| Error::InvalidMatchCandidate {
                canonical_identity_id,
                reason: "rank is outside u32 range",
            })?;

    Ok(RequestMatchCandidate {
        rank,
        canonical_identity_id,
        title: row.try_get("title")?,
        year: row.try_get("year")?,
        popularity: row.try_get("popularity")?,
        score: row.try_get("score")?,
    })
}

fn status_event_from_row(row: &SqliteRow) -> Result<RequestStatusEvent> {
    let actor = actor_from_row(row)?;
    let from_state = row
        .try_get::<Option<&str>, _>("from_state")?
        .map(parse_request_state)
        .transpose()?;

    Ok(RequestStatusEvent {
        id: row.try_get("id")?,
        request_id: row.try_get("request_id")?,
        from_state,
        to_state: parse_request_state(row.try_get("to_state")?)?,
        occurred_at: row.try_get("occurred_at")?,
        message: row.try_get("message")?,
        actor,
    })
}

fn identity_version_from_row(row: &SqliteRow) -> Result<RequestIdentityVersion> {
    let persisted_version = row.try_get::<i64, _>("version")?;
    let version = persisted_version
        .try_into()
        .map_err(|_| Error::InvalidIdentityVersion {
            version: persisted_version,
        })?;
    let provenance =
        RequestIdentityProvenance::parse(row.try_get("provenance")?).ok_or_else(|| {
            Error::InvalidIdentityProvenance {
                value: row.get::<String, _>("provenance"),
            }
        })?;

    Ok(RequestIdentityVersion {
        request_id: row.try_get("request_id")?,
        version,
        canonical_identity_id: row.try_get("canonical_identity_id")?,
        provenance,
        status_event_id: row.try_get("status_event_id")?,
        created_at: row.try_get("created_at")?,
        actor: actor_from_row(row)?,
    })
}

fn actor_from_row(row: &SqliteRow) -> Result<Option<RequestEventActor>> {
    match (
        row.try_get::<Option<&str>, _>("actor_kind")?,
        row.try_get::<Option<Id>, _>("actor_id")?,
    ) {
        (None, None) => Ok(None),
        (Some("system"), None) => Ok(Some(RequestEventActor::System)),
        (Some("user"), Some(id)) => Ok(Some(RequestEventActor::User(id))),
        _ => Err(Error::InvalidEventActor),
    }
}

fn parse_request_state(value: &str) -> Result<RequestState> {
    RequestState::parse(value).ok_or_else(|| Error::InvalidRequestState {
        value: value.to_owned(),
    })
}

fn parse_failure_reason(value: &str) -> Result<RequestFailureReason> {
    RequestFailureReason::parse(value).ok_or_else(|| Error::InvalidFailureReason {
        value: value.to_owned(),
    })
}
