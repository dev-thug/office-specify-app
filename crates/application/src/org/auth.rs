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
    // Constant work (R10): every failure path runs an argon2 verify —
    // unknown email, disabled user and missing credential all verify
    // against the dummy hash so they cannot be told apart by timing.
    let cred = match &user {
        Some(u) => repo::get_password_credential(db, u.id).await?,
        None => None,
    };
    let password_ok = crypto::verify_password(
        password,
        cred.as_ref()
            .map(|c| c.password_hash.as_str())
            .unwrap_or(DUMMY_HASH),
    );
    let Some(user) = user else {
        return Err(invalid_credentials());
    };
    if user.status != "active" {
        return Err(invalid_credentials());
    }
    let Some(cred) = cred else {
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
    if !password_ok {
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

    // Password hashing is pure CPU — do it before the tx so it cannot widen
    // the race window (R4).
    let hash = crypto::hash_password(password)?;

    // R4: the conditional single-use UPDATE is the FIRST statement of the
    // accept transaction — the row lock serializes concurrent accepts and
    // the losing request gets a generic 404. User create, password set,
    // principal, session and audit all commit with the claim.
    let mut tx = db.begin(inv.workspace_id).await?;
    if !repo::mark_invitation_accepted(&mut tx, inv.id).await? {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("invitation not found or expired"));
    }
    let user = match repo::find_user_by_email_tx(&mut tx, &inv.email).await? {
        Some(u) => u,
        None => repo::create_user_tx(&mut tx, &inv.email, display_name).await?,
    };
    repo::set_password_credential_tx(&mut tx, user.id, &hash).await?;
    // A second invitation for the same email reuses the existing principal.
    let principal_id = match repo::find_principal_for_user_tx(&mut tx, user.id).await? {
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
    let (session_id, session_token) =
        repo::create_session(&mut tx, principal_id, session_ttl).await?;
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

    let principal = repo::find_principal_for_user(db, inv.workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::internal("principal missing after accept"))?;

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
