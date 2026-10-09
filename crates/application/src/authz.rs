//! Authorization checks (spec §4.6, source §5.2–5.4). These are the only
//! authorization primitives module workers may use.

use contracts::{AppError, Ctx, ErrorCode, PrincipalKind, Role, Scope};
use uuid::Uuid;

use infrastructure::db::Tx;
use infrastructure::error::map_sqlx;

/// Human principals only — approvals, grants, suppression release,
/// permission changes.
pub fn require_human(ctx: &Ctx) -> Result<(), AppError> {
    if ctx.principal_kind == PrincipalKind::Human {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "this action requires a human principal",
        ))
    }
}

pub fn require_role(ctx: &Ctx, roles: &[Role]) -> Result<(), AppError> {
    if roles.contains(&ctx.role) {
        Ok(())
    } else {
        Err(AppError::forbidden(format!(
            "requires role {}",
            roles
                .iter()
                .map(|r| format!("{r:?}").to_lowercase())
                .collect::<Vec<_>>()
                .join(" or ")
        )))
    }
}

pub fn require_scope(ctx: &Ctx, scope: Scope) -> Result<(), AppError> {
    if ctx.scopes.contains(scope) {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::ScopeRequired,
            format!("missing scope {}", scope.as_str()),
        ))
    }
}

/// Agent principals may never approve, grant, send directly, reveal tokens,
/// or unsuppress (§5.4). `action` is a stable key for the error detail.
pub fn require_not_agent_for(ctx: &Ctx, action: &str) -> Result<(), AppError> {
    if ctx.principal_kind == PrincipalKind::Agent {
        return Err(AppError::new(
            ErrorCode::AgentDirectSendForbidden,
            format!("agents cannot {action}"),
        ));
    }
    Ok(())
}

/// Mailbox ACL from `mail_account_grants` (spec §4.6 — agents read the same
/// table; role scopes do not imply mailbox access).
#[derive(Debug, Clone, Copy, Default)]
pub struct MailboxGrant {
    pub can_read: bool,
    pub can_compose: bool,
    pub can_send: bool,
    pub can_approve: bool,
    pub can_assign: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxPerm {
    Read,
    Compose,
    Send,
    Approve,
    Assign,
}

pub async fn mailbox_grant(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<Option<MailboxGrant>, AppError> {
    let row: Option<(bool, bool, bool, bool, bool)> = sqlx::query_as(
        "SELECT can_read, can_compose, can_send, can_approve, can_assign
           FROM mail_account_grants
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(ctx.workspace_id)
    .bind(mail_account_id)
    .bind(ctx.principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row.map(
        |(can_read, can_compose, can_send, can_approve, can_assign)| MailboxGrant {
            can_read,
            can_compose,
            can_send,
            can_approve,
            can_assign,
        },
    ))
}

/// Scope each mailbox permission requires — the §5.2 formula is
/// conjunctive: grant row AND role/kind clause AND credential scope.
fn perm_scope(perm: MailboxPerm) -> Scope {
    match perm {
        MailboxPerm::Read => Scope::MailRead,
        MailboxPerm::Compose => Scope::MailCompose,
        MailboxPerm::Send => Scope::MailSend,
        MailboxPerm::Approve => Scope::MailApprove,
        MailboxPerm::Assign => Scope::MailAssign,
    }
}

/// §5.4 role/kind clause: `can_approve` additionally requires an
/// Admin/Manager human; `can_send` is never held by an agent.
fn perm_role_ok(ctx: &Ctx, perm: MailboxPerm) -> bool {
    match perm {
        MailboxPerm::Approve => {
            ctx.principal_kind == PrincipalKind::Human
                && matches!(ctx.role, Role::Admin | Role::Manager)
        }
        MailboxPerm::Send => ctx.principal_kind != PrincipalKind::Agent,
        _ => true,
    }
}

/// Require a mailbox permission. `MAILBOX_ACCESS_DENIED` (403); callers that
/// must hide existence map to 404 themselves.
///
/// Conjunctive (§5.2/R7): the grant row AND the §5.4 role clause AND the
/// credential/role-implied scope must all hold — a bad grant row alone can
/// never authorize.
pub async fn require_mailbox(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
    perm: MailboxPerm,
) -> Result<MailboxGrant, AppError> {
    let Some(grant) = mailbox_grant(tx, ctx, mail_account_id).await? else {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no access to this mailbox",
        ));
    };
    let grant_ok = match perm {
        MailboxPerm::Read => grant.can_read,
        MailboxPerm::Compose => grant.can_compose,
        MailboxPerm::Send => grant.can_send,
        MailboxPerm::Approve => grant.can_approve,
        MailboxPerm::Assign => grant.can_assign,
    };
    if grant_ok && perm_role_ok(ctx, perm) && ctx.scopes.contains(perm_scope(perm)) {
        Ok(grant)
    } else {
        Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no access to this mailbox",
        ))
    }
}

/// CRM edit rule (spec §4.6): Member may edit own or unassigned-new records
/// only; Manager/Admin may edit any.
pub fn crm_can_edit(ctx: &Ctx, owner_principal_id: Option<Uuid>) -> Result<(), AppError> {
    match ctx.role {
        Role::Admin | Role::Manager => Ok(()),
        Role::Member
            if owner_principal_id.is_none() || owner_principal_id == Some(ctx.principal_id) =>
        {
            Ok(())
        }
        _ => Err(AppError::forbidden("not the owner of this record")),
    }
}
