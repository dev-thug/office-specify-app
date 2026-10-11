//! Shared Office marketing authorization and campaign content rendering.
use contracts::{AppError, AuthMethod, Ctx, PrincipalKind, Role};
use infrastructure::db::Tx;
use uuid::Uuid;

/// Admins act through a browser session; agents (Admin-equivalent) act
/// through their service credential. Returns the session id for humans.
pub fn require_admin(ctx: &Ctx) -> Result<Option<Uuid>, AppError> {
    if !(ctx.role == Role::Admin || crate::authz::agent_admin(ctx)) {
        return Err(AppError::forbidden(
            "administrator required for Office marketing settings",
        ));
    }
    match ctx.auth {
        AuthMethod::Session { session_id } => Ok(Some(session_id)),
        AuthMethod::ServiceCredential { .. } if ctx.principal_kind == PrincipalKind::Agent => {
            Ok(None)
        }
        _ => Err(AppError::forbidden(
            "browser session required for Office marketing settings",
        )),
    }
}

pub async fn revalidate_admin_session_tx(tx: &mut Tx<'_>, ctx: &Ctx) -> Result<(), AppError> {
    let Some(session) = require_admin(ctx)? else {
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM principals WHERE workspace_id=$1 AND id=$2 \
             AND kind='agent' AND status='active' AND permission_version=$3 FOR SHARE)",
        )
        .bind(ctx.workspace_id)
        .bind(ctx.principal_id)
        .bind(ctx.permission_version)
        .fetch_one(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        if !active {
            return Err(AppError::forbidden("agent disabled or permissions changed"));
        }
        return Ok(());
    };
    let active:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM principals p JOIN user_sessions s ON s.workspace_id=p.workspace_id AND s.principal_id=p.id WHERE p.workspace_id=$1 AND p.id=$2 AND p.kind='human' AND p.role='admin' AND p.status='active' AND p.permission_version=$3 AND s.id=$4 AND s.revoked_at IS NULL AND s.expires_at>now() FOR SHARE OF p,s)")
        .bind(ctx.workspace_id).bind(ctx.principal_id).bind(ctx.permission_version).bind(session).fetch_one(tx.conn()).await.map_err(infrastructure::error::map_sqlx)?;
    if !active {
        return Err(AppError::forbidden(
            "Office administrator session or permissions changed",
        ));
    }
    Ok(())
}
