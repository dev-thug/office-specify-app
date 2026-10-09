//! Pre-auth and identity repositories (spec §4.5, A.5).
//!
//! Two access paths only:
//! - `users` / `user_password_credentials` are global tables (no
//!   `workspace_id`, no RLS) reached through `Db::begin_global()`.
//! - `user_sessions`, `member_invitations`, `service_credentials` are
//!   workspace-scoped RLS tables reached through `Db::begin_preauth()`
//!   (server-fixed `DEFAULT_WORKSPACE_ID`) — never a client-supplied
//!   workspace.
//!
//! Raw tokens are never stored or returned by lookups; stores keep sha256.

use chrono::{DateTime, Duration, Utc};
use contracts::{PrincipalKind, Role};
use uuid::Uuid;

use contracts::AppError;

use crate::crypto;
use crate::db::{Db, Tx};
use crate::error::map_sqlx;

/// Token prefix for service credentials (spec §4.4 `osk_…`).
pub const CREDENTIAL_PREFIX: &str = "osk_";
/// Login lockout: 5 failures → locked 15 minutes (spec §4.5).
pub const LOGIN_MAX_FAILURES: i32 = 5;
pub const LOGIN_LOCKOUT: Duration = Duration::minutes(15);
pub const INVITATION_TTL: Duration = Duration::hours(72);
pub const CREDENTIAL_TTL: Duration = Duration::days(365);

// ---------------------------------------------------------------- users

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub status: String,
}

/// Global-table lookup for login. Case-insensitive exact match — emails are
/// stored normalized lowercase at creation.
pub async fn find_user_by_email(db: &Db, email: &str) -> Result<Option<UserRow>, AppError> {
    let mut tx = db.begin_global().await?;
    let row = sqlx::query_as::<_, UserRow>(
        "SELECT id, email, display_name, status FROM users WHERE lower(email) = lower($1)",
    )
    .bind(email)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(row)
}

pub async fn create_user(db: &Db, email: &str, display_name: &str) -> Result<UserRow, AppError> {
    let mut tx = db.begin_global().await?;
    let row = sqlx::query_as::<_, UserRow>(
        "INSERT INTO users (email, display_name) VALUES ($1, $2)
         RETURNING id, email, display_name, status",
    )
    .bind(email)
    .bind(display_name)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(row)
}

/// In-transaction variants of the global user helpers — `users` has no
/// RLS, so they run unchanged inside the invitation-accept workspace tx
/// (R4: user create + credential set must commit with the single-use
/// claim).
pub async fn find_user_by_email_tx(
    tx: &mut Tx<'_>,
    email: &str,
) -> Result<Option<UserRow>, AppError> {
    sqlx::query_as::<_, UserRow>(
        "SELECT id, email, display_name, status FROM users WHERE lower(email) = lower($1)",
    )
    .bind(email)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn create_user_tx(
    tx: &mut Tx<'_>,
    email: &str,
    display_name: &str,
) -> Result<UserRow, AppError> {
    sqlx::query_as::<_, UserRow>(
        "INSERT INTO users (email, display_name) VALUES ($1, $2)
         RETURNING id, email, display_name, status",
    )
    .bind(email)
    .bind(display_name)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn set_password_credential_tx(
    tx: &mut Tx<'_>,
    user_id: Uuid,
    password_hash: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO user_password_credentials (user_id, password_hash)
         VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE
            SET password_hash = $2, failed_attempts = 0, locked_until = NULL,
                updated_at = now()",
    )
    .bind(user_id)
    .bind(password_hash)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// -------------------------------------------------- password credentials

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PasswordCredRow {
    pub user_id: Uuid,
    pub password_hash: String,
    pub failed_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
}

pub async fn get_password_credential(
    db: &Db,
    user_id: Uuid,
) -> Result<Option<PasswordCredRow>, AppError> {
    let mut tx = db.begin_global().await?;
    let row = sqlx::query_as::<_, PasswordCredRow>(
        "SELECT user_id, password_hash, failed_attempts, locked_until
           FROM user_password_credentials WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(row)
}

/// Upsert the password credential (invite accept / password change).
/// Resets the failure counter.
pub async fn set_password_credential(
    db: &Db,
    user_id: Uuid,
    password_hash: &str,
) -> Result<(), AppError> {
    let mut tx = db.begin_global().await?;
    sqlx::query(
        "INSERT INTO user_password_credentials (user_id, password_hash)
         VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE
            SET password_hash = $2, failed_attempts = 0, locked_until = NULL,
                updated_at = now()",
    )
    .bind(user_id)
    .bind(password_hash)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await
}

/// Record a failed login; returns the resulting locked_until. At
/// LOGIN_MAX_FAILURES sets `locked_until = now() + 15m` (spec §4.5).
/// An expired lock resets the counter to 1 on the next failed attempt —
/// the lock is a bounded penalty, not a ratchet (R5).
pub async fn record_login_failure(
    db: &Db,
    user_id: Uuid,
) -> Result<Option<DateTime<Utc>>, AppError> {
    let mut tx = db.begin_global().await?;
    let row: Option<(i32, Option<DateTime<Utc>>)> = sqlx::query_as(
        "UPDATE user_password_credentials
            SET failed_attempts = CASE
                    WHEN locked_until < now() THEN 1
                    ELSE failed_attempts + 1 END,
                locked_until = CASE
                    WHEN locked_until < now() THEN NULL
                    WHEN failed_attempts + 1 >= $2 THEN now() + interval '15 minutes'
                    ELSE locked_until END,
                updated_at = now()
          WHERE user_id = $1
          RETURNING failed_attempts, locked_until",
    )
    .bind(user_id)
    .bind(LOGIN_MAX_FAILURES)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(row.and_then(|(_, locked)| locked))
}

pub async fn reset_login_failures(db: &Db, user_id: Uuid) -> Result<(), AppError> {
    let mut tx = db.begin_global().await?;
    sqlx::query(
        "UPDATE user_password_credentials
            SET failed_attempts = 0, locked_until = NULL, updated_at = now()
          WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await
}

// ------------------------------------------------------------- principals

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PrincipalRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub kind: String,
    pub user_id: Option<Uuid>,
    pub name: String,
    pub role: String,
    pub status: String,
    pub permission_version: i64,
}

impl PrincipalRow {
    pub fn principal_kind(&self) -> PrincipalKind {
        match self.kind.as_str() {
            "agent" => PrincipalKind::Agent,
            "system" => PrincipalKind::System,
            _ => PrincipalKind::Human,
        }
    }
    pub fn role(&self) -> Role {
        match self.role.as_str() {
            "admin" => Role::Admin,
            "manager" => Role::Manager,
            "agent" => Role::Agent,
            "system" => Role::System,
            _ => Role::Member,
        }
    }
}

/// Human principal for a user inside a workspace (login path).
pub async fn find_principal_for_user(
    db: &Db,
    workspace_id: Uuid,
    user_id: Uuid,
) -> Result<Option<PrincipalRow>, AppError> {
    let mut tx = db.begin(workspace_id).await?;
    let row = sqlx::query_as::<_, PrincipalRow>(
        "SELECT id, workspace_id, kind, user_id, name, role, status, permission_version
           FROM principals
          WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(workspace_id)
    .bind(user_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(row)
}

/// Human principal for a user inside an open tx (accept-invitation path —
/// keeps the global-table read inside the accept transaction).
pub async fn find_principal_for_user_tx(
    tx: &mut Tx<'_>,
    user_id: Uuid,
) -> Result<Option<PrincipalRow>, AppError> {
    sqlx::query_as::<_, PrincipalRow>(
        "SELECT id, workspace_id, kind, user_id, name, role, status, permission_version
           FROM principals
          WHERE workspace_id = $1 AND user_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(user_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Insert a principal inside a workspace tx.
pub async fn insert_principal(
    tx: &mut Tx<'_>,
    kind: &str,
    user_id: Option<Uuid>,
    name: &str,
    role: &str,
    status: &str,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO principals (workspace_id, kind, user_id, name, role, status)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(user_id)
    .bind(name)
    .bind(role)
    .bind(status)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_principal(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
) -> Result<Option<PrincipalRow>, AppError> {
    sqlx::query_as::<_, PrincipalRow>(
        "SELECT id, workspace_id, kind, user_id, name, role, status, permission_version
           FROM principals WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// -------------------------------------------------------------- sessions

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SessionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Verified auth material for a request: session row + its principal.
pub struct SessionAuth {
    pub session: SessionRow,
    pub principal: PrincipalRow,
}

/// Create a session inside a workspace tx; returns (session id, raw token).
pub async fn create_session(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    ttl: Duration,
) -> Result<(Uuid, String), AppError> {
    let token = crypto::generate_token();
    let hash = crypto::sha256_bytes(token.as_bytes());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO user_sessions (workspace_id, principal_id, session_hash, expires_at)
         VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(&hash[..])
    .bind(Utc::now() + ttl)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((id, token))
}

/// Session token → verified session + principal (pre-auth path, A.5).
/// Caller enforces `principal.status='active'` (PRINCIPAL_DISABLED).
pub async fn lookup_session(db: &Db, raw_token: &str) -> Result<Option<SessionAuth>, AppError> {
    let hash = crypto::sha256_bytes(raw_token.as_bytes());
    let mut tx = db.begin_preauth().await?;
    let row: Option<SessionRow> = sqlx::query_as(
        "SELECT id, workspace_id, principal_id, expires_at, revoked_at
           FROM user_sessions
          WHERE session_hash = $1",
    )
    .bind(&hash[..])
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some(session) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    let principal = sqlx::query_as::<_, PrincipalRow>(
        "SELECT id, workspace_id, kind, user_id, name, role, status, permission_version
           FROM principals WHERE workspace_id = $1 AND id = $2",
    )
    .bind(session.workspace_id)
    .bind(session.principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    sqlx::query(
        "UPDATE user_sessions SET last_seen_at = now() WHERE id = $1 AND workspace_id = $2",
    )
    .bind(session.id)
    .bind(session.workspace_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(principal.map(|p| SessionAuth {
        session,
        principal: p,
    }))
}

/// Revoke inside a workspace tx (logout + audit in one commit).
pub async fn revoke_session(tx: &mut Tx<'_>, session_id: Uuid) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE user_sessions SET revoked_at = now()
          WHERE workspace_id = $1 AND id = $2 AND revoked_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(session_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

// ----------------------------------------------------- service credentials

pub struct CredentialAuth {
    pub credential_id: Uuid,
    pub principal: PrincipalRow,
    pub scopes: Vec<String>,
}

pub async fn create_service_credential(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    name: &str,
    scopes: &[String],
    ttl: Duration,
) -> Result<(Uuid, String), AppError> {
    let token = format!("{CREDENTIAL_PREFIX}{}", crypto::generate_token());
    let hash = crypto::sha256_bytes(token.as_bytes());
    let prefix = token.chars().take(12).collect::<String>();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO service_credentials
            (workspace_id, principal_id, name, key_prefix, token_hash, scopes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(name)
    .bind(prefix)
    .bind(&hash[..])
    .bind(scopes)
    .bind(Utc::now() + ttl)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((id, token))
}

/// `osk_…` bearer → credential + principal (pre-auth path).
pub async fn lookup_service_credential(
    db: &Db,
    raw_token: &str,
) -> Result<Option<CredentialAuth>, AppError> {
    let hash = crypto::sha256_bytes(raw_token.as_bytes());
    let mut tx = db.begin_preauth().await?;
    type CredentialRow = (
        Uuid,
        Uuid,
        Uuid,
        Vec<String>,
        DateTime<Utc>,
        Option<DateTime<Utc>>,
    );
    let row: Option<CredentialRow> = sqlx::query_as(
        "SELECT id, workspace_id, principal_id, scopes, expires_at, revoked_at
               FROM service_credentials WHERE token_hash = $1",
    )
    .bind(&hash[..])
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some((cred_id, ws, principal_id, scopes, expires_at, revoked_at)) = row else {
        tx.commit().await?;
        return Ok(None);
    };
    if revoked_at.is_some() || expires_at <= Utc::now() {
        tx.commit().await?;
        return Ok(None);
    }
    let principal = sqlx::query_as::<_, PrincipalRow>(
        "SELECT id, workspace_id, kind, user_id, name, role, status, permission_version
           FROM principals WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    sqlx::query(
        "UPDATE service_credentials SET last_used_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(cred_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(principal.map(|p| CredentialAuth {
        credential_id: cred_id,
        principal: p,
        scopes,
    }))
}

// ------------------------------------------------------------ invitations

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InvitationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email: String,
    pub role: String,
    pub invited_by: Uuid,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Create an invitation inside a workspace tx; returns (id, raw token).
/// The one-time URL is built by the caller — never emailed (X2).
pub async fn create_invitation(
    tx: &mut Tx<'_>,
    email: &str,
    role: &str,
    invited_by: Uuid,
) -> Result<(Uuid, String), AppError> {
    let token = crypto::generate_token();
    let hash = crypto::sha256_bytes(token.as_bytes());
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO member_invitations
            (workspace_id, email, role, token_hash, invited_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(email)
    .bind(role)
    .bind(&hash[..])
    .bind(invited_by)
    .bind(Utc::now() + INVITATION_TTL)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((id, token))
}

/// Public invitation preview by raw token (pre-auth path).
pub async fn find_invitation_by_token(
    db: &Db,
    raw_token: &str,
) -> Result<Option<InvitationRow>, AppError> {
    let hash = crypto::sha256_bytes(raw_token.as_bytes());
    let mut tx = db.begin_preauth().await?;
    let row = sqlx::query_as::<_, InvitationRow>(
        "SELECT id, workspace_id, email, role, invited_by, expires_at, accepted_at, revoked_at
           FROM member_invitations WHERE token_hash = $1",
    )
    .bind(&hash[..])
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(row)
}

/// Single-use claim of an invitation (R4): the conditional UPDATE is the
/// first statement of the accept transaction — the row lock serializes
/// concurrent accepts so exactly one wins. Returns false when the
/// invitation is already accepted, revoked or expired.
pub async fn mark_invitation_accepted(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE member_invitations SET accepted_at = now()
          WHERE workspace_id = $1 AND id = $2
            AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

// ------------------------------------------------------------- workspaces

/// Insert a workspace (global table, no RLS). Bootstrap path only.
pub async fn insert_workspace(db: &Db, name: &str) -> Result<Uuid, AppError> {
    let mut tx = db.begin_global().await?;
    let id: Uuid = sqlx::query_scalar("INSERT INTO workspaces (name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(id)
}

/// First workspace id — MVP is single-workspace (A.5). Used to derive the
/// default workspace when `DEFAULT_WORKSPACE_ID` isn't configured.
pub async fn first_workspace_id(db: &Db) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar("SELECT id FROM workspaces ORDER BY created_at, id LIMIT 1")
        .fetch_optional(db.pool())
        .await
        .map_err(map_sqlx)
}

/// Least-privilege worker check: authority bits only, never credential hashes.
pub async fn credential_authorizes(
    tx: &mut Tx<'_>,
    credential_id: Uuid,
    principal_id: Option<Uuid>,
    required_scopes: &[&str],
) -> Result<bool, AppError> {
    sqlx::query_scalar("SELECT credential_authorizes($1,$2,$3)")
        .bind(credential_id)
        .bind(principal_id)
        .bind(required_scopes)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)
}
