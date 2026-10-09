//! operations module repository (spec §6 — owned by the module worker).
//!
//! Reads the shared operational tables (`jobs`, `audit_logs`,
//! `outbox_events`, `workspaces`, `mail_accounts`, `outbound_messages`,
//! `email_messages`, `bookings`, `lead_proposals`) for the REST ops
//! surface: job get/retry, audit-log query, dashboard KPIs, alerts view and
//! the T30 send-workers restore lock. Writes are limited to the retry
//! re-queue (`jobs` UPDATE) and the workspace settings flag release —
//! both inside the caller's business transaction next to `audit::record`.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;
use crate::jobs::Job;

// ------------------------------------------------------------------ jobs

/// `GET /jobs/{id}` row. Workspace scoping comes from the tx (RLS).
pub async fn get_job(tx: &mut Tx<'_>, job_id: Uuid) -> Result<Option<Job>, AppError> {
    sqlx::query_as::<_, Job>(
        "SELECT id, workspace_id, kind, payload_version, payload, dedupe_key,
                requested_by, source_credential_id, state, available_at,
                lease_owner, lease_token, lease_expires_at, attempt_count,
                last_error_code, finished_at, external_effect, created_at
           FROM jobs
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, Default)]
pub struct JobListFilter {
    pub state: Option<String>,
    pub kind: Option<String>,
    /// `None` for Admin/Manager workspace view; otherwise own requested jobs.
    pub requested_by: Option<Uuid>,
}

/// Signed-cursor keyset page ordered by `(created_at DESC, id DESC)`.
pub async fn list_jobs(
    tx: &mut Tx<'_>,
    filter: &JobListFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<Job>, AppError> {
    let mut query = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT id, workspace_id, kind, payload_version, payload, dedupe_key,
                requested_by, source_credential_id, state, available_at,
                lease_owner, lease_token, lease_expires_at, attempt_count,
                last_error_code, finished_at, external_effect, created_at
           FROM jobs WHERE workspace_id = ",
    );
    query.push_bind(tx.workspace_id());
    if let Some(requested_by) = filter.requested_by {
        query.push(" AND requested_by = ");
        query.push_bind(requested_by);
    }
    if let Some(state) = &filter.state {
        query.push(" AND state = ");
        query.push_bind(state.clone());
    }
    if let Some(kind) = &filter.kind {
        query.push(" AND kind = ");
        query.push_bind(kind.clone());
    }
    if let Some((created_at, id)) = after {
        query.push(" AND (created_at < ");
        query.push_bind(created_at);
        query.push(" OR (created_at = ");
        query.push_bind(created_at);
        query.push(" AND id < ");
        query.push_bind(id);
        query.push("))");
    }
    query.push(" ORDER BY created_at DESC, id DESC LIMIT ");
    query.push_bind(limit);
    query
        .build_query_as::<Job>()
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

/// `POST /jobs/{id}/retry-requests` — atomic dead/retry_wait → queued
/// transition (§16.4). `attempt_count` resets so the operator-driven retry
/// gets a fresh bounded cycle (the auto policy already consumed its five).
/// Returns the updated row, or None when the state was not retryable —
/// callers re-read to distinguish `unknown` (B.5 code) from other states.
pub async fn requeue_job_for_retry(tx: &mut Tx<'_>, job_id: Uuid) -> Result<Option<Job>, AppError> {
    sqlx::query_as::<_, Job>(
        "UPDATE jobs
            SET state = 'queued',
                available_at = now(),
                lease_owner = NULL,
                lease_token = NULL,
                lease_expires_at = NULL,
                finished_at = NULL,
                attempt_count = 0
          WHERE workspace_id = $1 AND id = $2
            AND state IN ('dead','retry_wait')
        RETURNING id, workspace_id, kind, payload_version, payload, dedupe_key,
                  requested_by, source_credential_id, state, available_at,
                  lease_owner, lease_token, lease_expires_at, attempt_count,
                  last_error_code, finished_at, external_effect, created_at",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// -------------------------------------------------------------- audit log

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuditLogRow {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    /// Keyset anchor for the B.1.5 `(created_at DESC, id DESC)` sort —
    /// internal only, never serialized to the wire DTO.
    pub created_at: DateTime<Utc>,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
    pub requested_by: Option<Uuid>,
    pub approved_by: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub request_id: String,
    pub trace_id: Option<String>,
    pub details: serde_json::Value,
}

/// Documented filter allowlist for `GET /audit-logs` (§12.2 "필터는 문서화된
/// 필드 allowlist"). All fields AND-combine.
#[derive(Debug, Clone, Default)]
pub struct AuditLogFilter {
    pub action: Option<String>,
    pub target_type: Option<String>,
    pub target_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
    pub occurred_from: Option<DateTime<Utc>>,
    pub occurred_to: Option<DateTime<Utc>>,
}

/// Row-level visibility class for the audit query — the caller derives it
/// from `Ctx` (§5.3 "전체 감사·보안 로그" row).
#[derive(Debug, Clone)]
pub enum AuditVisibility {
    /// Admin, or a credential holding `operations.read`: every row.
    All,
    /// Manager "업무 범위": hides the auth/credential/principal security
    /// action family (list applied in `audit_page`).
    BusinessOnly,
    /// Member "본인 활동": rows the principal acted in or requested.
    OwnActivity(Uuid),
    /// Agent "본인 run 일부": rows the agent acted in, or that target one of
    /// its own agent_runs.
    AgentRuns { principal_id: Uuid },
}

/// Security-family action prefixes hidden from Manager scope. Stable audit
/// action keys only (the same strings `audit::record` callers use).
const SECURITY_ACTION_PREFIXES: &[&str] = &[
    "user.login",
    "user.logout",
    "session.",
    "credential.",
    "principal.",
];

/// Keyset page of audit rows ordered by `(created_at DESC, id DESC)` —
/// the B.1.5 default sort. `after` is the exclusive keyset bound.
pub async fn audit_page(
    tx: &mut Tx<'_>,
    filter: &AuditLogFilter,
    visibility: &AuditVisibility,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<AuditLogRow>, AppError> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(
        "SELECT id, occurred_at, created_at, action, target_type, target_id,
                actor_principal_id, requested_by, approved_by, job_id,
                request_id, trace_id, details
           FROM audit_logs
          WHERE workspace_id = ",
    );
    qb.push_bind(tx.workspace_id());
    match visibility {
        AuditVisibility::All => {}
        AuditVisibility::BusinessOnly => {
            for prefix in SECURITY_ACTION_PREFIXES {
                qb.push(" AND action NOT LIKE ");
                qb.push_bind(format!("{prefix}%"));
            }
        }
        AuditVisibility::OwnActivity(pid) => {
            qb.push(" AND (actor_principal_id = ");
            qb.push_bind(*pid);
            qb.push(" OR requested_by = ");
            qb.push_bind(*pid);
            qb.push(")");
        }
        AuditVisibility::AgentRuns { principal_id } => {
            qb.push(" AND (actor_principal_id = ");
            qb.push_bind(*principal_id);
            qb.push(
                " OR (target_type = 'agent_run' AND target_id IN (
                        SELECT id FROM agent_runs
                         WHERE workspace_id = ",
            );
            qb.push_bind(tx.workspace_id());
            qb.push(" AND principal_id = ");
            qb.push_bind(*principal_id);
            qb.push(")))");
        }
    }
    if let Some(action) = &filter.action {
        qb.push(" AND action = ");
        qb.push_bind(action.clone());
    }
    if let Some(target_type) = &filter.target_type {
        qb.push(" AND target_type = ");
        qb.push_bind(target_type.clone());
    }
    if let Some(target_id) = filter.target_id {
        qb.push(" AND target_id = ");
        qb.push_bind(target_id);
    }
    if let Some(actor) = filter.actor_principal_id {
        qb.push(" AND actor_principal_id = ");
        qb.push_bind(actor);
    }
    if let Some(from) = filter.occurred_from {
        qb.push(" AND occurred_at >= ");
        qb.push_bind(from);
    }
    if let Some(to) = filter.occurred_to {
        qb.push(" AND occurred_at < ");
        qb.push_bind(to);
    }
    if let Some((created_at, id)) = after {
        qb.push(" AND (created_at < ");
        qb.push_bind(created_at);
        qb.push(" OR (created_at = ");
        qb.push_bind(created_at);
        qb.push(" AND id < ");
        qb.push_bind(id);
        qb.push("))");
    }
    qb.push(" ORDER BY created_at DESC, id DESC LIMIT ");
    qb.push_bind(limit);
    qb.build_query_as::<AuditLogRow>()
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

// --------------------------------------------------------------- dashboard

/// KPI window + optional "mine" restriction (requested_by on the contacting
/// outbound). `scope=mine` restricts the contacted-cohort to outbounds the
/// caller requested; cohort-derived KPIs inherit the restriction.
#[derive(Debug, Clone)]
pub struct KpiWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// `Some(pid)` = scope mine.
    pub requested_by: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, sqlx::FromRow)]
pub struct DashboardQueueCount {
    pub count: i64,
    pub oldest_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy)]
pub struct MailboxQueueCounts {
    pub accessible: bool,
    pub reauth_required: DashboardQueueCount,
    pub sync_lag: DashboardQueueCount,
}

pub async fn workspace_timezone(tx: &mut Tx<'_>) -> Result<String, AppError> {
    sqlx::query_scalar("SELECT default_timezone FROM workspaces WHERE id = $1")
        .bind(tx.workspace_id())
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)?
        .ok_or_else(|| AppError::not_found("workspace not found"))
}

/// Count open/pending threads with at least one inbound message, visible via
/// the same principal-specific `can_read` grant used by Inbox list queries.
pub async fn inbox_unhandled_queue(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    mine: bool,
) -> Result<Option<DashboardQueueCount>, AppError> {
    let row: (bool, i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "WITH accessible_mailboxes AS (
            SELECT DISTINCT mail_account_id FROM mail_account_grants
             WHERE workspace_id = $1 AND principal_id = $2 AND can_read
        )
        SELECT EXISTS (SELECT 1 FROM accessible_mailboxes),
               count(DISTINCT t.id), min(t.updated_at)
          FROM email_threads t
          JOIN accessible_mailboxes am ON am.mail_account_id = t.mail_account_id
         WHERE t.workspace_id = $1
           AND t.status IN ('open','pending')
           AND (NOT $3::boolean OR t.assigned_principal_id = $2)
           AND EXISTS (
                SELECT 1 FROM email_messages m
                 WHERE m.workspace_id = t.workspace_id AND m.thread_id = t.id
                   AND m.direction = 'inbound'
           )",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(mine)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row.0.then_some(DashboardQueueCount {
        count: row.1,
        oldest_at: row.2,
    }))
}

/// Mailbox health counters are observable only where the caller has Inbox
/// read access. Five minutes follows the documented inbox polling target.
pub async fn mailbox_health_queues(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<MailboxQueueCounts, AppError> {
    let (accessible, reauth_count, reauth_oldest, sync_count, sync_oldest): (
        bool,
        i64,
        Option<DateTime<Utc>>,
        i64,
        Option<DateTime<Utc>>,
    ) = sqlx::query_as(
        "WITH accessible AS (
            SELECT DISTINCT ma.id, ma.status, ma.last_synced_at, ma.created_at
              FROM mail_accounts ma
              JOIN mail_account_grants g
                ON g.workspace_id = ma.workspace_id
               AND g.mail_account_id = ma.id
               AND g.principal_id = $2
               AND g.can_read
             WHERE ma.workspace_id = $1
        )
        SELECT count(*) > 0,
               count(*) FILTER (WHERE status = 'reauth_required'),
               min(COALESCE(last_synced_at, created_at))
                   FILTER (WHERE status = 'reauth_required'),
               count(*) FILTER (
                   WHERE status = 'active'
                     AND (last_synced_at IS NULL OR last_synced_at < now() - interval '5 minutes')
               ),
               min(COALESCE(last_synced_at, created_at)) FILTER (
                   WHERE status = 'active'
                     AND (last_synced_at IS NULL OR last_synced_at < now() - interval '5 minutes')
               )
          FROM accessible",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(MailboxQueueCounts {
        accessible,
        reauth_required: DashboardQueueCount {
            count: reauth_count,
            oldest_at: reauth_oldest,
        },
        sync_lag: DashboardQueueCount {
            count: sync_count,
            oldest_at: sync_oldest,
        },
    })
}

/// Unknown outbounds visible through `mail.read` mailbox grants.
pub async fn unknown_outbound_queue(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    requested_by: Option<Uuid>,
) -> Result<Option<DashboardQueueCount>, AppError> {
    let row: (bool, i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "WITH accessible_mailboxes AS (
            SELECT DISTINCT mail_account_id FROM mail_account_grants
             WHERE workspace_id = $1 AND principal_id = $2 AND can_read
        )
        SELECT EXISTS (SELECT 1 FROM accessible_mailboxes),
               count(om.id), min(om.created_at)
          FROM outbound_messages om
          JOIN accessible_mailboxes am ON am.mail_account_id = om.mail_account_id
         WHERE om.workspace_id = $1 AND om.status = 'unknown'
           AND ($3::uuid IS NULL OR om.requested_by = $3)",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row.0.then_some(DashboardQueueCount {
        count: row.1,
        oldest_at: row.2,
    }))
}

pub async fn dead_jobs_queue(
    tx: &mut Tx<'_>,
    requested_by: Option<Uuid>,
) -> Result<DashboardQueueCount, AppError> {
    sqlx::query_as(
        "SELECT count(*) AS count, min(created_at) AS oldest_at FROM jobs
          WHERE workspace_id = $1 AND state = 'dead'
            AND ($2::uuid IS NULL OR requested_by = $2)",
    )
    .bind(tx.workspace_id())
    .bind(requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn outbox_backlog_queue(tx: &mut Tx<'_>) -> Result<DashboardQueueCount, AppError> {
    sqlx::query_as(
        "SELECT count(*) AS count, min(created_at) AS oldest_at FROM outbox_events
          WHERE workspace_id = $1 AND published_at IS NULL",
    )
    .bind(tx.workspace_id())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn today_bookings_queue(
    tx: &mut Tx<'_>,
    host_principal_id: Option<Uuid>,
    day_start: DateTime<Utc>,
    next_day_start: DateTime<Utc>,
) -> Result<DashboardQueueCount, AppError> {
    sqlx::query_as(
        "SELECT count(*) AS count, min(starts_at) AS oldest_at FROM bookings
          WHERE workspace_id = $1
            AND ($2::uuid IS NULL OR host_principal_id = $2)
            AND starts_at >= $3 AND starts_at < $4
            AND status IN ('held','confirming','confirmed','canceling','unknown')",
    )
    .bind(tx.workspace_id())
    .bind(host_principal_id)
    .bind(day_start)
    .bind(next_day_start)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Count visible pending approval requests that are live, not self-requested,
/// and still in the target state the approval path accepts. Mail-bound
/// requests additionally require current read/approve grants and an enabled,
/// policy-allowed channel with workspace submissions enabled.
pub async fn actionable_pending_approvals_queue(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    can_approve_mail: bool,
) -> Result<DashboardQueueCount, AppError> {
    sqlx::query_as(
        "WITH actionable AS (
            SELECT ar.id, ar.created_at
              FROM approval_requests ar
              JOIN campaign_launches l
                ON l.workspace_id = ar.workspace_id AND l.id = ar.launch_id
              JOIN delivery_channels dc
                ON dc.workspace_id = l.workspace_id AND dc.id = l.delivery_channel_id
              JOIN campaign_revisions cr
                ON cr.workspace_id = l.workspace_id AND cr.id = l.revision_id
              JOIN mail_account_grants g
                ON g.workspace_id = cr.workspace_id
               AND g.mail_account_id = cr.mail_account_id
               AND g.principal_id = $2 AND g.can_read AND g.can_approve
              JOIN workspaces w ON w.id = ar.workspace_id
             WHERE ar.workspace_id = $1 AND ar.action_type = 'campaign_launch'
               AND ar.status = 'pending' AND ar.expires_at > now()
               AND ar.requested_by <> $2
               AND l.status IN ('frozen','awaiting_approval')
               AND l.manifest_hash = ar.snapshot_hash
               AND dc.enabled AND dc.policy_status = 'allowed'
               AND NOT w.emergency_stop AND $3::boolean
            UNION ALL
            SELECT ar.id, ar.created_at
              FROM approval_requests ar
              JOIN outbound_messages om
                ON om.workspace_id = ar.workspace_id AND om.id = ar.outbound_message_id
              JOIN delivery_channels dc
                ON dc.workspace_id = om.workspace_id AND dc.id = om.delivery_channel_id
              JOIN mail_account_grants g
                ON g.workspace_id = om.workspace_id
               AND g.mail_account_id = om.mail_account_id
               AND g.principal_id = $2 AND g.can_read AND g.can_approve
              JOIN workspaces w ON w.id = ar.workspace_id
             WHERE ar.workspace_id = $1 AND ar.action_type = 'message_submit'
               AND ar.status = 'pending' AND ar.expires_at > now()
               AND ar.requested_by <> $2 AND om.status = 'draft'
               AND om.payload_hash = ar.snapshot_hash
               AND dc.enabled AND dc.policy_status = 'allowed'
               AND NOT w.emergency_stop AND $3::boolean
            UNION ALL
            SELECT ar.id, ar.created_at
              FROM approval_requests ar
              JOIN booking_changes bc
                ON bc.workspace_id = ar.workspace_id AND bc.id = ar.booking_change_id
             WHERE ar.workspace_id = $1 AND ar.action_type = 'booking_change'
               AND ar.status = 'pending' AND ar.expires_at > now()
               AND ar.requested_by <> $2 AND bc.status = 'proposed'
            UNION ALL
            SELECT ar.id, ar.created_at
              FROM approval_requests ar
              JOIN suppressions s
                ON s.workspace_id = ar.workspace_id AND s.id = ar.suppression_id
             WHERE ar.workspace_id = $1 AND ar.action_type = 'suppression_release'
               AND ar.status = 'pending' AND ar.expires_at > now()
               AND ar.requested_by <> $2 AND s.released_at IS NULL
        )
        SELECT count(*) AS count, min(created_at) AS oldest_at FROM actionable",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(can_approve_mail)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// 적격 접촉 기업 수 — distinct companies with a provider-accepted outbound
/// in the window (§2.4 row 1). Company attribution goes through
/// `to_email_address_id → contact_emails → contacts.company_id`; outbounds
/// to addresses not linked to a company cannot be attributed and are
/// excluded (documented in the dashboard use case).
pub async fn eligible_contacted_companies(tx: &mut Tx<'_>, w: &KpiWindow) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(DISTINCT c.company_id)
           FROM outbound_messages om
           JOIN contact_emails ce
             ON ce.workspace_id = om.workspace_id
            AND ce.email_address_id = om.to_email_address_id
           JOIN contacts c
             ON c.workspace_id = om.workspace_id AND c.id = ce.contact_id
          WHERE om.workspace_id = $1
            AND om.status = 'accepted'
            AND om.accepted_at >= $2 AND om.accepted_at < $3
            AND c.company_id IS NOT NULL
            AND ($4::uuid IS NULL OR om.requested_by = $4)",
    )
    .bind(tx.workspace_id())
    .bind(w.from)
    .bind(w.to)
    .bind(w.requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
    .map(|n: i64| n)
}

/// 답장 기업 수 — cohort companies with a human-classified inbound message
/// observed in the window inside a thread that carries an accepted outbound
/// to that same company (§2.4 row 2 numerator). `auto_reply`/`bounce` are
/// excluded per the spec; `unsubscribe_request` counts as a human reply.
pub async fn reply_observed_companies(tx: &mut Tx<'_>, w: &KpiWindow) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "WITH cohort AS (
            SELECT DISTINCT c.company_id
              FROM outbound_messages om
              JOIN contact_emails ce
                ON ce.workspace_id = om.workspace_id
               AND ce.email_address_id = om.to_email_address_id
              JOIN contacts c
                ON c.workspace_id = om.workspace_id AND c.id = ce.contact_id
             WHERE om.workspace_id = $1
               AND om.status = 'accepted'
               AND om.accepted_at >= $2 AND om.accepted_at < $3
               AND c.company_id IS NOT NULL
               AND ($4::uuid IS NULL OR om.requested_by = $4)
        )
        SELECT count(*)
          FROM cohort co
         WHERE EXISTS (
            SELECT 1
              FROM email_messages em
              JOIN outbound_messages om
                ON om.workspace_id = em.workspace_id
               AND om.thread_id = em.thread_id
               AND om.status = 'accepted'
              JOIN contact_emails ce
                ON ce.workspace_id = om.workspace_id
               AND ce.email_address_id = om.to_email_address_id
              JOIN contacts c
                ON c.workspace_id = om.workspace_id AND c.id = ce.contact_id
             WHERE em.workspace_id = $1
               AND em.direction = 'inbound'
               AND em.classification IN
                   ('human_positive','human_negative','question','unsubscribe_request')
               AND em.observed_at >= $2 AND em.observed_at < $3
               AND c.company_id = co.company_id)",
    )
    .bind(tx.workspace_id())
    .bind(w.from)
    .bind(w.to)
    .bind(w.requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// 유효 미팅이 열린 기업 수 — cohort companies with a `confirmed` booking
/// starting inside the window (§2.4 row 3 numerator). `held`/`confirming`/
/// `unknown` never count — only the provider-confirmed meeting does.
pub async fn meeting_held_companies(tx: &mut Tx<'_>, w: &KpiWindow) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "WITH cohort AS (
            SELECT DISTINCT c.company_id
              FROM outbound_messages om
              JOIN contact_emails ce
                ON ce.workspace_id = om.workspace_id
               AND ce.email_address_id = om.to_email_address_id
              JOIN contacts c
                ON c.workspace_id = om.workspace_id AND c.id = ce.contact_id
             WHERE om.workspace_id = $1
               AND om.status = 'accepted'
               AND om.accepted_at >= $2 AND om.accepted_at < $3
               AND c.company_id IS NOT NULL
               AND ($4::uuid IS NULL OR om.requested_by = $4)
        )
        SELECT count(*)
          FROM cohort co
         WHERE EXISTS (
            SELECT 1
              FROM bookings b
              JOIN contact_emails ce
                ON ce.workspace_id = b.workspace_id
               AND ce.email_address_id = b.guest_email_address_id
              JOIN contacts c
                ON c.workspace_id = b.workspace_id AND c.id = ce.contact_id
             WHERE b.workspace_id = $1
               AND b.status = 'confirmed'
               AND b.starts_at >= $2 AND b.starts_at < $3
               AND c.company_id = co.company_id)",
    )
    .bind(tx.workspace_id())
    .bind(w.from)
    .bind(w.to)
    .bind(w.requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// 최초 답장 처리 시간 — for each human-classified inbound in the window on
/// a cohort thread, seconds from `observed_at` to the earliest accepted
/// staff outbound in the same thread; returns (sample size, median seconds).
/// `percentile_cont(0.5)` is the SQL median.
pub async fn first_reply_median(
    tx: &mut Tx<'_>,
    w: &KpiWindow,
) -> Result<(i64, Option<f64>), AppError> {
    let row: (i64, Option<f64>) = sqlx::query_as(
        "WITH cohort_threads AS (
            SELECT DISTINCT om.thread_id
              FROM outbound_messages om
              JOIN contact_emails ce
                ON ce.workspace_id = om.workspace_id
               AND ce.email_address_id = om.to_email_address_id
              JOIN contacts c
                ON c.workspace_id = om.workspace_id AND c.id = ce.contact_id
             WHERE om.workspace_id = $1
               AND om.status = 'accepted'
               AND om.accepted_at >= $2 AND om.accepted_at < $3
               AND c.company_id IS NOT NULL AND om.thread_id IS NOT NULL
               AND ($4::uuid IS NULL OR om.requested_by = $4)
        ),
        firsts AS (
            SELECT em.id,
                   MIN(EXTRACT(EPOCH FROM (o.accepted_at - em.observed_at))) AS delta
              FROM email_messages em
              JOIN cohort_threads ct ON ct.thread_id = em.thread_id
              JOIN outbound_messages o
                ON o.workspace_id = em.workspace_id
               AND o.thread_id = em.thread_id
               AND o.status = 'accepted'
               AND o.accepted_at > em.observed_at
             WHERE em.workspace_id = $1
               AND em.direction = 'inbound'
               AND em.classification IN
                   ('human_positive','human_negative','question','unsubscribe_request')
               AND em.observed_at >= $2 AND em.observed_at < $3
             GROUP BY em.id
        )
        SELECT count(*), percentile_cont(0.5) WITHIN GROUP (ORDER BY delta)
          FROM firsts",
    )
    .bind(tx.workspace_id())
    .bind(w.from)
    .bind(w.to)
    .bind(w.requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row)
}

/// 에이전트 후보 채택률 — (adopted, reviewed) proposal counts in the window
/// keyed on `reviewed_at` (§2.4 row 5). Denominator excludes unreviewed rows
/// ("미검토 건을 분모에 섞지 않음") and `needs_review` (review incomplete).
/// scope=mine narrows to proposals the caller reviewed.
pub async fn proposal_adoption(tx: &mut Tx<'_>, w: &KpiWindow) -> Result<(i64, i64), AppError> {
    let row: (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'accepted'),
                count(*) FILTER (WHERE status IN ('accepted','rejected'))
           FROM lead_proposals
          WHERE workspace_id = $1
            AND reviewed_at >= $2 AND reviewed_at < $3
            AND ($4::uuid IS NULL OR reviewed_by = $4)",
    )
    .bind(tx.workspace_id())
    .bind(w.from)
    .bind(w.to)
    .bind(w.requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row)
}

// --------------------------------------------------- operational counts

#[derive(Debug, Clone, Default)]
pub struct OpsCounts {
    pub unknown_outbound_messages: i64,
    pub unknown_jobs: i64,
    pub dead_jobs: i64,
    pub outbox_backlog: i64,
    pub reauth_required_mailboxes: i64,
    pub emergency_stop: bool,
    pub send_workers_locked: bool,
}

/// §2.4 "운영 안정성" counters — each monitored separately, never summed.
/// Workspace-wide regardless of the dashboard KPI scope.
pub async fn operational_counts(tx: &mut Tx<'_>) -> Result<OpsCounts, AppError> {
    let ws = tx.workspace_id();
    let (unknown_outbound, unknown_jobs, dead_jobs, outbox_backlog, reauth): (
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = sqlx::query_as(
        "SELECT
            (SELECT count(*) FROM outbound_messages
              WHERE workspace_id = $1 AND status = 'unknown'),
            (SELECT count(*) FROM jobs
              WHERE workspace_id = $1 AND state = 'unknown'),
            (SELECT count(*) FROM jobs
              WHERE workspace_id = $1 AND state = 'dead'),
            (SELECT count(*) FROM outbox_events
              WHERE workspace_id = $1 AND published_at IS NULL),
            (SELECT count(*) FROM mail_accounts
              WHERE workspace_id = $1 AND status = 'reauth_required')",
    )
    .bind(ws)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let flags = workspace_flags(tx).await?;
    Ok(OpsCounts {
        unknown_outbound_messages: unknown_outbound,
        unknown_jobs,
        dead_jobs,
        outbox_backlog,
        reauth_required_mailboxes: reauth,
        emergency_stop: flags.emergency_stop,
        send_workers_locked: flags.send_workers_locked,
    })
}

// ------------------------------------------------------------------ alerts

pub async fn outbox_backlog(tx: &mut Tx<'_>) -> Result<(i64, Option<DateTime<Utc>>), AppError> {
    sqlx::query_as(
        "SELECT count(*), min(created_at)
           FROM outbox_events
          WHERE workspace_id = $1 AND published_at IS NULL",
    )
    .bind(tx.workspace_id())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// unknown outbound count + unknown job count + a small investigation
/// sample of outbound ids (§17.2 "unknown submission > 0").
pub async fn unknown_outbound(tx: &mut Tx<'_>) -> Result<(i64, i64, Vec<Uuid>), AppError> {
    let messages: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbound_messages
          WHERE workspace_id = $1 AND status = 'unknown'",
    )
    .bind(tx.workspace_id())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id = $1 AND state = 'unknown'",
    )
    .bind(tx.workspace_id())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let samples: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM outbound_messages
          WHERE workspace_id = $1 AND status = 'unknown'
          ORDER BY created_at, id LIMIT 5",
    )
    .bind(tx.workspace_id())
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((messages, jobs, samples))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeadJobRow {
    pub id: Uuid,
    pub kind: String,
    pub last_error_code: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
}

pub async fn dead_jobs(tx: &mut Tx<'_>) -> Result<(i64, Vec<DeadJobRow>), AppError> {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE workspace_id = $1 AND state = 'dead'")
            .bind(tx.workspace_id())
            .fetch_one(tx.conn())
            .await
            .map_err(map_sqlx)?;
    let samples = sqlx::query_as::<_, DeadJobRow>(
        "SELECT id, kind, last_error_code, finished_at
           FROM jobs
          WHERE workspace_id = $1 AND state = 'dead'
          ORDER BY finished_at DESC NULLS LAST, id LIMIT 5",
    )
    .bind(tx.workspace_id())
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((count, samples))
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReauthMailboxRow {
    pub id: Uuid,
    pub name: String,
    pub last_synced_at: Option<DateTime<Utc>>,
}

/// §16.3 "Zoho 인증 만료 → reauth_required" surfaced as a §17.2 alert.
/// Metadata only — never tokens or message content (§5.6).
pub async fn reauth_mailboxes(tx: &mut Tx<'_>) -> Result<(i64, Vec<ReauthMailboxRow>), AppError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM mail_accounts
          WHERE workspace_id = $1 AND status = 'reauth_required'",
    )
    .bind(tx.workspace_id())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let samples = sqlx::query_as::<_, ReauthMailboxRow>(
        "SELECT id, name, last_synced_at
           FROM mail_accounts
          WHERE workspace_id = $1 AND status = 'reauth_required'
          ORDER BY name, id LIMIT 5",
    )
    .bind(tx.workspace_id())
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((count, samples))
}

// ------------------------------------------- workspace flags (T30 lock)

#[derive(Debug, Clone, Copy)]
pub struct WorkspaceFlags {
    pub emergency_stop: bool,
    /// T30: `workspaces.settings->>'send_workers_locked'` (absent = false).
    /// After a DB restore this stays true until an operator finishes the
    /// reconcile and releases it — send workers must refuse submissions
    /// while locked.
    pub send_workers_locked: bool,
}

pub async fn workspace_flags(tx: &mut Tx<'_>) -> Result<WorkspaceFlags, AppError> {
    let row: Option<(bool, bool)> = sqlx::query_as(
        "SELECT emergency_stop,
                COALESCE((settings->>'send_workers_locked')::boolean, false)
           FROM workspaces
          WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row
        .map(|(emergency_stop, send_workers_locked)| WorkspaceFlags {
            emergency_stop,
            send_workers_locked,
        })
        .unwrap_or(WorkspaceFlags {
            emergency_stop: false,
            send_workers_locked: false,
        }))
}

/// T30 — `operations::send_workers_locked(tx)` gate. Send/submission
/// workers call this at dispatch time (post-merge wiring lives in the
/// sending module): while true, no new outbound submission may start.
/// `workspaces.settings` is JSONB — the key is absent until a restore sets
/// it, and absent means false.
pub async fn send_workers_locked(tx: &mut Tx<'_>) -> Result<bool, AppError> {
    let locked: Option<bool> = sqlx::query_scalar(
        "SELECT COALESCE((settings->>'send_workers_locked')::boolean, false)
           FROM workspaces
          WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(locked.unwrap_or(false))
}

/// Test/ops helper — set the restore flag directly (the runbook path writes
/// `settings.send_workers_locked := true` during a restore; there is no REST
/// setter by design — release only).
pub async fn set_send_workers_locked(tx: &mut Tx<'_>, locked: bool) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE workspaces
            SET settings = jsonb_set(settings, '{send_workers_locked}', $2::jsonb, true)
          WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .bind(serde_json::Value::Bool(locked))
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Release the restore lock after reconcile — removes the key entirely so
/// "absent = false" semantics hold. Returns the prior lock state for audit.
pub async fn release_send_workers_lock(tx: &mut Tx<'_>) -> Result<bool, AppError> {
    let was: Option<bool> = sqlx::query_scalar(
        "SELECT COALESCE((settings->>'send_workers_locked')::boolean, false)
           FROM workspaces
          WHERE id = $1
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let was = was.unwrap_or(false);
    sqlx::query(
        "UPDATE workspaces
            SET settings = settings - 'send_workers_locked'
          WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(was)
}
