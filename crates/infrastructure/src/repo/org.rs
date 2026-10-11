//! org module repository (spec §6 — owned by the module worker).
//!
//! Workspace-scoped access through `Tx` only (RLS context set by `Db::begin`).
//! `workspaces`/`users` have no `workspace_id` column → no RLS; everything
//! else is workspace-scoped.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

// --------------------------------------------------------------------------
// own sessions and password credentials (Security settings)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OwnSessionRow {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

pub async fn list_own_sessions(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<OwnSessionRow>, AppError> {
    sqlx::query_as::<_, OwnSessionRow>(
        "SELECT id, created_at, last_seen_at, expires_at, revoked_at
           FROM user_sessions
          WHERE workspace_id = $1 AND principal_id = $2
            AND ($3::timestamptz IS NULL
                 OR created_at < $3
                 OR (created_at = $3 AND id < $4))
          ORDER BY created_at DESC, id DESC
          LIMIT $5",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(after.map(|(created_at, _)| created_at))
    .bind(after.map(|(_, id)| id).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_own_session(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    session_id: Uuid,
) -> Result<Option<OwnSessionRow>, AppError> {
    sqlx::query_as::<_, OwnSessionRow>(
        "SELECT id, created_at, last_seen_at, expires_at, revoked_at
           FROM user_sessions
          WHERE workspace_id = $1 AND principal_id = $2 AND id = $3",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(session_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn lock_own_session(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    session_id: Uuid,
) -> Result<Option<OwnSessionRow>, AppError> {
    sqlx::query_as::<_, OwnSessionRow>(
        "SELECT id, created_at, last_seen_at, expires_at, revoked_at
           FROM user_sessions
          WHERE workspace_id = $1 AND principal_id = $2 AND id = $3
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(session_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn revoke_own_session(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    session_id: Uuid,
) -> Result<Option<OwnSessionRow>, AppError> {
    sqlx::query_as::<_, OwnSessionRow>(
        "UPDATE user_sessions
            SET revoked_at = COALESCE(revoked_at, now())
          WHERE workspace_id = $1 AND principal_id = $2 AND id = $3
          RETURNING id, created_at, last_seen_at, expires_at, revoked_at",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(session_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Clone, sqlx::FromRow)]
pub struct PasswordCredentialLockRow {
    pub password_hash: String,
    pub failed_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
}

/// Lock the global password credential while verifying the current password
/// and replacing the hash. Only api_runtime has access to this table.
pub async fn lock_password_credential(
    tx: &mut Tx<'_>,
    user_id: Uuid,
) -> Result<Option<PasswordCredentialLockRow>, AppError> {
    sqlx::query_as::<_, PasswordCredentialLockRow>(
        "SELECT password_hash, failed_attempts, locked_until
           FROM user_password_credentials
          WHERE user_id = $1
          FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Migration 0017 narrow SECURITY DEFINER helper. It verifies that the
/// current session belongs to this user and revokes only their other
/// workspace sessions; api_runtime receives no cross-workspace table access.
pub async fn revoke_other_user_sessions(
    tx: &mut Tx<'_>,
    user_id: Uuid,
    current_session_id: Uuid,
) -> Result<i64, AppError> {
    sqlx::query_scalar("SELECT revoke_other_user_sessions($1, $2)")
        .bind(user_id)
        .bind(current_session_id)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// principals
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PrincipalFull {
    pub id: Uuid,
    pub kind: String,
    pub user_id: Option<Uuid>,
    pub name: String,
    pub role: String,
    pub status: String,
    pub permission_version: i64,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub avatar_updated_at: Option<DateTime<Utc>>,
    pub title: Option<String>,
}

const PRINCIPAL_COLS: &str =
    "id, kind, user_id, name, role, status, permission_version, version, created_at,
     (SELECT a.updated_at FROM principal_avatars a
       WHERE a.workspace_id = principals.workspace_id AND a.principal_id = principals.id)
       AS avatar_updated_at,
     title";

/// Keyset page of principals, `(created_at DESC, id DESC)` (B.1.5).
/// `after` = (created_at, id) of the last row of the previous page.
#[allow(clippy::too_many_arguments)]
pub async fn list_principals(
    tx: &mut Tx<'_>,
    kind: Option<&str>,
    role: Option<&str>,
    status: Option<&str>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<PrincipalFull>, AppError> {
    let sql = format!(
        "SELECT {PRINCIPAL_COLS} FROM principals
          WHERE workspace_id = $1
            AND ($2::text IS NULL OR kind = $2)
            AND ($3::text IS NULL OR role = $3)
            AND ($4::text IS NULL OR status = $4)
            AND ($5::timestamptz IS NULL
                 OR created_at < $5
                 OR (created_at = $5 AND id < $6))
          ORDER BY created_at DESC, id DESC
          LIMIT $7"
    );
    sqlx::query_as::<_, PrincipalFull>(&sql)
        .bind(tx.workspace_id())
        .bind(kind)
        .bind(role)
        .bind(status)
        .bind(after.map(|(t, _)| t))
        .bind(after.map(|(_, i)| i).unwrap_or(Uuid::nil()))
        .bind(limit)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

pub async fn get_principal_full(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<PrincipalFull>, AppError> {
    sqlx::query_as::<_, PrincipalFull>(&format!(
        "SELECT {PRINCIPAL_COLS} FROM principals WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Row lock for read-modify-write inside the request tx (A.7 last-admin
/// checks serialize on the principals rows of this workspace).
pub async fn lock_principal(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<PrincipalFull>, AppError> {
    sqlx::query_as::<_, PrincipalFull>(&format!(
        "SELECT {PRINCIPAL_COLS} FROM principals
              WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Active human admins — locked so the A.7 last-admin guard cannot race a
/// concurrent demotion/disable in another tx.
pub async fn count_active_admins_locked(tx: &mut Tx<'_>) -> Result<i64, AppError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM principals
          WHERE workspace_id = $1 AND kind = 'human' AND role = 'admin' AND status = 'active'
          ORDER BY id
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(ids.len() as i64)
}

/// Count active administrators with a queued/running disable request. The
/// caller first locks active admin rows through `count_active_admins_locked`,
/// so concurrent requests cannot both reserve the final admin slot.
pub async fn count_pending_admin_disables(tx: &mut Tx<'_>) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(*)
           FROM principals p
          WHERE p.workspace_id = $1 AND p.kind = 'human'
            AND p.role = 'admin' AND p.status = 'active'
            AND EXISTS (
                SELECT 1 FROM jobs j
                 WHERE j.workspace_id = p.workspace_id
                   AND j.kind = 'org.disable_principal'
                   AND j.state IN ('queued','running','retry_wait')
                   AND j.payload->>'principal_id' = p.id::text
            )",
    )
    .bind(tx.workspace_id())
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// An in-flight disable request for a principal, if one exists.
pub async fn pending_disable_job(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM jobs
          WHERE workspace_id = $1 AND kind = 'org.disable_principal'
            AND state IN ('queued','running','retry_wait')
            AND payload->>'principal_id' = $2::text
          ORDER BY created_at DESC, id DESC
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Move the linked employee login out of active state while an async
/// disable request is pending. Principal status changes only after cleanup.
pub async fn set_user_status_for_principal(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    status: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE users SET status = $3
          WHERE id = (
              SELECT user_id FROM principals
               WHERE workspace_id = $1 AND id = $2 AND kind = 'human'
          )",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(status)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Whether a principal owns open work that the disable job must hand off.
pub async fn has_reassignable_work(tx: &mut Tx<'_>, principal_id: Uuid) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM companies
             WHERE workspace_id = $1 AND owner_principal_id = $2
            UNION ALL
            SELECT 1 FROM contacts
             WHERE workspace_id = $1 AND owner_principal_id = $2
            UNION ALL
            SELECT 1 FROM tasks
             WHERE workspace_id = $1 AND owner_principal_id = $2 AND status = 'open'
            UNION ALL
            SELECT 1 FROM email_threads
             WHERE workspace_id = $1 AND assigned_principal_id = $2
        )",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Optimistic role/status update (A.2): bumps `version` (ETag) and
/// `permission_version` (§5.5 cache invalidation). Returns the new row, or
/// None when `expected_version` is stale → caller maps to 412.
pub async fn update_principal(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    role: &str,
    status: &str,
    name: &str,
    title: Option<&str>,
) -> Result<Option<PrincipalFull>, AppError> {
    // A rename alone does not change authority, so it leaves
    // permission_version (and every cached permission) untouched.
    sqlx::query_as::<_, PrincipalFull>(&format!(
        "UPDATE principals
                SET permission_version = permission_version
                        + CASE WHEN role <> $3 OR status <> $4 THEN 1 ELSE 0 END,
                    role = $3, status = $4, name = $6, title = $7,
                    version = version + 1
              WHERE workspace_id = $1 AND id = $2 AND version = $5
              RETURNING {PRINCIPAL_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(role)
    .bind(status)
    .bind(expected_version)
    .bind(name)
    .bind(title)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Invalidate principal caches when a disable request immediately revokes
/// access but defers the status transition until the handoff job completes.
pub async fn bump_principal_version(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<(i64, i64)>, AppError> {
    sqlx::query_as::<_, (i64, i64)>(
        "UPDATE principals
            SET version = version + 1,
                permission_version = permission_version + 1
          WHERE workspace_id = $1 AND id = $2
          RETURNING version, permission_version",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Status-only transition used by the disable flow — still optimistic.
pub async fn set_principal_status(
    tx: &mut Tx<'_>,
    id: Uuid,
    status: &str,
) -> Result<Option<PrincipalFull>, AppError> {
    sqlx::query_as::<_, PrincipalFull>(&format!(
        "UPDATE principals
                SET status = $3,
                    version = version + 1,
                    permission_version = permission_version + 1
              WHERE workspace_id = $1 AND id = $2 AND status <> $3
              RETURNING {PRINCIPAL_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(status)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Revoke every live session of the principal (§5.5 — disable/revoke is
/// immediate, not waiting for natural expiry).
pub async fn revoke_sessions_for_principal(tx: &mut Tx<'_>, id: Uuid) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE user_sessions SET revoked_at = now()
          WHERE workspace_id = $1 AND principal_id = $2 AND revoked_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// Revoke every live service credential; returns the revoked ids (callers
/// enqueue pending-work cancellation per credential, §5.5).
pub async fn revoke_credentials_for_principal(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Vec<Uuid>, AppError> {
    let rows = sqlx::query_scalar::<_, Uuid>(
        "UPDATE service_credentials SET revoked_at = now()
          WHERE workspace_id = $1 AND principal_id = $2 AND revoked_at IS NULL
          RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(rows)
}

/// The workspace's `system` principal (worker audit actor).
pub async fn system_principal_id(tx: &mut Tx<'_>) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM principals WHERE workspace_id = $1 AND kind = 'system' LIMIT 1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// member invitations
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InvitationFull {
    pub id: Uuid,
    pub email: String,
    pub role: String,
    pub invited_by: Uuid,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const INVITATION_COLS: &str =
    "id, email, role, invited_by, expires_at, accepted_at, revoked_at, created_at";

pub async fn list_invitations(
    tx: &mut Tx<'_>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<InvitationFull>, AppError> {
    sqlx::query_as::<_, InvitationFull>(&format!(
        "SELECT {INVITATION_COLS} FROM member_invitations
              WHERE workspace_id = $1
                AND ($2::timestamptz IS NULL
                     OR created_at < $2
                     OR (created_at = $2 AND id < $3))
              ORDER BY created_at DESC, id DESC
              LIMIT $4"
    ))
    .bind(tx.workspace_id())
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_invitation(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<InvitationFull>, AppError> {
    sqlx::query_as::<_, InvitationFull>(&format!(
        "SELECT {INVITATION_COLS} FROM member_invitations
              WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Returns true when this call performed the revocation (false = already
/// revoked or already accepted — caller decides the status code).
pub async fn revoke_invitation(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE member_invitations SET revoked_at = now()
          WHERE workspace_id = $1 AND id = $2
            AND revoked_at IS NULL AND accepted_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

// --------------------------------------------------------------------------
// service credentials
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CredentialRow {
    pub id: Uuid,
    pub principal_id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub expires_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const CREDENTIAL_COLS: &str =
    "id, principal_id, name, key_prefix, scopes, expires_at, last_used_at, revoked_at, created_at";

pub async fn list_credentials(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<Vec<CredentialRow>, AppError> {
    sqlx::query_as::<_, CredentialRow>(&format!(
        "SELECT {CREDENTIAL_COLS} FROM service_credentials
              WHERE workspace_id = $1 AND principal_id = $2
              ORDER BY created_at DESC, id DESC"
    ))
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `GET /credentials` — workspace-wide list (Admin).
pub async fn list_all_credentials(tx: &mut Tx<'_>) -> Result<Vec<CredentialRow>, AppError> {
    sqlx::query_as::<_, CredentialRow>(&format!(
        "SELECT {CREDENTIAL_COLS} FROM service_credentials
              WHERE workspace_id = $1
              ORDER BY created_at DESC, id DESC"
    ))
    .bind(tx.workspace_id())
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_credential(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<CredentialRow>, AppError> {
    sqlx::query_as::<_, CredentialRow>(&format!(
        "SELECT {CREDENTIAL_COLS} FROM service_credentials
              WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Idempotent revoke — returns the row in its post state.
pub async fn revoke_credential(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<CredentialRow>, AppError> {
    sqlx::query_as::<_, CredentialRow>(&format!(
        "UPDATE service_credentials SET revoked_at = now()
              WHERE workspace_id = $1 AND id = $2
              RETURNING {CREDENTIAL_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Denied-audit rows for an (actor, action) pair — the T03 proof surface:
/// every authorization denial writes `denied:<code>` into `details` (see
/// `audit::record`). Operational/security reporting can reuse this count.
pub async fn denied_audit_count(
    tx: &mut Tx<'_>,
    actor: Uuid,
    action: &str,
) -> Result<i64, AppError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM audit_logs
          WHERE workspace_id = $1 AND actor_principal_id = $2 AND action = $3
            AND details->>'outcome' LIKE 'denied:%'",
    )
    .bind(tx.workspace_id())
    .bind(actor)
    .bind(action)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Total audit rows for an action (any outcome) — used by tests asserting
/// that audited transitions (e.g. emergency-stop toggles) were recorded.
pub async fn audit_action_count(tx: &mut Tx<'_>, action: &str) -> Result<i64, AppError> {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM audit_logs WHERE workspace_id = $1 AND action = $2",
    )
    .bind(tx.workspace_id())
    .bind(action)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// workspaces / emergency stop
// --------------------------------------------------------------------------

/// `workspaces` has no RLS (no workspace_id column) — the row id is the
/// workspace id itself.
pub async fn get_workspace_state(tx: &mut Tx<'_>) -> Result<Option<(bool, i64)>, AppError> {
    sqlx::query_as::<_, (bool, i64)>(
        "SELECT emergency_stop, policy_version FROM workspaces WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Optimistic emergency-stop toggle. `policy_version` is the ETag source;
/// returns the new policy_version or None on stale If-Match.
pub async fn set_emergency_stop(
    tx: &mut Tx<'_>,
    enabled: bool,
    expected_policy_version: i64,
) -> Result<Option<i64>, AppError> {
    sqlx::query_scalar::<_, i64>(
        "UPDATE workspaces
            SET emergency_stop = $2, policy_version = policy_version + 1
          WHERE id = $1 AND policy_version = $3
          RETURNING policy_version",
    )
    .bind(tx.workspace_id())
    .bind(enabled)
    .bind(expected_policy_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Cross-module helper (wave-b-tasks: other modules call this for the §8.4
/// send gate): is the workspace emergency stop currently engaged?
/// `workspaces.emergency_stop` defaults to true — the safe default on a
/// missing row is also `true` (fail closed).
pub async fn emergency_stop_active(tx: &mut Tx<'_>) -> Result<bool, AppError> {
    let flag: Option<bool> =
        sqlx::query_scalar("SELECT emergency_stop FROM workspaces WHERE id = $1")
            .bind(tx.workspace_id())
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)?;
    Ok(flag.unwrap_or(true))
}

// --------------------------------------------------------------------------
// pending work (disable / credential revoke cleanup, §5.5, A.7)
// --------------------------------------------------------------------------

/// Cancel not-yet-dispatched jobs requested by a principal. Credential-
/// sourced jobs are canceled in the API transaction when those credentials
/// are revoked; worker_runtime must not read service_credentials (R22).
/// Running/unknown/finished jobs are never touched (§5.5).
pub async fn cancel_pending_jobs_for_principal(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE jobs SET state = 'canceled', finished_at = now()
          WHERE workspace_id = $1
            AND state IN ('queued','retry_wait')
            AND requested_by = $2",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// Cancel not-yet-dispatched jobs carrying a revoked credential.
pub async fn cancel_pending_jobs_for_credential(
    tx: &mut Tx<'_>,
    credential_id: Uuid,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE jobs SET state = 'canceled', finished_at = now()
          WHERE workspace_id = $1
            AND state IN ('queued','retry_wait')
            AND source_credential_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(credential_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

/// `jobs::enqueue` does not populate `requested_by`/`source_credential_id`
/// in the current base — tag the just-enqueued row so §5.5 pending-work
/// cancellation can attribute it. No-op when the row was claimed meanwhile.
// TODO(rebase): drop when the shared enqueue accepts requester attribution.
pub async fn tag_job_requester(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    requested_by: Uuid,
    source_credential_id: Option<Uuid>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE jobs SET requested_by = $3, source_credential_id = $4
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(job_id)
    .bind(requested_by)
    .bind(source_credential_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Existing job id for a (kind, dedupe_key) pair — the current base's
/// `jobs::enqueue` raises a UNIQUE conflict instead of returning the
/// existing id, so callers that want enqueue-or-attach look it up first.
// TODO(rebase): drop once jobs::enqueue is idempotent on (kind, dedupe_key).
pub async fn find_job_by_dedupe(
    tx: &mut Tx<'_>,
    kind: &str,
    dedupe_key: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM jobs WHERE workspace_id = $1 AND kind = $2 AND dedupe_key = $3",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(dedupe_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Reassign the leaving principal's owned records (A.7: owner/thread
/// hand-off before the disable finishes). Returns (companies, contacts,
/// tasks, threads) row counts. Meeting-host rows are NOT reassigned — host
/// changes require new booking links (§10.1).
pub async fn reassign_principal_ownership(
    tx: &mut Tx<'_>,
    from: Uuid,
    to: Uuid,
) -> Result<(u64, u64, u64, u64), AppError> {
    let companies = sqlx::query(
        "UPDATE companies SET owner_principal_id = $3
          WHERE workspace_id = $1 AND owner_principal_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(from)
    .bind(to)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    let contacts = sqlx::query(
        "UPDATE contacts SET owner_principal_id = $3
          WHERE workspace_id = $1 AND owner_principal_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(from)
    .bind(to)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    let tasks = sqlx::query(
        "UPDATE tasks SET owner_principal_id = $3
          WHERE workspace_id = $1 AND owner_principal_id = $2 AND status = 'open'",
    )
    .bind(tx.workspace_id())
    .bind(from)
    .bind(to)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    let threads = sqlx::query(
        "UPDATE email_threads SET assigned_principal_id = $3
          WHERE workspace_id = $1 AND assigned_principal_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(from)
    .bind(to)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok((companies, contacts, tasks, threads))
}

// --------------------------------------------------------------------------
// profile photos
// --------------------------------------------------------------------------

/// Stored profile photo: (content_type, bytes, updated_at).
pub type Avatar = (String, Vec<u8>, DateTime<Utc>);

pub async fn get_avatar(tx: &mut Tx<'_>, principal_id: Uuid) -> Result<Option<Avatar>, AppError> {
    sqlx::query_as(
        "SELECT content_type, bytes, updated_at FROM principal_avatars
          WHERE workspace_id = $1 AND principal_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn put_avatar(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    content_type: &str,
    bytes: &[u8],
    updated_by: Uuid,
) -> Result<DateTime<Utc>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO principal_avatars (workspace_id, principal_id, content_type, bytes, updated_by)
         VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (workspace_id, principal_id) DO UPDATE
            SET content_type = EXCLUDED.content_type, bytes = EXCLUDED.bytes,
                updated_by = EXCLUDED.updated_by, updated_at = clock_timestamp()
         RETURNING updated_at",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(content_type)
    .bind(bytes)
    .bind(updated_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn delete_avatar(tx: &mut Tx<'_>, principal_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query(
        "DELETE FROM principal_avatars WHERE workspace_id = $1 AND principal_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(result.rows_affected() > 0)
}
