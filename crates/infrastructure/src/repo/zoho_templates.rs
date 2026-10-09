//! Hash-only durable intent journal for Zoho Email API v2 template writes.

use crate::{db::Tx, error::map_sqlx};
use contracts::AppError;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OperationRow {
    pub id: Uuid,
    pub state: String,
    pub template_id: Option<String>,
    pub deleted: bool,
    pub request_hmac: Vec<u8>,
    pub provider_config_hmac: Vec<u8>,
}

/// Insert a write intent before any provider call, or return the existing
/// row for safe replay classification. The unique key serializes concurrent
/// use of the same client key within this actor/workspace/endpoint scope.
pub async fn claim(
    tx: &mut Tx<'_>,
    actor: Uuid,
    endpoint: &str,
    idempotency_key_hmac: &[u8; 32],
    request_hmac: &[u8; 32],
    provider_config_hmac: &[u8; 32],
) -> Result<(bool, OperationRow), AppError> {
    let id = Uuid::new_v4();
    let inserted = sqlx::query(
        "INSERT INTO zoho_email_template_operations (id,workspace_id,actor_principal_id,endpoint,idempotency_key_hmac,request_hmac,provider_config_hmac,state) VALUES ($1,$2,$3,$4,$5,$6,$7,'intent') ON CONFLICT (workspace_id,actor_principal_id,endpoint,idempotency_key_hmac) DO NOTHING",
    )
    .bind(id)
    .bind(tx.workspace_id())
    .bind(actor)
    .bind(endpoint)
    .bind(idempotency_key_hmac.as_slice())
    .bind(request_hmac.as_slice())
    .bind(provider_config_hmac.as_slice())
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected()
        == 1;
    let row = sqlx::query_as::<_, OperationRow>(
        "SELECT id,state,template_id,deleted,request_hmac,provider_config_hmac FROM zoho_email_template_operations WHERE workspace_id=$1 AND actor_principal_id=$2 AND endpoint=$3 AND idempotency_key_hmac=$4 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(actor)
    .bind(endpoint)
    .bind(idempotency_key_hmac.as_slice())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((inserted, row))
}

pub async fn mark_unknown(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE zoho_email_template_operations SET state='unknown' WHERE workspace_id=$1 AND id=$2 AND state='intent'")
        .bind(tx.workspace_id()).bind(id).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}

pub async fn complete(
    tx: &mut Tx<'_>,
    id: Uuid,
    template_id: &str,
    deleted: bool,
) -> Result<(), AppError> {
    let changed = sqlx::query("UPDATE zoho_email_template_operations SET state='completed',template_id=$3,deleted=$4,completed_at=now() WHERE workspace_id=$1 AND id=$2 AND state='intent'")
        .bind(tx.workspace_id()).bind(id).bind(template_id).bind(deleted).execute(tx.conn()).await.map_err(map_sqlx)?.rows_affected();
    if changed != 1 {
        return Err(AppError::internal(
            "Zoho template operation journal state changed unexpectedly",
        ));
    }
    Ok(())
}

/// A provider rejection or a stale If-Match means the mutation was not sent
/// (or was definitively rejected), so its key claim may be retried safely.
pub async fn release(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query("DELETE FROM zoho_email_template_operations WHERE workspace_id=$1 AND id=$2 AND state='intent'")
        .bind(tx.workspace_id()).bind(id).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}
