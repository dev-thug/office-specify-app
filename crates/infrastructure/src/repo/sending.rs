//! sending module repository (spec §6 — owned by the module worker).
//!
//! Tables owned by this module: `message_drafts`, `outbound_messages`,
//! `send_attempts`, `reply_claims`, `delivery_events`. The §8.4 pre-send
//! predicate reads against shared tables (workspaces, delivery_channels,
//! mail_account_grants, principals, approval_*, suppressions via
//! `compliance_core`, communication_bases, sender_identities,
//! campaign_enrollments/revisions, email_threads/messages) also live here —
//! read-side only for other modules' tables.
//!
//! Safety facts encoded here:
//! - `outbound_messages.purpose` MUST equal `delivery_channels.purpose`
//!   (composite FK) — `insert_outbound` takes the channel row, never a
//!   free-floating purpose.
//! - The submitting boundary (`outbound → 'submitting'` + `send_attempts`
//!   'started') commits BEFORE any provider call (A.6.4).
//! - `unknown` is terminal until provider reconciliation — nothing here
//!   resubmits.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

// ---------------------------------------------------------------------------
// message_drafts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DraftRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub mail_account_id: Uuid,
    pub owner_principal_id: Uuid,
    pub source_credential_id: Option<Uuid>,
    /// 'private' | 'mailbox'
    pub visibility: String,
    pub thread_id: Option<Uuid>,
    pub reply_to_message_id: Option<Uuid>,
    pub expected_last_inbound_id: Option<Uuid>,
    pub to_email_address_id: Uuid,
    pub template_version_id: Option<Uuid>,
    /// correspondence | marketing | transactional
    pub purpose: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub variables: serde_json::Value,
    /// draft | frozen | discarded
    pub status: String,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DraftListRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub owner_principal_id: Uuid,
    pub visibility: String,
    pub thread_id: Option<Uuid>,
    pub outbound_message_id: Option<Uuid>,
    pub purpose: String,
    pub subject: String,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const DRAFT_COLS: &str = "id, workspace_id, mail_account_id, owner_principal_id, source_credential_id, visibility, \
     thread_id, reply_to_message_id, expected_last_inbound_id, to_email_address_id, \
     template_version_id, purpose, subject, body_text, body_html, variables, status, \
     version, updated_at, created_at";

/// B.2 visibility rule: own drafts plus 'mailbox'-visible drafts on mailboxes
/// where the principal holds can_read.
pub async fn list_drafts(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    limit: i64,
    thread_id: Option<Uuid>,
    after: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<DraftListRow>, AppError> {
    sqlx::query_as::<_, DraftListRow>(
        "SELECT d.id, d.mail_account_id, d.owner_principal_id, d.visibility,
                d.thread_id, o.id AS outbound_message_id, d.purpose, d.subject,
                d.status, d.version, d.created_at, d.updated_at
           FROM message_drafts d
           LEFT JOIN outbound_messages o
             ON o.workspace_id = d.workspace_id AND o.source_draft_id = d.id
          WHERE d.workspace_id = $1
            AND EXISTS (
                SELECT 1 FROM mail_account_grants g
                 WHERE g.workspace_id = d.workspace_id
                   AND g.mail_account_id = d.mail_account_id
                   AND g.principal_id = $2 AND g.can_read
            )
            AND (d.owner_principal_id = $2
                 OR (d.visibility = 'mailbox' AND EXISTS (
                        SELECT 1 FROM mail_account_grants g
                         WHERE g.workspace_id = d.workspace_id
                           AND g.mail_account_id = d.mail_account_id
                           AND g.principal_id = $2 AND g.can_read)))
            AND ($3::uuid IS NULL OR d.thread_id = $3)
            AND ($4::timestamptz IS NULL OR (d.created_at, d.id) < ($4, $5))
          ORDER BY d.created_at DESC, d.id DESC
          LIMIT $6",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(thread_id)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_draft(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<DraftRow>, AppError> {
    sqlx::query_as::<_, DraftRow>(&format!(
        "SELECT {DRAFT_COLS} FROM message_drafts
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn find_frozen_outbound_for_draft(
    tx: &mut Tx<'_>,
    draft_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM outbound_messages
          WHERE workspace_id = $1 AND source_draft_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(draft_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_draft_for_update(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<DraftRow>, AppError> {
    sqlx::query_as::<_, DraftRow>(&format!(
        "SELECT {DRAFT_COLS} FROM message_drafts
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_draft(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    owner: Uuid,
    source_credential_id: Option<Uuid>,
    visibility: &str,
    thread_id: Option<Uuid>,
    reply_to_message_id: Option<Uuid>,
    expected_last_inbound_id: Option<Uuid>,
    to_email_address_id: Uuid,
    template_version_id: Option<Uuid>,
    purpose: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
    variables: &serde_json::Value,
) -> Result<DraftRow, AppError> {
    sqlx::query_as::<_, DraftRow>(&format!(
        "INSERT INTO message_drafts
            (workspace_id, mail_account_id, owner_principal_id, source_credential_id, visibility,
             thread_id, reply_to_message_id, expected_last_inbound_id,
             to_email_address_id, template_version_id, purpose, subject,
             body_text, body_html, variables)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
         RETURNING {DRAFT_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(owner)
    .bind(source_credential_id)
    .bind(visibility)
    .bind(thread_id)
    .bind(reply_to_message_id)
    .bind(expected_last_inbound_id)
    .bind(to_email_address_id)
    .bind(template_version_id)
    .bind(purpose)
    .bind(subject)
    .bind(body_text)
    .bind(body_html)
    .bind(variables)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Content-allowlist PATCH guarded by `version` (If-Match). Returns
/// `Ok(None)` on version mismatch or when the draft left 'draft'.
#[allow(clippy::too_many_arguments)]
pub async fn patch_draft(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    subject: Option<&str>,
    body_text: Option<&str>,
    body_html: Option<Option<&str>>,
    variables: Option<&serde_json::Value>,
    to_email_address_id: Option<Uuid>,
    expected_last_inbound_id: Option<Uuid>,
    visibility: Option<&str>,
) -> Result<Option<DraftRow>, AppError> {
    sqlx::query_as::<_, DraftRow>(&format!(
        "UPDATE message_drafts
            SET subject = COALESCE($4, subject),
                body_text = COALESCE($5, body_text),
                body_html = CASE WHEN $6 THEN $7 ELSE body_html END,
                variables = COALESCE($8, variables),
                to_email_address_id = COALESCE($9, to_email_address_id),
                expected_last_inbound_id = COALESCE($10, expected_last_inbound_id),
                visibility = COALESCE($11, visibility),
                version = version + 1,
                updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $3
            AND status = 'draft'
         RETURNING {DRAFT_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .bind(subject)
    .bind(body_text)
    .bind(body_html.is_some())
    .bind(body_html.flatten())
    .bind(variables)
    .bind(to_email_address_id)
    .bind(expected_last_inbound_id)
    .bind(visibility)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn set_draft_status(tx: &mut Tx<'_>, id: Uuid, status: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE message_drafts SET status = $3, version = version + 1,
                updated_at = now()
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

// ---------------------------------------------------------------------------
// outbound_messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OutboundRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub to_email_address_id: Uuid,
    pub communication_basis_id: Option<Uuid>,
    pub source_draft_id: Option<Uuid>,
    pub launch_id: Option<Uuid>,
    pub revision_id: Option<Uuid>,
    pub enrollment_id: Option<Uuid>,
    pub step_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub reply_to_message_id: Option<Uuid>,
    pub requested_by: Uuid,
    pub source_credential_id: Option<Uuid>,
    pub internet_message_id: Option<String>,
    pub explicit_submission_at: Option<DateTime<Utc>>,
    pub purpose: String,
    pub from_address_snapshot: String,
    pub to_address_snapshot: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub payload_hash: Vec<u8>,
    pub status: String,
    pub not_before: Option<DateTime<Utc>>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub provider_message_id: Option<String>,
    pub failure_code: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const OUTBOUND_COLS: &str = "id, workspace_id, mail_account_id, delivery_channel_id, \
     sender_identity_id, to_email_address_id, communication_basis_id, source_draft_id, \
     launch_id, revision_id, enrollment_id, step_id, thread_id, reply_to_message_id, \
     requested_by, source_credential_id, internet_message_id, explicit_submission_at, purpose, from_address_snapshot, \
     to_address_snapshot, subject, body_text, body_html, payload_hash, status, \
     not_before, accepted_at, provider_message_id, failure_code, version, created_at";

pub async fn get_outbound(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<OutboundRow>, AppError> {
    sqlx::query_as::<_, OutboundRow>(&format!(
        "SELECT {OUTBOUND_COLS} FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `FOR UPDATE` — submit/cancel/dispatch serialize on this row.
pub async fn get_outbound_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<OutboundRow>, AppError> {
    sqlx::query_as::<_, OutboundRow>(&format!(
        "SELECT {OUTBOUND_COLS} FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Input for `insert_outbound`. `purpose` is deliberately taken from the
/// verified channel row by the caller — the composite FK
/// `(workspace_id, delivery_channel_id, purpose)` enforces it.
#[derive(Debug)]
#[allow(clippy::too_many_arguments)]
pub struct NewOutbound {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub sender_identity_id: Uuid,
    pub to_email_address_id: Uuid,
    pub communication_basis_id: Option<Uuid>,
    pub source_draft_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub reply_to_message_id: Option<Uuid>,
    pub requested_by: Uuid,
    pub source_credential_id: Option<Uuid>,
    pub internet_message_id: Option<String>,
    pub purpose: String,
    pub from_address_snapshot: String,
    pub to_address_snapshot: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub payload_hash: [u8; 32],
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_outbound(tx: &mut Tx<'_>, n: &NewOutbound) -> Result<OutboundRow, AppError> {
    let res = sqlx::query_as::<_, OutboundRow>(&format!(
        "INSERT INTO outbound_messages
            (id, workspace_id, mail_account_id, delivery_channel_id,
             sender_identity_id, to_email_address_id, communication_basis_id,
             source_draft_id, thread_id, reply_to_message_id, requested_by,
             source_credential_id, internet_message_id, purpose, from_address_snapshot,
             to_address_snapshot, subject, body_text, body_html, payload_hash,
             status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,
                 'draft')
         RETURNING {OUTBOUND_COLS}"
    ))
    .bind(n.id)
    .bind(tx.workspace_id())
    .bind(n.mail_account_id)
    .bind(n.delivery_channel_id)
    .bind(n.sender_identity_id)
    .bind(n.to_email_address_id)
    .bind(n.communication_basis_id)
    .bind(n.source_draft_id)
    .bind(n.thread_id)
    .bind(n.reply_to_message_id)
    .bind(n.requested_by)
    .bind(n.source_credential_id)
    .bind(&n.internet_message_id)
    .bind(&n.purpose)
    .bind(&n.from_address_snapshot)
    .bind(&n.to_address_snapshot)
    .bind(&n.subject)
    .bind(&n.body_text)
    .bind(&n.body_html)
    .bind(&n.payload_hash[..])
    .fetch_one(tx.conn())
    .await;
    match res {
        Ok(o) => Ok(o),
        Err(e) if crate::error::is_unique_violation_on(&e, "outbound_source_draft_uq") => {
            Err(AppError::conflict("draft already has an outbound snapshot"))
        }
        Err(e) => Err(map_sqlx(e)),
    }
}

/// The snapshot trigger only allows status/not_before/provider fields to
/// change; every setter below stays inside that envelope.
pub async fn set_status(
    tx: &mut Tx<'_>,
    id: Uuid,
    status: &str,
    failure_code: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE outbound_messages
            SET status = $3, failure_code = $4, version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(status)
    .bind(failure_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn set_queued(
    tx: &mut Tx<'_>,
    id: Uuid,
    not_before: Option<DateTime<Utc>>,
    explicit_submission_at: DateTime<Utc>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE outbound_messages
            SET status = 'queued', not_before = $3,
                explicit_submission_at = $4, version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(not_before)
    .bind(explicit_submission_at)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Remove an unmarked legacy queue row before it can cross the provider
/// boundary. Caller holds the outbound row lock and records the audit event.
pub async fn return_legacy_queue_to_draft(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let reset = sqlx::query(
        "UPDATE outbound_messages
            SET status = 'draft', not_before = NULL, failure_code = NULL,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2
            AND status IN ('queued','claimed') AND explicit_submission_at IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM send_attempts sa
                 WHERE sa.workspace_id = outbound_messages.workspace_id
                   AND sa.outbound_message_id = outbound_messages.id
            )",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    if reset == 1 {
        // Keep the reserved claim as history. Claims already submitted to a
        // provider are never cleared by a legacy queue repair.
        sqlx::query(
            "UPDATE reply_claims rc SET state = 'released'
               FROM outbound_messages o
              WHERE o.workspace_id = $1 AND o.id = $2
                AND rc.workspace_id = o.workspace_id
                AND rc.mail_account_id = o.mail_account_id
                AND rc.thread_id = o.thread_id
                AND rc.inbound_message_id = o.reply_to_message_id
                AND rc.outbound_message_id = o.id
                AND rc.state = 'reserved'",
        )
        .bind(tx.workspace_id())
        .bind(id)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }
    Ok(reset == 1)
}

/// The submitting boundary — the last durable write before the provider
/// call (A.6.4). `lease_token` is the dispatch job's fencing token.
pub async fn mark_submitting(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE outbound_messages
            SET status = 'submitting', version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND status IN ('queued','claimed')",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn mark_accepted(
    tx: &mut Tx<'_>,
    id: Uuid,
    provider_message_id: &str,
    accepted_at: DateTime<Utc>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE outbound_messages
            SET status = 'accepted', accepted_at = $3, provider_message_id = $4,
                failure_code = NULL, version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(accepted_at)
    .bind(provider_message_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// send_attempts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SendAttemptRow {
    pub id: Uuid,
    pub outbound_message_id: Uuid,
    pub attempt_no: i32,
    pub lease_token: Uuid,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub error_code: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub async fn next_attempt_no(tx: &mut Tx<'_>, outbound_id: Uuid) -> Result<i32, AppError> {
    let n: Option<i32> = sqlx::query_scalar(
        "SELECT max(attempt_no) FROM send_attempts
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n.unwrap_or(0) + 1)
}

pub async fn insert_send_attempt(
    tx: &mut Tx<'_>,
    outbound_id: Uuid,
    attempt_no: i32,
    lease_token: Uuid,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO send_attempts
            (workspace_id, outbound_message_id, attempt_no, lease_token, status)
         VALUES ($1, $2, $3, $4, 'started')
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .bind(attempt_no)
    .bind(lease_token)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Finish an attempt. The DB guard only allows updating rows where
/// `finished_at IS NULL` (attempts never reopen).
#[allow(clippy::too_many_arguments)]
pub async fn finish_send_attempt(
    tx: &mut Tx<'_>,
    attempt_id: Uuid,
    status: &str,
    provider_message_id: Option<&str>,
    error_code: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE send_attempts
            SET status = $3, provider_message_id = $4, error_code = $5,
                finished_at = now()
          WHERE workspace_id = $1 AND id = $2 AND finished_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(attempt_id)
    .bind(status)
    .bind(provider_message_id)
    .bind(error_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

pub async fn latest_attempt(
    tx: &mut Tx<'_>,
    outbound_id: Uuid,
) -> Result<Option<SendAttemptRow>, AppError> {
    sqlx::query_as::<_, SendAttemptRow>(
        "SELECT id, outbound_message_id, attempt_no, lease_token, status,
                provider_message_id, error_code, started_at, finished_at
           FROM send_attempts
          WHERE workspace_id = $1 AND outbound_message_id = $2
          ORDER BY attempt_no DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ---------------------------------------------------------------------------
// reply_claims (A.6.5)
// ---------------------------------------------------------------------------

/// Insert the active claim for (mail_account, inbound_message_id). The
/// `reply_claim_active_uq` index guarantees at most one active row — a
/// conflict surfaces as `REPLY_ALREADY_SUBMITTED` (mapped by the caller).
pub async fn insert_reply_claim(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    thread_id: Uuid,
    inbound_message_id: Uuid,
    outbound_message_id: Uuid,
) -> Result<Uuid, AppError> {
    let res = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO reply_claims
            (workspace_id, mail_account_id, thread_id, inbound_message_id,
             outbound_message_id, state)
         VALUES ($1,$2,$3,$4,$5,'reserved')
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(thread_id)
    .bind(inbound_message_id)
    .bind(outbound_message_id)
    .fetch_one(tx.conn())
    .await;
    match res {
        Ok(id) => Ok(id),
        Err(e) if crate::error::is_unique_violation_on(&e, "reply_claim_active_uq") => {
            Err(AppError::new(
                contracts::ErrorCode::ReplyAlreadySubmitted,
                "an active reply already exists for this inbound message",
            ))
        }
        Err(e) => Err(map_sqlx(e)),
    }
}

/// Pre-check so the API can answer 409 cleanly (the unique index is the
/// real guard against races).
pub async fn active_claim_for_inbound(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    inbound_message_id: Uuid,
) -> Result<Option<(Uuid, String, Uuid)>, AppError> {
    sqlx::query_as(
        "SELECT id, state, outbound_message_id FROM reply_claims
          WHERE workspace_id = $1 AND mail_account_id = $2
            AND inbound_message_id = $3
            AND state IN ('reserved','submitted','accepted')",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(inbound_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Move this outbound's claim to `to_state` when it is currently in
/// `from_states`. Releases happen only through the explicit call sites
/// (cancel, failed outcome) — never implicitly on retry (A.6.5).
pub async fn transition_claim(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
    from_states: &[&str],
    to_state: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE reply_claims SET state = $4
          WHERE workspace_id = $1 AND outbound_message_id = $2
            AND state = ANY($3)",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .bind(from_states)
    .bind(to_state)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// delivery_events — provider-confirmed events only (T34: accepted does
// not imply delivered).
// ---------------------------------------------------------------------------

/// Idempotent insert keyed on `provider_event_key`. Returns false when the
/// event was already recorded (dedupe — §9.3/T14-style consumers rely on
/// the UNIQUE constraint).
pub async fn insert_delivery_event(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
    provider_event_key: &str,
    event_type: &str,
    occurred_at: DateTime<Utc>,
    details: &serde_json::Value,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "INSERT INTO delivery_events
            (workspace_id, outbound_message_id, provider_event_key, event_type,
             occurred_at, details)
         VALUES ($1,$2,$3,$4,$5,$6)
         ON CONFLICT (workspace_id, provider_event_key) DO NOTHING",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .bind(provider_event_key)
    .bind(event_type)
    .bind(occurred_at)
    .bind(details)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

pub async fn delivery_event_types(
    tx: &mut Tx<'_>,
    outbound_id: Uuid,
) -> Result<Vec<String>, AppError> {
    sqlx::query_scalar(
        "SELECT event_type FROM delivery_events
          WHERE workspace_id = $1 AND outbound_message_id = $2
          ORDER BY occurred_at, id",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ---------------------------------------------------------------------------
// jobs — cancel queued dispatch work when the business row is canceled
// ---------------------------------------------------------------------------

/// Cancel not-yet-running dispatch jobs for this outbound. `running` jobs
/// are left alone — the in-flight boundary is authoritative (A.6.4).
pub async fn cancel_queued_dispatch_jobs(
    tx: &mut Tx<'_>,
    outbound_id: Uuid,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE jobs SET state = 'canceled'
          WHERE workspace_id = $1 AND kind = 'sending.dispatch_outbound'
            AND payload->>'outbound_message_id' = $2
            AND state IN ('queued','retry_wait')",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id.to_string())
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// Count reconcile jobs already enqueued for this outbound (rearm cap —
/// `unknown` stays `unknown` forever unless evidence appears; we bound the
/// polling chain).
pub async fn reconcile_job_count(tx: &mut Tx<'_>, outbound_id: Uuid) -> Result<i64, AppError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id = $1 AND kind = 'sending.reconcile_unknown'
            AND payload->>'outbound_message_id' = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id.to_string())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n)
}

// ---------------------------------------------------------------------------
// §8.4 predicate reads + per-channel send-cap reservation
// ---------------------------------------------------------------------------

/// Serialize channel-level send-cap reservations before counting committed
/// attempts. The lock is taken after the recipient row lock so dispatches use
/// a consistent A.6.4 lock order.
pub async fn lock_channel(tx: &mut Tx<'_>, channel_id: Uuid) -> Result<bool, AppError> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM delivery_channels
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(found.is_some())
}

/// workspaces has no workspace_id/RLS (global table) — the tx still binds
/// the workspace id explicitly.
pub async fn workspace_flags(tx: &mut Tx<'_>) -> Result<Option<(bool, bool, String)>, AppError> {
    let row: Option<(bool, serde_json::Value, String)> = sqlx::query_as(
        "SELECT emergency_stop, settings, default_timezone FROM workspaces
          WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row.map(|(stop, settings, tz)| {
        let locked = settings
            .get("send_workers_locked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        (stop, locked, tz)
    }))
}

/// Channel + mailbox + connection join used by the §8.4 health check.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChannelState {
    pub enabled: bool,
    /// unverified | allowed | blocked
    pub policy_status: String,
    /// zoho_mail | delivery_provider | transactional_provider
    pub provider: String,
    /// The channel's own purpose — the outbound's purpose must equal it
    /// (composite FK safety fact).
    pub purpose: String,
    pub sender_identity_id: Uuid,
    pub mail_account_id: Uuid,
    pub internal_daily_cap: Option<i32>,
    pub mail_account_status: String,
    pub connection_status: String,
}

pub async fn channel_state(
    tx: &mut Tx<'_>,
    channel_id: Uuid,
) -> Result<Option<ChannelState>, AppError> {
    sqlx::query_as::<_, ChannelState>(
        "SELECT dc.enabled, dc.policy_status, dc.provider, dc.purpose,
                dc.sender_identity_id, dc.mail_account_id, dc.internal_daily_cap,
                ma.status AS mail_account_status,
                ic.status AS connection_status
           FROM delivery_channels dc
           JOIN mail_accounts ma
             ON ma.workspace_id = dc.workspace_id AND ma.id = dc.mail_account_id
           JOIN integration_connections ic
             ON ic.workspace_id = dc.workspace_id AND ic.id = dc.connection_id
          WHERE dc.workspace_id = $1 AND dc.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// principal liveness for the requester/approver re-check (§5.5).
pub async fn principal_state(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<Option<(String, String, String)>, AppError> {
    sqlx::query_as(
        "SELECT status, kind, role FROM principals
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Grant row for an arbitrary principal (the requester at dispatch time —
/// not the ctx principal).
pub async fn grant_for_principal(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    mail_account_id: Uuid,
) -> Result<Option<(bool, bool, bool, bool, bool)>, AppError> {
    sqlx::query_as(
        "SELECT can_read, can_compose, can_send, can_approve, can_assign
           FROM mail_account_grants
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// A bound credential must still be usable (§5.5 — revoking the credential
/// stops pending work).
pub async fn credential_usable(tx: &mut Tx<'_>, credential_id: Uuid) -> Result<bool, AppError> {
    crate::repo::auth::credential_authorizes(tx, credential_id, None, &[]).await
}

/// A frozen outbound must retain an active source credential owned by its
/// requesting principal. This keeps another principal from borrowing a
/// valid credential ID on a forged/restored row.
pub async fn source_credential_usable(
    tx: &mut Tx<'_>,
    credential_id: Uuid,
    principal_id: Uuid,
) -> Result<bool, AppError> {
    crate::repo::auth::credential_authorizes(tx, credential_id, Some(principal_id), &[]).await
}

/// Latest approval request targeting this outbound (any status — the
/// evaluator interprets it).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApprovalState {
    pub id: Uuid,
    pub requested_by: Uuid,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub snapshot_hash: Vec<u8>,
}

pub async fn latest_outbound_approval(
    tx: &mut Tx<'_>,
    outbound_id: Uuid,
) -> Result<Option<ApprovalState>, AppError> {
    sqlx::query_as::<_, ApprovalState>(
        "SELECT id, requested_by, status, expires_at, snapshot_hash
           FROM approval_requests
          WHERE workspace_id = $1 AND outbound_message_id = $2
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The latest launch-level approval state and the frozen membership facts for
/// one campaign outbound. Campaign approval pins the full manifest; the
/// enrollment/step joins prove that this immutable payload belongs to the
/// approved launch.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CampaignLaunchApprovalState {
    pub launch_status: String,
    pub launch_manifest_hash: Option<Vec<u8>>,
    pub launch_expires_at: DateTime<Utc>,
    pub launch_revision_id: Uuid,
    pub launch_channel_id: Uuid,
    pub launch_requested_by: Uuid,
    pub revision_status: String,
    pub revision_channel_id: Uuid,
    pub request_id: Option<Uuid>,
    pub request_requested_by: Option<Uuid>,
    pub request_action_type: Option<String>,
    pub request_status: Option<String>,
    pub request_snapshot_hash: Option<Vec<u8>>,
    pub request_expires_at: Option<DateTime<Utc>>,
    pub request_version: Option<i64>,
    pub decision: Option<String>,
    pub decision_snapshot_hash: Option<Vec<u8>>,
    pub decision_version_at_decision: Option<i64>,
    pub decided_by: Option<Uuid>,
    pub enrollment_status: Option<String>,
    pub enrollment_launch_id: Option<Uuid>,
    pub enrollment_revision_id: Option<Uuid>,
    pub enrollment_email_address_id: Option<Uuid>,
    pub step_revision_id: Option<Uuid>,
    pub step_order: Option<i32>,
    pub payload_hash_len: i32,
}

pub async fn campaign_launch_approval_state(
    tx: &mut Tx<'_>,
    outbound: &OutboundRow,
) -> Result<Option<CampaignLaunchApprovalState>, AppError> {
    if outbound.launch_id.is_none() {
        return Ok(None);
    }
    sqlx::query_as::<_, CampaignLaunchApprovalState>(
        "SELECT l.status AS launch_status, l.manifest_hash AS launch_manifest_hash,
                l.expires_at AS launch_expires_at, l.revision_id AS launch_revision_id,
                l.delivery_channel_id AS launch_channel_id,
                l.requested_by AS launch_requested_by,
                r.status AS revision_status, r.delivery_channel_id AS revision_channel_id,
                ar.id AS request_id, ar.requested_by AS request_requested_by,
                ar.action_type AS request_action_type, ar.status AS request_status,
                ar.snapshot_hash AS request_snapshot_hash,
                ar.expires_at AS request_expires_at, ar.version AS request_version,
                d.decision, d.snapshot_hash AS decision_snapshot_hash,
                d.version_at_decision AS decision_version_at_decision,
                d.decided_by,
                e.status AS enrollment_status, e.launch_id AS enrollment_launch_id,
                e.revision_id AS enrollment_revision_id,
                e.email_address_id AS enrollment_email_address_id,
                s.revision_id AS step_revision_id, s.step_order,
                octet_length(o.payload_hash) AS payload_hash_len
           FROM outbound_messages o
           JOIN campaign_launches l
             ON l.workspace_id = o.workspace_id AND l.id = o.launch_id
           JOIN campaign_revisions r
             ON r.workspace_id = o.workspace_id AND r.id = o.revision_id
           LEFT JOIN campaign_enrollments e
             ON e.workspace_id = o.workspace_id AND e.id = o.enrollment_id
           LEFT JOIN campaign_steps s
             ON s.workspace_id = o.workspace_id AND s.id = o.step_id
           LEFT JOIN LATERAL (
                SELECT id, workspace_id, requested_by, action_type, status, snapshot_hash,
                       expires_at, version
                  FROM approval_requests
                 WHERE workspace_id = o.workspace_id AND launch_id = l.id
                   AND action_type = 'campaign_launch'
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1
           ) ar ON true
           LEFT JOIN approval_decisions d
             ON d.workspace_id = ar.workspace_id
            AND d.approval_request_id = ar.id
          WHERE o.workspace_id = $1 AND o.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound.id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The decision row for an approval request (one per request by UNIQUE).
pub async fn approval_decision(
    tx: &mut Tx<'_>,
    request_id: Uuid,
) -> Result<Option<(String, Uuid)>, AppError> {
    sqlx::query_as(
        "SELECT decision, decided_by FROM approval_decisions
          WHERE workspace_id = $1 AND approval_request_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(request_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Most recent *valid* basis for (address, purpose) — any jurisdiction;
/// jurisdiction selection belongs to the compliance review flow.
pub async fn any_valid_basis(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = $2 AND purpose = $3
            AND status = 'valid'
            AND (valid_until IS NULL OR valid_until > now())
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Is this specific basis still valid? (the frozen outbound pins a basis
/// row; revocation/expiry between freeze and send must fail closed).
pub async fn basis_still_valid(tx: &mut Tx<'_>, basis_id: Uuid) -> Result<bool, AppError> {
    let ok: Option<bool> = sqlx::query_scalar(
        "SELECT (status = 'valid' AND (valid_until IS NULL OR valid_until > now()))
           FROM communication_bases
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(basis_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(ok.unwrap_or(false))
}

/// Campaign-linked sends only: enrollment must still be active.
pub async fn enrollment_status(
    tx: &mut Tx<'_>,
    enrollment_id: Uuid,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT status FROM campaign_enrollments
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(enrollment_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Campaign-linked sends only: the revision's schedule_rules (+timezone).
pub async fn campaign_schedule_rules(
    tx: &mut Tx<'_>,
    revision_id: Uuid,
) -> Result<Option<serde_json::Value>, AppError> {
    sqlx::query_scalar(
        "SELECT schedule_rules FROM campaign_revisions
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(revision_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Sender identity must still be verified at send time.
pub async fn sender_identity_status(
    tx: &mut Tx<'_>,
    sender_identity_id: Uuid,
) -> Result<Option<(String, String)>, AppError> {
    sqlx::query_as(
        "SELECT status, address FROM sender_identities
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(sender_identity_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Provider-accepted, in-flight, and unknown attempts for a channel in the
/// rolling 24h window. This durable PostgreSQL count reserves room for a
/// submission before provider I/O and does not rely on Redis counters.
pub async fn channel_attempts_24h(tx: &mut Tx<'_>, channel_id: Uuid) -> Result<i64, AppError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM send_attempts sa
           JOIN outbound_messages om
             ON om.workspace_id = sa.workspace_id
            AND om.id = sa.outbound_message_id
          WHERE om.workspace_id = $1
            AND om.delivery_channel_id = $2
            AND sa.started_at > now() - interval '24 hours'
            AND sa.status IN ('started','accepted','unknown')",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n)
}

/// Latest accepted_at on the channel — the minimum-gap/cooldown predicate.
pub async fn channel_last_accepted_at(
    tx: &mut Tx<'_>,
    channel_id: Uuid,
) -> Result<Option<DateTime<Utc>>, AppError> {
    sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT max(accepted_at) FROM outbound_messages
          WHERE workspace_id = $1 AND delivery_channel_id = $2
            AND status = 'accepted'",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ---------------------------------------------------------------------------
// reply linkage + email rows
// ---------------------------------------------------------------------------

/// Thread row `FOR UPDATE` — A.6.5 locks the thread before the claim.
pub async fn get_thread_for_update(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
) -> Result<Option<(Uuid, Option<Uuid>)>, AppError> {
    sqlx::query_as(
        "SELECT mail_account_id, last_inbound_message_id FROM email_threads
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(thread_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_thread(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
) -> Result<Option<(Uuid, Option<Uuid>)>, AppError> {
    sqlx::query_as(
        "SELECT mail_account_id, last_inbound_message_id FROM email_threads
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(thread_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Verify a message exists as an inbound message in (mailbox, thread).
pub async fn inbound_message_in_thread(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    thread_id: Uuid,
    message_id: Uuid,
) -> Result<bool, AppError> {
    let ok: Option<bool> = sqlx::query_scalar(
        "SELECT (direction = 'inbound') FROM email_messages
          WHERE workspace_id = $1 AND mail_account_id = $2
            AND thread_id = $3 AND id = $4",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(thread_id)
    .bind(message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(ok.unwrap_or(false))
}

/// A reply draft may only target the normalized address on the inbound's
/// `From` participant. Require one unambiguous inbound sender and verify
/// both its text and any normalized-address link to prevent recipient
/// retargeting through legacy or manually altered draft rows.
pub async fn reply_recipient_matches(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    thread_id: Uuid,
    inbound_message_id: Uuid,
    recipient_email_address_id: Uuid,
) -> Result<bool, AppError> {
    let Some((_, recipient_key)) = email_address_row(tx, recipient_email_address_id).await? else {
        return Ok(false);
    };
    let senders: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT p.address, ea.normalized_key
           FROM email_messages m
           JOIN message_participants p
             ON p.workspace_id = m.workspace_id AND p.email_message_id = m.id
            AND p.role = 'from'
           LEFT JOIN email_addresses ea
             ON ea.workspace_id = p.workspace_id AND ea.id = p.email_address_id
          WHERE m.workspace_id = $1 AND m.mail_account_id = $2
            AND m.thread_id = $3 AND m.id = $4 AND m.direction = 'inbound'",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(thread_id)
    .bind(inbound_message_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if senders.len() != 1 {
        return Ok(false);
    }
    let (sender_address, linked_sender_key) = &senders[0];
    let Ok(sender_key) = domain::email_address::normalize(
        sender_address,
        domain::email_address::LocalPartPolicy::Lowercase,
    ) else {
        return Ok(false);
    };
    Ok(sender_key.normalized_key == recipient_key
        && linked_sender_key
            .as_ref()
            .is_none_or(|linked_key| linked_key == &sender_key.normalized_key))
}

/// The reply target's RFC Message-ID (for In-Reply-To headers, §11.3).
pub async fn message_internet_id(
    tx: &mut Tx<'_>,
    message_id: Uuid,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT internet_message_id FROM email_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
    .map(|o| o.flatten())
}

/// Address row snapshot fields for freezing.
pub async fn email_address_row(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<(String, String)>, AppError> {
    sqlx::query_as(
        "SELECT address_original, normalized_key FROM email_addresses
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Insert the `unsubscribe_tokens` row for a caller-minted raw token.
/// The token must be generated BEFORE the outbound row exists (its URL is
/// baked into the immutable body), so `issue_unsubscribe_token`'s own
/// INSERT-then-return ordering cannot be used; this variant takes the raw
/// token, hashes it, and stores only the hash (same as the shared helper).
pub async fn insert_unsubscribe_token_hash(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    outbound_message_id: Uuid,
    raw_token: &str,
) -> Result<Uuid, AppError> {
    let hash = crate::crypto::sha256_bytes(raw_token.as_bytes());
    sqlx::query_scalar(
        "INSERT INTO unsubscribe_tokens
            (workspace_id, token_hash, email_address_id, outbound_message_id)
         VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(&hash[..])
    .bind(email_address_id)
    .bind(outbound_message_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Confirm that a raw token recovered from a frozen message is the active
/// marketing capability for that exact outbound and recipient. The raw token
/// is hashed before it reaches SQL and is never persisted by this lookup.
pub async fn unsubscribe_token_belongs_to_outbound(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    outbound_message_id: Uuid,
    raw_token: &str,
) -> Result<bool, AppError> {
    let token_hash = crate::crypto::sha256_bytes(raw_token.as_bytes());
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM unsubscribe_tokens
             WHERE workspace_id = $1
               AND email_address_id = $2
               AND outbound_message_id = $3
               AND token_hash = $4
               AND purpose_scope = 'marketing'
               AND revoked_at IS NULL
        )",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(outbound_message_id)
    .bind(token_hash.to_vec())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `workspaces.settings->>'public_base_url'` — the externally reachable
/// base used for unsubscribe links (defaults to the dev bind).
pub async fn public_base_url(tx: &mut Tx<'_>) -> Result<String, AppError> {
    let v: Option<String> =
        sqlx::query_scalar("SELECT settings->>'public_base_url' FROM workspaces WHERE id = $1")
            .bind(tx.workspace_id())
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)
            .map(|o| o.flatten())?;
    crate::config::public_base_url(v)
}

// ---------------------------------------------------------------------------
// enqueue-once helper — jobs::enqueue maps a dedupe conflict to a hard
// Conflict error; dispatch re-arms and reconcile chains want insert-or-skip
// semantics instead (safe under racing dispatches).
// ---------------------------------------------------------------------------

/// Insert a job only if (workspace, kind, dedupe_key) is free. Returns
/// `Some(id)` when inserted, `None` when an equivalent job already exists.
/// TODO(rebase): replace with `jobs::enqueue` once it is idempotent on
/// `(kind, dedupe_key)` per wave-b-common.
pub async fn enqueue_once(
    tx: &mut Tx<'_>,
    kind: &str,
    payload_version: i32,
    payload: serde_json::Value,
    available_at: DateTime<Utc>,
    dedupe_key: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO jobs
            (workspace_id, kind, payload_version, payload, available_at, dedupe_key)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (workspace_id, kind, dedupe_key) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(payload_version)
    .bind(&payload)
    .bind(available_at)
    .bind(dedupe_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}
