//! Server identity row queries.

use kino_core::{Id, Timestamp};

use crate::{Db, Result};

/// Return the stable server identity, inserting it on first use.
pub async fn get_or_create(db: &Db) -> Result<Id> {
    let mut tx = db.write_pool().begin().await?;

    let existing: Option<Id> = sqlx::query_scalar("SELECT id FROM server_identity LIMIT 1")
        .fetch_optional(&mut *tx)
        .await?;

    if let Some(id) = existing {
        tx.commit().await?;
        return Ok(id);
    }

    let id = Id::new();
    sqlx::query(
        r#"
        INSERT INTO server_identity (id, created_at)
        VALUES (?1, ?2)
        "#,
    )
    .bind(id)
    .bind(Timestamp::now())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(id)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_call_inserts_identity_and_second_call_reuses_it()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let db = crate::test_db().await?;

        let first = get_or_create(&db).await?;
        let second = get_or_create(&db).await?;
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM server_identity")
            .fetch_one(db.read_pool())
            .await?;

        assert_eq!(first, second);
        assert_eq!(count, 1);

        db.close().await;
        Ok(())
    }
}
