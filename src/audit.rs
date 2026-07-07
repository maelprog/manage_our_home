use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Writes one row to `audit_log`. Callers pass an open transaction so the
/// audit entry commits atomically with the action it records (AC #16).
pub async fn record(
    tx: &mut Transaction<'_, Postgres>,
    actor_user_id: Option<Uuid>,
    action: &str,
    target_type: &str,
    target_id: &str,
    metadata: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO audit_log (actor_user_id, action, target_type, target_id, metadata)
        VALUES ($1, $2, $3, $4, $5)
        "#,
        actor_user_id,
        action,
        target_type,
        target_id,
        metadata,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}
