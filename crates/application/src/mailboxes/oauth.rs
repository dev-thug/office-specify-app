//! Zoho OAuth connection flow (spec §6, §14.1, B.2 mail-accounts rows).
//!
//! `POST .../connection-requests` creates a `pending`
//! `integration_connections` row and returns the provider authorization URL
//! carrying an HMAC-signed, expiring `state` — no network I/O at request
//! time. `GET /integrations/zoho/callback` verifies `state`, exchanges the
//! code through the [`TokenExchange`] abstraction (fake in dev/test, never a
//! real provider call), stores tokens in the `SecretStore` under
//! `secret_ref` — never as DB plaintext — and activates the connection.
//!
//! Deviation (report): `Config` has no Zoho fields yet, so client id and
//! the public base URL come from `OUTREACH_ZOHO_CLIENT_ID` /
//! `OUTREACH_PUBLIC_BASE_URL` env with dev/test defaults; a `TokenExchange`
//! trait lives here until the shared provider traits grow an OAuth method
//! (shared-change request in the task report).

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use contracts::mailboxes::{CalendarConnectionResponse, OAuthStartResponse};
use contracts::{AppError, Ctx, ErrorCode, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::config::Config;
use infrastructure::crypto;
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::providers::ProviderError;
use infrastructure::repo::mailboxes as repo;
use infrastructure::secrets::{FileSecretStore, SecretStore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `mailbox.reauth_required` (integration-contracts / §16.2) — emitted on
/// token-exchange failure so operations can alert.
pub const EVENT_MAILBOX_REAUTH: &str = "mailbox.reauth_required";

/// OAuth `state` validity window.
const STATE_TTL: Duration = Duration::minutes(10);

// --------------------------------------------------------------------------
// config resolution (deviation — see module docs)
// --------------------------------------------------------------------------

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Zoho OAuth client id. Dev/test falls back to a literal so the flow is
/// exercisable without real credentials.
pub fn zoho_client_id(config: &Config) -> Option<String> {
    env("OUTREACH_ZOHO_CLIENT_ID").or_else(|| {
        config
            .app_env
            .is_dev_or_test()
            .then(|| "dev-zoho-client-id".to_string())
    })
}

/// Public base URL for redirects (`OUTREACH_PUBLIC_BASE_URL` or the bound
/// api address).
pub fn public_base(config: &Config) -> String {
    env("OUTREACH_PUBLIC_BASE_URL").unwrap_or_else(|| format!("http://{}", config.api_addr))
}

fn redirect_uri(config: &Config) -> String {
    format!("{}/api/v1/integrations/zoho/callback", public_base(config))
}

// --------------------------------------------------------------------------
// token exchange abstraction
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct TokenBundle {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_seconds: i64,
    /// Provider account subject → `integration_connections.provider_subject`.
    pub provider_subject: String,
    /// Mail flow: provider account id + primary address for the mailbox row.
    pub provider_account_id: Option<String>,
    pub primary_address: Option<String>,
    /// Calendar flow: provider calendar id.
    pub provider_calendar_id: Option<String>,
    pub scopes: Vec<String>,
}

/// OAuth code exchange — the provider-trait shape this module needs.
/// Implemented by [`FakeTokenExchange`] in dev/test and a non-calling
/// skeleton elsewhere (real HTTP exchange is not implemented in Wave B).
#[async_trait]
pub trait TokenExchange: Send + Sync {
    async fn exchange(
        &self,
        code: &str,
        redirect_uri: &str,
        region: &str,
    ) -> Result<TokenBundle, ProviderError>;
}

/// Deterministic dev/test exchanger. `fail*` codes simulate provider
/// rejection → `mailbox.reauth_required`; everything else succeeds with
/// synthetic account ids (never real provider data).
struct FakeTokenExchange;

#[async_trait]
impl TokenExchange for FakeTokenExchange {
    async fn exchange(
        &self,
        code: &str,
        _redirect_uri: &str,
        _region: &str,
    ) -> Result<TokenBundle, ProviderError> {
        if code.starts_with("fail") {
            return Err(ProviderError::ReauthRequired);
        }
        let short = &crypto::sha256_hex(code.as_bytes())[..8];
        Ok(TokenBundle {
            access_token: format!("fake-access-{short}"),
            refresh_token: Some(format!("fake-refresh-{short}")),
            expires_in_seconds: 3600,
            provider_subject: format!("fake-subject-{short}"),
            provider_account_id: Some(format!("fake-acct-{short}")),
            primary_address: Some(format!("shared-{short}@test.dev")),
            provider_calendar_id: Some(format!("fake-cal-{short}")),
            scopes: vec![],
        })
    }
}

/// Skeleton for real envs — never performs I/O (spec §4.9: no real
/// provider calls in Wave B).
struct UnimplementedExchange;

#[async_trait]
impl TokenExchange for UnimplementedExchange {
    async fn exchange(
        &self,
        _code: &str,
        _redirect_uri: &str,
        _region: &str,
    ) -> Result<TokenBundle, ProviderError> {
        Err(ProviderError::unavailable(
            "zoho oauth exchange not implemented",
        ))
    }
}

pub fn token_exchanger(config: &Config) -> Arc<dyn TokenExchange> {
    if config.app_env.is_dev_or_test() {
        Arc::new(FakeTokenExchange)
    } else {
        Arc::new(UnimplementedExchange)
    }
}

fn secret_store(config: &Config) -> Result<FileSecretStore, AppError> {
    let key = config
        .secret_store_key
        .ok_or_else(|| AppError::internal("OUTREACH_SECRET_KEY not configured"))?;
    Ok(FileSecretStore::new(&config.secrets_file, key))
}

// --------------------------------------------------------------------------
// signed `state` (§14.1): b64(json).b64(hmac) — binds connection+workspace
// --------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct OAuthState {
    v: u8,
    /// `mail` or `calendar`.
    flow: String,
    workspace_id: Uuid,
    connection_id: Uuid,
    /// Calendar flow: the host principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<Uuid>,
    nonce: String,
    exp: i64,
}

fn sign_state(state: &OAuthState, hmac_key: &[u8]) -> String {
    let body = crypto::b64_encode(&serde_json::to_vec(state).unwrap_or_else(|_| b"{}".to_vec()));
    let sig = crypto::b64_encode(&crypto::hmac_sha256(hmac_key, body.as_bytes()));
    format!("{body}.{sig}")
}

fn verify_state(encoded: &str, hmac_key: &[u8]) -> Result<OAuthState, AppError> {
    let invalid = || AppError::validation("invalid oauth state", vec![]);
    let (body, sig) = encoded.split_once('.').ok_or_else(invalid)?;
    let sig_bytes = crypto::b64_decode(sig).ok_or_else(invalid)?;
    if !crypto::hmac_sha256_verify(hmac_key, body.as_bytes(), &sig_bytes) {
        return Err(invalid());
    }
    let raw = crypto::b64_decode(body).ok_or_else(invalid)?;
    let state: OAuthState = serde_json::from_slice(&raw).map_err(|_| invalid())?;
    if state.v != 1 || state.exp <= Utc::now().timestamp() {
        return Err(invalid());
    }
    Ok(state)
}

// --------------------------------------------------------------------------
// connection-requests (mail + calendar share the builder)
// --------------------------------------------------------------------------

/// `POST /mail-accounts/connection-requests` (Admin) — a `pending`
/// `zoho_mail` connection + authorization URL. No network I/O.
pub async fn request_mail_connection(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    region: &str,
) -> Result<OAuthStartResponse, AppError> {
    super::require_admin_audited(db, ctx, "mailbox.connect", "integration_connection", None)
        .await?;
    start_oauth(
        db,
        ctx,
        config,
        region,
        "zoho_mail",
        domain::mailboxes::ZOHO_MAIL_SCOPES,
        None,
    )
    .await
}

/// `POST /mail-accounts/{id}/reconnect` (Admin) — reauthorization bound to
/// the account's existing connection.
pub async fn reconnect_mail_account(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    mail_account_id: Uuid,
) -> Result<OAuthStartResponse, AppError> {
    super::require_admin_audited(
        db,
        ctx,
        "mailbox.reconnect",
        "integration_connection",
        Some(mail_account_id),
    )
    .await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let (account, _grant) = repo::get_mail_account(&mut tx, ctx.principal_id, mail_account_id)
        .await?
        .ok_or_else(|| AppError::not_found("mail account not found"))?;
    let conn = repo::get_connection(&mut tx, account.connection_id)
        .await?
        .ok_or_else(|| AppError::internal("connection missing for account"))?;
    tx.commit().await?;
    if conn.provider != "zoho_mail" {
        return Err(AppError::validation(
            "only zoho_mail connections re-authenticate through this flow",
            vec![],
        ));
    }
    let state = OAuthState {
        v: 1,
        flow: "mail".into(),
        workspace_id: ctx.workspace_id,
        connection_id: conn.id,
        host: None,
        nonce: crypto::generate_token(),
        exp: (Utc::now() + STATE_TTL).timestamp(),
    };
    let expires_at = Utc::now() + STATE_TTL;
    audit_connect_start(db, ctx, conn.id, "mailbox.reconnect").await?;
    Ok(OAuthStartResponse {
        connection_id: conn.id,
        authorization_url: authorization_url(
            config,
            &conn.region,
            domain::mailboxes::ZOHO_MAIL_SCOPES,
            &sign_state(&state, &config.hmac_key),
        )?,
        state_expires_at: expires_at,
    })
}

/// `POST /calendar-connections/connection-requests` (host or Admin).
/// `host` defaults to the caller; naming another host requires Admin.
pub async fn request_calendar_connection(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    host: Option<Uuid>,
    region: &str,
) -> Result<OAuthStartResponse, AppError> {
    crate::authz::require_not_agent_for(ctx, "connect a calendar")?;
    let host = host.unwrap_or(ctx.principal_id);
    if host != ctx.principal_id {
        super::require_admin_audited(db, ctx, "calendar.connect", "integration_connection", None)
            .await?;
    }
    // The host must be an active human principal (calendar is personal).
    let mut tx = db.begin(ctx.workspace_id).await?;
    let ok = infrastructure::repo::org::get_principal_full(&mut tx, host)
        .await?
        .is_some_and(|p| p.kind == "human" && p.status == "active");
    tx.commit().await?;
    if !ok {
        return Err(AppError::validation(
            "host must be an active human principal",
            vec![Violation {
                field: "host_principal_id".into(),
                code: "invalid".into(),
            }],
        ));
    }
    start_oauth(
        db,
        ctx,
        config,
        region,
        "zoho_calendar",
        domain::mailboxes::ZOHO_CALENDAR_SCOPES,
        Some(host),
    )
    .await
}

/// Shared OAuth start: insert pending connection + sign state + build URL.
async fn start_oauth(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    region: &str,
    provider: &str,
    scopes: &[&str],
    host: Option<Uuid>,
) -> Result<OAuthStartResponse, AppError> {
    if !domain::mailboxes::zoho_region_allowed(region) {
        return Err(AppError::validation(
            "unknown zoho region",
            vec![Violation {
                field: "region".into(),
                code: "invalid".into(),
            }],
        ));
    }
    if zoho_client_id(config).is_none() {
        return Err(AppError::dependency_unavailable(
            "zoho oauth is not configured",
        ));
    }
    let connection_id = Uuid::new_v4();
    let scope_strings: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
    let mut tx = db.begin(ctx.workspace_id).await?;
    repo::insert_pending_connection(
        &mut tx,
        connection_id,
        provider,
        region,
        &scope_strings,
        ctx.principal_id,
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "mailbox.connect",
            "integration_connection",
            ctx.request_id.clone(),
        )
        .with_target(connection_id),
    )
    .await?;
    tx.commit().await?;

    let expires_at = Utc::now() + STATE_TTL;
    let state = OAuthState {
        v: 1,
        flow: if provider == "zoho_mail" {
            "mail".into()
        } else {
            "calendar".into()
        },
        workspace_id: ctx.workspace_id,
        connection_id,
        host,
        nonce: crypto::generate_token(),
        exp: expires_at.timestamp(),
    };
    Ok(OAuthStartResponse {
        connection_id,
        authorization_url: authorization_url(
            config,
            region,
            scopes,
            &sign_state(&state, &config.hmac_key),
        )?,
        state_expires_at: expires_at,
    })
}

fn authorization_url(
    config: &Config,
    region: &str,
    scopes: &[&str],
    state: &str,
) -> Result<String, AppError> {
    let client_id =
        zoho_client_id(config).ok_or_else(|| AppError::internal("zoho client id missing"))?;
    let base = domain::mailboxes::zoho_accounts_base(region);
    let url = format!(
        "{base}/oauth/v2/auth?response_type=code&client_id={client_id}\
         &redirect_uri={}&scope={}&access_type=offline&prompt=consent&state={state}",
        urlencode(&redirect_uri(config)),
        urlencode(&scopes.join(",")),
    );
    Ok(url)
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

async fn audit_connect_start(
    db: &Db,
    ctx: &Ctx,
    connection_id: Uuid,
    action: &str,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            action,
            "integration_connection",
            ctx.request_id.clone(),
        )
        .with_target(connection_id),
    )
    .await?;
    tx.commit().await
}

// --------------------------------------------------------------------------
// callback (public — the browser arrives here from Zoho, no session)
// --------------------------------------------------------------------------

#[derive(Debug)]
pub enum CallbackOutcome {
    /// Mail connection activated; mailbox row ready.
    MailConnected { account_id: Uuid },
    /// Calendar connection activated; calendar row ready.
    CalendarConnected(Box<CalendarConnectionResponse>),
}

/// `GET /integrations/zoho/callback?code=&state=`.
///
/// Error handling (§14.1/§16.3): provider denial or a failed exchange marks
/// the connection `reauth_required`, emits `mailbox.reauth_required` (mail
/// flow) and surfaces `INTEGRATION_REAUTH_REQUIRED`; an invalid `state`
/// fails 400 before any row is touched.
pub async fn zoho_callback(
    db: &Db,
    config: &Config,
    request_id: &str,
    state_param: &str,
    code: Option<&str>,
    provider_error: Option<&str>,
) -> Result<CallbackOutcome, AppError> {
    let state = verify_state(state_param, &config.hmac_key)?;
    let mut tx = db.begin(state.workspace_id).await?;
    let Some(conn) = repo::get_connection(&mut tx, state.connection_id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("connection request not found"));
    };
    let expected_provider = if state.flow == "mail" {
        "zoho_mail"
    } else {
        "zoho_calendar"
    };
    if conn.provider != expected_provider {
        let _ = tx.rollback().await;
        return Err(AppError::validation(
            "oauth state/connection mismatch",
            vec![],
        ));
    }
    if conn.status == "disabled" {
        let _ = tx.rollback().await;
        return Err(AppError::conflict("connection is disabled"));
    }

    // Provider-side denial or missing code → reauth_required + event.
    if let Some(err_code) = provider_error.filter(|e| !e.is_empty()) {
        fail_connection(&mut tx, &conn, request_id, &state, err_code).await?;
        tx.commit().await?;
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            format!("provider denied the connection request ({err_code})"),
        ));
    }
    let Some(code) = code.filter(|c| !c.is_empty()) else {
        let _ = tx.rollback().await;
        return Err(AppError::validation("missing authorization code", vec![]));
    };

    // Token exchange through the provider abstraction — never raw HTTP.
    let exchanger = token_exchanger(config);
    let bundle = match exchanger
        .exchange(code, &redirect_uri(config), &conn.region)
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let code = match &e {
                ProviderError::ReauthRequired => "REAUTH_REQUIRED",
                ProviderError::RateLimited { .. } => "RATE_LIMITED",
                ProviderError::NotConfigured => "NOT_CONFIGURED",
                ProviderError::Unavailable(_) => "UNAVAILABLE",
            };
            fail_connection(&mut tx, &conn, request_id, &state, code).await?;
            tx.commit().await?;
            return Err(match e {
                ProviderError::ReauthRequired => AppError::new(
                    ErrorCode::IntegrationReauthRequired,
                    "provider rejected the token exchange",
                ),
                _ => AppError::dependency_unavailable("provider exchange failed"),
            });
        }
    };

    // Tokens → SecretStore under secret_ref; only the ref name stays in DB.
    let store = secret_store(config)?;
    let secret_payload = serde_json::json!({
        "refresh_token": bundle.refresh_token,
        "access_token": bundle.access_token,
        "expires_in_seconds": bundle.expires_in_seconds,
        "stored_at": Utc::now(),
    })
    .to_string();
    if let Err(e) = store.put(&conn.secret_ref, &secret_payload).await {
        let _ = tx.rollback().await;
        return Err(e);
    }
    let Some(conn) =
        repo::mark_connection_active(&mut tx, conn.id, &bundle.provider_subject, &bundle.scopes)
            .await?
    else {
        let _ = tx.rollback().await;
        return Err(AppError::internal("connection update failed"));
    };

    let outcome = if state.flow == "mail" {
        let account_id = bundle
            .provider_account_id
            .clone()
            .unwrap_or_else(|| format!("acct-{}", Uuid::new_v4()));
        let address = bundle.primary_address.clone().unwrap_or_else(|| {
            format!("mailbox-{}@unknown", &account_id[..8.min(account_id.len())])
        });
        let account =
            repo::upsert_mail_account(&mut tx, conn.id, &account_id, &address, &address).await?;
        // A.7: the connecting admin gets explicit grants here — audited —
        // never implicitly elsewhere.
        repo::upsert_grant(
            &mut tx,
            account.id,
            conn.connected_by,
            &repo::GrantFlags {
                can_read: true,
                can_compose: true,
                can_send: true,
                can_approve: true,
                can_assign: true,
            },
            conn.connected_by,
        )
        .await?;
        audit::record(
            &mut tx,
            AuditEntry::new(
                conn.connected_by,
                "mailbox.connected",
                "mail_account",
                request_id,
            )
            .with_target(account.id),
        )
        .await?;
        CallbackOutcome::MailConnected {
            account_id: account.id,
        }
    } else {
        let host = state.host.unwrap_or(conn.connected_by);
        let cal_id = bundle
            .provider_calendar_id
            .clone()
            .unwrap_or_else(|| "primary".to_string());
        let row = repo::upsert_calendar_connection(&mut tx, host, conn.id, &cal_id).await?;
        audit::record(
            &mut tx,
            AuditEntry::new(
                conn.connected_by,
                "calendar.connected",
                "calendar_connection",
                request_id,
            )
            .with_target(row.id),
        )
        .await?;
        CallbackOutcome::CalendarConnected(Box::new(CalendarConnectionResponse {
            id: row.id,
            host_principal_id: row.host_principal_id,
            connection_id: row.connection_id,
            provider: row.provider,
            provider_calendar_id: row.provider_calendar_id,
            status: row.status,
            include_in_busy: row.include_in_busy,
            version: row.version,
            created_at: row.created_at,
        }))
    };
    tx.commit().await?;
    Ok(outcome)
}

/// Mark a connection `reauth_required` after a failed exchange/denial and
/// emit `mailbox.reauth_required` (mail flow) in the same tx.
async fn fail_connection(
    tx: &mut Tx<'_>,
    conn: &repo::IntegrationConnectionRow,
    request_id: &str,
    state: &OAuthState,
    error_code: &str,
) -> Result<(), AppError> {
    repo::mark_connection_reauth(tx, conn.id, error_code).await?;
    if state.flow == "mail" {
        for account_id in repo::account_ids_for_connection(tx, conn.id).await? {
            repo::set_mail_account_status(tx, account_id, "reauth_required").await?;
        }
        outbox::emit(
            tx,
            EVENT_MAILBOX_REAUTH,
            "integration_connection",
            conn.id,
            conn.version,
            serde_json::json!({"connection_id": conn.id}),
        )
        .await?;
    }
    audit::record(
        tx,
        AuditEntry::new(
            conn.connected_by,
            "integration.reauth_required",
            "integration_connection",
            request_id,
        )
        .with_target(conn.id),
    )
    .await
}
