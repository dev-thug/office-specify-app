//! Org administration use cases (spec §6, B.2 조직·연동 rows): principals
//! list/get/patch, member invitations, agents, service credentials,
//! disable-requests, emergency stop.
//!
//! Auth (`auth.rs`) and `/me` (`me.rs`) live beside this file untouched;
//! every mutation runs inside a single workspace tx with `audit::record` +
//! `outbox::emit` + `jobs::enqueue` per §16.2. `_tx` variants take the
//! caller's transaction so `idempotency::run` can bind the whole
//! request; the `db` wrappers begin/commit for direct calls.

use chrono::{DateTime, Utc};
use contracts::org::{
    CreateAgentRequest, CreateCredentialRequest, CreateInvitationRequest,
    CredentialCreatedResponse, CredentialResponse, CredentialRevokedResponse,
    DisableRequestRequest, DisableRequestResponse, EmergencyStopRequest, EmergencyStopResponse,
    InvitationCreatedResponse, InvitationResponse, InvitationRevokedResponse,
    PatchPrincipalRequest, PrincipalResponse,
};
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Role, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::jobs;
use infrastructure::repo::auth as auth_repo;
use infrastructure::repo::org as repo;
use uuid::Uuid;

/// Job kind contracts (integration-contracts.md). Payload v1.
pub const JOB_DISABLE_PRINCIPAL: &str = "org.disable_principal";
pub const JOB_REVOKE_CREDENTIAL_JOBS: &str = "org.revoke_credential_jobs";
/// Outbox event (integration-contracts.md / §16.2).
pub const EVENT_PRINCIPAL_DISABLED: &str = "principal.disabled";

/// Credential TTL bounds.
const CREDENTIAL_MAX_DAYS: i64 = 730;
const CREDENTIAL_DEFAULT_DAYS: i64 = 365;

// --------------------------------------------------------------------------
// helpers
// --------------------------------------------------------------------------

fn role_from_str(role: &str) -> Role {
    match role {
        "admin" => Role::Admin,
        "manager" => Role::Manager,
        "agent" => Role::Agent,
        "system" => Role::System,
        _ => Role::Member,
    }
}

fn kind_from_str(kind: &str) -> PrincipalKind {
    match kind {
        "agent" => PrincipalKind::Agent,
        "system" => PrincipalKind::System,
        _ => PrincipalKind::Human,
    }
}

pub fn principal_response(row: &repo::PrincipalFull) -> PrincipalResponse {
    PrincipalResponse {
        id: row.id,
        kind: kind_from_str(&row.kind),
        user_id: row.user_id,
        name: row.name.clone(),
        role: role_from_str(&row.role),
        status: row.status.clone(),
        permission_version: row.permission_version,
        version: row.version,
        created_at: row.created_at,
    }
}

fn credential_response(row: &repo::CredentialRow) -> CredentialResponse {
    CredentialResponse {
        id: row.id,
        principal_id: row.principal_id,
        name: row.name.clone(),
        key_prefix: row.key_prefix.clone(),
        scopes: row.scopes.clone(),
        expires_at: row.expires_at,
        last_used_at: row.last_used_at,
        revoked_at: row.revoked_at,
        created_at: row.created_at,
    }
}

fn invitation_response(row: &repo::InvitationFull) -> InvitationResponse {
    InvitationResponse {
        id: row.id,
        email: row.email.clone(),
        role: role_from_str(&row.role),
        expires_at: row.expires_at,
        accepted_at: row.accepted_at,
        revoked_at: row.revoked_at,
        created_at: row.created_at,
    }
}

/// Admin-gated mutations audit denied attempts (T03: an agent hitting the
/// role/grant endpoints leaves a `denied:<code>` audit row).
async fn audit_denied(
    db: &Db,
    ctx: &Ctx,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
    err: &AppError,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        action,
        target_type,
        ctx.request_id.clone(),
    )
    .denied(&err.code);
    entry.target_id = target_id;
    audit::record(&mut tx, entry).await?;
    tx.commit().await
}

/// Admin-only org administration. Agents are rejected with the §5.4 code;
/// every denial is audited (T03).
///
/// `check_admin` is the same gate exposed for the route layer, whose
/// `idempotency::run` wrappers call the `*_tx` cores directly and therefore
/// need the authz check separately.
pub async fn check_admin(
    db: &Db,
    ctx: &Ctx,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
) -> Result<(), AppError> {
    require_admin_audited(db, ctx, action, target_type, target_id).await
}

/// The workspace roster is an admin session surface. Service credentials
/// carry explicit tool scopes but there is no org.read scope, so even a
/// human-owned bearer credential must not enumerate principals.
pub async fn check_roster_read(
    db: &Db,
    ctx: &Ctx,
    action: &str,
    target_id: Option<Uuid>,
) -> Result<(), AppError> {
    if !matches!(&ctx.auth, AuthMethod::Session { .. }) {
        let err = AppError::forbidden("principal roster requires an admin session");
        audit_denied(db, ctx, action, "principal", target_id, &err).await?;
        return Err(err);
    }
    require_admin_audited(db, ctx, action, "principal", target_id).await
}

/// Human sessions may read only the active-human picker; service credentials
/// and agents have no organization-read scope and cannot enumerate the roster.
pub async fn check_principal_picker_read(db: &Db, ctx: &Ctx) -> Result<(), AppError> {
    let allowed = ctx.principal_kind == PrincipalKind::Human
        && matches!(&ctx.auth, AuthMethod::Session { .. });
    if allowed {
        return Ok(());
    }
    let err = AppError::forbidden("principal picker requires a human session");
    audit_denied(db, ctx, "principal.list", "principal", None, &err).await?;
    Err(err)
}

async fn require_admin_audited(
    db: &Db,
    ctx: &Ctx,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
) -> Result<(), AppError> {
    let res = (|| {
        crate::authz::require_not_agent_for(ctx, "administer the organization")?;
        crate::authz::require_human(ctx)?;
        crate::authz::require_role(ctx, &[Role::Admin])
    })();
    match res {
        Ok(()) => Ok(()),
        Err(e) => {
            audit_denied(db, ctx, action, target_type, target_id, &e).await?;
            Err(e)
        }
    }
}

/// A.7: refuse to demote/disable the last active admin in the workspace.
/// Call inside a tx that already locked the principal rows.
async fn guard_last_admin(
    tx: &mut Tx<'_>,
    target: &repo::PrincipalFull,
    new_role: &str,
    new_status: &str,
) -> Result<(), AppError> {
    let losing_admin = (target.kind == "human" && target.role == "admin")
        && (new_role != "admin" || new_status != "active");
    if !losing_admin {
        return Ok(());
    }
    if repo::count_active_admins_locked(tx).await? <= 1 {
        return Err(AppError::conflict(
            "the last active admin cannot be demoted or disabled",
        ));
    }
    Ok(())
}

// --------------------------------------------------------------------------
// principals
// --------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct PrincipalFilter {
    pub kind: Option<String>,
    pub role: Option<String>,
    pub status: Option<String>,
}

/// Keyset page — `cursor_seed` = (created_at, id) of the last item when
/// `has_more`; the route layer signs it into `next_cursor`.
pub struct ListResult<T> {
    pub items: Vec<T>,
    pub has_more: bool,
    pub cursor_seed: Option<(DateTime<Utc>, Uuid)>,
}

/// `GET /principals` — human and agent principals, keyset page.
pub async fn list_principals(
    db: &Db,
    ctx: &Ctx,
    filter: &PrincipalFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<ListResult<PrincipalResponse>, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_principals(
        &mut tx,
        filter.kind.as_deref(),
        filter.role.as_deref(),
        filter.status.as_deref(),
        after,
        limit + 1,
    )
    .await?;
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<repo::PrincipalFull> = rows.into_iter().take(limit as usize).collect();
    let cursor_seed = has_more
        .then(|| rows.last().map(|r| (r.created_at, r.id)))
        .flatten();
    Ok(ListResult {
        items: rows.iter().map(principal_response).collect(),
        has_more,
        cursor_seed,
    })
}

/// `GET /principals/{id}` (+ETag = `version`).
pub async fn get_principal(db: &Db, ctx: &Ctx, id: Uuid) -> Result<PrincipalResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_principal_full(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("principal not found"))?;
    tx.commit().await?;
    Ok(principal_response(&row))
}

/// `PATCH /principals/{id}` — optimistic role/status change (If-Match).
/// Last-admin guard (A.7); `invited` is not settable; disabling revokes
/// sessions+credentials and emits `principal.disabled` in the same tx.
pub async fn patch_principal(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &PatchPrincipalRequest,
) -> Result<PrincipalResponse, AppError> {
    require_admin_audited(db, ctx, "principal.update", "principal", Some(id)).await?;
    if req.role.is_none() && req.status.is_none() {
        return Err(AppError::validation("nothing to update", vec![]));
    }
    let new_role = req.role.map(domain::org::role_to_str);
    let new_status = req.status.clone();
    if let Some(s) = &new_status
        && !domain::org::settable_status_valid(s)
    {
        return Err(AppError::validation(
            "status must be active or disabled",
            vec![Violation {
                field: "status".into(),
                code: "invalid".into(),
            }],
        ));
    }
    if new_role.is_some_and(|r| !domain::org::human_role_valid(r)) {
        return Err(AppError::validation(
            "role must be admin, manager or member",
            vec![Violation {
                field: "role".into(),
                code: "invalid".into(),
            }],
        ));
    }
    if new_status.as_deref() == Some("disabled") {
        return Err(AppError::validation(
            "use the disable-requests flow so owned work is handed off first",
            vec![Violation {
                field: "status".into(),
                code: "use_disable_requests".into(),
            }],
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    // Lock admins before the target principal. Disable requests and role
    // changes use the same lock order, so simultaneous admin changes cannot
    // deadlock or both pass the last-admin check.
    let _active_admins = repo::count_active_admins_locked(&mut tx).await?;
    let Some(cur) = repo::lock_principal(&mut tx, id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("principal not found"));
    };
    if cur.kind == "system" {
        let _ = tx.rollback().await;
        return Err(AppError::conflict("system principals cannot be changed"));
    }
    if new_role.is_some() && cur.kind != "human" {
        let _ = tx.rollback().await;
        return Err(AppError::validation(
            "role can only change on human principals",
            vec![Violation {
                field: "role".into(),
                code: "invalid".into(),
            }],
        ));
    }
    let role: String = new_role
        .map(String::from)
        .unwrap_or_else(|| cur.role.clone());
    let status = new_status.clone().unwrap_or_else(|| cur.status.clone());
    if cur.status == "invited" {
        let _ = tx.rollback().await;
        return Err(AppError::conflict(
            "an invited principal must accept the invitation first",
        ));
    }
    if repo::pending_disable_job(&mut tx, id).await?.is_some() {
        let _ = tx.rollback().await;
        return Err(AppError::conflict(
            "a pending disable request must finish before the principal can change",
        ));
    }
    guard_last_admin(&mut tx, &cur, &role, &status).await?;
    if cur.version != expected_version {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "principal was changed; refetch and retry",
        ));
    }

    let Some(row) = repo::update_principal(&mut tx, id, expected_version, &role, &status).await?
    else {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "principal was changed; refetch and retry",
        ));
    };
    if cur.status == "disabled" && status == "active" {
        repo::set_user_status_for_principal(&mut tx, id, "active").await?;
    }
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "principal.update",
            "principal",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    tx.commit().await?;
    Ok(principal_response(&row))
}

// --------------------------------------------------------------------------
// member invitations
// --------------------------------------------------------------------------

/// `POST /member-invitations` tx core — `invite_base` is the public base
/// URL ("" → relative link). Returns the one-time link ONCE (X2).
pub async fn create_invitation_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateInvitationRequest,
    invite_base: &str,
) -> Result<InvitationCreatedResponse, AppError> {
    if !domain::org::invitation_email_valid(&req.email) {
        return Err(AppError::validation(
            "invalid email",
            vec![Violation {
                field: "email".into(),
                code: "invalid".into(),
            }],
        ));
    }
    if !domain::org::invitation_role_valid(req.role) {
        return Err(AppError::validation(
            "invitations are for human roles only",
            vec![Violation {
                field: "role".into(),
                code: "invalid".into(),
            }],
        ));
    }
    let email = req.email.trim().to_lowercase();
    let role = domain::org::role_to_str(req.role);
    let (id, token) = auth_repo::create_invitation(tx, &email, role, ctx.principal_id).await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "member_invitation.create",
            "member_invitation",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    let row = repo::get_invitation(tx, id)
        .await?
        .ok_or_else(|| AppError::internal("invitation missing after insert"))?;
    Ok(InvitationCreatedResponse {
        id,
        email: row.email,
        role: role_from_str(&row.role),
        expires_at: row.expires_at,
        invite_url: Some(format!("{invite_base}/api/v1/auth/invitations/{token}")),
        created_at: row.created_at,
    })
}

/// `POST /member-invitations` — authz + tx management around
/// [`create_invitation_tx`].
pub async fn create_invitation(
    db: &Db,
    ctx: &Ctx,
    req: &CreateInvitationRequest,
    invite_base: &str,
) -> Result<InvitationCreatedResponse, AppError> {
    require_admin_audited(
        db,
        ctx,
        "member_invitation.create",
        "member_invitation",
        None,
    )
    .await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    match create_invitation_tx(&mut tx, ctx, req, invite_base).await {
        Ok(r) => {
            tx.commit().await?;
            Ok(r)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// `GET /member-invitations`.
pub async fn list_invitations(
    db: &Db,
    ctx: &Ctx,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<ListResult<InvitationResponse>, AppError> {
    crate::authz::require_role(ctx, &[Role::Admin])?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_invitations(&mut tx, after, limit + 1).await?;
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<repo::InvitationFull> = rows.into_iter().take(limit as usize).collect();
    let cursor_seed = has_more
        .then(|| rows.last().map(|r| (r.created_at, r.id)))
        .flatten();
    Ok(ListResult {
        items: rows.iter().map(invitation_response).collect(),
        has_more,
        cursor_seed,
    })
}

/// `POST /member-invitations/{id}/revoke` — idempotent: revoking an
/// already-revoked invite returns the same 200 body.
pub async fn revoke_invitation(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<InvitationRevokedResponse, AppError> {
    require_admin_audited(
        db,
        ctx,
        "member_invitation.revoke",
        "member_invitation",
        Some(id),
    )
    .await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    repo::revoke_invitation(&mut tx, id).await?;
    let Some(row) = repo::get_invitation(&mut tx, id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("invitation not found"));
    };
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "member_invitation.revoke",
            "member_invitation",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    tx.commit().await?;
    Ok(InvitationRevokedResponse {
        id,
        revoked_at: row.revoked_at.unwrap_or_else(Utc::now),
    })
}

// --------------------------------------------------------------------------
// agents + service credentials
// --------------------------------------------------------------------------

/// `POST /agents` tx core — agent principal + audit (intended scopes in
/// metadata only; credentials carry the real scopes).
pub async fn create_agent_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateAgentRequest,
) -> Result<PrincipalResponse, AppError> {
    if req.name.trim().is_empty() {
        return Err(AppError::validation(
            "name is required",
            vec![Violation {
                field: "name".into(),
                code: "required".into(),
            }],
        ));
    }
    let scopes = req.scopes.clone().unwrap_or_default();
    let scopes = domain::org::validate_agent_scopes(&scopes)
        .map_err(|v| AppError::validation("invalid agent scopes", v))?;
    let id =
        auth_repo::insert_principal(tx, "agent", None, req.name.trim(), "agent", "active").await?;
    let row = repo::get_principal_full(tx, id)
        .await?
        .ok_or_else(|| AppError::internal("agent missing after insert"))?;
    audit::record(
        tx,
        AuditEntry {
            metadata: serde_json::json!({"scopes": scopes}),
            ..AuditEntry::new(
                ctx.principal_id,
                "agent.create",
                "principal",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await?;
    Ok(principal_response(&row))
}

/// `POST /agents` — authz + tx management.
pub async fn create_agent(
    db: &Db,
    ctx: &Ctx,
    req: &CreateAgentRequest,
) -> Result<PrincipalResponse, AppError> {
    require_admin_audited(db, ctx, "agent.create", "principal", None).await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    match create_agent_tx(&mut tx, ctx, req).await {
        Ok(r) => {
            tx.commit().await?;
            Ok(r)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// `POST /agents/{id}/credentials` tx core — the `osk_…` secret is created
/// hashed-at-rest and appears only in `secret` on the fresh response.
pub async fn create_credential_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    agent_id: Uuid,
    req: &CreateCredentialRequest,
) -> Result<CredentialCreatedResponse, AppError> {
    if req.name.trim().is_empty() {
        return Err(AppError::validation(
            "name is required",
            vec![Violation {
                field: "name".into(),
                code: "required".into(),
            }],
        ));
    }
    let scopes = domain::org::validate_agent_scopes(&req.scopes)
        .map_err(|v| AppError::validation("invalid credential scopes", v))?;
    let days = req
        .expires_in_days
        .map(i64::from)
        .unwrap_or(CREDENTIAL_DEFAULT_DAYS)
        .clamp(1, CREDENTIAL_MAX_DAYS);
    let agent = repo::get_principal_full(tx, agent_id)
        .await?
        .ok_or_else(|| AppError::not_found("agent not found"))?;
    if agent.kind != "agent" {
        return Err(AppError::validation(
            "credentials can only be issued to agent principals",
            vec![Violation {
                field: "id".into(),
                code: "not_an_agent".into(),
            }],
        ));
    }
    if agent.status != "active" {
        return Err(AppError::conflict("agent is not active"));
    }
    if repo::pending_disable_job(tx, agent_id).await?.is_some() {
        return Err(AppError::conflict("agent has a pending disable request"));
    }
    let (id, token) = auth_repo::create_service_credential(
        tx,
        agent_id,
        req.name.trim(),
        &scopes,
        chrono::Duration::days(days),
    )
    .await?;
    let row = repo::get_credential(tx, id)
        .await?
        .ok_or_else(|| AppError::internal("credential missing after insert"))?;
    audit::record(
        tx,
        AuditEntry {
            metadata: serde_json::json!({"scopes": scopes, "key_prefix": row.key_prefix}),
            ..AuditEntry::new(
                ctx.principal_id,
                "credential.create",
                "service_credential",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await?;
    Ok(CredentialCreatedResponse {
        id: row.id,
        principal_id: row.principal_id,
        name: row.name,
        key_prefix: row.key_prefix,
        scopes: row.scopes,
        expires_at: row.expires_at,
        secret: Some(token),
    })
}

/// `POST /agents/{id}/credentials` — authz + tx management.
pub async fn create_credential(
    db: &Db,
    ctx: &Ctx,
    agent_id: Uuid,
    req: &CreateCredentialRequest,
) -> Result<CredentialCreatedResponse, AppError> {
    require_admin_audited(db, ctx, "credential.create", "service_credential", None).await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    match create_credential_tx(&mut tx, ctx, agent_id, req).await {
        Ok(r) => {
            tx.commit().await?;
            Ok(r)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// `GET /credentials` — all service credentials (secrets never listed).
pub async fn list_credentials(db: &Db, ctx: &Ctx) -> Result<Vec<CredentialResponse>, AppError> {
    crate::authz::require_role(ctx, &[Role::Admin])?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_all_credentials(&mut tx).await?;
    tx.commit().await?;
    Ok(rows.iter().map(credential_response).collect())
}

/// `POST /credentials/{id}/revoke` — revokes the credential and enqueues
/// `org.revoke_credential_jobs` to cancel its not-yet-dispatched work
/// (§5.5). Idempotent: an already-revoked credential returns 200 without a
/// second job.
pub async fn revoke_credential(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<CredentialRevokedResponse, AppError> {
    require_admin_audited(db, ctx, "credential.revoke", "service_credential", Some(id)).await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(before) = repo::get_credential(&mut tx, id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("credential not found"));
    };
    let Some(row) = repo::revoke_credential(&mut tx, id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("credential not found"));
    };
    if before.revoked_at.is_none() {
        enqueue_revoke_credential_jobs(&mut tx, id).await?;
        audit::record(
            &mut tx,
            AuditEntry::new(
                ctx.principal_id,
                "credential.revoke",
                "service_credential",
                ctx.request_id.clone(),
            )
            .with_target(id),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(CredentialRevokedResponse {
        id,
        revoked_at: row.revoked_at.unwrap_or_else(Utc::now),
    })
}

// --------------------------------------------------------------------------
// disable-requests (A.7 managed disable)
// --------------------------------------------------------------------------

/// `POST /principals/{id}/disable-requests` tx core — revoke all access at
/// request time, then queue the atomic handoff/deactivation job. Principal
/// status and `principal.disabled` are changed only after cleanup succeeds.
pub async fn request_disable_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    req: &DisableRequestRequest,
) -> Result<DisableRequestResponse, AppError> {
    let cancel_pending = req.cancel_pending.unwrap_or(true);
    if req.reassign_to == Some(id) {
        return Err(AppError::validation(
            "reassign_to must be a different principal",
            vec![Violation {
                field: "reassign_to".into(),
                code: "invalid".into(),
            }],
        ));
    }
    // Serialize all admin-disable requests and last-admin role changes.
    let active_admins = repo::count_active_admins_locked(tx).await?;
    let Some(cur) = repo::lock_principal(tx, id).await? else {
        return Err(AppError::not_found("principal not found"));
    };
    if cur.kind == "system" {
        return Err(AppError::conflict("system principals cannot be disabled"));
    }
    if cur.status == "disabled" {
        return Err(AppError::conflict("principal is already disabled"));
    }
    if let Some(existing) = repo::pending_disable_job(tx, id).await? {
        return Ok(DisableRequestResponse {
            job_id: existing,
            principal_id: id,
        });
    }
    if cur.kind == "human" && cur.role == "admin" {
        let pending = repo::count_pending_admin_disables(tx).await?;
        if active_admins.saturating_sub(pending) <= 1 {
            return Err(AppError::conflict(
                "the last active admin cannot be scheduled for disable",
            ));
        }
    }
    if let Some(target) = req.reassign_to {
        let valid = repo::get_principal_full(tx, target)
            .await?
            .is_some_and(|p| p.kind == "human" && p.status == "active")
            && repo::pending_disable_job(tx, target).await?.is_none();
        if !valid {
            return Err(AppError::validation(
                "reassign_to must be an active human principal",
                vec![Violation {
                    field: "reassign_to".into(),
                    code: "invalid".into(),
                }],
            ));
        }
    }
    if req.reassign_to.is_none() && repo::has_reassignable_work(tx, id).await? {
        return Err(AppError::conflict(
            "reassign_to is required while the principal owns open work",
        ));
    }

    // Do not leave a session or service credential usable while cleanup is
    // queued. Human login is also blocked via users.status; principal status
    // remains active only until the worker commits the handoff.
    repo::set_user_status_for_principal(tx, id, "disabled").await?;
    repo::revoke_sessions_for_principal(tx, id).await?;
    let creds = repo::revoke_credentials_for_principal(tx, id).await?;
    for cred_id in creds {
        // api_runtime may inspect service_credentials; cancel credential-
        // sourced work before enqueueing the worker handoff job. The worker
        // runtime deliberately cannot read the credential table (R22).
        repo::cancel_pending_jobs_for_credential(tx, cred_id).await?;
        enqueue_revoke_credential_jobs(tx, cred_id).await?;
    }
    let Some((pending_version, _permission_version)) = repo::bump_principal_version(tx, id).await?
    else {
        return Err(AppError::not_found("principal not found"));
    };
    let dedupe = format!("disable:{id}:{pending_version}");
    let job_id = jobs::enqueue(
        tx,
        JOB_DISABLE_PRINCIPAL,
        1,
        serde_json::json!({
            "principal_id": id,
            "reassign_to": req.reassign_to,
            "cancel_pending": cancel_pending,
        }),
        Utc::now(),
        Some(&dedupe),
    )
    .await?;
    repo::tag_job_requester(tx, job_id, ctx.principal_id, None).await?;
    audit::record(
        tx,
        AuditEntry {
            metadata: serde_json::json!({
                "reassign_to": req.reassign_to,
                "cancel_pending": cancel_pending,
                "job_id": job_id,
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                "principal.disable",
                "principal",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await?;
    Ok(DisableRequestResponse {
        job_id,
        principal_id: id,
    })
}

/// `POST /principals/{id}/disable-requests` — authz + tx management.
pub async fn request_disable(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    req: &DisableRequestRequest,
) -> Result<DisableRequestResponse, AppError> {
    require_admin_audited(db, ctx, "principal.disable", "principal", Some(id)).await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    match request_disable_tx(&mut tx, ctx, id, req).await {
        Ok(r) => {
            tx.commit().await?;
            Ok(r)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

// --------------------------------------------------------------------------
// emergency stop (B.2 운영 row)
// --------------------------------------------------------------------------

/// `GET`-side state for the PUT flow's read/precondition.
pub async fn emergency_stop_state(db: &Db, ctx: &Ctx) -> Result<EmergencyStopResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let (enabled, policy_version) = repo::get_workspace_state(&mut tx)
        .await?
        .ok_or_else(|| AppError::internal("workspace missing"))?;
    tx.commit().await?;
    Ok(EmergencyStopResponse {
        enabled,
        policy_version,
    })
}

/// `PUT /operations/emergency-stop` — ON: Admin or Manager; OFF: Admin +
/// reason. Optimistic on `policy_version` (If-Match). Audited (A.7); the
/// flag blocks new submission work elsewhere (§8.4 send gate).
pub async fn set_emergency_stop(
    db: &Db,
    ctx: &Ctx,
    expected_policy_version: i64,
    req: &EmergencyStopRequest,
) -> Result<EmergencyStopResponse, AppError> {
    let action = "operations.emergency_stop";
    let res = (|| {
        crate::authz::require_not_agent_for(ctx, "toggle the emergency stop")?;
        crate::authz::require_human(ctx)?;
        if req.enabled {
            crate::authz::require_role(ctx, &[Role::Admin, Role::Manager])
        } else {
            crate::authz::require_role(ctx, &[Role::Admin])
        }
    })();
    if let Err(e) = res {
        audit_denied(db, ctx, action, "workspace", Some(ctx.workspace_id), &e).await?;
        return Err(e);
    }
    if !req.enabled && req.reason.trim().is_empty() {
        return Err(AppError::validation(
            "resuming requires a reason",
            vec![Violation {
                field: "reason".into(),
                code: "required".into(),
            }],
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(new_version) =
        repo::set_emergency_stop(&mut tx, req.enabled, expected_policy_version).await?
    else {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "workspace policy changed; refetch and retry",
        ));
    };
    audit::record(
        &mut tx,
        AuditEntry {
            metadata: serde_json::json!({
                "enabled": req.enabled,
                "reason": req.reason,
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                action,
                "workspace",
                ctx.request_id.clone(),
            )
            .with_target(ctx.workspace_id)
        },
    )
    .await?;
    tx.commit().await?;
    Ok(EmergencyStopResponse {
        enabled: req.enabled,
        policy_version: new_version,
    })
}

// --------------------------------------------------------------------------
// shared job helpers
// --------------------------------------------------------------------------

/// Enqueue `org.revoke_credential_jobs` for one revoked credential
/// (dedupe `revoke-cred:<id>` — repeated revokes reuse the first job).
pub async fn enqueue_revoke_credential_jobs(
    tx: &mut Tx<'_>,
    credential_id: Uuid,
) -> Result<Uuid, AppError> {
    let dedupe = format!("revoke-cred:{credential_id}");
    if let Some(existing) =
        repo::find_job_by_dedupe(tx, JOB_REVOKE_CREDENTIAL_JOBS, &dedupe).await?
    {
        return Ok(existing);
    }
    jobs::enqueue(
        tx,
        JOB_REVOKE_CREDENTIAL_JOBS,
        1,
        serde_json::json!({"credential_id": credential_id}),
        Utc::now(),
        Some(&dedupe),
    )
    .await
}
