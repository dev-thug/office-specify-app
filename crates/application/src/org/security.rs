//! Self-service account security operations exposed by the Security screen.
//! Session hashes and password values never cross this module's response or
//! audit boundaries.

use chrono::{DateTime, Utc};
use contracts::org::{PasswordChangedResponse, UserSessionResponse, UserSessionRevokedResponse};
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::crypto;
use infrastructure::db::{Db, Tx};
use infrastructure::repo::{auth as auth_repo, org as repo};
use uuid::Uuid;

pub struct SessionPage {
    pub items: Vec<UserSessionResponse>,
    pub has_more: bool,
    pub cursor_seed: Option<(DateTime<Utc>, Uuid)>,
}

/// Restrict Security settings to an authenticated human browser session.
/// Service credentials and Agents do not have an account-security scope.
pub fn require_human_session(ctx: &Ctx) -> Result<Uuid, AppError> {
    if ctx.principal_kind != PrincipalKind::Human {
        return Err(AppError::forbidden(
            "account security requires a human session",
        ));
    }
    match &ctx.auth {
        AuthMethod::Session { session_id } => Ok(*session_id),
        _ => Err(AppError::forbidden(
            "account security requires a human session",
        )),
    }
}

fn session_response(row: repo::OwnSessionRow, current_session_id: Uuid) -> UserSessionResponse {
    UserSessionResponse {
        id: row.id,
        created_at: row.created_at,
        last_seen_at: row.last_seen_at,
        expires_at: row.expires_at,
        revoked_at: row.revoked_at,
        current: row.id == current_session_id,
    }
}

/// List only sessions owned by the authenticated principal in this
/// workspace. The SELECT list deliberately excludes `session_hash`.
pub async fn list_own_sessions(
    db: &Db,
    ctx: &Ctx,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<SessionPage, AppError> {
    let current_session_id = require_human_session(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_own_sessions(&mut tx, ctx.principal_id, after, limit + 1).await?;
    tx.commit().await?;

    let has_more = rows.len() as i64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let cursor_seed = has_more
        .then(|| rows.last().map(|row| (row.created_at, row.id)))
        .flatten();
    Ok(SessionPage {
        items: rows
            .into_iter()
            .map(|row| session_response(row, current_session_id))
            .collect(),
        has_more,
        cursor_seed,
    })
}

/// Revoke one of the caller's non-current sessions. Looking up by both id
/// and principal makes foreign ids indistinguishable from missing sessions.
pub async fn revoke_own_session_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    session_id: Uuid,
) -> Result<UserSessionRevokedResponse, AppError> {
    let current_session_id = require_human_session(ctx)?;
    if current_session_id == session_id {
        return Err(AppError::conflict(
            "use logout to revoke the current session",
        ));
    }
    let before = repo::get_own_session(tx, ctx.principal_id, session_id)
        .await?
        .ok_or_else(|| AppError::not_found("session not found"))?;
    let row = repo::revoke_own_session(tx, ctx.principal_id, session_id)
        .await?
        .ok_or_else(|| AppError::not_found("session not found"))?;
    if before.revoked_at.is_none() {
        audit::record(
            tx,
            AuditEntry::new(
                ctx.principal_id,
                "user.session_revoked",
                "user_session",
                ctx.request_id.clone(),
            )
            .with_target(session_id),
        )
        .await?;
    }
    Ok(UserSessionRevokedResponse {
        id: row.id,
        revoked_at: row.revoked_at.unwrap_or_else(Utc::now),
    })
}

/// Change the authenticated user's global password inside the idempotency
/// transaction. Current-password verification occurs while its credential
/// row is locked; current session is retained by migration 0017's narrowly
/// scoped SECURITY DEFINER helper.
pub async fn change_password_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    current_password: &str,
    new_password: &str,
) -> Result<PasswordChangedResponse, AppError> {
    let current_session_id = require_human_session(ctx)?;
    let length = new_password.chars().count();
    if !(crypto::PASSWORD_MIN_LEN..=crypto::PASSWORD_MAX_LEN).contains(&length) {
        return Err(AppError::validation(
            format!(
                "password must be {}–{} characters",
                crypto::PASSWORD_MIN_LEN,
                crypto::PASSWORD_MAX_LEN
            ),
            vec![Violation {
                field: "new_password".into(),
                code: "length".into(),
            }],
        ));
    }

    // Hash before taking row locks; never include this PHC string in audit or
    // idempotency response data.
    let new_password_hash = crypto::hash_password(new_password)?;
    let principal = auth_repo::get_principal(tx, ctx.principal_id)
        .await?
        .filter(|p| p.kind == "human" && p.status == "active")
        .ok_or_else(|| AppError::forbidden("active human principal required"))?;
    let user_id = principal
        .user_id
        .ok_or_else(|| AppError::forbidden("principal has no login account"))?;

    // Serialize global password changes before locking an individual session.
    // Otherwise concurrent changes from two sessions can deadlock while each
    // transaction revokes the other session.
    let credential = repo::lock_password_credential(tx, user_id)
        .await?
        .ok_or_else(invalid_credentials)?;
    repo::lock_own_session(tx, ctx.principal_id, current_session_id)
        .await?
        .filter(|s| s.revoked_at.is_none() && s.expires_at > Utc::now())
        .ok_or_else(|| AppError::new(ErrorCode::AuthenticationRequired, "session expired"))?;

    if let Some(locked_until) = credential.locked_until
        && locked_until > Utc::now()
    {
        let seconds = (locked_until - Utc::now()).num_seconds().max(1) as u32;
        return Err(AppError::new(
            ErrorCode::AccountLocked,
            "too many failed attempts; try again later",
        )
        .with_retry_after(seconds));
    }
    if !crypto::verify_password(current_password, &credential.password_hash) {
        return Err(invalid_credentials());
    }

    auth_repo::set_password_credential_tx(tx, user_id, &new_password_hash).await?;
    let revoked_other_sessions =
        repo::revoke_other_user_sessions(tx, user_id, current_session_id).await?;
    audit::record(
        tx,
        AuditEntry {
            metadata: serde_json::json!({
                "revoked_other_sessions": revoked_other_sessions,
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                "user.password_changed",
                "user",
                ctx.request_id.clone(),
            )
            .with_target(user_id)
        },
    )
    .await?;
    Ok(PasswordChangedResponse { changed: true })
}

fn invalid_credentials() -> AppError {
    AppError::new(ErrorCode::InvalidCredentials, "invalid email or password")
}
