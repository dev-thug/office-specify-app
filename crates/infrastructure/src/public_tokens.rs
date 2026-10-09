//! Public-token resolution wrappers (R16). Unsubscribe and booking
//! capability endpoints arrive with only the raw token — no workspace
//! context — so `SECURITY DEFINER` functions (migration 0005) resolve
//! `(workspace_id, token_id)` from the token hash without leaking
//! RLS-invisible rows. The caller then opens a normal workspace tx and
//! reads the row there. Used tokens still resolve: a repeat POST is an
//! idempotent success (T24) decided on `used_at`.

use crate::db::{Db, Tx};
use crate::error::map_sqlx;
use contracts::AppError;
use uuid::Uuid;

/// (workspace_id, token_id) as returned by the definer functions.
#[derive(Debug, Clone, Copy)]
pub struct TokenLocation {
    pub workspace_id: Uuid,
    pub token_id: Uuid,
}

async fn resolve(db: &Db, func: &str, token: &str) -> Result<TokenLocation, AppError> {
    let hash = crate::crypto::sha256_bytes(token.as_bytes());
    let mut tx = db.begin_global().await?;
    // Function name is a compile-time constant — never user input.
    let row: Option<(Uuid, Uuid)> =
        sqlx::query_as(&format!("SELECT workspace_id, token_id FROM {func}($1)"))
            .bind(&hash[..])
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)?;
    tx.commit().await?;
    row.map(|(workspace_id, token_id)| TokenLocation {
        workspace_id,
        token_id,
    })
    .ok_or_else(|| AppError::not_found("token not found"))
}

/// Workspace + row id for an unsubscribe token (`unsubscribe_tokens`).
pub async fn unsubscribe_token(db: &Db, token: &str) -> Result<TokenLocation, AppError> {
    resolve(db, "resolve_unsubscribe_token", token).await
}

/// Workspace + row id for a public booking-link token (`booking_links`).
pub async fn booking_link_token(db: &Db, token: &str) -> Result<TokenLocation, AppError> {
    resolve(db, "resolve_booking_link_token", token).await
}

/// Workspace + row id for a booking manage token (`booking_manage_tokens`).
pub async fn booking_manage_token(db: &Db, token: &str) -> Result<TokenLocation, AppError> {
    resolve(db, "resolve_booking_manage_token", token).await
}

/// Resolve an unsubscribe token end to end: token → workspace → scoped
/// tx + row. Used tokens resolve too; revoked capabilities are rejected
/// (repeat POST = idempotent success, T24).
pub async fn resolve_unsubscribe_token<'db>(
    db: &'db Db,
    token: &str,
) -> Result<(Tx<'db>, crate::repo::compliance_core::UnsubscribeToken), AppError> {
    let loc = unsubscribe_token(db, token).await?;
    let mut tx = db.begin(loc.workspace_id).await?;
    let row = crate::repo::compliance_core::resolve_unsubscribe_token(&mut tx, token)
        .await?
        .ok_or_else(|| AppError::not_found("token not found"))?;
    if row.revoked_at.is_some() {
        return Err(AppError::not_found("token not found"));
    }
    Ok((tx, row))
}
