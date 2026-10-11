//! Workspace-level Zoho Mail OAuth client settings (non-secret columns only;
//! the client secret is kept in the encrypted SecretStore under `secret_ref`).

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ZohoOAuthSettingsRow {
    pub client_id: String,
    pub redirect_uri: Option<String>,
    pub secret_ref: String,
    pub updated_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

pub async fn get(tx: &mut Tx<'_>) -> Result<Option<ZohoOAuthSettingsRow>, AppError> {
    sqlx::query_as::<_, ZohoOAuthSettingsRow>(
        "SELECT client_id, redirect_uri, secret_ref, updated_by, updated_at, version
           FROM zoho_mail_oauth_settings
          WHERE workspace_id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn upsert(
    tx: &mut Tx<'_>,
    client_id: &str,
    redirect_uri: Option<&str>,
    secret_ref: &str,
    updated_by: Uuid,
) -> Result<ZohoOAuthSettingsRow, AppError> {
    sqlx::query_as::<_, ZohoOAuthSettingsRow>(
        "INSERT INTO zoho_mail_oauth_settings
            (workspace_id, client_id, redirect_uri, secret_ref, updated_by)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (workspace_id) DO UPDATE
            SET client_id = EXCLUDED.client_id,
                redirect_uri = EXCLUDED.redirect_uri,
                secret_ref = EXCLUDED.secret_ref,
                updated_by = EXCLUDED.updated_by,
                updated_at = now(),
                version = zoho_mail_oauth_settings.version + 1
         RETURNING client_id, redirect_uri, secret_ref, updated_by, updated_at, version",
    )
    .bind(tx.workspace_id())
    .bind(client_id)
    .bind(redirect_uri)
    .bind(secret_ref)
    .bind(updated_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}
