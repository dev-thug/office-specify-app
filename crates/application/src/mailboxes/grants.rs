//! Mailbox grant administration (B.3.2, §5.4).
//!
//! `PUT /mail-accounts/{id}/grants/{principal_id}`: first grant uses
//! `If-None-Match: *` (concurrent creators → 412), updates use
//! `If-Match: <grant version>`. The DDL CHECK chain is pre-validated in
//! `domain::mailboxes`; `can_approve` requires an Admin/Manager target;
//! agents can never receive `can_send`/`can_approve` (T03 territory —
//! denied attempts are audited). Every change bumps the target's
//! `permission_version` (§5.5) so cached permissions invalidate.

use contracts::AppError;
use contracts::Ctx;
use contracts::ErrorCode;
use contracts::Violation;
use contracts::mailboxes::{GrantListItem, GrantResponse, PutGrantRequest};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::repo::mailboxes as repo;
use uuid::Uuid;

/// Precondition from the request headers (B.3.2).
#[derive(Debug, Clone, Copy)]
pub enum GrantPrecondition {
    /// `If-None-Match: *` — create only; an existing grant → 412.
    Create,
    /// `If-Match: "n"` — update grant version n; absent → 404, stale → 412.
    Update(i64),
}

fn response(row: &repo::GrantRow, permission_version: i64) -> GrantResponse {
    GrantResponse {
        mail_account_id: row.mail_account_id,
        principal_id: row.principal_id,
        can_read: row.can_read,
        can_compose: row.can_compose,
        can_send: row.can_send,
        can_approve: row.can_approve,
        can_assign: row.can_assign,
        version: row.version,
        permission_version,
        pending_actions_revalidation_requested: true,
    }
}

fn flags(req: &PutGrantRequest) -> repo::GrantFlags {
    repo::GrantFlags {
        can_read: req.can_read,
        can_compose: req.can_compose,
        can_send: req.can_send,
        can_approve: req.can_approve,
        can_assign: req.can_assign,
    }
}

/// `GET /mail-accounts/{id}/grants` (Admin).
pub async fn list_grants(
    db: &Db,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<Vec<GrantListItem>, AppError> {
    super::require_admin_audited(
        db,
        ctx,
        "mailbox.grant_list",
        "mail_account",
        Some(mail_account_id),
    )
    .await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    if repo::get_mail_account(&mut tx, ctx.principal_id, mail_account_id)
        .await?
        .is_none()
    {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("mail account not found"));
    }
    let rows = repo::list_grants(&mut tx, mail_account_id).await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|r| GrantListItem {
            principal_id: r.principal_id,
            principal_name: r.principal_name,
            principal_kind: r.principal_kind,
            can_read: r.can_read,
            can_compose: r.can_compose,
            can_send: r.can_send,
            can_approve: r.can_approve,
            can_assign: r.can_assign,
            version: r.version,
        })
        .collect())
}

/// `PUT /mail-accounts/{id}/grants/{principal_id}` (Admin, audited).
/// Returns the grant + the target's new `permission_version`; the response
/// ETag is the grant `version`.
pub async fn put_grant(
    db: &Db,
    ctx: &Ctx,
    mail_account_id: Uuid,
    principal_id: Uuid,
    precondition: GrantPrecondition,
    req: &PutGrantRequest,
) -> Result<GrantResponse, AppError> {
    super::require_admin_audited(
        db,
        ctx,
        "mailbox.grant",
        "mail_account",
        Some(mail_account_id),
    )
    .await?;
    let violations = domain::mailboxes::grant_flag_violations(
        req.can_read,
        req.can_compose,
        req.can_send,
        req.can_approve,
        req.can_assign,
    );
    if !violations.is_empty() {
        return Err(AppError::validation(
            "grant flags violate the dependency chain",
            violations,
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    if repo::get_mail_account(&mut tx, ctx.principal_id, mail_account_id)
        .await?
        .is_none()
    {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("mail account not found"));
    }
    let target = infrastructure::repo::org::get_principal_full(&mut tx, principal_id)
        .await?
        .ok_or_else(|| AppError::not_found("principal not found"))?;

    // Agent principals can never hold send/approve (§5.4, B.3.2 policy).
    if target.kind == "agent" && (req.can_send || req.can_approve) {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::AgentDirectSendForbidden,
            "agents cannot receive can_send or can_approve",
        ));
    }
    // `can_approve` additionally requires Admin/Manager (§5.4).
    if req.can_approve && !matches!(target.role.as_str(), "admin" | "manager") {
        let _ = tx.rollback().await;
        return Err(AppError::validation(
            "can_approve requires an Admin or Manager principal",
            vec![Violation {
                field: "can_approve".into(),
                code: "ROLE_REQUIRED".into(),
            }],
        ));
    }

    let existing = repo::get_grant(&mut tx, mail_account_id, principal_id).await?;
    let grant = match (precondition, existing) {
        (GrantPrecondition::Create, Some(_)) => {
            let _ = tx.rollback().await;
            return Err(AppError::new(
                ErrorCode::VersionMismatch,
                "a grant for this principal already exists",
            ));
        }
        (GrantPrecondition::Create, None) => {
            repo::insert_grant(
                &mut tx,
                mail_account_id,
                principal_id,
                &flags(req),
                ctx.principal_id,
            )
            .await?
        }
        (GrantPrecondition::Update(_), None) => {
            let _ = tx.rollback().await;
            return Err(AppError::not_found("grant not found"));
        }
        (GrantPrecondition::Update(v), Some(g)) if g.version != v => {
            let _ = tx.rollback().await;
            return Err(AppError::new(
                ErrorCode::VersionMismatch,
                "grant was changed; refetch and retry",
            ));
        }
        (GrantPrecondition::Update(v), Some(_)) => {
            repo::update_grant(&mut tx, mail_account_id, principal_id, &flags(req), v)
                .await?
                .ok_or_else(|| {
                    AppError::new(
                        ErrorCode::VersionMismatch,
                        "grant was changed; refetch and retry",
                    )
                })?
        }
    };

    // §5.5: invalidate the target's cached permissions.
    let permission_version = repo::bump_permission_version(&mut tx, principal_id).await?;
    audit::record(
        &mut tx,
        AuditEntry {
            metadata: serde_json::json!({
                "principal_id": principal_id,
                "can_read": req.can_read,
                "can_compose": req.can_compose,
                "can_send": req.can_send,
                "can_approve": req.can_approve,
                "can_assign": req.can_assign,
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                "mailbox.grant",
                "mail_account",
                ctx.request_id.clone(),
            )
            .with_target(mail_account_id)
        },
    )
    .await?;
    tx.commit().await?;
    Ok(response(&grant, permission_version))
}
