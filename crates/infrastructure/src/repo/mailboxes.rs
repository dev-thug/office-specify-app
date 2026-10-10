//! mailboxes module repository (spec §6 — owned by the module worker).
//!
//! Exposes `grant_allows` — the mailbox ACL helper other modules call for
//! every read that can reveal message content, subject, attachment, or a
//! mail-derived timeline row (§5.4/§5.6, wave-b-tasks).

use chrono::{DateTime, Utc};
use contracts::{AppError, ErrorCode};
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

// --------------------------------------------------------------------------
// mail accounts
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MailAccountRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub provider_account_id: String,
    pub primary_address: String,
    pub name: String,
    pub status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// Flat LEFT-JOIN row: account columns + the caller's grant columns
/// (all-NULL when no grant — `grant_id` marks presence).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AccountGrantJoin {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub provider_account_id: String,
    pub primary_address: String,
    pub name: String,
    pub status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub grant_id: Option<Uuid>,
    pub principal_id: Option<Uuid>,
    pub can_read: Option<bool>,
    pub can_compose: Option<bool>,
    pub can_send: Option<bool>,
    pub can_approve: Option<bool>,
    pub can_assign: Option<bool>,
    pub granted_by: Option<Uuid>,
    pub grant_version: Option<i64>,
    pub grant_created_at: Option<DateTime<Utc>>,
}

/// Account row + the caller's grant (None when none).
pub type AccountWithGrant = (MailAccountRow, Option<GrantRow>);

impl AccountGrantJoin {
    pub fn split(self) -> AccountWithGrant {
        let grant = self.grant_id.map(|gid| GrantRow {
            id: gid,
            workspace_id: self.workspace_id,
            mail_account_id: self.id,
            principal_id: self.principal_id.unwrap_or_default(),
            can_read: self.can_read.unwrap_or(false),
            can_compose: self.can_compose.unwrap_or(false),
            can_send: self.can_send.unwrap_or(false),
            can_approve: self.can_approve.unwrap_or(false),
            can_assign: self.can_assign.unwrap_or(false),
            granted_by: self.granted_by.unwrap_or_default(),
            version: self.grant_version.unwrap_or(1),
            created_at: self.grant_created_at.unwrap_or_else(Utc::now),
        });
        (
            MailAccountRow {
                id: self.id,
                workspace_id: self.workspace_id,
                connection_id: self.connection_id,
                provider: self.provider,
                provider_account_id: self.provider_account_id,
                primary_address: self.primary_address,
                name: self.name,
                status: self.status,
                last_synced_at: self.last_synced_at,
                version: self.version,
                created_at: self.created_at,
            },
            grant,
        )
    }
}

/// `ma.*` + `g.*` select list for the join queries.
const ACCOUNT_GRANT_COLS: &str = "ma.id, ma.workspace_id, ma.connection_id, ma.provider,
       ma.provider_account_id, ma.primary_address, ma.name, ma.status,
       ma.last_synced_at, ma.version, ma.created_at,
       g.id AS grant_id, g.principal_id,
       g.can_read, g.can_compose, g.can_send, g.can_approve, g.can_assign,
       g.granted_by, g.version AS grant_version, g.created_at AS grant_created_at";

/// All accounts (Admin management view), with the caller's grant joined.
pub async fn list_mail_accounts(
    tx: &mut Tx<'_>,
    caller: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<AccountWithGrant>, AppError> {
    let rows = sqlx::query_as::<_, AccountGrantJoin>(
        "SELECT {ACCOUNT_GRANT_COLS}
           FROM mail_accounts ma
           LEFT JOIN mail_account_grants g
             ON g.workspace_id = ma.workspace_id
            AND g.mail_account_id = ma.id
            AND g.principal_id = $2
          WHERE ma.workspace_id = $1
            AND ($3::timestamptz IS NULL
                 OR ma.created_at < $3
                 OR (ma.created_at = $3 AND ma.id < $4))
          ORDER BY ma.created_at DESC, ma.id DESC
          LIMIT $5"
            .replace("{ACCOUNT_GRANT_COLS}", ACCOUNT_GRANT_COLS)
            .as_str(),
    )
    .bind(tx.workspace_id())
    .bind(caller)
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(rows.into_iter().map(AccountGrantJoin::split).collect())
}

/// Accounts the caller can actually read (grant `can_read`, §5.6).
pub async fn list_accessible_mail_accounts(
    tx: &mut Tx<'_>,
    caller: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<AccountWithGrant>, AppError> {
    let rows = sqlx::query_as::<_, AccountGrantJoin>(
        "SELECT {ACCOUNT_GRANT_COLS}
           FROM mail_accounts ma
           JOIN mail_account_grants g
             ON g.workspace_id = ma.workspace_id
            AND g.mail_account_id = ma.id
            AND g.principal_id = $2
            AND g.can_read
          WHERE ma.workspace_id = $1
            AND ($3::timestamptz IS NULL
                 OR ma.created_at < $3
                 OR (ma.created_at = $3 AND ma.id < $4))
          ORDER BY ma.created_at DESC, ma.id DESC
          LIMIT $5"
            .replace("{ACCOUNT_GRANT_COLS}", ACCOUNT_GRANT_COLS)
            .as_str(),
    )
    .bind(tx.workspace_id())
    .bind(caller)
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(rows.into_iter().map(AccountGrantJoin::split).collect())
}

pub async fn get_mail_account(
    tx: &mut Tx<'_>,
    caller: Uuid,
    id: Uuid,
) -> Result<Option<AccountWithGrant>, AppError> {
    let row = sqlx::query_as::<_, AccountGrantJoin>(
        "SELECT {ACCOUNT_GRANT_COLS}
           FROM mail_accounts ma
           LEFT JOIN mail_account_grants g
             ON g.workspace_id = ma.workspace_id
            AND g.mail_account_id = ma.id
            AND g.principal_id = $2
          WHERE ma.workspace_id = $1 AND ma.id = $3"
            .replace("{ACCOUNT_GRANT_COLS}", ACCOUNT_GRANT_COLS)
            .as_str(),
    )
    .bind(tx.workspace_id())
    .bind(caller)
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row.map(AccountGrantJoin::split))
}

/// Resolve an account for an internal mailbox-bound provider. Workspace RLS
/// and an explicit workspace predicate are always applied; caller grants are
/// checked by the application endpoint before binding.
pub async fn get_mail_account_scoped(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<MailAccountRow>, AppError> {
    sqlx::query_as::<_, MailAccountRow>(
        "SELECT id, workspace_id, connection_id, provider, provider_account_id,
                primary_address, name, status, last_synced_at, version, created_at
           FROM mail_accounts
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Upsert the account after a successful OAuth exchange: one
/// `(connection, provider_account_id)` maps to one account (UNIQUE).
pub async fn upsert_mail_account(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
    provider_account_id: &str,
    primary_address: &str,
    name: &str,
) -> Result<MailAccountRow, AppError> {
    sqlx::query_as::<_, MailAccountRow>(
        "INSERT INTO mail_accounts
            (workspace_id, connection_id, provider_account_id, primary_address, name, status)
         VALUES ($1, $2, $3, $4, $5, 'active')
         ON CONFLICT (workspace_id, connection_id, provider_account_id)
         DO UPDATE SET status = 'active', primary_address = $4, version = mail_accounts.version + 1
         RETURNING id, workspace_id, connection_id, provider, provider_account_id,
                   primary_address, name, status, last_synced_at, version, created_at",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .bind(provider_account_id)
    .bind(primary_address)
    .bind(name)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn set_mail_account_status(
    tx: &mut Tx<'_>,
    id: Uuid,
    status: &str,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE mail_accounts SET status = $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(status)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Mail accounts hanging off a connection (reauth fan-out).
pub async fn account_ids_for_connection(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM mail_accounts WHERE workspace_id = $1 AND connection_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// grants
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GrantRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub mail_account_id: Uuid,
    pub principal_id: Uuid,
    pub can_read: bool,
    pub can_compose: bool,
    pub can_send: bool,
    pub can_approve: bool,
    pub can_assign: bool,
    pub granted_by: Uuid,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl GrantRow {
    /// `mail.*` action list for `allowed_actions` (B.3.1).
    pub fn allowed_actions(&self) -> Vec<String> {
        [
            (self.can_read, "mail.read"),
            (self.can_compose, "mail.compose"),
            (self.can_send, "mail.send"),
            (self.can_approve, "mail.approve"),
            (self.can_assign, "mail.assign"),
        ]
        .into_iter()
        .filter(|(ok, _)| *ok)
        .map(|(_, s)| s.to_string())
        .collect()
    }
}

const GRANT_COLS: &str = "id, workspace_id, mail_account_id, principal_id,
       can_read, can_compose, can_send, can_approve, can_assign,
       granted_by, version, created_at";

/// The cross-module mailbox ACL helper (wave-b-tasks): does `principal`
/// hold `action` on `mail_account_id`? `action` accepts `read` /
/// `can_read` / `mail.read` forms; unknown actions are a caller bug →
/// validation error rather than a silent `false`.
///
/// Admins get NOTHING implicit here (A.7/§5.6 — management listing is not
/// content access).
pub async fn grant_allows(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    mail_account_id: Uuid,
    action: impl AsRef<str>,
) -> Result<bool, AppError> {
    let column = match domain::mailboxes::normalize_grant_action(action.as_ref()) {
        Some(a) => format!("can_{a}"),
        None => {
            return Err(AppError::validation(
                format!("unknown grant action '{}'", action.as_ref()),
                vec![],
            ));
        }
    };
    let sql = format!(
        "SELECT {column} FROM mail_account_grants
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3"
    );
    let flag: Option<bool> = sqlx::query_scalar(&sql)
        .bind(tx.workspace_id())
        .bind(mail_account_id)
        .bind(principal_id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)?;
    Ok(flag.unwrap_or(false))
}

pub async fn get_grant(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    principal_id: Uuid,
) -> Result<Option<GrantRow>, AppError> {
    sqlx::query_as::<_, GrantRow>(&format!(
        "SELECT {GRANT_COLS} FROM mail_account_grants
              WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3"
    ))
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Grant row + principal display info for the admin grant list.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct GrantJoinRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub mail_account_id: Uuid,
    pub principal_id: Uuid,
    pub can_read: bool,
    pub can_compose: bool,
    pub can_send: bool,
    pub can_approve: bool,
    pub can_assign: bool,
    pub granted_by: Uuid,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub principal_name: String,
    pub principal_kind: String,
}

/// Grant list joined to principals for the admin view.
pub async fn list_grants(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
) -> Result<Vec<GrantJoinRow>, AppError> {
    sqlx::query_as::<_, GrantJoinRow>(
        "SELECT g.id, g.workspace_id, g.mail_account_id, g.principal_id,
                g.can_read, g.can_compose, g.can_send, g.can_approve, g.can_assign,
                g.granted_by, g.version, g.created_at,
                p.name AS principal_name, p.kind AS principal_kind
           FROM mail_account_grants g
           JOIN principals p
             ON p.workspace_id = g.workspace_id AND p.id = g.principal_id
          WHERE g.workspace_id = $1 AND g.mail_account_id = $2
          ORDER BY p.name, g.id",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Grant-or-leave upsert used by the OAuth callback (A.7: the connector is
/// explicitly granted full flags — an existing grant is never clobbered).
pub async fn upsert_grant(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    principal_id: Uuid,
    flags: &GrantFlags,
    granted_by: Uuid,
) -> Result<GrantRow, AppError> {
    sqlx::query(
        "INSERT INTO mail_account_grants
            (workspace_id, mail_account_id, principal_id,
             can_read, can_compose, can_send, can_approve, can_assign, granted_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (workspace_id, mail_account_id, principal_id) DO NOTHING",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(principal_id)
    .bind(flags.can_read)
    .bind(flags.can_compose)
    .bind(flags.can_send)
    .bind(flags.can_approve)
    .bind(flags.can_assign)
    .bind(granted_by)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    get_grant(tx, mail_account_id, principal_id)
        .await?
        .ok_or_else(|| AppError::internal("grant missing after upsert"))
}

/// Insert a new grant. The `(workspace, mail_account, principal)` UNIQUE
/// race maps to 412 — B.3.2: a first grant that loses the race sees
/// `VERSION_MISMATCH`.
pub async fn insert_grant(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    principal_id: Uuid,
    flags: &GrantFlags,
    granted_by: Uuid,
) -> Result<GrantRow, AppError> {
    let res = sqlx::query_as::<_, GrantRow>(&format!(
        "INSERT INTO mail_account_grants
                (workspace_id, mail_account_id, principal_id,
                 can_read, can_compose, can_send, can_approve, can_assign, granted_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING {GRANT_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(principal_id)
    .bind(flags.can_read)
    .bind(flags.can_compose)
    .bind(flags.can_send)
    .bind(flags.can_approve)
    .bind(flags.can_assign)
    .bind(granted_by)
    .fetch_one(tx.conn())
    .await;
    match res {
        Ok(row) => Ok(row),
        Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("23505") => {
            Err(AppError::new(
                ErrorCode::VersionMismatch,
                "a grant for this principal already exists",
            ))
        }
        Err(e) => Err(map_sqlx(e)),
    }
}

/// Optimistic update; None = stale version (412).
pub async fn update_grant(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    principal_id: Uuid,
    flags: &GrantFlags,
    expected_version: i64,
) -> Result<Option<GrantRow>, AppError> {
    sqlx::query_as::<_, GrantRow>(&format!(
        "UPDATE mail_account_grants
                SET can_read = $4, can_compose = $5, can_send = $6,
                    can_approve = $7, can_assign = $8,
                    version = version + 1
              WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3
                AND version = $9
              RETURNING {GRANT_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(principal_id)
    .bind(flags.can_read)
    .bind(flags.can_compose)
    .bind(flags.can_send)
    .bind(flags.can_approve)
    .bind(flags.can_assign)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, Copy)]
pub struct GrantFlags {
    pub can_read: bool,
    pub can_compose: bool,
    pub can_send: bool,
    pub can_approve: bool,
    pub can_assign: bool,
}

/// §5.5: a permission change bumps `permission_version` so cached
/// permissions invalidate. Returns the new value.
pub async fn bump_permission_version(tx: &mut Tx<'_>, principal_id: Uuid) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "UPDATE principals SET permission_version = permission_version + 1
          WHERE workspace_id = $1 AND id = $2
          RETURNING permission_version",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// integration connections (OAuth credential references — never secrets)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IntegrationConnectionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub provider_subject: Option<String>,
    pub region: String,
    pub secret_ref: String,
    pub scopes: Vec<String>,
    pub status: String,
    pub connected_by: Uuid,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_error_code: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// `secret_ref` convention: `secret://zoho_{provider}/{connection_id}` —
/// the SecretStore key; token plaintext never reaches the DB.
pub fn connection_secret_ref(provider: &str, connection_id: Uuid) -> String {
    format!("secret://zoho_{provider}/{connection_id}")
}

/// Create a `pending` connection for an OAuth start (mail or calendar).
pub async fn insert_pending_connection(
    tx: &mut Tx<'_>,
    id: Uuid,
    provider: &str,
    region: &str,
    scopes: &[String],
    connected_by: Uuid,
) -> Result<IntegrationConnectionRow, AppError> {
    sqlx::query_as::<_, IntegrationConnectionRow>(
        "INSERT INTO integration_connections
            (id, workspace_id, provider, region, secret_ref, scopes, status, connected_by)
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7)
         RETURNING id, workspace_id, provider, provider_subject, region, secret_ref,
                   scopes, status, connected_by, last_checked_at, last_error_code,
                   version, created_at",
    )
    .bind(id)
    .bind(tx.workspace_id())
    .bind(provider)
    .bind(region)
    .bind(connection_secret_ref(provider, id))
    .bind(scopes)
    .bind(connected_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_connection(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<IntegrationConnectionRow>, AppError> {
    sqlx::query_as::<_, IntegrationConnectionRow>(
        "SELECT id, workspace_id, provider, provider_subject, region, secret_ref,
                scopes, status, connected_by, last_checked_at, last_error_code,
                version, created_at
           FROM integration_connections WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Lock a connection while loading or refreshing its mailbox token bundle.
pub async fn get_connection_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<IntegrationConnectionRow>, AppError> {
    sqlx::query_as::<_, IntegrationConnectionRow>(
        "SELECT id, workspace_id, provider, provider_subject, region, secret_ref,
                scopes, status, connected_by, last_checked_at, last_error_code,
                version, created_at
           FROM integration_connections
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OAuthRequestRow {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub principal_id: Uuid,
    pub session_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

/// Persist a hash of real Mail OAuth state, never the state value itself.
pub async fn insert_oauth_request(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
    principal_id: Uuid,
    session_id: Uuid,
    state_hash: &[u8],
    expires_at: DateTime<Utc>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO integration_oauth_requests
            (workspace_id, connection_id, principal_id, session_id, state_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .bind(principal_id)
    .bind(session_id)
    .bind(state_hash)
    .bind(expires_at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Atomically consume a state hash for its recorded human/session owner.
pub async fn claim_oauth_request(
    tx: &mut Tx<'_>,
    state_hash: &[u8],
    principal_id: Uuid,
    session_id: Uuid,
) -> Result<Option<OAuthRequestRow>, AppError> {
    sqlx::query_as::<_, OAuthRequestRow>(
        "UPDATE integration_oauth_requests
            SET used_at = now()
          WHERE workspace_id = $1 AND state_hash = $2 AND principal_id = $3
            AND session_id = $4 AND used_at IS NULL AND expires_at > now()
          RETURNING id, connection_id, principal_id, session_id, expires_at, used_at",
    )
    .bind(tx.workspace_id())
    .bind(state_hash)
    .bind(principal_id)
    .bind(session_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Recheck the administrator and browser session while the OAuth callback is
/// activating a connection, including the permission version captured at
/// authorization start.
pub async fn active_mail_oauth_owner(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    session_id: Uuid,
    permission_version: i64,
) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
              FROM principals p
              JOIN user_sessions s
                ON s.workspace_id = p.workspace_id AND s.principal_id = p.id
             WHERE p.workspace_id = $1 AND p.id = $2 AND p.kind = 'human'
               AND p.role = 'admin' AND p.status = 'active'
               AND p.permission_version = $4
               AND s.id = $3 AND s.revoked_at IS NULL AND s.expires_at > now()
             FOR SHARE OF p, s
         )",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(session_id)
    .bind(permission_version)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Persist provider folder coordinates learned from a genuine message-list
/// response. These IDs contain no user message data.
pub async fn record_provider_message_locations(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    locations: &[(String, String)],
) -> Result<(), AppError> {
    for (message_id, folder_id) in locations {
        if !valid_provider_id(message_id) || !valid_provider_id(folder_id) {
            return Err(AppError::validation(
                "invalid Zoho Mail provider location",
                vec![],
            ));
        }
        sqlx::query(
            "INSERT INTO mail_provider_message_locations
                (workspace_id, mail_account_id, provider_message_id, provider_folder_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (workspace_id, mail_account_id, provider_message_id)
             DO UPDATE SET provider_folder_id = EXCLUDED.provider_folder_id,
                           updated_at = now()",
        )
        .bind(tx.workspace_id())
        .bind(mail_account_id)
        .bind(message_id)
        .bind(folder_id)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }
    Ok(())
}

/// Find the folder previously observed for a real provider message ID.
pub async fn provider_folder_id_for_message(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    provider_message_id: &str,
) -> Result<Option<String>, AppError> {
    if !valid_provider_id(provider_message_id) {
        return Ok(None);
    }
    sqlx::query_scalar(
        "SELECT provider_folder_id FROM mail_provider_message_locations
          WHERE workspace_id = $1 AND mail_account_id = $2 AND provider_message_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(provider_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProviderReplyTargetRow {
    pub purpose: String,
    pub has_reply_target: bool,
    pub provider_message_id: Option<String>,
}

/// Resolve an Office outbound correlation UUID to the Zoho provider ID of its
/// stored reply target. Both workspace and mailbox are part of the join. The
/// boolean distinguishes a new message from a reply whose provider ID is not
/// known yet, so callers never silently send the latter as a new email.
pub async fn provider_reply_target(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    outbound_id: Uuid,
) -> Result<Option<ProviderReplyTargetRow>, AppError> {
    sqlx::query_as::<_, ProviderReplyTargetRow>(
        "SELECT o.purpose,
                o.reply_to_message_id IS NOT NULL AS has_reply_target,
                m.provider_message_id
           FROM outbound_messages o
           LEFT JOIN email_messages m
             ON m.workspace_id = o.workspace_id
            AND m.mail_account_id = o.mail_account_id
            AND m.id = o.reply_to_message_id
            AND m.direction = 'inbound'
            AND m.provider_message_id ~ '^[0-9]{1,64}$'
          WHERE o.workspace_id = $1 AND o.id = $2 AND o.mail_account_id = $3
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(outbound_id)
    .bind(mail_account_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

fn valid_provider_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_digit())
}

/// Successful exchange → `active` with the provider's subject + scopes.
pub async fn mark_connection_active(
    tx: &mut Tx<'_>,
    id: Uuid,
    provider_subject: &str,
    scopes: &[String],
) -> Result<Option<IntegrationConnectionRow>, AppError> {
    sqlx::query_as::<_, IntegrationConnectionRow>(
        "UPDATE integration_connections
            SET status = 'active', provider_subject = $3, scopes = $4,
                last_checked_at = now(), last_error_code = NULL, version = version + 1
          WHERE workspace_id = $1 AND id = $2
          RETURNING id, workspace_id, provider, provider_subject, region, secret_ref,
                    scopes, status, connected_by, last_checked_at, last_error_code,
                    version, created_at",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(provider_subject)
    .bind(scopes)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Failed exchange/refresh → `reauth_required` (§16.3) + error code.
pub async fn mark_connection_reauth(
    tx: &mut Tx<'_>,
    id: Uuid,
    error_code: &str,
) -> Result<Option<IntegrationConnectionRow>, AppError> {
    sqlx::query_as::<_, IntegrationConnectionRow>(
        "UPDATE integration_connections
            SET status = 'reauth_required', last_error_code = $3,
                last_checked_at = now(), version = version + 1
          WHERE workspace_id = $1 AND id = $2
          RETURNING id, workspace_id, provider, provider_subject, region, secret_ref,
                    scopes, status, connected_by, last_checked_at, last_error_code,
                    version, created_at",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(error_code)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// sender identities (referenced by delivery channels)
// --------------------------------------------------------------------------

/// Insert a sender identity (used by delivery-channel setup + tests; the
/// address-verification flow lives in the sending module).
pub async fn insert_sender_identity(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    address: &str,
    display_name: &str,
    verified: bool,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, $3, $4,
                CASE WHEN $5 THEN 'verified' ELSE 'pending' END,
                CASE WHEN $5 THEN now() ELSE NULL END)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(address)
    .bind(display_name)
    .bind(verified)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_sender_identity(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<(Uuid, String)>, AppError> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT mail_account_id, status FROM sender_identities
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// delivery channels (+ reviews)
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DeliveryChannelRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub mail_account_id: Uuid,
    pub sender_identity_id: Uuid,
    pub connection_id: Uuid,
    pub provider: String,
    pub purpose: String,
    pub name: String,
    pub policy_status: String,
    pub enabled: bool,
    pub capabilities: serde_json::Value,
    pub internal_daily_cap: Option<i32>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const CHANNEL_COLS: &str = "id, workspace_id, mail_account_id, sender_identity_id,
       connection_id, provider, purpose, name, policy_status, enabled,
       capabilities, internal_daily_cap, version, created_at";

/// Channel list. `restricted_to` = Some(principal) filters to channels on
/// mail accounts that principal has `can_read` on (§5.6); None = full
/// management view.
pub async fn list_delivery_channels(
    tx: &mut Tx<'_>,
    restricted_to: Option<Uuid>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<DeliveryChannelRow>, AppError> {
    sqlx::query_as::<_, DeliveryChannelRow>(&format!(
        "SELECT {CHANNEL_COLS} FROM delivery_channels dc
              WHERE dc.workspace_id = $1
                AND dc.provider <> 'zoho_campaigns'
                AND ($2::uuid IS NULL OR EXISTS (
                        SELECT 1 FROM mail_account_grants g
                         WHERE g.workspace_id = dc.workspace_id
                           AND g.mail_account_id = dc.mail_account_id
                           AND g.principal_id = $2 AND g.can_read))
                AND ($3::timestamptz IS NULL
                     OR dc.created_at < $3
                     OR (dc.created_at = $3 AND dc.id < $4))
              ORDER BY dc.created_at DESC, dc.id DESC
              LIMIT $5"
    ))
    .bind(tx.workspace_id())
    .bind(restricted_to)
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_delivery_channel(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<DeliveryChannelRow>, AppError> {
    sqlx::query_as::<_, DeliveryChannelRow>(&format!(
        "SELECT {CHANNEL_COLS} FROM delivery_channels WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[allow(clippy::too_many_arguments)] // Mirrors the validated delivery_channels INSERT columns.
pub async fn insert_delivery_channel(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    sender_identity_id: Uuid,
    connection_id: Uuid,
    provider: &str,
    purpose: &str,
    name: &str,
    capabilities: &serde_json::Value,
    internal_daily_cap: Option<i32>,
) -> Result<DeliveryChannelRow, AppError> {
    sqlx::query_as::<_, DeliveryChannelRow>(&format!(
        "INSERT INTO delivery_channels
                (workspace_id, mail_account_id, sender_identity_id, connection_id,
                 provider, purpose, name, policy_status, enabled, capabilities,
                 internal_daily_cap)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'unverified', false, $8, $9)
             RETURNING {CHANNEL_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(mail_account_id)
    .bind(sender_identity_id)
    .bind(connection_id)
    .bind(provider)
    .bind(purpose)
    .bind(name)
    .bind(capabilities)
    .bind(internal_daily_cap)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Optimistic patch (enabled/daily_cap); None = stale version.
pub async fn update_delivery_channel(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    enabled: bool,
    internal_daily_cap: Option<i32>,
) -> Result<Option<DeliveryChannelRow>, AppError> {
    sqlx::query_as::<_, DeliveryChannelRow>(&format!(
        "UPDATE delivery_channels
                SET enabled = $3, internal_daily_cap = $4, version = version + 1
              WHERE workspace_id = $1 AND id = $2 AND version = $5
              RETURNING {CHANNEL_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(enabled)
    .bind(internal_daily_cap)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Latest review conclusion moves `policy_status`; a non-`allowed` verdict
/// also force-disables the channel (DDL CHECK would reject enabled +
/// non-allowed anyway).
pub async fn apply_review_result(
    tx: &mut Tx<'_>,
    channel_id: Uuid,
    result: &str,
) -> Result<Option<DeliveryChannelRow>, AppError> {
    sqlx::query_as::<_, DeliveryChannelRow>(&format!(
        "UPDATE delivery_channels
                SET policy_status = $3,
                    enabled = enabled AND ($3 = 'allowed'),
                    version = version + 1
              WHERE workspace_id = $1 AND id = $2
              RETURNING {CHANNEL_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(channel_id)
    .bind(result)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChannelReviewRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub delivery_channel_id: Uuid,
    pub reviewed_by: Uuid,
    pub result: String,
    pub purpose: String,
    pub policy_url: Option<String>,
    pub contract_evidence_ref: Option<String>,
    pub notes: Option<String>,
    pub reviewed_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Append-only review insert (spec §4.9: enabling needs one of these with
/// `result = 'allowed'`).
#[allow(clippy::too_many_arguments)] // Mirrors the append-only channel_reviews INSERT columns.
pub async fn insert_channel_review(
    tx: &mut Tx<'_>,
    channel_id: Uuid,
    reviewed_by: Uuid,
    result: &str,
    purpose: &str,
    policy_url: Option<&str>,
    contract_evidence_ref: Option<&str>,
    notes: Option<&str>,
    expires_at: Option<DateTime<Utc>>,
) -> Result<ChannelReviewRow, AppError> {
    sqlx::query_as::<_, ChannelReviewRow>(
        "INSERT INTO channel_reviews
            (workspace_id, delivery_channel_id, reviewed_by, result, purpose,
             policy_url, contract_evidence_ref, notes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING id, workspace_id, delivery_channel_id, reviewed_by, result,
                   purpose, policy_url, contract_evidence_ref, notes, reviewed_at,
                   expires_at, created_at",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .bind(reviewed_by)
    .bind(result)
    .bind(purpose)
    .bind(policy_url)
    .bind(contract_evidence_ref)
    .bind(notes)
    .bind(expires_at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Latest `allowed` review — activation gate evidence (§4.9). An expired
/// allowance does not count; the caller maps that to
/// `PROVIDER_POLICY_UNVERIFIED`.
pub async fn latest_allow_review(
    tx: &mut Tx<'_>,
    channel_id: Uuid,
) -> Result<Option<ChannelReviewRow>, AppError> {
    sqlx::query_as::<_, ChannelReviewRow>(
        "SELECT id, workspace_id, delivery_channel_id, reviewed_by, result,
                purpose, policy_url, contract_evidence_ref, notes, reviewed_at,
                expires_at, created_at
           FROM channel_reviews
          WHERE workspace_id = $1 AND delivery_channel_id = $2 AND result = 'allowed'
          ORDER BY reviewed_at DESC, id DESC
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(channel_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// --------------------------------------------------------------------------
// calendar connections
// --------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CalendarConnectionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub provider: String,
    pub host_principal_id: Uuid,
    pub connection_id: Uuid,
    pub provider_calendar_id: String,
    pub status: String,
    pub include_in_busy: bool,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// `host` = Some(id) restricts to that host's connections (member/agent
/// self view); None = all (Admin/Manager).
pub async fn list_calendar_connections(
    tx: &mut Tx<'_>,
    host: Option<Uuid>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<CalendarConnectionRow>, AppError> {
    sqlx::query_as::<_, CalendarConnectionRow>(
        "SELECT id, workspace_id, provider, host_principal_id, connection_id,
                provider_calendar_id, status, include_in_busy, version, created_at
           FROM calendar_connections
          WHERE workspace_id = $1
            AND ($2::uuid IS NULL OR host_principal_id = $2)
            AND ($3::timestamptz IS NULL
                 OR created_at < $3
                 OR (created_at = $3 AND id < $4))
          ORDER BY created_at DESC, id DESC
          LIMIT $5",
    )
    .bind(tx.workspace_id())
    .bind(host)
    .bind(after.map(|(t, _)| t))
    .bind(after.map(|(_, i)| i).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Upsert on `(workspace, connection, provider_calendar_id)` — the OAuth
/// callback is replay-safe.
pub async fn upsert_calendar_connection(
    tx: &mut Tx<'_>,
    host_principal_id: Uuid,
    connection_id: Uuid,
    provider_calendar_id: &str,
) -> Result<CalendarConnectionRow, AppError> {
    sqlx::query_as::<_, CalendarConnectionRow>(
        "INSERT INTO calendar_connections
            (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
         VALUES ($1, $2, $3, $4, 'active')
         ON CONFLICT (workspace_id, connection_id, provider_calendar_id)
         DO UPDATE SET status = 'active', version = calendar_connections.version + 1
         RETURNING id, workspace_id, provider, host_principal_id, connection_id,
                   provider_calendar_id, status, include_in_busy, version, created_at",
    )
    .bind(tx.workspace_id())
    .bind(host_principal_id)
    .bind(connection_id)
    .bind(provider_calendar_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}
