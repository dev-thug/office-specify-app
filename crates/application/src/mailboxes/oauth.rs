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
//! Runtime configuration never invents an OAuth client ID. The legacy fake
//! Mail/Calendar exchange is available only to explicit Config::test fixtures.
//! Real Campaigns OAuth is implemented separately in the marketing module.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{Duration, Utc};
use contracts::mailboxes::{CalendarConnectionResponse, OAuthStartResponse};
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Role, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::config::Config;
use infrastructure::crypto;
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::providers::ProviderError;
use infrastructure::repo::mailboxes as repo;
use infrastructure::secrets::{FileSecretStore, SecretStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

/// Runtime IDs must be configured. Only explicit test fixtures use a stub.
pub fn zoho_client_id(config: &Config) -> Option<String> {
    config
        .zoho
        .mail_client_id
        .clone()
        .filter(|value| infrastructure::providers::zoho_campaigns::valid_client_id(value))
        .or_else(|| {
            config
                .zoho
                .fake_for_tests
                .then(|| "test-zoho-client-id".to_string())
        })
}

/// Public base URL for redirects (`OUTREACH_PUBLIC_BASE_URL` or the bound
/// api address).
pub fn public_base(config: &Config) -> String {
    env("OUTREACH_PUBLIC_BASE_URL").unwrap_or_else(|| format!("http://{}", config.api_addr))
}

fn redirect_uri(config: &Config) -> String {
    config
        .zoho
        .mail_redirect_uri
        .clone()
        .unwrap_or_else(|| format!("{}/api/v1/integrations/zoho/callback", public_base(config)))
}

/// Resolve validated Mail OAuth credentials without touching the separate
/// Campaigns credential or its broader marketing scopes.
pub fn mail_oauth_config(
    config: &Config,
) -> Result<infrastructure::providers::zoho_campaigns::CampaignsOAuthConfig, AppError> {
    let mail = infrastructure::providers::zoho_campaigns::CampaignsOAuthConfig {
        client_id: config.zoho.mail_client_id.clone(),
        client_secret: config.zoho.mail_client_secret.clone(),
        redirect_uri: config.zoho.mail_redirect_uri.clone(),
        allow_local_redirect: config.zoho.campaigns.allow_local_redirect,
    };
    mail.validate().map_err(|_| {
        AppError::dependency_unavailable(
            "Zoho Mail OAuth is not configured; set the Mail client ID, secret, and HTTPS callback URI",
        )
    })?;
    Ok(mail)
}

/// Whether the live Mail sender has the complete OAuth setup required by
/// connection starts and the runtime provider router.
pub fn live_mail_oauth_configured(config: &Config) -> bool {
    mail_oauth_config(config).is_ok()
}

/// Detect any explicit live Mail OAuth setting. A generated development
/// callback URI alone is not a configured OAuth client; partial credentials
/// must select the fail-closed router rather than a fake provider.
pub fn mail_oauth_settings_present(config: &Config) -> bool {
    let explicit_redirect = env("ZOHO_MAIL_REDIRECT_URI").is_some();
    let public_origin = env("PUBLIC_BASE_URL")
        .or_else(|| env("OUTREACH_PUBLIC_BASE_URL"))
        .unwrap_or_else(|| {
            if config.app_env.is_dev_or_test() {
                "http://localhost:3000".into()
            } else {
                String::new()
            }
        });
    let generated_redirect = format!(
        "{}/api/v1/integrations/zoho/callback",
        public_origin.trim_end_matches('/')
    );
    config.zoho.mail_client_id.is_some()
        || config.zoho.mail_client_secret.is_some()
        || explicit_redirect
        || config
            .zoho
            .mail_redirect_uri
            .as_deref()
            .is_some_and(|redirect| redirect != generated_redirect)
}

// --------------------------------------------------------------------------
// token exchange abstraction
// --------------------------------------------------------------------------

#[derive(Clone)]
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
/// Implemented by the explicit fake fixture or the real US Zoho Mail adapter.
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

pub fn token_exchanger(config: &Config) -> Result<Arc<dyn TokenExchange>, AppError> {
    if config.zoho.fake_for_tests {
        Ok(Arc::new(FakeTokenExchange))
    } else {
        Ok(Arc::new(super::real_provider::MailTokenExchange::new(
            config,
        )?))
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
    /// Live Mail callback URI fixed at OAuth start. The callback rejects a
    /// changed server configuration before consuming the stored state hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    redirect_uri: Option<String>,
    #[serde(default)]
    permission_version: i64,
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
    let session_id = if config.zoho.fake_for_tests {
        None
    } else {
        Some(require_browser_oauth_session(db, ctx, "mailbox.connect", None).await?)
    };
    start_oauth(
        db,
        ctx,
        config,
        OAuthStartParams {
            region,
            provider: "zoho_mail",
            scopes: domain::mailboxes::ZOHO_MAIL_SCOPES,
            host: None,
            session_id,
        },
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
    if !config.zoho.fake_for_tests {
        let mail_config = mail_oauth_config(config)?;
        let session_id =
            require_browser_oauth_session(db, ctx, "mailbox.reconnect", Some(mail_account_id))
                .await?;
        let mut tx = db.begin(ctx.workspace_id).await?;
        let (account, _grant) = repo::get_mail_account(&mut tx, ctx.principal_id, mail_account_id)
            .await?
            .ok_or_else(|| AppError::not_found("mail account not found"))?;
        let conn = repo::get_connection_for_update(&mut tx, account.connection_id)
            .await?
            .ok_or_else(|| AppError::internal("connection missing for account"))?;
        if conn.provider != "zoho_mail" || conn.region != "com" || conn.status == "disabled" {
            return Err(AppError::validation(
                "only an enabled US Zoho Mail connection can be reconnected",
                vec![],
            ));
        }
        let expires_at = Utc::now() + STATE_TTL;
        let state = OAuthState {
            v: 1,
            flow: "mail".into(),
            workspace_id: ctx.workspace_id,
            connection_id: conn.id,
            host: None,
            redirect_uri: mail_config.redirect_uri,
            permission_version: ctx.permission_version,
            nonce: crypto::generate_token(),
            exp: expires_at.timestamp(),
        };
        let signed_state = sign_state(&state, &config.hmac_key);
        let state_hash = crypto::sha256_bytes(signed_state.as_bytes());
        repo::insert_oauth_request(
            &mut tx,
            conn.id,
            ctx.principal_id,
            session_id,
            &state_hash,
            expires_at,
        )
        .await?;
        audit::record(
            &mut tx,
            AuditEntry::new(
                ctx.principal_id,
                "mailbox.reconnect",
                "integration_connection",
                ctx.request_id.clone(),
            )
            .with_target(conn.id),
        )
        .await?;
        tx.commit().await?;
        return Ok(OAuthStartResponse {
            connection_id: conn.id,
            authorization_url: authorization_url(
                config,
                "com",
                domain::mailboxes::ZOHO_MAIL_SCOPES,
                &signed_state,
            )?,
            state_expires_at: expires_at,
        });
    }
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
        redirect_uri: None,
        permission_version: ctx.permission_version,
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
        OAuthStartParams {
            region,
            provider: "zoho_calendar",
            scopes: domain::mailboxes::ZOHO_CALENDAR_SCOPES,
            host: Some(host),
            session_id: None,
        },
    )
    .await
}

struct OAuthStartParams<'a> {
    region: &'a str,
    provider: &'static str,
    scopes: &'static [&'static str],
    host: Option<Uuid>,
    session_id: Option<Uuid>,
}

/// Shared OAuth start: insert pending connection + sign state + build URL.
async fn start_oauth(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    params: OAuthStartParams<'_>,
) -> Result<OAuthStartResponse, AppError> {
    let OAuthStartParams {
        region,
        provider,
        scopes,
        host,
        session_id,
    } = params;
    if !domain::mailboxes::zoho_region_allowed(region) {
        return Err(AppError::validation(
            "unknown zoho region",
            vec![Violation {
                field: "region".into(),
                code: "invalid".into(),
            }],
        ));
    }
    let live_mail = provider == "zoho_mail" && !config.zoho.fake_for_tests;
    let live_mail_redirect = if live_mail {
        mail_oauth_config(config)?.redirect_uri
    } else {
        None
    };
    if live_mail {
        if region != "com" || session_id.is_none() {
            return Err(AppError::dependency_unavailable(
                "live Zoho Mail connections require the US data center and an administrator browser session",
            ));
        }
    } else if provider == "zoho_calendar" && !config.zoho.fake_for_tests {
        return Err(AppError::dependency_unavailable(
            "Zoho Calendar OAuth is not configured",
        ));
    } else if zoho_client_id(config).is_none() {
        return Err(AppError::dependency_unavailable(
            "Zoho Mail OAuth is not configured; set Mail OAuth credentials on the server",
        ));
    }
    let connection_id = Uuid::new_v4();
    let scope_strings: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
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
        redirect_uri: live_mail_redirect,
        permission_version: ctx.permission_version,
        nonce: crypto::generate_token(),
        exp: expires_at.timestamp(),
    };
    let signed_state = sign_state(&state, &config.hmac_key);
    let state_hash = crypto::sha256_bytes(signed_state.as_bytes());
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
    if let Some(session_id) = session_id {
        repo::insert_oauth_request(
            &mut tx,
            connection_id,
            ctx.principal_id,
            session_id,
            &state_hash,
            expires_at,
        )
        .await?;
    }
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
    Ok(OAuthStartResponse {
        connection_id,
        authorization_url: authorization_url(config, region, scopes, &signed_state)?,
        state_expires_at: expires_at,
    })
}

async fn require_browser_oauth_session(
    db: &Db,
    ctx: &Ctx,
    action: &str,
    target_id: Option<Uuid>,
) -> Result<Uuid, AppError> {
    if let AuthMethod::Session { session_id } = &ctx.auth {
        return Ok(*session_id);
    }
    let error =
        AppError::forbidden("live Zoho Mail OAuth requires an administrator browser session");
    super::audit_denied(db, ctx, action, "integration_connection", target_id, &error).await?;
    Err(error)
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
    let mut url = reqwest_url(&format!("{base}/oauth/v2/auth"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri(config))
        .append_pair("scope", &scopes.join(","))
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state);
    Ok(url.to_string())
}

fn reqwest_url(
    value: &str,
) -> Result<infrastructure::providers::zoho_campaigns::http::ProviderUrl, AppError> {
    value
        .parse()
        .map_err(|_| AppError::internal("invalid Zoho authorization endpoint"))
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
    auth: Option<&Ctx>,
) -> Result<CallbackOutcome, AppError> {
    let state = verify_state(state_param, &config.hmac_key)?;
    if config.zoho.fake_for_tests {
        return zoho_callback_fake(db, config, request_id, state, code, provider_error).await;
    }
    let ctx = auth.ok_or_else(|| {
        AppError::new(
            ErrorCode::AuthenticationRequired,
            "Zoho Mail callback requires the administrator browser session that started OAuth",
        )
    })?;
    let session_id = require_live_mail_callback_owner(ctx)?;
    if state.flow != "mail" || state.workspace_id != ctx.workspace_id {
        return Err(AppError::validation("invalid oauth state", vec![]));
    }
    zoho_mail_callback_real(
        db,
        config,
        RealMailCallback {
            request_id,
            raw_state: state_param,
            state: &state,
            ctx,
            session_id,
            code,
            provider_error,
        },
    )
    .await
}

async fn zoho_callback_fake(
    db: &Db,
    config: &Config,
    request_id: &str,
    state: OAuthState,
    code: Option<&str>,
    provider_error: Option<&str>,
) -> Result<CallbackOutcome, AppError> {
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
    if provider_error.is_some() {
        fail_connection(&mut tx, &conn, request_id, &state, "consent_denied").await?;
        tx.commit().await?;
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "provider denied the connection request",
        ));
    }
    let Some(code) = code.filter(|c| !c.is_empty()) else {
        let _ = tx.rollback().await;
        return Err(AppError::validation("missing authorization code", vec![]));
    };

    // Token exchange through the provider abstraction — never raw HTTP.
    let exchanger = token_exchanger(config)?;
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

fn require_live_mail_callback_owner(ctx: &Ctx) -> Result<Uuid, AppError> {
    if ctx.principal_kind != PrincipalKind::Human || ctx.role != Role::Admin {
        return Err(AppError::forbidden(
            "live Zoho Mail OAuth requires a human administrator",
        ));
    }
    match &ctx.auth {
        AuthMethod::Session { session_id } => Ok(*session_id),
        _ => Err(AppError::forbidden(
            "live Zoho Mail OAuth requires the initiating browser session",
        )),
    }
}

struct RealMailCallback<'a> {
    request_id: &'a str,
    raw_state: &'a str,
    state: &'a OAuthState,
    ctx: &'a Ctx,
    session_id: Uuid,
    code: Option<&'a str>,
    provider_error: Option<&'a str>,
}

async fn zoho_mail_callback_real(
    db: &Db,
    config: &Config,
    callback: RealMailCallback<'_>,
) -> Result<CallbackOutcome, AppError> {
    let RealMailCallback {
        request_id,
        raw_state,
        state,
        ctx,
        session_id,
        code,
        provider_error,
    } = callback;
    let mail_config = mail_oauth_config(config)?;
    if state.redirect_uri.as_deref() != mail_config.redirect_uri.as_deref() {
        return Err(AppError::validation(
            "Zoho Mail OAuth callback configuration changed; start a new connection",
            vec![],
        ));
    }
    if raw_state.len() > 2048 || state.connection_id.is_nil() {
        return Err(AppError::validation("invalid oauth state", vec![]));
    }
    if provider_error.is_some() {
        let conn = claim_live_mail_oauth_request(db, raw_state, state, ctx, session_id).await?;
        mark_mail_connection_reauth(db, &conn, request_id, state, "consent_denied").await?;
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Zoho Mail consent was denied; start a new connection",
        ));
    }
    let Some(code) = code.filter(|code| !code.is_empty()) else {
        return Err(AppError::validation("missing authorization code", vec![]));
    };
    if code.len() > 4096 || code.chars().any(char::is_control) {
        return Err(AppError::validation("invalid authorization code", vec![]));
    }

    let conn = claim_live_mail_oauth_request(db, raw_state, state, ctx, session_id).await?;
    let store = secret_store(config)?;
    let exchanger = super::real_provider::MailTokenExchange::new(config)?;
    let bundle = match exchanger
        .exchange_for_account(
            code,
            state.redirect_uri.as_deref().unwrap_or_default(),
            "com",
            conn.provider_subject.as_deref(),
        )
        .await
    {
        Ok(bundle) => bundle,
        Err(ProviderError::ReauthRequired) => {
            mark_mail_connection_reauth(db, &conn, request_id, state, "oauth_rejected").await?;
            return Err(AppError::new(
                ErrorCode::IntegrationReauthRequired,
                "Zoho Mail rejected the authorization; reconnect the mailbox",
            ));
        }
        Err(ProviderError::NotConfigured) => {
            return Err(AppError::dependency_unavailable(
                "Zoho Mail OAuth is not configured",
            ));
        }
        Err(ProviderError::RateLimited { .. }) | Err(ProviderError::Unavailable(_)) => {
            return Err(AppError::dependency_unavailable(
                "Zoho Mail authorization is temporarily unavailable",
            ));
        }
    };
    let provider_account_id = bundle
        .provider_account_id
        .as_deref()
        .filter(|id| valid_zoho_id(id))
        .ok_or_else(|| AppError::dependency_unavailable("Zoho Mail account discovery failed"))?;
    let primary_address = bundle
        .primary_address
        .as_deref()
        .filter(|address| valid_mail_address(address))
        .ok_or_else(|| AppError::dependency_unavailable("Zoho Mail account address is invalid"))?;

    let mut tx = db.begin(ctx.workspace_id).await?;
    let current = repo::get_connection_for_update(&mut tx, conn.id)
        .await?
        .ok_or_else(|| AppError::not_found("connection request not found"))?;
    if current.provider != "zoho_mail" || current.region != "com" || current.status == "disabled" {
        return Err(AppError::conflict(
            "Zoho Mail connection is no longer available",
        ));
    }
    if !repo::active_mail_oauth_owner(
        &mut tx,
        ctx.principal_id,
        session_id,
        state.permission_version,
    )
    .await?
    {
        return Err(AppError::forbidden(
            "the initiating session or administrator permission is no longer active",
        ));
    }

    let refresh_token = if let Some(refresh_token) = bundle.refresh_token.clone() {
        Some(refresh_token)
    } else {
        store
            .get(&current.secret_ref)
            .await?
            .as_deref()
            .map(read_refresh_token)
            .transpose()?
            .flatten()
    };
    let Some(refresh_token) = refresh_token else {
        fail_connection(
            &mut tx,
            &current,
            request_id,
            state,
            "refresh_token_missing",
        )
        .await?;
        tx.commit().await?;
        return Err(AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Zoho Mail did not provide a refresh token; reconnect with offline consent",
        ));
    };

    let stored_at = Utc::now();
    let secret_payload = serde_json::json!({
        "access_token": bundle.access_token,
        "refresh_token": refresh_token,
        "expires_in_seconds": bundle.expires_in_seconds,
        "stored_at": stored_at,
    })
    .to_string();
    store.put(&current.secret_ref, &secret_payload).await?;
    let active = repo::mark_connection_active(
        &mut tx,
        current.id,
        &bundle.provider_subject,
        &bundle.scopes,
    )
    .await?
    .ok_or_else(|| AppError::internal("connection update failed"))?;
    let account = repo::upsert_mail_account(
        &mut tx,
        active.id,
        provider_account_id,
        primary_address,
        primary_address,
    )
    .await?;
    repo::upsert_grant(
        &mut tx,
        account.id,
        ctx.principal_id,
        &repo::GrantFlags {
            can_read: true,
            can_compose: true,
            can_send: true,
            can_approve: true,
            can_assign: true,
        },
        ctx.principal_id,
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "mailbox.connected",
            "mail_account",
            request_id,
        )
        .with_target(account.id),
    )
    .await?;
    tx.commit().await?;
    Ok(CallbackOutcome::MailConnected {
        account_id: account.id,
    })
}

async fn claim_live_mail_oauth_request(
    db: &Db,
    raw_state: &str,
    state: &OAuthState,
    ctx: &Ctx,
    session_id: Uuid,
) -> Result<repo::IntegrationConnectionRow, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    if !repo::active_mail_oauth_owner(
        &mut tx,
        ctx.principal_id,
        session_id,
        state.permission_version,
    )
    .await?
    {
        return Err(AppError::forbidden(
            "the initiating session or administrator permission is no longer active",
        ));
    }
    let hash = crypto::sha256_bytes(raw_state.as_bytes());
    let request = repo::claim_oauth_request(&mut tx, &hash, ctx.principal_id, session_id)
        .await?
        .ok_or_else(|| AppError::validation("invalid or already used oauth state", vec![]))?;
    if request.connection_id != state.connection_id
        || request.principal_id != ctx.principal_id
        || request.session_id != session_id
        || request.expires_at <= Utc::now()
    {
        return Err(AppError::validation("invalid oauth state", vec![]));
    }
    let conn = repo::get_connection_for_update(&mut tx, request.connection_id)
        .await?
        .ok_or_else(|| AppError::not_found("connection request not found"))?;
    if conn.provider != "zoho_mail" || conn.region != "com" || conn.status == "disabled" {
        return Err(AppError::validation(
            "oauth state/connection mismatch",
            vec![],
        ));
    }
    tx.commit().await?;
    Ok(conn)
}

async fn mark_mail_connection_reauth(
    db: &Db,
    conn: &repo::IntegrationConnectionRow,
    request_id: &str,
    state: &OAuthState,
    error_code: &str,
) -> Result<(), AppError> {
    let mut tx = db.begin(state.workspace_id).await?;
    if let Some(current) = repo::get_connection_for_update(&mut tx, conn.id).await?
        && current.status != "disabled"
    {
        fail_connection(&mut tx, &current, request_id, state, error_code).await?;
    }
    tx.commit().await
}

fn read_refresh_token(raw: &str) -> Result<Option<String>, AppError> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|_| AppError::internal("stored Zoho Mail credential is invalid"))?;
    Ok(value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .map(str::to_owned))
}

fn valid_zoho_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn valid_mail_address(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 320
        && value.contains('@')
        && !value.chars().any(char::is_control)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_mail_oauth_requires_mail_client_secret_and_safe_callback() {
        let mut config = Config::test();
        config.zoho.fake_for_tests = false;
        assert!(mail_oauth_config(&config).is_err());

        config.zoho.mail_client_id = Some("1000.mail-client".into());
        config.zoho.mail_client_secret = Some(
            infrastructure::providers::zoho_campaigns::SecretValue::new("mail-client-secret"),
        );
        config.zoho.mail_redirect_uri =
            Some("https://office.example/api/v1/integrations/zoho/callback".into());
        assert!(mail_oauth_config(&config).is_ok());

        config.zoho.mail_redirect_uri = Some("http://attacker.example/callback".into());
        assert!(mail_oauth_config(&config).is_err());
    }
}
