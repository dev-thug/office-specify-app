//! Durable settings and UID checkpoints for SMTP-profile IMAP inbox sync.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::{db::Tx, error::map_sqlx};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SmtpInboxMonitorRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub smtp_connection_id: Uuid,
    pub mail_account_id: Uuid,
    pub enabled: bool,
    pub imap_host: String,
    pub cursor_uidvalidity: Option<i64>,
    pub cursor_uid: i64,
    pub imported_messages: i64,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub active_job_id: Option<Uuid>,
    pub scheduled_job_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Read-only SMTP identity and existing secret reference needed by the
/// worker. This intentionally performs no `FOR UPDATE`: worker_runtime has
/// profile read access only, while monitor row locking serializes inbox polls.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SmtpInboxProfileRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub username: String,
    pub status: String,
    pub has_password: bool,
    pub secret_ref: String,
    pub integration_status: String,
}

const MONITOR_COLS: &str = "id, workspace_id, smtp_connection_id, mail_account_id,
    enabled, imap_host, cursor_uidvalidity, cursor_uid, imported_messages,
    last_success_at, last_error_code, next_sync_at, active_job_id,
    scheduled_job_id, version, created_at, updated_at";

pub async fn get_for_connection(
    tx: &mut Tx<'_>,
    smtp_connection_id: Uuid,
) -> Result<Option<SmtpInboxMonitorRow>, AppError> {
    sqlx::query_as::<_, SmtpInboxMonitorRow>(&format!(
        "SELECT {MONITOR_COLS} FROM smtp_inbox_monitors
          WHERE workspace_id = $1 AND smtp_connection_id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(smtp_connection_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_for_connection_for_update(
    tx: &mut Tx<'_>,
    smtp_connection_id: Uuid,
) -> Result<Option<SmtpInboxMonitorRow>, AppError> {
    sqlx::query_as::<_, SmtpInboxMonitorRow>(&format!(
        "SELECT {MONITOR_COLS} FROM smtp_inbox_monitors
          WHERE workspace_id = $1 AND smtp_connection_id = $2
          FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(smtp_connection_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_for_id(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
) -> Result<Option<SmtpInboxMonitorRow>, AppError> {
    sqlx::query_as::<_, SmtpInboxMonitorRow>(&format!(
        "SELECT {MONITOR_COLS} FROM smtp_inbox_monitors
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_for_id_for_update(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
) -> Result<Option<SmtpInboxMonitorRow>, AppError> {
    sqlx::query_as::<_, SmtpInboxMonitorRow>(&format!(
        "SELECT {MONITOR_COLS} FROM smtp_inbox_monitors
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn profile_for_sync(
    tx: &mut Tx<'_>,
    smtp_connection_id: Uuid,
) -> Result<Option<SmtpInboxProfileRow>, AppError> {
    sqlx::query_as::<_, SmtpInboxProfileRow>(
        "SELECT sc.id, sc.mail_account_id, sc.username, sc.status,
                sc.has_password, ic.secret_ref, ic.status AS integration_status
           FROM smtp_connections sc
           JOIN integration_connections ic
             ON ic.workspace_id = sc.workspace_id AND ic.id = sc.id
          WHERE sc.workspace_id = $1 AND sc.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(smtp_connection_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn active_job_exists(tx: &mut Tx<'_>, job_id: Uuid) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM jobs
             WHERE workspace_id = $1 AND id = $2
               AND state IN ('queued','running','retry_wait'))",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn save_configuration(
    tx: &mut Tx<'_>,
    smtp_connection_id: Uuid,
    mail_account_id: Uuid,
    imap_host: &str,
    enabled: bool,
    reset_cursor: bool,
) -> Result<SmtpInboxMonitorRow, AppError> {
    sqlx::query_as::<_, SmtpInboxMonitorRow>(&format!(
        "INSERT INTO smtp_inbox_monitors
            (workspace_id, smtp_connection_id, mail_account_id, enabled, imap_host)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (workspace_id, smtp_connection_id) DO UPDATE
            SET enabled = EXCLUDED.enabled,
                imap_host = EXCLUDED.imap_host,
                cursor_uidvalidity = CASE WHEN $6 THEN NULL ELSE smtp_inbox_monitors.cursor_uidvalidity END,
                cursor_uid = CASE WHEN $6 THEN 0 ELSE smtp_inbox_monitors.cursor_uid END,
                last_error_code = CASE WHEN $6 THEN NULL ELSE smtp_inbox_monitors.last_error_code END,
                active_job_id = NULL,
                scheduled_job_id = NULL,
                next_sync_at = NULL,
                version = smtp_inbox_monitors.version + 1
         RETURNING {MONITOR_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(smtp_connection_id)
    .bind(mail_account_id)
    .bind(enabled)
    .bind(imap_host)
    .bind(reset_cursor)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn cancel_pending_jobs(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
    reason: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE jobs
            SET state = 'canceled', finished_at = now(), last_error_code = $3,
                lease_owner = NULL, lease_token = NULL, lease_expires_at = NULL
          WHERE workspace_id = $1 AND kind = 'smtp_inbox.poll'
            AND payload ->> 'monitor_id' = $2::text
            AND state IN ('queued','retry_wait')",
    )
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .bind(reason)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn set_active_job(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
    job_id: Option<Uuid>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE smtp_inbox_monitors
            SET active_job_id = $3,
                scheduled_job_id = NULL,
                next_sync_at = NULL,
                last_error_code = NULL,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .bind(job_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn mark_poll_started(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
    job_id: Uuid,
    is_scheduled: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE smtp_inbox_monitors
            SET active_job_id = $3,
                scheduled_job_id = CASE WHEN $4 THEN NULL ELSE scheduled_job_id END,
                next_sync_at = CASE WHEN $4 THEN NULL ELSE next_sync_at END,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .bind(job_id)
    .bind(is_scheduled)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn complete_poll(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
    uid_validity: u32,
    last_uid: u32,
    imported: usize,
    active_job_id: Option<Uuid>,
    scheduled_job_id: Option<Uuid>,
    next_sync_at: Option<DateTime<Utc>>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE smtp_inbox_monitors
            SET cursor_uidvalidity = $3,
                cursor_uid = $4,
                imported_messages = imported_messages + $5,
                last_success_at = now(),
                last_error_code = NULL,
                active_job_id = $6,
                scheduled_job_id = $7,
                next_sync_at = $8,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .bind(i64::from(uid_validity))
    .bind(i64::from(last_uid))
    .bind(imported as i64)
    .bind(active_job_id)
    .bind(scheduled_job_id)
    .bind(next_sync_at)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn fail_poll(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
    error_code: &str,
    scheduled_job_id: Option<Uuid>,
    next_sync_at: Option<DateTime<Utc>>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE smtp_inbox_monitors
            SET last_error_code = $3,
                active_job_id = NULL,
                scheduled_job_id = $4,
                next_sync_at = $5,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .bind(error_code)
    .bind(scheduled_job_id)
    .bind(next_sync_at)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn disable_monitor(
    tx: &mut Tx<'_>,
    monitor_id: Uuid,
    error_code: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE smtp_inbox_monitors
            SET enabled = false,
                last_error_code = $3,
                active_job_id = NULL,
                scheduled_job_id = NULL,
                next_sync_at = NULL,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(monitor_id)
    .bind(error_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}
