use crate::{db::Tx, error::map_sqlx};
use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub struct ConnectionRow {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub region: String,
    pub plan: String,
    pub secret_ref: String,
    pub last_verified_at: Option<DateTime<Utc>>,
    pub lists_count: i32,
    pub created_at: DateTime<Utc>,
}
#[derive(sqlx::FromRow)]
pub struct OAuthStateRow {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub principal_id: Uuid,
    pub session_id: Uuid,
    pub redirect_uri: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}
pub async fn list_connections(tx: &mut Tx<'_>) -> Result<Vec<ConnectionRow>, AppError> {
    sqlx::query_as("SELECT m.id,m.name,c.status,c.region,m.plan,c.secret_ref,m.last_verified_at,m.lists_count,m.created_at FROM marketing_connections m JOIN integration_connections c ON c.workspace_id=m.workspace_id AND c.id=m.id WHERE m.workspace_id=$1 AND c.provider='zoho_campaigns' ORDER BY m.created_at DESC LIMIT 100")
    .bind(tx.workspace_id()).fetch_all(tx.conn()).await.map_err(map_sqlx)
}
pub async fn get_connection(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ConnectionRow>, AppError> {
    sqlx::query_as("SELECT m.id,m.name,c.status,c.region,m.plan,c.secret_ref,m.last_verified_at,m.lists_count,m.created_at FROM marketing_connections m JOIN integration_connections c ON c.workspace_id=m.workspace_id AND c.id=m.id WHERE m.workspace_id=$1 AND m.id=$2 AND c.provider='zoho_campaigns'")
    .bind(tx.workspace_id()).bind(id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}
pub async fn insert_connection(
    tx: &mut Tx<'_>,
    id: Uuid,
    actor: Uuid,
    secret_ref: &str,
    scopes: &[String],
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO integration_connections(id,workspace_id,provider,region,secret_ref,scopes,status,connected_by) VALUES($1,$2,'zoho_campaigns','com',$3,$4,'pending',$5)")
    .bind(id).bind(tx.workspace_id()).bind(secret_ref).bind(scopes).bind(actor).execute(tx.conn()).await.map_err(map_sqlx)?;
    sqlx::query("INSERT INTO marketing_connections(id,workspace_id) VALUES($1,$2)")
        .bind(id)
        .bind(tx.workspace_id())
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    Ok(())
}
pub async fn insert_state(
    tx: &mut Tx<'_>,
    connection: Uuid,
    actor: Uuid,
    session: Uuid,
    hash: &[u8],
    redirect: &str,
    expires: DateTime<Utc>,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO marketing_oauth_states(workspace_id,connection_id,principal_id,session_id,state_hash,redirect_uri,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
    .bind(tx.workspace_id()).bind(connection).bind(actor).bind(session).bind(hash).bind(redirect).bind(expires).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}
pub async fn lock_state(tx: &mut Tx<'_>, hash: &[u8]) -> Result<Option<OAuthStateRow>, AppError> {
    sqlx::query_as("SELECT id,connection_id,principal_id,session_id,redirect_uri,expires_at,used_at FROM marketing_oauth_states WHERE workspace_id=$1 AND state_hash=$2 FOR UPDATE")
    .bind(tx.workspace_id()).bind(hash).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}
pub async fn consume_state(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query("UPDATE marketing_oauth_states SET used_at=now() WHERE workspace_id=$1 AND id=$2 AND used_at IS NULL").bind(tx.workspace_id()).bind(id).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}
pub async fn mark_status(
    tx: &mut Tx<'_>,
    id: Uuid,
    status: &str,
    error: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query("UPDATE integration_connections SET status=$3,last_error_code=$4,last_checked_at=now(),version=version+1 WHERE workspace_id=$1 AND id=$2 AND provider='zoho_campaigns'")
    .bind(tx.workspace_id()).bind(id).bind(status).bind(error).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}
pub async fn mark_verified(tx: &mut Tx<'_>, id: Uuid, count: i32) -> Result<(), AppError> {
    sqlx::query("UPDATE marketing_connections SET last_verified_at=now(),lists_count=$3 WHERE workspace_id=$1 AND id=$2").bind(tx.workspace_id()).bind(id).bind(count).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}
pub async fn lock_connection(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ConnectionRow>, AppError> {
    sqlx::query_as("SELECT m.id,m.name,c.status,c.region,m.plan,c.secret_ref,m.last_verified_at,m.lists_count,m.created_at FROM marketing_connections m JOIN integration_connections c ON c.workspace_id=m.workspace_id AND c.id=m.id WHERE m.workspace_id=$1 AND m.id=$2 AND c.provider='zoho_campaigns' FOR UPDATE OF c,m")
    .bind(tx.workspace_id()).bind(id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}
