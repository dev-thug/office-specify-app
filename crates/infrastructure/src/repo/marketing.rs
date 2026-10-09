use crate::{db::Tx, error::map_sqlx};
use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub struct ConnectionRow {
    pub id: Uuid,
    pub scopes: Vec<String>,
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
    sqlx::query_as("SELECT m.id,c.scopes,m.name,c.status,c.region,m.plan,c.secret_ref,m.last_verified_at,m.lists_count,m.created_at FROM marketing_connections m JOIN integration_connections c ON c.workspace_id=m.workspace_id AND c.id=m.id WHERE m.workspace_id=$1 AND c.provider='zoho_campaigns' ORDER BY m.created_at DESC LIMIT 100")
    .bind(tx.workspace_id()).fetch_all(tx.conn()).await.map_err(map_sqlx)
}
pub async fn get_connection(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ConnectionRow>, AppError> {
    sqlx::query_as("SELECT m.id,c.scopes,m.name,c.status,c.region,m.plan,c.secret_ref,m.last_verified_at,m.lists_count,m.created_at FROM marketing_connections m JOIN integration_connections c ON c.workspace_id=m.workspace_id AND c.id=m.id WHERE m.workspace_id=$1 AND m.id=$2 AND c.provider='zoho_campaigns'")
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
    sqlx::query_as("SELECT m.id,c.scopes,m.name,c.status,c.region,m.plan,c.secret_ref,m.last_verified_at,m.lists_count,m.created_at FROM marketing_connections m JOIN integration_connections c ON c.workspace_id=m.workspace_id AND c.id=m.id WHERE m.workspace_id=$1 AND m.id=$2 AND c.provider='zoho_campaigns' FOR UPDATE OF c,m")
    .bind(tx.workspace_id()).bind(id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}

pub async fn get_connection_scopes(tx: &mut Tx<'_>, id: Uuid) -> Result<Vec<String>, AppError> {
    sqlx::query_scalar("SELECT scopes FROM integration_connections WHERE workspace_id=$1 AND id=$2 AND provider='zoho_campaigns'")
        .bind(tx.workspace_id()).bind(id).fetch_optional(tx.conn()).await.map_err(map_sqlx)?
        .ok_or_else(|| AppError::not_found("Campaigns connection not found"))
}
pub async fn sender_verification_status(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar("SELECT status FROM sender_identities WHERE workspace_id=$1 AND id=$2")
        .bind(tx.workspace_id())
        .bind(id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
}
pub async fn channel_connection_id(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar("SELECT connection_id FROM delivery_channels WHERE workspace_id=$1 AND id=$2 AND provider='zoho_campaigns' AND purpose='marketing'")
        .bind(tx.workspace_id()).bind(id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}
pub async fn normalized_email(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<String>, AppError> {
    sqlx::query_scalar("SELECT normalized_key FROM email_addresses WHERE workspace_id=$1 AND id=$2")
        .bind(tx.workspace_id())
        .bind(id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
}

pub async fn find_sender_by_address(
    tx: &mut Tx<'_>,
    account: Uuid,
    address: &str,
) -> Result<Option<(Uuid, String)>, AppError> {
    sqlx::query_as("SELECT id,status FROM sender_identities WHERE workspace_id=$1 AND mail_account_id=$2 AND address=$3")
        .bind(tx.workspace_id()).bind(account).bind(address).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}

pub async fn has_active_campaign_connection(tx: &mut Tx<'_>) -> Result<bool, AppError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM marketing_connections m JOIN integration_connections c ON c.workspace_id=m.workspace_id AND c.id=m.id WHERE m.workspace_id=$1 AND c.provider='zoho_campaigns' AND c.status='active')")
        .bind(tx.workspace_id()).fetch_one(tx.conn()).await.map_err(map_sqlx)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PriorProviderList {
    pub export_id: Uuid,
    pub provider_list_key: String,
}
/// Bounded stale prior lists that contain one of the new launch's addresses.
/// Used before contact mutations so an old campaign opt-out is not overwritten.
pub async fn prior_provider_lists(
    tx: &mut Tx<'_>,
    connection: Uuid,
    email_ids: &[Uuid],
    current_export: Uuid,
    stale_before: chrono::DateTime<chrono::Utc>,
    limit: i64,
) -> Result<Vec<PriorProviderList>, AppError> {
    sqlx::query_as("SELECT DISTINCT e.id AS export_id,e.provider_list_key FROM marketing_exports e JOIN marketing_export_recipients r ON r.workspace_id=e.workspace_id AND r.export_id=e.id WHERE e.workspace_id=$1 AND e.connection_id=$2 AND r.email_address_id=ANY($3) AND e.id<>$4 AND e.provider_list_key IS NOT NULL AND (e.unsubscribe_checked_at IS NULL OR e.unsubscribe_checked_at<$5) ORDER BY e.id LIMIT $6")
        .bind(tx.workspace_id()).bind(connection).bind(email_ids).bind(current_export).bind(stale_before).bind(limit.clamp(1,101)).fetch_all(tx.conn()).await.map_err(map_sqlx)
}

/// Mutable contact fields must stay pinned while a provider may still render
/// them for an earlier bulk delivery, including an uncertain send outcome.
pub async fn recipient_delivery_in_flight(
    tx: &mut Tx<'_>,
    current_export: Uuid,
    email_ids: &[Uuid],
) -> Result<bool, AppError> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM marketing_exports e JOIN marketing_export_recipients r ON r.workspace_id=e.workspace_id AND r.export_id=e.id WHERE e.workspace_id=$1 AND e.id<>$2 AND r.email_address_id=ANY($3) AND (e.state IN ('submitting','in_progress') OR (e.state='unknown' AND EXISTS(SELECT 1 FROM marketing_operations o WHERE o.workspace_id=e.workspace_id AND o.export_id=e.id AND o.kind='send_campaign' AND o.status IN ('started','unknown','completed')))))")
        .bind(tx.workspace_id()).bind(current_export).bind(email_ids).fetch_one(tx.conn()).await.map_err(map_sqlx)
}

pub async fn mark_unsubscribe_checked(tx: &mut Tx<'_>, export_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE marketing_exports SET unsubscribe_checked_at=now() WHERE workspace_id=$1 AND id=$2",
    )
    .bind(tx.workspace_id())
    .bind(export_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}
