//! Pairing row queries.

use kino_core::{Id, Pairing, PairingPlatform, PairingStatus, Timestamp};

use crate::{Db, Error, Result};

const FRESH_CODE_ATTEMPTS: usize = 8;

type PairingRow = (
    Id,
    String,
    String,
    String,
    String,
    Option<Id>,
    Timestamp,
    Timestamp,
    Option<Timestamp>,
);

/// Insert a pairing row.
pub async fn insert(db: &Db, pairing: &Pairing) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO pairings (
            id,
            code,
            device_name,
            platform,
            status,
            token_id,
            created_at,
            expires_at,
            approved_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
    )
    .bind(pairing.id)
    .bind(&pairing.code)
    .bind(&pairing.device_name)
    .bind(pairing.platform.as_str())
    .bind(pairing.status.as_str())
    .bind(pairing.token_id)
    .bind(pairing.created_at)
    .bind(pairing.expires_at)
    .bind(pairing.approved_at)
    .execute(db.write_pool())
    .await?;

    Ok(())
}

/// Find a pairing by its six-digit code.
pub async fn find_by_code(db: &Db, code: &str) -> Result<Option<Pairing>> {
    let row: Option<PairingRow> = sqlx::query_as(
        r#"
        SELECT
            id,
            code,
            device_name,
            platform,
            status,
            token_id,
            created_at,
            expires_at,
            approved_at
        FROM pairings
        WHERE code = ?1
        "#,
    )
    .bind(code)
    .fetch_optional(db.read_pool())
    .await?;

    row.map(pairing_from_row).transpose()
}

/// Update a pairing's lifecycle status and approval timestamp.
///
/// Use [`approve`] for the pending-to-approved transition so the token and
/// approval timestamp are written in the same constrained statement.
pub async fn update_status(
    db: &Db,
    id: Id,
    status: PairingStatus,
    approved_at: Option<Timestamp>,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE pairings
        SET status = ?1, approved_at = ?2
        WHERE id = ?3
        "#,
    )
    .bind(status.as_str())
    .bind(approved_at)
    .bind(id)
    .execute(db.write_pool())
    .await?;

    Ok(result.rows_affected())
}

/// Link a device token to an already approved or consumed pairing.
pub async fn link_token(db: &Db, id: Id, token_id: Id) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE pairings
        SET token_id = ?1
        WHERE id = ?2
        "#,
    )
    .bind(token_id)
    .bind(id)
    .execute(db.write_pool())
    .await?;

    Ok(result.rows_affected())
}

/// Atomically approve a pending pairing and attach the minted device token.
pub async fn approve(db: &Db, id: Id, token_id: Id, approved_at: Timestamp) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE pairings
        SET status = ?1, token_id = ?2, approved_at = ?3
        WHERE id = ?4 AND status = 'pending'
        "#,
    )
    .bind(PairingStatus::Approved.as_str())
    .bind(token_id)
    .bind(approved_at)
    .bind(id)
    .execute(db.write_pool())
    .await?;

    Ok(result.rows_affected())
}

/// Mark an approved pairing consumed without changing its approval metadata.
pub async fn mark_consumed(db: &Db, id: Id) -> Result<u64> {
    let result = sqlx::query(
        r#"
        UPDATE pairings
        SET status = ?1
        WHERE id = ?2 AND status = 'approved'
        "#,
    )
    .bind(PairingStatus::Consumed.as_str())
    .bind(id)
    .execute(db.write_pool())
    .await?;

    Ok(result.rows_affected())
}

/// Delete old terminal pairings.
pub async fn delete_expired(db: &Db, older_than: Timestamp) -> Result<u64> {
    let result = sqlx::query(
        r#"
        DELETE FROM pairings
        WHERE status IN ('expired', 'consumed') AND expires_at < ?1
        "#,
    )
    .bind(older_than)
    .execute(db.write_pool())
    .await?;

    Ok(result.rows_affected())
}

/// Insert a pairing, retrying fresh codes when the generated code collides.
pub async fn try_insert_with_fresh_code<F>(
    db: &Db,
    mut generate: F,
    device_name: impl Into<String>,
    platform: PairingPlatform,
    created_at: Timestamp,
    expires_at: Timestamp,
) -> Result<Pairing>
where
    F: FnMut() -> String,
{
    let device_name = device_name.into();
    for attempt in 0..FRESH_CODE_ATTEMPTS {
        let pairing = Pairing::new(
            Id::new(),
            generate(),
            device_name.clone(),
            platform,
            created_at,
            expires_at,
        );

        match insert(db, &pairing).await {
            Ok(()) => return Ok(pairing),
            Err(Error::Sqlx(err))
                if is_unique_code_violation(&err) && attempt + 1 < FRESH_CODE_ATTEMPTS => {}
            Err(err) => return Err(err),
        }
    }

    unreachable!("fresh code insertion loop returns on the final failed attempt")
}

/// Return true when an error is caused by a unique collision on `pairings.code`.
pub fn is_unique_code_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) => {
            database_error.is_unique_violation()
                && database_error.message().contains("pairings.code")
        }
        _ => false,
    }
}

fn pairing_from_row(row: PairingRow) -> Result<Pairing> {
    let platform = row
        .3
        .parse::<PairingPlatform>()
        .map_err(|err| Error::Sqlx(sqlx::Error::Decode(Box::new(err))))?;
    let status = row
        .4
        .parse::<PairingStatus>()
        .map_err(|err| Error::Sqlx(sqlx::Error::Decode(Box::new(err))))?;

    Ok(Pairing {
        id: row.0,
        code: row.1,
        device_name: row.2,
        platform,
        status,
        token_id: row.5,
        created_at: row.6,
        expires_at: row.7,
        approved_at: row.8,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use kino_core::{DeviceToken, SEEDED_USER_ID};

    use super::*;

    #[tokio::test]
    async fn pairing_moves_from_pending_to_approved_to_consumed()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = crate::test_db().await?;
        let created_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let approved_at: Timestamp = "2026-05-14T01:01:00Z".parse()?;
        let pairing = Pairing::new(
            Id::new(),
            "123456",
            "Living room Apple TV",
            PairingPlatform::Tvos,
            created_at,
            expires_at,
        );
        let token_id = insert_device_token(&db, "Living room Apple TV", created_at).await?;
        let replacement_token_id =
            insert_device_token(&db, "Replacement Apple TV", created_at).await?;

        insert(&db, &pairing).await?;

        let pending = find_by_code(&db, "123456").await?.unwrap();
        assert_eq!(pending.status, PairingStatus::Pending);
        assert_eq!(pending.token_id, None);
        assert_eq!(pending.approved_at, None);

        let approved = approve(&db, pairing.id, token_id, approved_at).await?;
        assert_eq!(approved, 1);

        let linked = link_token(&db, pairing.id, replacement_token_id).await?;
        assert_eq!(linked, 1);

        let approved_pairing = find_by_code(&db, "123456").await?.unwrap();
        assert_eq!(approved_pairing.status, PairingStatus::Approved);
        assert_eq!(approved_pairing.token_id, Some(replacement_token_id));
        assert_eq!(approved_pairing.approved_at, Some(approved_at));

        let consumed = mark_consumed(&db, pairing.id).await?;
        assert_eq!(consumed, 1);

        let consumed_pairing = find_by_code(&db, "123456").await?.unwrap();
        assert_eq!(consumed_pairing.status, PairingStatus::Consumed);
        assert_eq!(consumed_pairing.token_id, Some(replacement_token_id));
        assert_eq!(consumed_pairing.approved_at, Some(approved_at));

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn pairing_moves_from_pending_to_expired()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = crate::test_db().await?;
        let created_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let pairing = Pairing::new(
            Id::new(),
            "234567",
            "Bedroom iPad",
            PairingPlatform::Ios,
            created_at,
            expires_at,
        );

        insert(&db, &pairing).await?;
        let updated = update_status(&db, pairing.id, PairingStatus::Expired, None).await?;

        assert_eq!(updated, 1);
        let expired = find_by_code(&db, "234567").await?.unwrap();
        assert_eq!(expired.status, PairingStatus::Expired);
        assert_eq!(expired.token_id, None);
        assert_eq!(expired.approved_at, None);

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn fresh_code_insert_retries_unique_code_collisions()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = crate::test_db().await?;
        let created_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let existing = Pairing::new(
            Id::new(),
            "345678",
            "Existing iPhone",
            PairingPlatform::Ios,
            created_at,
            expires_at,
        );
        let mut codes = vec![String::from("456789"), String::from("345678")];

        insert(&db, &existing).await?;
        let inserted = try_insert_with_fresh_code(
            &db,
            || codes.pop().unwrap_or_else(|| String::from("567890")),
            "Kitchen Mac",
            PairingPlatform::Macos,
            created_at,
            expires_at,
        )
        .await?;

        assert_eq!(inserted.code, "456789");
        assert_ne!(inserted.id, existing.id);
        assert!(find_by_code(&db, "456789").await?.is_some());

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_code_insert_surfaces_unique_violation()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = crate::test_db().await?;
        let created_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let first = Pairing::new(
            Id::new(),
            "345678",
            "Existing iPhone",
            PairingPlatform::Ios,
            created_at,
            expires_at,
        );
        let second = Pairing::new(
            Id::new(),
            "345678",
            "Kitchen Mac",
            PairingPlatform::Macos,
            created_at,
            expires_at,
        );

        insert(&db, &first).await?;
        let err = match insert(&db, &second).await {
            Ok(()) => panic!("duplicate pairing code was accepted"),
            Err(Error::Sqlx(err)) => err,
            Err(err) => panic!("unexpected error: {err}"),
        };

        assert!(is_unique_code_violation(&err));

        db.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn delete_expired_removes_only_old_terminal_rows()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = crate::test_db().await?;
        let old_created_at: Timestamp = "2026-05-14T01:00:00Z".parse()?;
        let old_expires_at: Timestamp = "2026-05-14T01:05:00Z".parse()?;
        let new_expires_at: Timestamp = "2026-05-14T01:15:00Z".parse()?;
        let approved_at: Timestamp = "2026-05-14T01:01:00Z".parse()?;
        let cutoff: Timestamp = "2026-05-14T01:10:00Z".parse()?;

        let pending = Pairing::new(
            Id::new(),
            "100000",
            "Pending",
            PairingPlatform::Ios,
            old_created_at,
            old_expires_at,
        );
        let approved = Pairing::new(
            Id::new(),
            "100001",
            "Approved",
            PairingPlatform::Tvos,
            old_created_at,
            old_expires_at,
        );
        let expired_old = Pairing::new(
            Id::new(),
            "100002",
            "Expired old",
            PairingPlatform::Macos,
            old_created_at,
            old_expires_at,
        );
        let expired_new = Pairing::new(
            Id::new(),
            "100003",
            "Expired new",
            PairingPlatform::Macos,
            old_created_at,
            new_expires_at,
        );
        let consumed = Pairing::new(
            Id::new(),
            "100004",
            "Consumed",
            PairingPlatform::Ios,
            old_created_at,
            old_expires_at,
        );

        for pairing in [&pending, &approved, &expired_old, &expired_new, &consumed] {
            insert(&db, pairing).await?;
        }

        let approved_token = insert_device_token(&db, "Approved", old_created_at).await?;
        let consumed_token = insert_device_token(&db, "Consumed", old_created_at).await?;
        approve(&db, approved.id, approved_token, approved_at).await?;
        approve(&db, consumed.id, consumed_token, approved_at).await?;
        update_status(&db, expired_old.id, PairingStatus::Expired, None).await?;
        update_status(&db, expired_new.id, PairingStatus::Expired, None).await?;
        mark_consumed(&db, consumed.id).await?;

        let deleted = delete_expired(&db, cutoff).await?;

        assert_eq!(deleted, 2);
        assert!(find_by_code(&db, "100000").await?.is_some());
        assert!(find_by_code(&db, "100001").await?.is_some());
        assert!(find_by_code(&db, "100002").await?.is_none());
        assert!(find_by_code(&db, "100003").await?.is_some());
        assert!(find_by_code(&db, "100004").await?.is_none());

        db.close().await;
        Ok(())
    }

    async fn insert_device_token(db: &Db, label: &str, created_at: Timestamp) -> Result<Id> {
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
}
