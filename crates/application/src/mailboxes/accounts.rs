//! Mail account metadata reads (B.2: `GET /mail-accounts[/{id}]`).
//!
//! Two views per §5.3/B.3.1: Admin sees the full management list (account
//! metadata is not message content); every other principal sees only
//! accounts they hold `can_read` on — no implicit mailbox access, and
//! admins get none either on content-bearing paths (T02).

use chrono::{DateTime, Utc};
use contracts::AppError;
use contracts::Ctx;
use contracts::mailboxes::MailAccountResponse;
use contracts::{PrincipalKind, Role};
use infrastructure::db::Db;
use infrastructure::repo::mailboxes as repo;
use uuid::Uuid;

/// Keyset page with a `cursor_seed` the route layer signs into `next_cursor`.
pub struct AccountPage {
    pub items: Vec<MailAccountResponse>,
    pub has_more: bool,
    pub cursor_seed: Option<(DateTime<Utc>, Uuid)>,
}

fn response(
    (account, grant): repo::AccountWithGrant,
    principal_kind: PrincipalKind,
) -> MailAccountResponse {
    let mut allowed_actions = grant.map(|g| g.allowed_actions()).unwrap_or_default();
    if principal_kind == PrincipalKind::Agent {
        // The can_send ACL is used only by the explicit approved-submit
        // service. It must not advertise direct `mail.send`/`mail.approve`.
        allowed_actions.retain(|action| action != "mail.send" && action != "mail.approve");
    }
    MailAccountResponse {
        id: account.id,
        connection_id: account.connection_id,
        provider: account.provider,
        provider_account_id: account.provider_account_id,
        primary_address: account.primary_address,
        name: account.name,
        status: account.status,
        last_synced_at: account.last_synced_at,
        version: account.version,
        created_at: account.created_at,
        allowed_actions,
    }
}

/// `GET /mail-accounts` — Admin: all accounts; others: `can_read` only.
pub async fn list_mail_accounts(
    db: &Db,
    ctx: &Ctx,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<AccountPage, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = if ctx.role == Role::Admin {
        repo::list_mail_accounts(&mut tx, ctx.principal_id, after, limit + 1).await?
    } else {
        repo::list_accessible_mail_accounts(&mut tx, ctx.principal_id, after, limit + 1).await?
    };
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<repo::AccountWithGrant> = rows.into_iter().take(limit as usize).collect();
    let cursor_seed = has_more
        .then(|| rows.last().map(|(a, _)| (a.created_at, a.id)))
        .flatten();
    Ok(AccountPage {
        items: rows
            .into_iter()
            .map(|row| response(row, ctx.principal_kind))
            .collect(),
        has_more,
        cursor_seed,
    })
}

/// `GET /mail-accounts/{id}` (+ETag). Metadata (no content) — Admin always,
/// others need `can_read`, missing grant hides existence (404, §5.6).
pub async fn get_mail_account(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<MailAccountResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_mail_account(&mut tx, ctx.principal_id, id).await?;
    tx.commit().await?;
    let Some(pair) = row else {
        return Err(AppError::not_found("mail account not found"));
    };
    if ctx.role != Role::Admin && !pair.1.as_ref().is_some_and(|g| g.can_read) {
        return Err(AppError::not_found("mail account not found"));
    }
    Ok(response(pair, ctx.principal_kind))
}
