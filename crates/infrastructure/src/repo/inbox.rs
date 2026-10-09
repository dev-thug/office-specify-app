//! inbox module repository (spec §6 — owned by the module worker).
//!
//! Tables (A.4): `email_threads`, `email_messages`, `message_participants`,
//! `thread_read_states`, `message_attachments` (+`stored_objects`),
//! `webhook_events`, `sync_cursors`. Reply drafts write the shared
//! `message_drafts` table (owned by `sending` for freeze/submit).
//!
//! Every query binds `tx.workspace_id()` explicitly — RLS is the backstop,
//! not the primary filter. Mailbox ACL is enforced in the queries
//! themselves (`mail_account_grants.can_read`) so ungranted principals see
//! neither content nor existence (§5.4/§5.6).

use chrono::{DateTime, Utc};
use contracts::AppError;
use serde_json::Value;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;
use crate::providers::ProviderMessage;

// ------------------------------------------------------------ row shapes

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ThreadRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub provider_thread_id: Option<String>,
    pub contact_id: Option<Uuid>,
    pub assigned_principal_id: Option<Uuid>,
    pub last_inbound_message_id: Option<Uuid>,
    pub status: String,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

const THREAD_COLS: &str = "id, mail_account_id, provider_thread_id, contact_id, \
     assigned_principal_id, last_inbound_message_id, status, version, \
     updated_at, created_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MessageRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub thread_id: Uuid,
    pub outbound_message_id: Option<Uuid>,
    pub direction: String,
    pub provider_message_id: Option<String>,
    pub internet_message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub reference_ids: Vec<String>,
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub message_at: DateTime<Utc>,
    pub classification: String,
    pub classification_reviewed_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

const MESSAGE_COLS: &str = "id, mail_account_id, thread_id, outbound_message_id, \
     direction, provider_message_id, internet_message_id, in_reply_to, \
     reference_ids, subject, body_text, body_html, message_at, \
     classification, classification_reviewed_by, created_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ParticipantRow {
    pub email_message_id: Uuid,
    pub role: String,
    pub address: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AttachmentRow {
    pub id: Uuid,
    pub email_message_id: Uuid,
    pub stored_object_id: Uuid,
    pub filename: String,
    pub is_inline: bool,
    pub media_type: String,
    pub byte_size: i64,
    pub scan_status: String,
    pub storage_key: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MailAccountRow {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider_account_id: String,
    pub primary_address: String,
    pub status: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ConnectionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub secret_ref: String,
    pub status: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WebhookEventRow {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider_event_key: String,
    pub redacted_payload: Value,
    pub status: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DraftRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub owner_principal_id: Uuid,
    pub visibility: String,
    pub thread_id: Option<Uuid>,
    pub reply_to_message_id: Option<Uuid>,
    pub expected_last_inbound_id: Option<Uuid>,
    pub to_email_address_id: Uuid,
    pub purpose: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of the `GET /email-threads` listing (latest-message projection +
/// per-caller unread flag).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ThreadListRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub assigned_principal_id: Option<Uuid>,
    pub status: String,
    pub subject: Option<String>,
    pub snippet: Option<String>,
    pub message_count: i64,
    pub unread: bool,
    pub last_message_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

// ------------------------------------------------------------ read paths

/// Listing filter for `GET /email-threads`. `assigned_to` semantics:
/// `Some(Some(id))` = that principal, `Some(None)` = unassigned only.
#[derive(Debug, Clone, Default)]
pub struct ThreadFilter {
    pub mail_account_id: Option<Uuid>,
    pub status: Option<String>,
    pub assigned_to: Option<Option<Uuid>>,
    /// Case-insensitive substring match on the latest message's subject.
    pub q: Option<String>,
}

/// Cursor-paginated inbox list (B.1.5 Inbox sort: `updated_at DESC, id
/// DESC`). Rows are limited to mailboxes where `principal_id` holds
/// `can_read` — content columns come only from those threads (§5.6).
///
/// `after` is the keyset `(updated_at, id)` of the last row of the previous
/// page. Returns up to `limit` rows (caller over-fetches by one for
/// `has_more`).
#[allow(clippy::too_many_arguments)]
pub async fn list_threads(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    filter: &ThreadFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<ThreadListRow>, AppError> {
    // Dynamic WHERE — every arm appends a numbered bind in lockstep.
    let mut sql = String::from(
        "SELECT t.id, t.mail_account_id, t.contact_id, t.assigned_principal_id,
                t.status, lm.subject,
                left(coalesce(lm.body_text, ''), 200) AS snippet,
                (SELECT count(*) FROM email_messages mc
                  WHERE mc.workspace_id = t.workspace_id AND mc.thread_id = t.id
                 ) AS message_count,
                lm.message_at AS last_message_at,
                EXISTS (
                    SELECT 1 FROM email_messages im
                     WHERE im.workspace_id = t.workspace_id
                       AND im.thread_id = t.id
                       AND im.direction = 'inbound'
                       AND im.message_at > COALESCE(rs.last_read_at, '1970-01-01'::timestamptz)
                ) AS unread,
                t.version, t.updated_at, t.created_at
           FROM email_threads t
           JOIN mail_account_grants g
             ON g.workspace_id = t.workspace_id
            AND g.mail_account_id = t.mail_account_id
            AND g.principal_id = $2
            AND g.can_read
           LEFT JOIN LATERAL (
                SELECT m.subject, m.body_text, m.message_at
                  FROM email_messages m
                 WHERE m.workspace_id = t.workspace_id AND m.thread_id = t.id
                 ORDER BY m.message_at DESC, m.id DESC
                 LIMIT 1
           ) lm ON true
           LEFT JOIN thread_read_states rs
             ON rs.workspace_id = t.workspace_id
            AND rs.thread_id = t.id
            AND rs.principal_id = $2
          WHERE t.workspace_id = $1",
    );
    // $1 = workspace, $2 = principal, then filters.
    let mut binds: Vec<String> = Vec::new(); // serialized bind values by position
    let mut n = 2usize;
    let mut next = |val: String| {
        n += 1;
        binds.push(val);
        n
    };

    if let Some(ma) = filter.mail_account_id {
        let p = next(ma.to_string());
        sql.push_str(&format!(" AND t.mail_account_id = ${p}::uuid"));
    }
    if let Some(status) = &filter.status {
        let p = next(status.clone());
        sql.push_str(&format!(" AND t.status = ${p}"));
    }
    if let Some(assigned) = &filter.assigned_to {
        match assigned {
            Some(pid) => {
                let p = next(pid.to_string());
                sql.push_str(&format!(" AND t.assigned_principal_id = ${p}::uuid"));
            }
            None => sql.push_str(" AND t.assigned_principal_id IS NULL"),
        }
    }
    if let Some(q) = &filter.q {
        let escaped = q
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let p = next(format!("%{escaped}%"));
        sql.push_str(&format!(" AND lm.subject ILIKE ${p} ESCAPE '\\'"));
    }
    let after_idx = after.map(|(ts, id)| {
        let a = next(ts.to_rfc3339());
        let b = next(id.to_string());
        (a, b)
    });
    if let Some((a, b)) = after_idx {
        sql.push_str(&format!(
            " AND (t.updated_at, t.id) < (${a}::timestamptz, ${b}::uuid)"
        ));
    }
    let limit_idx = next(limit.to_string());
    sql.push_str(&format!(
        " ORDER BY t.updated_at DESC, t.id DESC LIMIT ${limit_idx}::bigint"
    ));

    // Re-bind in order: binds[i] corresponds to placeholder $(i+3).
    let mut q = sqlx::query_as::<_, ThreadListRow>(&sql)
        .bind(tx.workspace_id())
        .bind(principal_id);
    for b in &binds {
        q = q.bind(b);
    }
    q.fetch_all(tx.conn()).await.map_err(map_sqlx)
}

/// Thread row by id — no ACL here (callers decide 404-vs-403 semantics).
pub async fn get_thread(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ThreadRow>, AppError> {
    sqlx::query_as::<_, ThreadRow>(&format!(
        "SELECT {THREAD_COLS} FROM email_threads WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Thread row `FOR UPDATE` — read-state stays independent, but assignment /
/// status / inbound-append mutations serialize on this lock.
pub async fn lock_thread(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ThreadRow>, AppError> {
    sqlx::query_as::<_, ThreadRow>(&format!(
        "SELECT {THREAD_COLS} FROM email_threads
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn thread_messages(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
) -> Result<Vec<MessageRow>, AppError> {
    sqlx::query_as::<_, MessageRow>(&format!(
        "SELECT {MESSAGE_COLS} FROM email_messages
          WHERE workspace_id = $1 AND thread_id = $2
          ORDER BY message_at, id"
    ))
    .bind(tx.workspace_id())
    .bind(thread_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_message(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<MessageRow>, AppError> {
    sqlx::query_as::<_, MessageRow>(&format!(
        "SELECT {MESSAGE_COLS} FROM email_messages WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn participants_for(
    tx: &mut Tx<'_>,
    message_ids: &[Uuid],
) -> Result<Vec<ParticipantRow>, AppError> {
    sqlx::query_as::<_, ParticipantRow>(
        "SELECT email_message_id, role, address, display_name
           FROM message_participants
          WHERE workspace_id = $1 AND email_message_id = ANY($2)
          ORDER BY role, address",
    )
    .bind(tx.workspace_id())
    .bind(message_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Attachment rows joined to their stored object (media type / size / scan
/// status / storage key). The join never exposes `storage_key` to DTOs.
pub async fn attachments_for(
    tx: &mut Tx<'_>,
    message_ids: &[Uuid],
) -> Result<Vec<AttachmentRow>, AppError> {
    sqlx::query_as::<_, AttachmentRow>(
        "SELECT a.id, a.email_message_id, a.stored_object_id, a.filename,
                a.is_inline, o.media_type, o.byte_size, o.scan_status, o.storage_key
           FROM message_attachments a
           JOIN stored_objects o
             ON o.workspace_id = a.workspace_id AND o.id = a.stored_object_id
          WHERE a.workspace_id = $1 AND a.email_message_id = ANY($2)
          ORDER BY a.filename, a.id",
    )
    .bind(tx.workspace_id())
    .bind(message_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Single attachment + object + owning mailbox id for ACL re-checks.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AttachmentObjectRow {
    pub id: Uuid,
    pub stored_object_id: Uuid,
    pub filename: String,
    pub is_inline: bool,
    pub media_type: String,
    pub byte_size: i64,
    pub scan_status: String,
    pub storage_key: String,
    pub mail_account_id: Uuid,
}

pub async fn get_attachment_object(
    tx: &mut Tx<'_>,
    attachment_id: Uuid,
) -> Result<Option<AttachmentObjectRow>, AppError> {
    sqlx::query_as::<_, AttachmentObjectRow>(
        "SELECT a.id, a.stored_object_id, a.filename, a.is_inline,
                o.media_type, o.byte_size, o.scan_status, o.storage_key,
                m.mail_account_id
           FROM message_attachments a
           JOIN stored_objects o
             ON o.workspace_id = a.workspace_id AND o.id = a.stored_object_id
           JOIN email_messages m
             ON m.workspace_id = a.workspace_id AND m.id = a.email_message_id
          WHERE a.workspace_id = $1 AND a.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(attachment_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_read_state(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
    principal_id: Uuid,
) -> Result<Option<DateTime<Utc>>, AppError> {
    sqlx::query_scalar(
        "SELECT last_read_at FROM thread_read_states
          WHERE workspace_id = $1 AND thread_id = $2 AND principal_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(thread_id)
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------- write paths

/// Optimistic-lock mutation of `assigned_principal_id` (A.2). Returns
/// `Ok(None)` on version mismatch — the caller maps that to 412 after a
/// re-read (404 if the row vanished).
pub async fn set_assignment(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
    expected_version: i64,
    assigned: Option<Uuid>,
) -> Result<Option<ThreadRow>, AppError> {
    sqlx::query_as::<_, ThreadRow>(&format!(
        "UPDATE email_threads
            SET assigned_principal_id = $4,
                version = version + 1,
                updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $3
          RETURNING {THREAD_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(thread_id)
    .bind(expected_version)
    .bind(assigned)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Optimistic-lock status patch — same contract as `set_assignment`.
pub async fn set_status(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
    expected_version: i64,
    status: &str,
) -> Result<Option<ThreadRow>, AppError> {
    sqlx::query_as::<_, ThreadRow>(&format!(
        "UPDATE email_threads
            SET status = $4,
                version = version + 1,
                updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $3
          RETURNING {THREAD_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(thread_id)
    .bind(expected_version)
    .bind(status)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Per-principal read mark — monotonic (B.1.4): an older timestamp never
/// lowers the stored mark. Returns the effective stored value.
pub async fn upsert_read_state(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
    principal_id: Uuid,
    at: DateTime<Utc>,
) -> Result<DateTime<Utc>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO thread_read_states
            (workspace_id, thread_id, principal_id, last_read_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (workspace_id, thread_id, principal_id)
         DO UPDATE SET last_read_at = GREATEST(thread_read_states.last_read_at, EXCLUDED.last_read_at),
                       version = thread_read_states.version + 1
         RETURNING last_read_at",
    )
    .bind(tx.workspace_id())
    .bind(thread_id)
    .bind(principal_id)
    .bind(at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Human review of an inbound classification (§9.4). Records the reviewer
/// principal — the reason string lives in audit metadata, not the table.
pub async fn set_classification(
    tx: &mut Tx<'_>,
    message_id: Uuid,
    classification: &str,
    reviewed_by: Uuid,
) -> Result<Option<MessageRow>, AppError> {
    sqlx::query_as::<_, MessageRow>(&format!(
        "UPDATE email_messages
            SET classification = $3, classification_reviewed_by = $4
          WHERE workspace_id = $1 AND id = $2
          RETURNING {MESSAGE_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(message_id)
    .bind(classification)
    .bind(reviewed_by)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Insert a reply draft into the shared `message_drafts` table
/// (B.3.10). `sending` owns freeze/submit — this only creates `draft` rows.
#[allow(clippy::too_many_arguments)]
pub async fn insert_reply_draft(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    owner: Uuid,
    visibility: &str,
    thread_id: Uuid,
    reply_to_message_id: Option<Uuid>,
    expected_last_inbound_id: Option<Uuid>,
    to_email_address_id: Uuid,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
) -> Result<DraftRow, AppError> {
    sqlx::query_as::<_, DraftRow>(
        "INSERT INTO message_drafts
            (workspace_id, mail_account_id, owner_principal_id, visibility,
             thread_id, reply_to_message_id, expected_last_inbound_id,
             to_email_address_id, purpose, subject, body_text, body_html)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'correspondence', $9, $10, $11)
         RETURNING id, mail_account_id, owner_principal_id, visibility,
                   thread_id, reply_to_message_id, expected_last_inbound_id,
                   to_email_address_id, purpose, subject, body_text, body_html,
                   status, version, created_at, updated_at",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(owner)
    .bind(visibility)
    .bind(thread_id)
    .bind(reply_to_message_id)
    .bind(expected_last_inbound_id)
    .bind(to_email_address_id)
    .bind(subject)
    .bind(body_text)
    .bind(body_html)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------------- mail accounts

pub async fn get_mail_account(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<MailAccountRow>, AppError> {
    sqlx::query_as::<_, MailAccountRow>(
        "SELECT id, connection_id, provider_account_id, primary_address, status
           FROM mail_accounts WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Mail account wired to a connection (webhook ingress resolves which
/// mailbox an event belongs to). Multiple accounts per connection are a
/// config error — we take the active one deterministically.
pub async fn mail_account_for_connection(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
) -> Result<Option<MailAccountRow>, AppError> {
    sqlx::query_as::<_, MailAccountRow>(
        "SELECT id, connection_id, provider_account_id, primary_address, status
           FROM mail_accounts
          WHERE workspace_id = $1 AND connection_id = $2
          ORDER BY CASE status WHEN 'active' THEN 0 ELSE 1 END, created_at
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Webhook ingress: resolve the connection (and thus the workspace +
/// `secret_ref`) — runs under `begin_preauth` (default workspace, A.5).
pub async fn get_connection(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
) -> Result<Option<ConnectionRow>, AppError> {
    sqlx::query_as::<_, ConnectionRow>(
        "SELECT id, workspace_id, provider, secret_ref, status
           FROM integration_connections WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn touch_mail_account_synced(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE mail_accounts SET last_synced_at = now(), version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// ------------------------------------------------------- sync internals

/// `sync_cursors` row for (mail_account, stream). `None` = first sync.
pub async fn get_sync_cursor(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    stream_key: &str,
) -> Result<Option<Value>, AppError> {
    sqlx::query_scalar(
        "SELECT cursor_data FROM sync_cursors
          WHERE workspace_id = $1 AND mail_account_id = $2 AND stream_key = $3",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(stream_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn upsert_sync_cursor(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    stream_key: &str,
    cursor_data: &Value,
    ok: bool,
    error_code: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sync_cursors
            (workspace_id, mail_account_id, stream_key, cursor_data, last_success_at, last_error_code)
         VALUES ($1, $2, $3, $4, CASE WHEN $5 THEN now() END, $6)
         ON CONFLICT (workspace_id, mail_account_id, stream_key)
         DO UPDATE SET cursor_data = EXCLUDED.cursor_data,
                       last_success_at = CASE WHEN $5 THEN now() ELSE sync_cursors.last_success_at END,
                       last_error_code = $6,
                       version = sync_cursors.version + 1",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(stream_key)
    .bind(cursor_data)
    .bind(ok)
    .bind(error_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// `email_addresses` row for a normalized key — insert if absent (the
/// `from`/`to` participants of observed mail become address rows so other
/// modules can key off them).
pub async fn upsert_email_address(
    tx: &mut Tx<'_>,
    normalized_key: &str,
    original: &str,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1, $2, $3)
         ON CONFLICT (workspace_id, normalized_key)
         DO UPDATE SET last_seen_at = now()
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(normalized_key)
    .bind(original)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// First contact linked to an address (§9.2 "명시적 CRM 연결" — the only
/// contact link inbox performs; subject similarity is never used).
pub async fn contact_for_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT ce.contact_id FROM contact_emails ce
          WHERE ce.workspace_id = $1 AND ce.email_address_id = $2
          ORDER BY ce.is_primary DESC, ce.created_at LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Thread matching step 1 (§9.2): verified provider thread id.
pub async fn thread_by_provider_id(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    provider_thread_id: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM email_threads
          WHERE workspace_id = $1 AND mail_account_id = $2 AND provider_thread_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(provider_thread_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Thread matching step 2: `In-Reply-To` / `References` headers resolve to a
/// known `internet_message_id` in the same mailbox.
pub async fn thread_by_references(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    refs: &[String],
) -> Result<Option<Uuid>, AppError> {
    if refs.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar(
        "SELECT thread_id FROM email_messages
          WHERE workspace_id = $1 AND mail_account_id = $2
            AND internet_message_id = ANY($3)
          ORDER BY message_at DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(refs)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn create_thread(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    provider_thread_id: Option<&str>,
    contact_id: Option<Uuid>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id, provider_thread_id, contact_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (workspace_id, mail_account_id, provider_thread_id)
             WHERE provider_thread_id IS NOT NULL
         DO UPDATE SET provider_thread_id = EXCLUDED.provider_thread_id
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(provider_thread_id)
    .bind(contact_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Insert an observed message. Returns `None` when the
/// `(mail_account, provider_message_id)` pair already exists — the
/// dedupe anchor for webhook/sync overlap (T14, §9.1).
#[allow(clippy::too_many_arguments)]
pub async fn insert_message_if_new(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    thread_id: Uuid,
    direction: &str,
    msg: &ProviderMessage,
    classification: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction,
             provider_message_id, internet_message_id, in_reply_to, reference_ids,
             subject, body_text, body_html, message_at, classification)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (workspace_id, mail_account_id, provider_message_id)
             WHERE provider_message_id IS NOT NULL
         DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(thread_id)
    .bind(direction)
    .bind(&msg.provider_message_id)
    .bind(&msg.internet_message_id)
    .bind(&msg.in_reply_to)
    .bind(&msg.references)
    .bind(&msg.subject)
    .bind(&msg.body_text)
    .bind(&msg.body_html)
    .bind(msg.message_at)
    .bind(classification)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Message whose provider id is already recorded.
pub async fn message_by_provider_id(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    provider_message_id: &str,
) -> Result<Option<MessageRow>, AppError> {
    sqlx::query_as::<_, MessageRow>(&format!(
        "SELECT {MESSAGE_COLS} FROM email_messages
          WHERE workspace_id = $1 AND mail_account_id = $2 AND provider_message_id = $3"
    ))
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(provider_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn insert_participant(
    tx: &mut Tx<'_>,
    email_message_id: Uuid,
    role: &str,
    address: &str,
    display_name: Option<&str>,
    email_address_id: Option<Uuid>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO message_participants
            (workspace_id, email_message_id, role, address, display_name, email_address_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (workspace_id, email_message_id, role, address) DO NOTHING",
    )
    .bind(tx.workspace_id())
    .bind(email_message_id)
    .bind(role)
    .bind(address)
    .bind(display_name)
    .bind(email_address_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Advance `last_inbound_message_id` when the new inbound is newer than the
/// current pointer. Also bumps the thread version so detail ETags change.
pub async fn advance_last_inbound(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
    message_id: Uuid,
    message_at: DateTime<Utc>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE email_threads t
            SET last_inbound_message_id = $3,
                version = version + 1,
                updated_at = now()
          WHERE t.workspace_id = $1 AND t.id = $2
            AND (t.last_inbound_message_id IS NULL
                 OR (SELECT m.message_at FROM email_messages m
                      WHERE m.workspace_id = t.workspace_id
                        AND m.id = t.last_inbound_message_id) <= $4)",
    )
    .bind(tx.workspace_id())
    .bind(thread_id)
    .bind(message_id)
    .bind(message_at)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// ---------------------------------------------------------- webhooks

/// Deduped webhook insert (§14.3). `Ok(None)` = already stored → ACK
/// without re-processing. `signature_verified` is always true — the CHECK
/// constraint refuses anything else, and callers must verify before insert.
pub async fn insert_webhook_event(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
    provider_event_key: &str,
    payload_hash: &[u8; 32],
    redacted_payload: &Value,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO webhook_events
            (workspace_id, connection_id, provider_event_key, payload_hash,
             redacted_payload, signature_verified)
         VALUES ($1, $2, $3, $4, $5, true)
         ON CONFLICT (workspace_id, connection_id, provider_event_key) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .bind(provider_event_key)
    .bind(&payload_hash[..])
    .bind(redacted_payload)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_webhook_event(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<WebhookEventRow>, AppError> {
    sqlx::query_as::<_, WebhookEventRow>(
        "SELECT id, connection_id, provider_event_key, redacted_payload, status
           FROM webhook_events WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn mark_webhook_event(tx: &mut Tx<'_>, id: Uuid, status: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE webhook_events SET status = $3, processed_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(status)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Principal exists (and is active) — assignment target validation.
pub async fn principal_active(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let n: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM principals
          WHERE workspace_id = $1 AND id = $2 AND status = 'active'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n.is_some())
}

/// `email_address_id` of a message's `from` participant (reply-drafts:
/// recipient = whoever wrote the inbound — never an arbitrary Reply-To,
/// B.3.10).
pub async fn from_address_id(tx: &mut Tx<'_>, message_id: Uuid) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT email_address_id FROM message_participants
          WHERE workspace_id = $1 AND email_message_id = $2 AND role = 'from'
            AND email_address_id IS NOT NULL
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `from` address text of a message (fallback for `from_address_id`).
pub async fn from_address(tx: &mut Tx<'_>, message_id: Uuid) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT address FROM message_participants
          WHERE workspace_id = $1 AND email_message_id = $2 AND role = 'from'
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Contact's (primary) address row for drafts where the sender's address
/// row is not linked to a `from` participant.
pub async fn contact_primary_address(
    tx: &mut Tx<'_>,
    contact_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT email_address_id FROM contact_emails
          WHERE workspace_id = $1 AND contact_id = $2
          ORDER BY is_primary DESC, created_at LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------- test/dev fixture rows
//
// Attachment ingest is not wired into `ProviderMessage` in v1.0 (fake
// providers carry no attachments), so object+attachment rows are written
// by fixtures and, later, a real ingest path — these helpers keep the SQL
// in one owned place.

/// Insert a `stored_objects` row (bytes themselves live in the object
/// store; this row is metadata only).
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn insert_stored_object(
    tx: &mut Tx<'_>,
    storage_key: &str,
    media_type: &str,
    byte_size: i64,
    content_hash: &[u8],
    scan_status: &str,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO stored_objects
            (workspace_id, storage_key, media_type, byte_size, content_hash, scan_status)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(storage_key)
    .bind(media_type)
    .bind(byte_size)
    .bind(content_hash)
    .bind(scan_status)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Map an attachment row onto a message's stored object.
#[doc(hidden)]
pub async fn insert_message_attachment(
    tx: &mut Tx<'_>,
    email_message_id: Uuid,
    stored_object_id: Uuid,
    filename: &str,
    is_inline: bool,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO message_attachments
            (workspace_id, email_message_id, stored_object_id, filename, is_inline)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(email_message_id)
    .bind(stored_object_id)
    .bind(filename)
    .bind(is_inline)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Count jobs of a kind in this workspace — test/support assertion helper
/// (e.g. "a `campaigns.pause_address` job was coalesced in").
#[doc(hidden)]
pub async fn count_jobs(tx: &mut Tx<'_>, kind: &str) -> Result<i64, AppError> {
    sqlx::query_scalar("SELECT count(*) FROM jobs WHERE workspace_id = $1 AND kind = $2")
        .bind(tx.workspace_id())
        .bind(kind)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)
}

/// Count `outbox_events` rows of a type — test/support assertion helper.
#[doc(hidden)]
pub async fn count_outbox(tx: &mut Tx<'_>, event_type: &str) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events WHERE workspace_id = $1 AND event_type = $2",
    )
    .bind(tx.workspace_id())
    .bind(event_type)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------- coalesced jobs

/// Enqueue with `(kind, dedupe_key)` coalescing — same-table
/// `ON CONFLICT DO NOTHING` so duplicate triggers (multiple inbound
/// messages of the same sender, repeated webhooks) enqueue once.
///
/// TODO(rebase): switch to `crate::jobs::enqueue` once it is idempotent on
/// `(kind, dedupe_key)` per the wave-B framework fix.
pub async fn enqueue_coalesced(
    tx: &mut Tx<'_>,
    kind: &str,
    payload: &Value,
    dedupe_key: &str,
) -> Result<Option<Uuid>, AppError> {
    enqueue_coalesced_at(tx, kind, payload, dedupe_key, Utc::now()).await
}

/// `enqueue_coalesced` with an explicit `available_at` (scheduled work such
/// as the periodic re-scan).
pub async fn enqueue_coalesced_at(
    tx: &mut Tx<'_>,
    kind: &str,
    payload: &Value,
    dedupe_key: &str,
    available_at: DateTime<Utc>,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO jobs (workspace_id, kind, payload_version, payload, available_at, dedupe_key)
         VALUES ($1, $2, 1, $3, $5, $4)
         ON CONFLICT (workspace_id, kind, dedupe_key) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(payload)
    .bind(dedupe_key)
    .bind(available_at)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ---------------------------------------------------------- HTML sanitize

/// Sanitize inbound HTML for API display (§15.1, T26): an allowlist of
/// formatting tags survives; script/style/form/iframe/object/embed and all
/// `on*`/style attributes are removed; `<img>` keeps `alt` only — no
/// `src`, so remote images never load and local/cid references are not
/// exposed either (conservative v1 choice).
///
/// `ammonia` is already a workspace dependency of this crate.
pub fn sanitize_html(html: &str) -> String {
    use std::collections::HashSet;
    let tags: HashSet<&'static str> = [
        "a",
        "abbr",
        "b",
        "blockquote",
        "br",
        "code",
        "div",
        "em",
        "figcaption",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "i",
        "img",
        "li",
        "ol",
        "p",
        "pre",
        "s",
        "span",
        "strong",
        "sub",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "tr",
        "u",
        "ul",
    ]
    .into_iter()
    .collect();
    let mut generic_attrs: HashSet<&'static str> = HashSet::new();
    generic_attrs.insert("title");
    generic_attrs.insert("lang");
    let mut a_attrs: HashSet<&'static str> = HashSet::new();
    a_attrs.insert("href");
    let mut img_attrs: HashSet<&'static str> = HashSet::new();
    img_attrs.insert("alt");
    let mut td_attrs: HashSet<&'static str> = HashSet::new();
    td_attrs.insert("colspan");
    td_attrs.insert("rowspan");
    let mut tag_attrs = std::collections::HashMap::new();
    tag_attrs.insert("a", a_attrs);
    tag_attrs.insert("img", img_attrs);
    tag_attrs.insert("td", td_attrs);
    tag_attrs.insert("th", {
        let mut s = HashSet::new();
        s.insert("colspan");
        s.insert("rowspan");
        s
    });

    ammonia::Builder::default()
        .tags(tags)
        .generic_attributes(generic_attrs)
        .tag_attributes(tag_attrs)
        .link_rel(Some("noopener noreferrer nofollow"))
        .url_schemes(["https", "http", "mailto"].into_iter().collect())
        .strip_comments(true)
        .clean(html)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_script_style_handlers_and_remote_images() {
        let dirty = r#"<div onclick="x()"><script>alert(1)</script>
            <style>.a{}</style><form action="/x"><input></form>
            <img src="https://tracker.example/p.gif" alt="pix" onerror="x()">
            <a href="https://ok.example" onmouseover="y()">link</a>
            <iframe src="https://evil.example"></iframe>
            <p style="color:red">hi</p></div>"#;
        let out = sanitize_html(dirty);
        assert!(!out.contains("<script"), "{out}");
        assert!(!out.contains("alert(1)"), "script content kept: {out}");
        assert!(!out.contains("<style"), "{out}");
        assert!(!out.contains("<form"), "{out}");
        assert!(!out.contains("<iframe"), "{out}");
        assert!(!out.contains("onclick"), "{out}");
        assert!(!out.contains("onerror"), "{out}");
        assert!(!out.contains("onmouseover"), "{out}");
        assert!(!out.contains("style="), "{out}");
        // img present but cannot load anything remote.
        assert!(out.contains("<img"), "{out}");
        assert!(!out.contains("src="), "{out}");
        assert!(out.contains("alt=\"pix\""), "{out}");
        // Safe link survives with rel hardening.
        assert!(out.contains("href=\"https://ok.example\""), "{out}");
        assert!(out.contains("noopener"), "{out}");
        assert!(out.contains("hi"), "{out}");
    }

    #[test]
    fn sanitize_blocks_javascript_and_data_urls() {
        let out = sanitize_html(
            r#"<a href="javascript:alert(1)">x</a><a href="data:text/html;base64,PHNjcj5h">y</a>"#,
        );
        assert!(!out.contains("javascript:"), "{out}");
        assert!(!out.contains("data:"), "{out}");
    }
}
