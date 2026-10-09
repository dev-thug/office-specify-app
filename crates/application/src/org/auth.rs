//! Auth use cases (spec §4.5, deviation X1 — email+password, X2 —
//! invitation preview URL, no email send).
//!
//! Lockout: 5 consecutive failures → `locked_until = now()+15m`
//! (`ACCOUNT_LOCKED`). Credential failures return the generic
//! `INVALID_CREDENTIALS` — no user enumeration.

use chrono::Utc;
use contracts::org::{InvitationPreviewResponse, MeResponse};
use contracts::{AppError, ErrorCode};
use infrastructure::crypto;
use infrastructure::db::Db;
use infrastructure::repo::auth as repo;
use uuid::Uuid;

/// Outcome of a successful login or invitation accept.
pub struct LoginOutcome {
    /// Raw session token — caller sets the `outreach_session` cookie.
    pub session_token: String,
    /// Raw CSRF token for the `outreach_csrf` cookie (double-submit).
    pub csrf_token: String,
    pub session_id: Uuid,
    pub me: MeResponse,
}

/// `POST /auth/login`.
///
/// `workspace_id` is the server-fixed default workspace (A.5), never
/// client-supplied.
pub async fn login(
    db: &Db,
    workspace_id: Uuid,
    session_ttl: chrono::Duration,
    email: &str,
    password: &str,
    request_id: &str,
) -> Result<LoginOutcome, AppError> {
    let user = repo::find_user_by_email(db, email).await?;
    let Some(user) = user else {
        // Constant-ish time: verify against a dummy hash so missing users
        // are not distinguishable by timing.
        let _ = crypto::verify_password(password, DUMMY_HASH);
        return Err(invalid_credentials());
    };
    if user.status != "active" {
        return Err(invalid_credentials());
    }
    let Some(cred) = repo::get_password_credential(db, user.id).await? else {
        return Err(invalid_credentials());
    };
    if let Some(locked) = cred.locked_until
        && locked > Utc::now()
    {
        let secs = (locked - Utc::now()).num_seconds().max(1) as u32;
        return Err(AppError::new(
            ErrorCode::AccountLocked,
            "too many failed attempts; try again later",
        )
        .with_retry_after(secs));
    }
    if !crypto::verify_password(password, &cred.password_hash) {
        let locked = repo::record_login_failure(db, user.id).await?;
        if let Some(until) = locked
            && until > Utc::now()
        {
            let secs = (until - Utc::now()).num_seconds().max(1) as u32;
            return Err(AppError::new(
                ErrorCode::AccountLocked,
                "too many failed attempts; try again later",
            )
            .with_retry_after(secs));
        }
        return Err(invalid_credentials());
    }
    repo::reset_login_failures(db, user.id).await?;

    let principal = repo::find_principal_for_user(db, workspace_id, user.id)
        .await?
        .ok_or_else(invalid_credentials)?;
    if principal.status != "active" {
        return Err(AppError::new(
            ErrorCode::PrincipalDisabled,
            "this account is disabled",
        ));
    }

    let mut tx = db.begin(workspace_id).await?;
    let (session_id, session_token) =
        repo::create_session(&mut tx, principal.id, session_ttl).await?;
    infrastructure::audit::record(
        &mut tx,
        infrastructure::audit::AuditEntry::new(principal.id, "user.login", "principal", request_id),
    )
    .await?;
    tx.commit().await?;

    let csrf_token = crypto::generate_token();
    Ok(LoginOutcome {
        session_token,
        csrf_token,
        session_id,
        me: super::me::me_for_principal(db, &principal).await?,
    })
}

/// `POST /auth/logout` — revoke the session inside a tx with audit.
pub async fn logout(db: &Db, ctx: &contracts::Ctx, session_id: Uuid) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    repo::revoke_session(&mut tx, session_id).await?;
    infrastructure::audit::record(
        &mut tx,
        infrastructure::audit::AuditEntry::new(
            ctx.principal_id,
            "user.logout",
            "session",
            ctx.request_id.clone(),
        )
        .with_target(session_id),
    )
    .await?;
    tx.commit().await
}

/// `GET /auth/invitations/{token}` — public preview (X2).
pub async fn invitation_preview(
    db: &Db,
    token: &str,
) -> Result<InvitationPreviewResponse, AppError> {
    let inv = repo::find_invitation_by_token(db, token)
        .await?
        .filter(|i| i.accepted_at.is_none() && i.revoked_at.is_none() && i.expires_at > Utc::now());
    let Some(inv) = inv else {
        return Err(AppError::not_found("invitation not found or expired"));
    };
    Ok(InvitationPreviewResponse {
        email: inv.email,
        role: match inv.role.as_str() {
            "admin" => contracts::Role::Admin,
            "manager" => contracts::Role::Manager,
            _ => contracts::Role::Member,
        },
        expires_at: inv.expires_at,
    })
}

/// `POST /auth/invitations/{token}/accept` — create user + password
/// credential + human principal, mark accepted, log in (spec §4.5).
pub async fn accept_invitation(
    db: &Db,
    session_ttl: chrono::Duration,
    token: &str,
    display_name: &str,
    password: &str,
    request_id: &str,
) -> Result<LoginOutcome, AppError> {
    if display_name.trim().is_empty() {
        return Err(AppError::validation(
            "display name is required",
            vec![contracts::Violation {
                field: "display_name".into(),
                code: "required".into(),
            }],
        ));
    }
    validate_password(password)?;

    let inv = repo::find_invitation_by_token(db, token)
        .await?
        .filter(|i| i.accepted_at.is_none() && i.revoked_at.is_none() && i.expires_at > Utc::now())
        .ok_or_else(|| AppError::not_found("invitation not found or expired"))?;

    // Create (or reuse) the user row — global table.
    let user = match repo::find_user_by_email(db, &inv.email).await? {
        Some(u) => u,
        None => repo::create_user(db, &inv.email, display_name).await?,
    };
    let hash = crypto::hash_password(password)?;
    repo::set_password_credential(db, user.id, &hash).await?;

    let mut tx = db.begin(inv.workspace_id).await?;
    // A re-accept race or a second invitation for the same email reuses the
    // existing principal when present.
    let principal_id = match repo::find_principal_for_user(db, inv.workspace_id, user.id).await? {
        Some(p) => p.id,
        None => {
            repo::insert_principal(
                &mut tx,
                "human",
                Some(user.id),
                display_name,
                &inv.role,
                "active",
            )
            .await?
        }
    };
    repo::mark_invitation_accepted(&mut tx, inv.id).await?;
    infrastructure::audit::record(
        &mut tx,
        infrastructure::audit::AuditEntry::new(
            principal_id,
            "member_invitation.accepted",
            "member_invitation",
            request_id,
        )
        .with_target(inv.id),
    )
    .await?;
    tx.commit().await?;

    // Log the new member in.
    let principal = repo::find_principal_for_user(db, inv.workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::internal("principal missing after accept"))?;
    let mut tx = db.begin(inv.workspace_id).await?;
    let (session_id, session_token) =
        repo::create_session(&mut tx, principal.id, session_ttl).await?;
    tx.commit().await?;

    Ok(LoginOutcome {
        session_token,
        csrf_token: crypto::generate_token(),
        session_id,
        me: super::me::me_for_principal(db, &principal).await?,
    })
}

fn invalid_credentials() -> AppError {
    AppError::new(ErrorCode::InvalidCredentials, "invalid email or password")
}

fn validate_password(password: &str) -> Result<(), AppError> {
    let len = password.chars().count();
    if !(crypto::PASSWORD_MIN_LEN..=crypto::PASSWORD_MAX_LEN).contains(&len) {
        return Err(AppError::validation(
            format!(
                "password must be {}–{} characters",
                crypto::PASSWORD_MIN_LEN,
                crypto::PASSWORD_MAX_LEN
            ),
            vec![contracts::Violation {
                field: "password".into(),
                code: "length".into(),
            }],
        ));
    }
    Ok(())
}

/// Static dummy argon2id hash for timing parity on unknown users.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$MDEyMzQ1Njc4OWFiY2RlZg$\
     kEfB38A4D9zGKrUJk6h2Qf2V0Uu4wGmF3cYQk1J8wS0";
