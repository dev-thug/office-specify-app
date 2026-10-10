//! SMTP profile persistence. Credentials are referenced only through the
//! existing integration connection secret reference.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::{db::Tx, error::map_sqlx};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SmtpConnectionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub mail_account_id: Uuid,
    pub sender_identity_id: Uuid,
    pub channel_id: Uuid,
    pub purpose: String,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub security: String,
    pub username: String,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub custom_headers: serde_json::Value,
    pub status: String,
    pub last_error_code: Option<String>,
    pub has_password: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SmtpBindingRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub mail_account_id: Uuid,
    pub sender_identity_id: Uuid,
    pub channel_id: Uuid,
    pub purpose: String,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub security: String,
    pub username: String,
    pub from_address: String,
    pub from_name: String,
    pub reply_to: Option<String>,
    pub custom_headers: serde_json::Value,
    pub status: String,
    pub last_error_code: Option<String>,
    pub has_password: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub secret_ref: String,
    pub integration_status: String,
    pub channel_enabled: bool,
    pub channel_policy_status: String,
    pub sender_status: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SenderIdentityRow {
    pub id: Uuid,
    pub address: String,
    pub display_name: String,
    pub status: String,
}

const PROFILE_COLS: &str = "sc.id, sc.workspace_id, sc.mail_account_id, sc.sender_identity_id,
    sc.channel_id, sc.name, sc.host, sc.port, sc.security, sc.username,
    sc.from_address, sc.from_name, sc.reply_to, sc.custom_headers, sc.status,
    sc.last_error_code, sc.has_password, sc.version, sc.created_at,
    (SELECT dc.purpose FROM delivery_channels dc
      WHERE dc.workspace_id = sc.workspace_id AND dc.id = sc.channel_id) AS purpose";

/// Return only SMTP profiles whose mailbox is readable and sendable by the
/// caller. The API still gates the operation to a human administrator.
pub async fn list_connections(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<Vec<SmtpConnectionRow>, AppError> {
    sqlx::query_as::<_, SmtpConnectionRow>(&format!(
        "SELECT {PROFILE_COLS}
           FROM smtp_connections sc
          WHERE sc.workspace_id = $1
            AND EXISTS (
                SELECT 1 FROM mail_account_grants g
                 WHERE g.workspace_id = sc.workspace_id
                   AND g.mail_account_id = sc.mail_account_id
                   AND g.principal_id = $2 AND g.can_send)
          ORDER BY sc.created_at DESC, sc.id DESC"
    ))
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn find_sender_identity(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    address: &str,
) -> Result<Vec<SenderIdentityRow>, AppError> {
    sqlx::query_as::<_, SenderIdentityRow>(
        "SELECT id, address, display_name, status
           FROM sender_identities
          WHERE workspace_id = $1 AND mail_account_id = $2
            AND lower(address) = lower($3)
          ORDER BY id
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(address)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn verify_sender_identity(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    sender_identity_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE sender_identities
            SET status = 'verified', verified_at = COALESCE(verified_at, now())
          WHERE workspace_id = $1 AND mail_account_id = $2 AND id = $3
            AND status = 'pending'",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(sender_identity_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn insert_pending_integration(
    tx: &mut Tx<'_>,
    id: Uuid,
    connected_by: Uuid,
    secret_ref: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO integration_connections
            (id, workspace_id, provider, region, secret_ref, scopes, status, connected_by)
         VALUES ($1, $2, 'delivery_provider', 'smtp', $3, '{}', 'pending', $4)",
    )
    .bind(id)
    .bind(tx.workspace_id())
    .bind(secret_ref)
    .bind(connected_by)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_connection(
    tx: &mut Tx<'_>,
    id: Uuid,
    mail_account_id: Uuid,
    sender_identity_id: Uuid,
    channel_id: Uuid,
    name: &str,
    host: &str,
    port: u16,
    security: &str,
    username: &str,
    from_address: &str,
    from_name: &str,
    reply_to: Option<&str>,
    custom_headers: &serde_json::Value,
) -> Result<SmtpConnectionRow, AppError> {
    sqlx::query_as::<_, SmtpConnectionRow>(&format!(
        "INSERT INTO smtp_connections AS sc
            (id, workspace_id, mail_account_id, sender_identity_id, channel_id,
             name, host, port, security, username, from_address, from_name,
             reply_to, custom_headers, status, has_password)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,'pending',true)
         RETURNING {PROFILE_COLS}"
    ))
    .bind(id)
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(sender_identity_id)
    .bind(channel_id)
    .bind(name)
    .bind(host)
    .bind(i32::from(port))
    .bind(security)
    .bind(username)
    .bind(from_address)
    .bind(from_name)
    .bind(reply_to)
    .bind(custom_headers)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_for_update(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<SmtpBindingRow>, AppError> {
    sqlx::query_as::<_, SmtpBindingRow>(
        "SELECT sc.id, sc.workspace_id, sc.mail_account_id, sc.sender_identity_id,
                sc.channel_id, sc.name, sc.host, sc.port, sc.security, sc.username,
                sc.from_address, sc.from_name, sc.reply_to, sc.custom_headers,
                sc.status, sc.last_error_code, sc.has_password, sc.version, sc.created_at,
                ic.secret_ref, ic.status AS integration_status,
                dc.enabled AS channel_enabled, dc.policy_status AS channel_policy_status,
                dc.purpose AS purpose, si.status AS sender_status
           FROM smtp_connections sc
           JOIN integration_connections ic
             ON ic.workspace_id = sc.workspace_id AND ic.id = sc.id
           JOIN delivery_channels dc
             ON dc.workspace_id = sc.workspace_id AND dc.id = sc.channel_id
            AND dc.connection_id = sc.id AND dc.provider = 'delivery_provider'
            AND dc.purpose IN ('marketing','correspondence')
           JOIN sender_identities si
             ON si.workspace_id = sc.workspace_id
            AND si.mail_account_id = sc.mail_account_id
            AND si.id = sc.sender_identity_id
          WHERE sc.workspace_id = $1 AND sc.id = $2
          FOR UPDATE OF sc, ic, dc, si",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Resolve solely by `(workspace_id, channel_id)`. A From address or message
/// correlation identifier is never used as a routing key.
pub async fn resolve_for_channel(
    tx: &mut Tx<'_>,
    channel_id: Uuid,
) -> Result<Option<SmtpBindingRow>, AppError> {
    sqlx::query_as::<_, SmtpBindingRow>(
        "SELECT sc.id, sc.workspace_id, sc.mail_account_id, sc.sender_identity_id,
                sc.channel_id, sc.name, sc.host, sc.port, sc.security, sc.username,
                sc.from_address, sc.from_name, sc.reply_to, sc.custom_headers,
                sc.status, sc.last_error_code, sc.has_password, sc.version, sc.created_at,
                ic.secret_ref, ic.status AS integration_status,
                dc.enabled AS channel_enabled, dc.policy_status AS channel_policy_status,
                dc.purpose AS purpose, si.status AS sender_status
           FROM smtp_connections sc
           JOIN integration_connections ic
             ON ic.workspace_id = sc.workspace_id AND ic.id = sc.id
           JOIN delivery_channels dc
             ON dc.workspace_id = sc.workspace_id AND dc.id = sc.channel_id
            AND dc.connection_id = sc.id AND dc.provider = 'delivery_provider'
            AND dc.purpose IN ('marketing','correspondence')
           JOIN sender_identities si
             ON si.workspace_id = sc.workspace_id
            AND si.mail_account_id = sc.mail_account_id
            AND si.id = sc.sender_identity_id
          WHERE sc.workspace_id = $1 AND sc.channel_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// True when a channel points at an SMTP integration, even if its profile is
/// missing. Such channels must fail closed instead of falling through to a
/// generic or test delivery adapter.
pub async fn channel_uses_smtp_integration(
    tx: &mut Tx<'_>,
    channel_id: Uuid,
) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
              FROM delivery_channels dc
              JOIN integration_connections ic
                ON ic.workspace_id = dc.workspace_id AND ic.id = dc.connection_id
             WHERE dc.workspace_id = $1 AND dc.id = $2
               AND dc.provider = 'delivery_provider'
               AND ic.provider = 'delivery_provider' AND ic.region = 'smtp'
        )",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn record_verify_result(
    tx: &mut Tx<'_>,
    id: Uuid,
    profile_status: &str,
    integration_status: &str,
    error_code: Option<&str>,
    has_password: bool,
) -> Result<Option<SmtpConnectionRow>, AppError> {
    sqlx::query(
        "UPDATE integration_connections
            SET status = $3, last_checked_at = now(), last_error_code = $4,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND provider = 'delivery_provider'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(integration_status)
    .bind(error_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    sqlx::query_as::<_, SmtpConnectionRow>(&format!(
        "UPDATE smtp_connections AS sc
            SET status = $3, last_error_code = $4, has_password = $5,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2
          RETURNING {PROFILE_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(profile_status)
    .bind(error_code)
    .bind(has_password)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn activate_verified(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<SmtpConnectionRow>, AppError> {
    let eligible: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
              FROM smtp_connections sc
              JOIN integration_connections ic
                ON ic.workspace_id = sc.workspace_id AND ic.id = sc.id
              JOIN delivery_channels dc
                ON dc.workspace_id = sc.workspace_id AND dc.id = sc.channel_id
               AND dc.connection_id = sc.id AND dc.provider = 'delivery_provider'
              JOIN sender_identities si
                ON si.workspace_id = sc.workspace_id
               AND si.mail_account_id = sc.mail_account_id
               AND si.id = sc.sender_identity_id
             WHERE sc.workspace_id = $1 AND sc.id = $2
               AND sc.status IN ('pending','active')
               AND ic.status IN ('pending','active')
               AND sc.has_password
               AND si.status = 'verified'
               AND dc.purpose IN ('marketing','correspondence')
               AND dc.policy_status = 'allowed'
               AND (SELECT cr.result FROM channel_reviews cr
                     WHERE cr.workspace_id = sc.workspace_id
                       AND cr.delivery_channel_id = sc.channel_id
                     ORDER BY cr.reviewed_at DESC, cr.id DESC LIMIT 1) = 'allowed'
               AND COALESCE((SELECT cr.expires_at FROM channel_reviews cr
                              WHERE cr.workspace_id = sc.workspace_id
                                AND cr.delivery_channel_id = sc.channel_id
                              ORDER BY cr.reviewed_at DESC, cr.id DESC LIMIT 1) > now(), true)
        )",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if !eligible {
        return Ok(None);
    }
    let updated = sqlx::query(
        "UPDATE integration_connections ic
            SET status = 'active', last_checked_at = now(), last_error_code = NULL,
                version = version + 1
          WHERE ic.workspace_id = $1 AND ic.id = $2
            AND ic.provider = 'delivery_provider'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    if updated != 1 {
        return Ok(None);
    }
    let channel_updated = sqlx::query(
        "UPDATE delivery_channels dc
            SET enabled = true, version = version + 1
          WHERE dc.workspace_id = $1
            AND dc.id = (SELECT channel_id FROM smtp_connections
                          WHERE workspace_id = $1 AND id = $2)
            AND dc.connection_id = $2
            AND dc.provider = 'delivery_provider'
            AND dc.purpose IN ('marketing','correspondence')
            AND dc.policy_status = 'allowed'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    if channel_updated != 1 {
        return Ok(None);
    }
    sqlx::query_as::<_, SmtpConnectionRow>(&format!(
        "UPDATE smtp_connections AS sc
            SET status = 'active', last_error_code = NULL, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND has_password
          RETURNING {PROFILE_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn disable_channel(tx: &mut Tx<'_>, connection_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE delivery_channels SET enabled = false, version = version + 1
          WHERE workspace_id = $1 AND connection_id = $2
            AND provider = 'delivery_provider'",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn disable(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<SmtpConnectionRow>, AppError> {
    sqlx::query(
        "UPDATE integration_connections
            SET status = 'disabled', last_checked_at = now(), last_error_code = NULL,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND provider = 'delivery_provider'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    sqlx::query(
        "UPDATE delivery_channels
            SET enabled = false, version = version + 1
          WHERE workspace_id = $1 AND connection_id = $2
            AND provider = 'delivery_provider'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    sqlx::query_as::<_, SmtpConnectionRow>(&format!(
        "UPDATE smtp_connections AS sc
            SET status = 'disabled', last_error_code = NULL, has_password = false,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2
          RETURNING {PROFILE_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}
