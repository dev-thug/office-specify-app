//! campaigns module repository (spec §6 — owned by the module worker).
//!
//! All campaign/revision/step/launch/enrollment SQL plus the campaign-owned
//! `outbound_messages` step snapshots (shared table — sending's worker owns
//! submission; campaigns only creates frozen `draft`/`queued` rows). Freeze
//! rules (A.6.2) are enforced twice: state transitions here use guarded
//! UPDATEs, and migration-0003 triggers reject structural edits of frozen
//! parents.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;
use crate::jobs;

// ------------------------------------------------------------------ rows

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CampaignRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub name: String,
    pub objective: Option<String>,
    pub owner_principal_id: Uuid,
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RevisionRow {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub mail_account_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub revision_no: i32,
    pub status: String,
    pub timezone: String,
    pub schedule_rules: serde_json::Value,
    pub stop_on_reply: bool,
    pub stop_on_booking: bool,
    pub created_by: Uuid,
    pub frozen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StepRow {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub template_version_id: Uuid,
    pub step_order: i32,
    pub delay_hours: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LaunchRow {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub requested_by: Uuid,
    pub status: String,
    pub manifest_hash: Option<Vec<u8>>,
    pub frozen_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EnrollmentRow {
    pub id: Uuid,
    pub launch_id: Uuid,
    pub revision_id: Uuid,
    pub email_address_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub status: String,
    pub current_step_id: Option<Uuid>,
    pub next_send_at: Option<DateTime<Utc>>,
    pub stop_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChannelRow {
    pub id: Uuid,
    pub mail_account_id: Uuid,
    pub sender_identity_id: Uuid,
    pub provider: String,
    pub purpose: String,
    pub policy_status: String,
    pub enabled: bool,
    pub internal_daily_cap: Option<i32>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TemplateVersionRow {
    pub id: Uuid,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub variable_schema: serde_json::Value,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StepOutboundRow {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub step_id: Uuid,
    pub to_address_snapshot: String,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub status: String,
    pub not_before: Option<DateTime<Utc>>,
    pub payload_hash: Vec<u8>,
    pub created_at: DateTime<Utc>,
}

/// Joined (revision, step_count) — sqlx maps flat rows into `flatten`.
#[derive(Debug, sqlx::FromRow)]
pub struct RevisionWithCount {
    #[sqlx(flatten)]
    pub revision: RevisionRow,
    pub step_count: i64,
}

/// Joined (step outbound, step_order).
#[derive(Debug, sqlx::FromRow)]
pub struct StepOutboundWithOrder {
    #[sqlx(flatten)]
    pub outbound: StepOutboundRow,
    pub step_order: i32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EnrollmentCounts {
    pub total: i64,
    pub pending: i64,
    pub active: i64,
    pub completed: i64,
    pub stopped: i64,
    pub suppressed: i64,
}

// -------------------------------------------------------------- campaigns

pub async fn insert_campaign(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    name: &str,
    objective: Option<&str>,
    owner_principal_id: Uuid,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO campaigns (workspace_id, mail_account_id, name, objective, owner_principal_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(name)
    .bind(objective)
    .bind(owner_principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_campaign(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<CampaignRow>, AppError> {
    sqlx::query_as::<_, CampaignRow>(
        "SELECT id, mail_account_id, name, objective, owner_principal_id,
                status, version, created_at, updated_at
           FROM campaigns WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Campaign list, mailbox-ACL filtered (§5.6/B.2): only campaigns whose
/// mail account the principal can read. Keyset (created_at DESC, id DESC).
pub async fn list_campaigns(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<CampaignRow>, AppError> {
    sqlx::query_as::<_, CampaignRow>(
        "SELECT c.id, c.mail_account_id, c.name, c.objective, c.owner_principal_id,
                c.status, c.version, c.created_at, c.updated_at
           FROM campaigns c
          WHERE c.workspace_id = $1
            AND EXISTS (SELECT 1 FROM mail_account_grants g
                         WHERE g.workspace_id = c.workspace_id
                           AND g.mail_account_id = c.mail_account_id
                           AND g.principal_id = $2 AND g.can_read)
            AND ($3::timestamptz IS NULL
                 OR (c.created_at, c.id) < ($3, $4))
          ORDER BY c.created_at DESC, c.id DESC
          LIMIT $5",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn mail_account_exists(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let n: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM mail_accounts WHERE workspace_id = $1 AND id = $2")
            .bind(tx.workspace_id())
            .bind(id)
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)?;
    Ok(n.is_some())
}

/// Recompute `campaigns.status` from its launches' states (best-effort
/// mirror; the launch rows are the source of truth). Only touches status +
/// version; never rewrites owner/name.
pub async fn sync_campaign_status(tx: &mut Tx<'_>, campaign_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE campaigns c
            SET status = COALESCE((
                    SELECT CASE
                        WHEN bool_or(l.status = 'running') THEN 'active'
                        WHEN bool_or(l.status = 'paused') THEN 'paused'
                        WHEN bool_or(l.status IN ('building','frozen','awaiting_approval','approved'))
                            THEN 'awaiting_approval'
                        WHEN bool_or(l.status = 'completed') THEN 'completed'
                        WHEN count(*) > 0 THEN 'canceled'
                        ELSE NULL END
                      FROM campaign_revisions r
                      JOIN campaign_launches l ON l.workspace_id = r.workspace_id
                                            AND l.revision_id = r.id
                     WHERE r.workspace_id = c.workspace_id
                       AND r.campaign_id = c.id), c.status),
                version = version + 1,
                updated_at = now()
          WHERE c.workspace_id = $1 AND c.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// -------------------------------------------------------------- revisions

pub async fn next_revision_no(tx: &mut Tx<'_>, campaign_id: Uuid) -> Result<i32, AppError> {
    let n: i32 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(revision_no), 0) + 1 FROM campaign_revisions
          WHERE workspace_id = $1 AND campaign_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n)
}

/// `campaign_revisions.purpose` defaults to 'marketing' (DDL CHECK); the
/// composite FK (workspace_id, delivery_channel_id, purpose) →
/// delivery_channels requires the channel to be purpose='marketing' —
/// callers validate `ChannelRow.purpose` first for a clean 422.
#[allow(clippy::too_many_arguments)]
pub async fn insert_revision(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    mail_account_id: Uuid,
    delivery_channel_id: Uuid,
    revision_no: i32,
    timezone: &str,
    schedule_rules: &serde_json::Value,
    stop_on_reply: bool,
    stop_on_booking: bool,
    created_by: Uuid,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO campaign_revisions
            (workspace_id, campaign_id, mail_account_id, delivery_channel_id,
             revision_no, timezone, schedule_rules, stop_on_reply, stop_on_booking, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .bind(mail_account_id)
    .bind(delivery_channel_id)
    .bind(revision_no)
    .bind(timezone)
    .bind(schedule_rules)
    .bind(stop_on_reply)
    .bind(stop_on_booking)
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_revision(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<RevisionRow>, AppError> {
    sqlx::query_as::<_, RevisionRow>(
        "SELECT id, campaign_id, mail_account_id, delivery_channel_id, revision_no,
                status, timezone, schedule_rules, stop_on_reply, stop_on_booking,
                created_by, frozen_at, created_at
           FROM campaign_revisions WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_revision_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<RevisionRow>, AppError> {
    sqlx::query_as::<_, RevisionRow>(
        "SELECT id, campaign_id, mail_account_id, delivery_channel_id, revision_no,
                status, timezone, schedule_rules, stop_on_reply, stop_on_booking,
                created_by, frozen_at, created_at
           FROM campaign_revisions WHERE workspace_id = $1 AND id = $2
           FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// draft → frozen in place (A.6.2). False when already frozen / missing.
pub async fn freeze_revision(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE campaign_revisions SET status = 'frozen', frozen_at = now()
          WHERE workspace_id = $1 AND id = $2 AND status = 'draft'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

pub async fn list_revisions(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
) -> Result<Vec<RevisionWithCount>, AppError> {
    sqlx::query_as::<_, RevisionWithCount>(
        "SELECT r.id, r.campaign_id, r.mail_account_id, r.delivery_channel_id, r.revision_no,
                r.status, r.timezone, r.schedule_rules, r.stop_on_reply, r.stop_on_booking,
                r.created_by, r.frozen_at, r.created_at,
                (SELECT count(*) FROM campaign_steps s
                  WHERE s.workspace_id = r.workspace_id AND s.revision_id = r.id) AS step_count
           FROM campaign_revisions r
          WHERE r.workspace_id = $1 AND r.campaign_id = $2
          ORDER BY r.revision_no DESC",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------------------------ steps

pub async fn insert_step(
    tx: &mut Tx<'_>,
    revision_id: Uuid,
    template_version_id: Uuid,
    step_order: i32,
    delay_hours: i32,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO campaign_steps (workspace_id, revision_id, template_version_id, step_order, delay_hours)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(revision_id)
    .bind(template_version_id)
    .bind(step_order)
    .bind(delay_hours)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn list_steps(tx: &mut Tx<'_>, revision_id: Uuid) -> Result<Vec<StepRow>, AppError> {
    sqlx::query_as::<_, StepRow>(
        "SELECT id, revision_id, template_version_id, step_order, delay_hours
           FROM campaign_steps
          WHERE workspace_id = $1 AND revision_id = $2
          ORDER BY step_order",
    )
    .bind(tx.workspace_id())
    .bind(revision_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The step with `step_order = $3` of a revision (for next-step scheduling).
pub async fn step_at_order(
    tx: &mut Tx<'_>,
    revision_id: Uuid,
    step_order: i32,
) -> Result<Option<StepRow>, AppError> {
    sqlx::query_as::<_, StepRow>(
        "SELECT id, revision_id, template_version_id, step_order, delay_hours
           FROM campaign_steps
          WHERE workspace_id = $1 AND revision_id = $2 AND step_order = $3",
    )
    .bind(tx.workspace_id())
    .bind(revision_id)
    .bind(step_order)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_template_version(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<TemplateVersionRow>, AppError> {
    sqlx::query_as::<_, TemplateVersionRow>(
        "SELECT id, subject, body_text, body_html, variable_schema
           FROM template_versions WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ---------------------------------------------------------------- channels

pub async fn get_channel(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ChannelRow>, AppError> {
    sqlx::query_as::<_, ChannelRow>(
        "SELECT id, mail_account_id, sender_identity_id, provider, purpose,
                policy_status, enabled, internal_daily_cap
           FROM delivery_channels WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// (address, display_name) of a verified/pending sender identity.
pub async fn get_sender_identity(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<(String, String)>, AppError> {
    sqlx::query_as(
        "SELECT address, display_name FROM sender_identities
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Workspace emergency stop flag (§16 / workspaces.emergency_stop).
pub async fn emergency_stop_active(tx: &mut Tx<'_>) -> Result<bool, AppError> {
    let stop: bool = sqlx::query_scalar("SELECT emergency_stop FROM workspaces WHERE id = $1")
        .bind(tx.workspace_id())
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)?;
    Ok(stop)
}

/// Global launch-blocking reasons for a channel (B.3.6/B.3.7 codes).
/// Returns B.5 code names; empty = channel ready.
pub fn channel_blocking_reasons(channel: &ChannelRow, emergency_stop: bool) -> Vec<String> {
    let mut out = Vec::new();
    if emergency_stop {
        out.push("WORKSPACE_EMERGENCY_STOP".to_string());
    }
    if channel.policy_status == "blocked" {
        out.push("PROVIDER_POLICY_BLOCKED".to_string());
    } else if channel.policy_status != "allowed" || !channel.enabled {
        out.push("PROVIDER_POLICY_UNVERIFIED".to_string());
    }
    out
}

// ---------------------------------------------------------------- launches

pub async fn insert_launch(
    tx: &mut Tx<'_>,
    revision_id: Uuid,
    delivery_channel_id: Uuid,
    requested_by: Uuid,
    expires_at: DateTime<Utc>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO campaign_launches
            (workspace_id, revision_id, delivery_channel_id, requested_by, expires_at)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(revision_id)
    .bind(delivery_channel_id)
    .bind(requested_by)
    .bind(expires_at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_launch(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<LaunchRow>, AppError> {
    sqlx::query_as::<_, LaunchRow>(
        "SELECT id, revision_id, delivery_channel_id, requested_by, status,
                manifest_hash, frozen_at, expires_at, version, created_at
           FROM campaign_launches WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_launch_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<LaunchRow>, AppError> {
    sqlx::query_as::<_, LaunchRow>(
        "SELECT id, revision_id, delivery_channel_id, requested_by, status,
                manifest_hash, frozen_at, expires_at, version, created_at
           FROM campaign_launches WHERE workspace_id = $1 AND id = $2
           FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Guarded status transition: only when the current state legally moves to
/// `to` (callers use `domain::campaigns::launch_status::can_transition`)
/// and `expected_version` matches when provided. Returns rows_affected.
pub async fn transition_launch(
    tx: &mut Tx<'_>,
    id: Uuid,
    to: &str,
    expected_version: Option<i64>,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE campaign_launches
            SET status = $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2
            AND ($4::bigint IS NULL OR version = $4)",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(to)
    .bind(expected_version)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// building → frozen with the computed manifest (A.6.2). All structural
/// columns are frozen by this update; the 0003 trigger makes them immutable
/// from here on.
pub async fn freeze_launch_row(
    tx: &mut Tx<'_>,
    id: Uuid,
    manifest_hash: &[u8],
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE campaign_launches
            SET status = 'frozen', manifest_hash = $3, frozen_at = now(), version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND status = 'building'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(manifest_hash)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

pub async fn list_launches_for_campaign(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
) -> Result<Vec<LaunchRow>, AppError> {
    sqlx::query_as::<_, LaunchRow>(
        "SELECT l.id, l.revision_id, l.delivery_channel_id, l.requested_by, l.status,
                l.manifest_hash, l.frozen_at, l.expires_at, l.version, l.created_at
           FROM campaign_launches l
          WHERE l.workspace_id = $1
            AND EXISTS (SELECT 1 FROM campaign_revisions r
                         WHERE r.workspace_id = l.workspace_id
                           AND r.id = l.revision_id AND r.campaign_id = $2)
          ORDER BY l.created_at DESC, l.id DESC",
    )
    .bind(tx.workspace_id())
    .bind(campaign_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------------------ enrollments

/// Insert one enrollment row (called while the launch is still `building`;
/// the frozen-enrollment guard rejects inserts afterwards).
pub async fn insert_enrollment(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
    revision_id: Uuid,
    email_address_id: Uuid,
    contact_id: Option<Uuid>,
    status: &str,
    stop_reason: Option<&str>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO campaign_enrollments
            (workspace_id, launch_id, revision_id, email_address_id, contact_id, status, stop_reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .bind(revision_id)
    .bind(email_address_id)
    .bind(contact_id)
    .bind(status)
    .bind(stop_reason)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn list_enrollments(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<Vec<EnrollmentRow>, AppError> {
    sqlx::query_as::<_, EnrollmentRow>(
        "SELECT id, launch_id, revision_id, email_address_id, contact_id,
                status, current_step_id, next_send_at, stop_reason, created_at
           FROM campaign_enrollments
          WHERE workspace_id = $1 AND launch_id = $2
          ORDER BY id",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn enrollment_counts(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<EnrollmentCounts, AppError> {
    let row: (i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT count(*),
                count(*) FILTER (WHERE status = 'pending'),
                count(*) FILTER (WHERE status = 'active'),
                count(*) FILTER (WHERE status = 'completed'),
                count(*) FILTER (WHERE status = 'stopped'),
                count(*) FILTER (WHERE status = 'suppressed')
           FROM campaign_enrollments
          WHERE workspace_id = $1 AND launch_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(EnrollmentCounts {
        total: row.0,
        pending: row.1,
        active: row.2,
        completed: row.3,
        stopped: row.4,
        suppressed: row.5,
    })
}

pub async fn get_enrollment_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<EnrollmentRow>, AppError> {
    sqlx::query_as::<_, EnrollmentRow>(
        "SELECT id, launch_id, revision_id, email_address_id, contact_id,
                status, current_step_id, next_send_at, stop_reason, created_at
           FROM campaign_enrollments
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Execution-state-only update — the columns the frozen-enrollment guard
/// allows to change (status, current_step_id, next_send_at, stop_reason).
pub async fn set_enrollment_state(
    tx: &mut Tx<'_>,
    id: Uuid,
    status: &str,
    current_step_id: Option<Uuid>,
    next_send_at: Option<DateTime<Utc>>,
    stop_reason: Option<&str>,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE campaign_enrollments
            SET status = $3, current_step_id = $4, next_send_at = $5, stop_reason = $6
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(status)
    .bind(current_step_id)
    .bind(next_send_at)
    .bind(stop_reason)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// pending → active only when no *other* active enrollment exists for the
/// address (RECIPIENT_ACTIVE_ELSEWHERE; `active_campaign_address_uq` is the
/// ultimate guard). Returns false on conflict or non-pending state.
pub async fn activate_enrollment(
    tx: &mut Tx<'_>,
    id: Uuid,
    first_step_id: Uuid,
    next_send_at: DateTime<Utc>,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE campaign_enrollments e
            SET status = 'active', current_step_id = $3, next_send_at = $4
          WHERE e.workspace_id = $1 AND e.id = $2 AND e.status = 'pending'
            AND NOT EXISTS (SELECT 1 FROM campaign_enrollments x
                             WHERE x.workspace_id = e.workspace_id
                               AND x.email_address_id = e.email_address_id
                               AND x.status = 'active' AND x.id <> e.id)",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(first_step_id)
    .bind(next_send_at)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Pause/stop enrollments of an address per §8.4 rules. `flag` is the
/// revision flag the reason is governed by ("stop_on_reply" /
/// "stop_on_booking") or None for unconditional stops. Returns enrollment
/// ids that transitioned (for outbound cancellation).
pub async fn stop_enrollments_for_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    flag: Option<&str>,
    new_status: &str,
    stop_reason: &str,
    from_statuses: &[&str],
) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar(
        "UPDATE campaign_enrollments e
            SET status = $4, stop_reason = $5, next_send_at = NULL
          FROM campaign_revisions r
          WHERE e.workspace_id = $1 AND e.email_address_id = $2
            AND e.revision_id = r.id AND r.workspace_id = e.workspace_id
            AND e.status = ANY($6)
            AND ($3::text IS NULL
                 OR ($3 = 'stop_on_reply' AND r.stop_on_reply)
                 OR ($3 = 'stop_on_booking' AND r.stop_on_booking))
          RETURNING e.id",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(flag)
    .bind(new_status)
    .bind(stop_reason)
    .bind(from_statuses)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// All non-terminal enrollments of a launch (pause/cancel fans out).
pub async fn stop_launch_enrollments(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
    stop_reason: &str,
) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar(
        "UPDATE campaign_enrollments
            SET status = 'stopped', stop_reason = $3, next_send_at = NULL
          WHERE workspace_id = $1 AND launch_id = $2
            AND status IN ('pending','active')
          RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .bind(stop_reason)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Deterministic contact link for an enrollment snapshot (T31: one address
/// on N contacts → exactly one enrollment; the contact link is informational
/// only — the dedupe key is `email_address_id`).
pub async fn first_contact_for_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT contact_id FROM contact_emails
          WHERE workspace_id = $1 AND email_address_id = $2
          ORDER BY contact_id LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// (contact name, company name) for render variables — system-resolved, so
/// user-supplied variables may never override them (§8.1).
pub async fn contact_and_company_names(
    tx: &mut Tx<'_>,
    contact_id: Uuid,
) -> Result<Option<(String, Option<String>)>, AppError> {
    sqlx::query_as(
        "SELECT c.name, co.name
           FROM contacts c
           LEFT JOIN companies co ON co.workspace_id = c.workspace_id
                                 AND co.id = c.company_id
          WHERE c.workspace_id = $1 AND c.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------------------- eligibility

/// (address_original, verification_status) for a recipient id.
pub async fn get_email_address(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<(String, String)>, AppError> {
    sqlx::query_as(
        "SELECT address_original, verification_status FROM email_addresses
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Any valid marketing communication basis for the address. Campaign sends
/// are purpose='marketing' (composite-FK requirement); jurisdiction is a
/// workspace policy concern handled by the §8.4 send-time chain.
pub async fn valid_marketing_basis(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = $2
            AND purpose = 'marketing' AND status = 'valid'
            AND (valid_until IS NULL OR valid_until > now())
          ORDER BY created_at DESC, id
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// True when the address already has an `active` enrollment anywhere in the
/// workspace (RECIPIENT_ACTIVE_ELSEWHERE; §8.4 "활성 마케팅 캠페인 동시 참여
/// 금지").
pub async fn recipient_active_elsewhere(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<bool, AppError> {
    let n: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM campaign_enrollments
          WHERE workspace_id = $1 AND email_address_id = $2 AND status = 'active'
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n.is_some())
}

// ------------------------------------------------- outbound step snapshots

/// Insert the frozen per-(enrollment, step) outbound snapshot. `payload_hash`
/// is the frozen content hash; status starts 'draft' — the scheduler
/// transitions it to 'queued' with `not_before` when its send time arrives.
#[allow(clippy::too_many_arguments)]
pub async fn insert_step_outbound(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    delivery_channel_id: Uuid,
    sender_identity_id: Uuid,
    to_email_address_id: Uuid,
    communication_basis_id: Option<Uuid>,
    launch_id: Uuid,
    revision_id: Uuid,
    enrollment_id: Uuid,
    step_id: Uuid,
    requested_by: Uuid,
    from_address: &str,
    to_address: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
    payload_hash: &[u8],
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO outbound_messages
            (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
             to_email_address_id, communication_basis_id,
             launch_id, revision_id, enrollment_id, step_id,
             requested_by, purpose,
             from_address_snapshot, to_address_snapshot, subject, body_text, body_html,
             payload_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'marketing',
                 $12, $13, $14, $15, $16, $17)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(delivery_channel_id)
    .bind(sender_identity_id)
    .bind(to_email_address_id)
    .bind(communication_basis_id)
    .bind(launch_id)
    .bind(revision_id)
    .bind(enrollment_id)
    .bind(step_id)
    .bind(requested_by)
    .bind(from_address)
    .bind(to_address)
    .bind(subject)
    .bind(body_text)
    .bind(body_html)
    .bind(payload_hash)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The frozen outbound for (enrollment, step) — scheduling/idempotency.
pub async fn outbound_for_enrollment_step(
    tx: &mut Tx<'_>,
    enrollment_id: Uuid,
    step_id: Uuid,
) -> Result<Option<StepOutboundRow>, AppError> {
    sqlx::query_as::<_, StepOutboundRow>(
        "SELECT id, enrollment_id, step_id, to_address_snapshot, subject, body_text, body_html,
                status, not_before, payload_hash, created_at
           FROM outbound_messages
          WHERE workspace_id = $1 AND enrollment_id = $2 AND step_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(enrollment_id)
    .bind(step_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Cursor page of a launch's step snapshots, joined with step_order.
pub async fn list_launch_outbounds(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<StepOutboundWithOrder>, AppError> {
    sqlx::query_as::<_, StepOutboundWithOrder>(
        "SELECT o.id, o.enrollment_id, o.step_id, o.to_address_snapshot, o.subject,
                o.body_text, o.body_html,
                o.status, o.not_before, o.payload_hash, o.created_at,
                s.step_order
           FROM outbound_messages o
           JOIN campaign_steps s ON s.workspace_id = o.workspace_id
                                AND s.id = o.step_id
          WHERE o.workspace_id = $1 AND o.launch_id = $2
            AND ($3::timestamptz IS NULL OR (o.created_at, o.id) < ($3, $4))
          ORDER BY o.created_at DESC, o.id DESC
          LIMIT $5",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// draft/queued → queued with a concrete send time (scheduler path).
/// Immutable-payload trigger allows status/not_before/version only.
pub async fn queue_outbound_at(
    tx: &mut Tx<'_>,
    id: Uuid,
    not_before: DateTime<Utc>,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE outbound_messages
            SET status = 'queued', not_before = $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND status IN ('draft','queued')",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(not_before)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Cancel not-yet-submitted outbound rows of the given enrollments
/// (draft/queued/claimed → canceled; submitting+ are in-flight and tracked
/// separately per §8.6).
pub async fn cancel_pending_outbounds_for_enrollments(
    tx: &mut Tx<'_>,
    enrollment_ids: &[Uuid],
) -> Result<u64, AppError> {
    if enrollment_ids.is_empty() {
        return Ok(0);
    }
    let n = sqlx::query(
        "UPDATE outbound_messages
            SET status = 'canceled', version = version + 1
          WHERE workspace_id = $1 AND enrollment_id = ANY($2)
            AND status IN ('draft','queued','claimed')",
    )
    .bind(tx.workspace_id())
    .bind(enrollment_ids)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// Cancel all not-yet-submitted outbound rows of a launch (pause/cancel).
pub async fn cancel_pending_outbounds_for_launch(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE outbound_messages
            SET status = 'canceled', version = version + 1
          WHERE workspace_id = $1 AND launch_id = $2
            AND status IN ('draft','queued','claimed')",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// Outbound ids of not-yet-submitted rows for the given enrollments — for
/// dispatch-job dedupe-key cancellation.
pub async fn pending_outbound_ids_for_enrollments(
    tx: &mut Tx<'_>,
    enrollment_ids: &[Uuid],
) -> Result<Vec<Uuid>, AppError> {
    if enrollment_ids.is_empty() {
        return Ok(vec![]);
    }
    sqlx::query_scalar(
        "SELECT id FROM outbound_messages
          WHERE workspace_id = $1 AND enrollment_id = ANY($2)
            AND status IN ('draft','queued','claimed')",
    )
    .bind(tx.workspace_id())
    .bind(enrollment_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Outbound ids of not-yet-submitted rows for a launch — for dispatch-job
/// dedupe-key cancellation.
pub async fn pending_outbound_ids_for_launch(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM outbound_messages
          WHERE workspace_id = $1 AND launch_id = $2
            AND status IN ('draft','queued','claimed')",
    )
    .bind(tx.workspace_id())
    .bind(launch_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Lookup an outbound row's owning enrollment (outbound.failed path).
pub async fn get_outbound_enrollment(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT enrollment_id FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2 AND enrollment_id IS NOT NULL",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `accepted_at` of an outbound message (next-step delay anchor).
pub async fn outbound_accepted_at(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
) -> Result<Option<DateTime<Utc>>, AppError> {
    sqlx::query_scalar(
        "SELECT accepted_at FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------------------------- misc

/// Resolve the `from` participant's email_address_id for a message when the
/// event payload lacks `from_email_address_id`.
pub async fn message_from_address_id(
    tx: &mut Tx<'_>,
    email_message_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT email_address_id FROM message_participants
          WHERE workspace_id = $1 AND email_message_id = $2
            AND role = 'from' AND email_address_id IS NOT NULL
          ORDER BY id LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The workspace system principal (worker audit actor).
pub async fn system_principal_id(tx: &mut Tx<'_>) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM principals WHERE workspace_id = $1 AND kind = 'system'
          ORDER BY created_at, id LIMIT 1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Attribution persisted by the API when it enqueues a launch freeze. The
/// worker uses this row plus the launch requester to reconstruct the original
/// authorization context and reject stale/revoked requesters before links or
/// frozen content are created.
pub async fn job_requester(
    tx: &mut Tx<'_>,
    job_id: Uuid,
) -> Result<Option<(Option<Uuid>, Option<Uuid>)>, AppError> {
    sqlx::query_as(
        "SELECT requested_by, source_credential_id FROM jobs
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Enqueue-or-return-existing on (kind, dedupe_key). TODO(rebase): the
/// foundation's `jobs::enqueue` will do this natively — this local helper
/// exists until that lands (wave-b-common "Framework fixes").
pub async fn enqueue_dedup(
    tx: &mut Tx<'_>,
    kind: &str,
    payload: serde_json::Value,
    available_at: DateTime<Utc>,
    dedupe_key: &str,
) -> Result<Uuid, AppError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM jobs
          WHERE workspace_id = $1 AND kind = $2 AND dedupe_key = $3",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(dedupe_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if let Some(id) = existing {
        return Ok(id);
    }
    match jobs::enqueue(tx, kind, 1, payload, available_at, Some(dedupe_key)).await {
        Ok(id) => Ok(id),
        Err(_) => {
            // Lost an insert race — the other tx won; fetch its job.
            sqlx::query_scalar(
                "SELECT id FROM jobs
                  WHERE workspace_id = $1 AND kind = $2 AND dedupe_key = $3",
            )
            .bind(tx.workspace_id())
            .bind(kind)
            .bind(dedupe_key)
            .fetch_one(tx.conn())
            .await
            .map_err(map_sqlx)
        }
    }
}

/// Cancel still-queued/retrying jobs by dedupe key (pause/cancel/revoke
/// side effects). Never touches running/unknown/completed jobs.
pub async fn cancel_queued_jobs(
    tx: &mut Tx<'_>,
    kind: &str,
    dedupe_keys: &[String],
) -> Result<u64, AppError> {
    if dedupe_keys.is_empty() {
        return Ok(0);
    }
    let n = sqlx::query(
        "UPDATE jobs SET state = 'canceled', finished_at = now()
          WHERE workspace_id = $1 AND kind = $2
            AND dedupe_key = ANY($3) AND state IN ('queued','retry_wait')",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(dedupe_keys)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// Dispatch dedupe key for `sending.dispatch_outbound` (integration contract).
pub fn dispatch_dedupe_key(outbound_message_id: Uuid) -> String {
    format!("dispatch:{outbound_message_id}")
}
