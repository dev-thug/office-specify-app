//! mailboxes module use cases (spec §6 — owned by the module worker).
//!
//! - `accounts`/`grants`: mailbox metadata + the §5.4 grant matrix.
//! - `oauth`: Zoho connection-requests / callback / reconnect (§14.1).
//! - `channels`: delivery channels + policy reviews (T33 gate).
//! - `calendar`: calendar connection state (§10.1).

pub mod accounts;
pub mod calendar;
pub mod channels;
pub mod grants;
pub mod oauth;
pub mod oauth_settings;
pub mod real_provider;

pub use real_provider::{DevFallbackMailProvider, MailProviderRouter};

pub use accounts::{get_mail_account, list_mail_accounts};
pub use calendar::{list_calendar_connections, request_calendar_connection};
pub use channels::{
    create_delivery_channel, list_delivery_channels, patch_delivery_channel,
    review_delivery_channel,
};
pub use grants::{GrantPrecondition, list_grants, put_grant};
pub use oauth::{CallbackOutcome, reconnect_mail_account, request_mail_connection, zoho_callback};

use contracts::{AppError, Ctx, Role};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use uuid::Uuid;

/// Record a `denied:<code>` audit row for an authorization failure
/// (T03 pattern — shared shape with `org::admin`, kept local because
/// module use cases must not call each other).
pub(crate) async fn audit_denied(
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

/// Admin gate for mailbox administration (connect, grants, channels).
/// Agents are rejected with the §5.4 code; every denial is audited.
pub(crate) async fn require_admin_audited(
    db: &Db,
    ctx: &Ctx,
    action: &str,
    target_type: &str,
    target_id: Option<Uuid>,
) -> Result<(), AppError> {
    let res = (|| {
        crate::authz::require_not_agent_for(ctx, "administer mailboxes")?;
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
