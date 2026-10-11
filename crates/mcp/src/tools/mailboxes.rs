//! mailboxes module MCP tools: mail accounts, Zoho OAuth settings and
//! connection requests, mailbox grants, delivery channels, calendar
//! connections, and Office SMTP profiles with their IMAP inbox sync. Each
//! tool mirrors the REST handler in `crates/api/src/routes/{mailboxes,smtp,
//! smtp_inbox}.rs` and calls the same application use cases.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use contracts::mailboxes::{
    CreateChannelReviewRequest, CreateDeliveryChannelRequest, LocalCalendarConnectionRequest,
    PatchDeliveryChannelRequest, Purpose, PutGrantRequest, ReviewResult, ZohoOAuthSettingsRequest,
};
use contracts::smtp::CreateSmtpConnectionRequest;
use contracts::{AppError, Ctx};
use infrastructure::crypto;
use infrastructure::cursor::Cursor;
use infrastructure::idempotency::{self, Stored};
use infrastructure::secrets::{FileSecretStore, SecretStore};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{ToolDef, arguments, result, tool_with_db, tool_with_services};

const IDEMPOTENCY_KEY_MIN_LEN: usize = 16;
const PAGE_DEFAULT_LIMIT: u32 = 50;
const PAGE_MAX_LIMIT: u32 = 200;

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}

fn object_schema() -> Value {
    json!({"type": "object"})
}

fn page_schema() -> Value {
    json!({
        "type": "object",
        "required": ["items", "next_cursor", "has_more"],
        "properties": {
            "items": {"type": "array", "items": {"type": "object"}},
            "next_cursor": {"type": ["string", "null"]},
            "has_more": {"type": "boolean"}
        }
    })
}

fn items_schema() -> Value {
    json!({
        "type": "object",
        "required": ["items"],
        "properties": {"items": {"type": "array", "items": {"type": "object"}}}
    })
}

fn validate_idempotency_key(key: &str) -> Result<(), AppError> {
    if (IDEMPOTENCY_KEY_MIN_LEN..=idempotency::KEY_MAX_LEN).contains(&key.len()) {
        Ok(())
    } else {
        Err(AppError::validation(
            "idempotency_key must be 16..=128 characters",
            vec![contracts::Violation {
                field: "idempotency_key".into(),
                code: "LENGTH".into(),
            }],
        ))
    }
}

/// Same limits as the REST `PageParams` extractor (default 50, max 200, >0).
fn page_limit(limit: Option<u32>) -> Result<i64, AppError> {
    match limit {
        None => Ok(PAGE_DEFAULT_LIMIT as i64),
        Some(0) => Err(AppError::validation(
            "invalid pagination parameters",
            vec![contracts::Violation {
                field: "limit".into(),
                code: "invalid".into(),
            }],
        )),
        Some(n) => Ok(n.min(PAGE_MAX_LIMIT) as i64),
    }
}

fn keyset_after(
    hmac_key: &[u8],
    ctx: &Ctx,
    cursor: Option<&str>,
    filter_hash: &str,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let Some(raw) = cursor else { return Ok(None) };
    let cur = Cursor::decode(raw, hmac_key, ctx.principal_id, filter_hash)?;
    let mut it = cur.sort_values.iter();
    let (Some(Value::String(ts)), Some(Value::String(id))) = (it.next(), it.next()) else {
        return Err(AppError::validation("invalid pagination cursor", vec![]));
    };
    let ts = DateTime::parse_from_rfc3339(ts)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    let id = id
        .parse::<Uuid>()
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    Ok(Some((ts, id)))
}

fn next_cursor(
    hmac_key: &[u8],
    ctx: &Ctx,
    seed: Option<(DateTime<Utc>, Uuid)>,
    filter_hash: &str,
) -> Option<String> {
    seed.map(|(ts, id)| {
        Cursor::new(
            vec![json!(ts.to_rfc3339()), json!(id)],
            filter_hash.to_string(),
            ctx.principal_id,
        )
        .encode(hmac_key)
    })
}

fn idempotency_scope(ctx: &Ctx, endpoint: &str, key: String) -> idempotency::Scope {
    idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.into(),
        key,
    }
}

// --- SMTP helpers (identical to `crates/api/src/routes/smtp.rs`) ----------

fn smtp_create_request_hash(key: &[u8], req: &CreateSmtpConnectionRequest) -> [u8; 32] {
    let mut canonical = b"office.smtp.create.v1\0".to_vec();
    push_field(&mut canonical, req.name.as_bytes());
    push_field(&mut canonical, req.mail_account_id.as_bytes());
    push_field(&mut canonical, req.host.as_bytes());
    push_field(&mut canonical, &req.port.to_be_bytes());
    push_field(&mut canonical, req.security.as_bytes());
    push_field(&mut canonical, req.username.as_bytes());
    // Only the keyed digest is persisted; the raw password never is.
    push_field(&mut canonical, req.password.as_bytes());
    push_field(&mut canonical, req.from_address.as_bytes());
    push_field(&mut canonical, req.from_name.as_bytes());
    push_field(
        &mut canonical,
        req.reply_to.as_deref().unwrap_or_default().as_bytes(),
    );
    push_field(&mut canonical, &[u8::from(req.sender_confirmed)]);
    for (name, value) in &req.custom_headers {
        push_field(&mut canonical, name.as_bytes());
        push_field(&mut canonical, value.as_bytes());
    }
    let purpose = req.purpose.as_deref().unwrap_or("marketing");
    if purpose != "marketing" {
        push_field(&mut canonical, b"purpose");
        push_field(&mut canonical, purpose.as_bytes());
    }
    crypto::hmac_sha256(key, &canonical)
}

fn smtp_action_hash(tag: &str, id: Uuid) -> [u8; 32] {
    crypto::sha256_bytes(format!("{tag}:{id}").as_bytes())
}

fn push_field(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

async fn smtp_cleanup_secret(config: &infrastructure::config::Config, workspace: Uuid, id: Uuid) {
    let Some(key) = config.secret_store_key else {
        return;
    };
    let store = FileSecretStore::new(&config.secrets_file, key);
    let _ = store
        .delete(&application::smtp::secret_ref(workspace, id))
        .await;
}

async fn smtp_delete_secret(
    config: &infrastructure::config::Config,
    workspace: Uuid,
    id: Uuid,
) -> Result<(), AppError> {
    let Some(key) = config.secret_store_key else {
        return Ok(());
    };
    let store = FileSecretStore::new(&config.secrets_file, key);
    store
        .delete(&application::smtp::secret_ref(workspace, id))
        .await?;
    Ok(())
}

// --------------------------------------------------------------------------
// inputs
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyInput {}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PageInput {
    /// Page size (default 50, max 200).
    #[serde(default)]
    limit: Option<u32>,
    /// `next_cursor` from the previous page.
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailAccountIdInput {
    mail_account_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PutOAuthSettingsInput {
    client_id: String,
    /// Write-only; omit to keep the stored secret.
    #[serde(default)]
    client_secret: Option<String>,
    /// Exact callback URI registered in the Zoho API Console; omit for the generated default.
    #[serde(default)]
    redirect_uri: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConnectionRequestInput {
    /// Zoho datacenter region (allowlisted server-side).
    region: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PutGrantInput {
    mail_account_id: Uuid,
    principal_id: Uuid,
    can_read: bool,
    can_compose: bool,
    can_send: bool,
    can_approve: bool,
    can_assign: bool,
    /// Set true to create the first grant for this principal (fails if one exists).
    #[serde(default)]
    create: bool,
    /// Current grant `version` when updating an existing grant.
    #[serde(default)]
    expected_version: Option<i64>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum PurposeInput {
    Correspondence,
    Marketing,
    Transactional,
}

impl From<PurposeInput> for Purpose {
    fn from(value: PurposeInput) -> Self {
        match value {
            PurposeInput::Correspondence => Purpose::Correspondence,
            PurposeInput::Marketing => Purpose::Marketing,
            PurposeInput::Transactional => Purpose::Transactional,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ReviewResultInput {
    Allowed,
    Blocked,
    Unverified,
}

impl From<ReviewResultInput> for ReviewResult {
    fn from(value: ReviewResultInput) -> Self {
        match value {
            ReviewResultInput::Allowed => ReviewResult::Allowed,
            ReviewResultInput::Blocked => ReviewResult::Blocked,
            ReviewResultInput::Unverified => ReviewResult::Unverified,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateChannelInput {
    /// Delivery-side integration connection id.
    connection_id: Uuid,
    mail_account_id: Uuid,
    /// Sender identity on that mail account.
    sender_identity_id: Uuid,
    purpose: PurposeInput,
    name: String,
    #[serde(default)]
    internal_daily_cap: Option<i32>,
    #[serde(default)]
    capabilities: Option<Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateChannelInput {
    delivery_channel_id: Uuid,
    /// Current channel `version` (the REST ETag / If-Match value).
    expected_version: i64,
    /// Fields to change: `enabled` (bool) and/or `internal_daily_cap` (integer).
    changes: Map<String, Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReviewChannelInput {
    delivery_channel_id: Uuid,
    result: ReviewResultInput,
    #[serde(default)]
    policy_url: Option<String>,
    #[serde(default)]
    contract_evidence_ref: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    /// When this verification expires (re-review due).
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CalendarConnectionRequestInput {
    /// Calendar host; defaults to the caller. Another principal requires Admin.
    #[serde(default)]
    host_principal_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdempotentInput {
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateSmtpInput {
    name: String,
    mail_account_id: Uuid,
    /// `marketing` (default) | `correspondence` | `transactional`.
    #[serde(default)]
    purpose: Option<String>,
    host: String,
    port: u16,
    /// `tls` or `starttls`.
    security: String,
    username: String,
    /// Write-only SMTP password; stored encrypted and never returned.
    password: String,
    from_address: String,
    from_name: String,
    #[serde(default)]
    reply_to: Option<String>,
    #[serde(default)]
    custom_headers: BTreeMap<String, String>,
    /// Attestation that from_address is a registered sender or authorized alias.
    sender_confirmed: bool,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SmtpActionInput {
    smtp_connection_id: Uuid,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SmtpIdInput {
    smtp_connection_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetInboxSyncInput {
    smtp_connection_id: Uuid,
    enabled: bool,
    /// Public IMAP hostname; port 993 (TLS) and INBOX are server-fixed.
    imap_host: String,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

// --------------------------------------------------------------------------
// tools
// --------------------------------------------------------------------------

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    let mut tools = mailbox_tools();
    tools.extend(channel_and_calendar_tools());
    tools.extend(smtp_tools());
    tools
}

fn mailbox_tools() -> Vec<ToolDef> {
    vec![
        tool_with_services(
            "mailboxes_list_accounts",
            "List connected mail accounts (management metadata and the caller's allowed_actions; no mail content). Paginated with limit/cursor.",
            schema::<PageInput>(),
            page_schema(),
            false,
            |services, ctx, args| async move {
                let input: PageInput = arguments(args)?;
                let limit = page_limit(input.limit)?;
                let key = &services.config.hmac_key;
                let filter_hash = Cursor::hash_filter(&json!({"list": "mail-accounts"}));
                let after = keyset_after(key, &ctx, input.cursor.as_deref(), &filter_hash)?;
                let res =
                    application::mailboxes::list_mail_accounts(&services.db, &ctx, after, limit)
                        .await?;
                let cursor = next_cursor(key, &ctx, res.cursor_seed, &filter_hash);
                result(json!({"items": res.items, "next_cursor": cursor, "has_more": res.has_more}))
            },
        ),
        tool_with_db(
            "mailboxes_get_account",
            "Get one mail account's metadata, status, version and the caller's allowed_actions.",
            schema::<MailAccountIdInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: MailAccountIdInput = arguments(args)?;
                result(
                    application::mailboxes::get_mail_account(&db, &ctx, input.mail_account_id)
                        .await?,
                )
            },
        ),
        tool_with_services(
            "mailboxes_get_oauth_settings",
            "Read the effective Zoho Mail OAuth client settings (source, client_id, callback URI, missing fields); the secret is never returned. Admin only (agents need the org.admin scope).",
            schema::<EmptyInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let _: EmptyInput = arguments(args)?;
                let res = application::mailboxes::oauth_settings::get_settings(
                    &services.db,
                    &ctx,
                    &services.config,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_services(
            "mailboxes_put_oauth_settings",
            "Save the workspace Zoho Mail OAuth client_id, optional write-only client_secret (omit to keep), and optional redirect_uri. Admin only (agents need the org.admin scope).",
            schema::<PutOAuthSettingsInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: PutOAuthSettingsInput = arguments(args)?;
                let req = ZohoOAuthSettingsRequest {
                    client_id: input.client_id,
                    client_secret: input.client_secret,
                    redirect_uri: input.redirect_uri,
                };
                let res = application::mailboxes::oauth_settings::put_settings(
                    &services.db,
                    &ctx,
                    &services.config,
                    &req,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_services(
            "mailboxes_request_connection",
            "Prepare a Zoho Mail OAuth connection for a region and return the authorization_url a human must open to finish consent. No provider call is made. Admin only (agents need the org.admin scope).",
            schema::<ConnectionRequestInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: ConnectionRequestInput = arguments(args)?;
                let res = application::mailboxes::oauth::request_mail_connection(
                    &services.db,
                    &ctx,
                    &services.config,
                    &input.region,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_services(
            "mailboxes_reconnect_account",
            "Return a fresh OAuth authorization_url to re-authorize an existing mail account (e.g. after reauth_required). Admin only (agents need the org.admin scope).",
            schema::<MailAccountIdInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: MailAccountIdInput = arguments(args)?;
                let res = application::mailboxes::oauth::reconnect_mail_account(
                    &services.db,
                    &ctx,
                    &services.config,
                    input.mail_account_id,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_db(
            "mailboxes_list_grants",
            "List explicit per-principal grants (read/compose/send/approve/assign + version) on a mail account. Admin only (agents need the org.admin scope).",
            schema::<MailAccountIdInput>(),
            items_schema(),
            false,
            |db, ctx, args| async move {
                let input: MailAccountIdInput = arguments(args)?;
                let items =
                    application::mailboxes::list_grants(&db, &ctx, input.mail_account_id).await?;
                result(json!({"items": items}))
            },
        ),
        tool_with_db(
            "mailboxes_set_grant",
            "Create or replace a principal's grant flags on a mail account. Pass create=true for the first grant, or expected_version (current grant version) to update. Admin only (agents need the org.admin scope).",
            schema::<PutGrantInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: PutGrantInput = arguments(args)?;
                use application::mailboxes::GrantPrecondition;
                let precondition = match (input.create, input.expected_version) {
                    (true, None) => GrantPrecondition::Create,
                    (false, Some(version)) => GrantPrecondition::Update(version),
                    _ => {
                        return Err(AppError::new(
                            contracts::ErrorCode::VersionPreconditionRequired,
                            "provide exactly one of create=true (first grant) or expected_version (update)",
                        ));
                    }
                };
                let req = PutGrantRequest {
                    can_read: input.can_read,
                    can_compose: input.can_compose,
                    can_send: input.can_send,
                    can_approve: input.can_approve,
                    can_assign: input.can_assign,
                };
                let res = application::mailboxes::put_grant(
                    &db,
                    &ctx,
                    input.mail_account_id,
                    input.principal_id,
                    precondition,
                    &req,
                )
                .await?;
                result(res)
            },
        ),
    ]
}

fn channel_and_calendar_tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
            "delivery_channels_create",
            "Create a delivery channel binding a connection, mail account and sender identity for a purpose. It starts disabled with policy unverified. Admin only (agents need the org.admin scope).",
            schema::<CreateChannelInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: CreateChannelInput = arguments(args)?;
                let req = CreateDeliveryChannelRequest {
                    connection_id: input.connection_id,
                    mail_account_id: input.mail_account_id,
                    sender_identity_id: input.sender_identity_id,
                    purpose: input.purpose.into(),
                    name: input.name,
                    internal_daily_cap: input.internal_daily_cap,
                    capabilities: input.capabilities,
                };
                result(application::mailboxes::create_delivery_channel(&db, &ctx, &req).await?)
            },
        ),
        tool_with_services(
            "delivery_channels_list",
            "List delivery channels with purpose, policy_status, enabled flag, cap and version (mailbox ACL applies). Paginated with limit/cursor.",
            schema::<PageInput>(),
            page_schema(),
            false,
            |services, ctx, args| async move {
                let input: PageInput = arguments(args)?;
                let limit = page_limit(input.limit)?;
                let key = &services.config.hmac_key;
                let filter_hash = Cursor::hash_filter(&json!({"list": "delivery-channels"}));
                let after = keyset_after(key, &ctx, input.cursor.as_deref(), &filter_hash)?;
                let res = application::mailboxes::list_delivery_channels(
                    &services.db,
                    &ctx,
                    after,
                    limit,
                )
                .await?;
                let cursor = next_cursor(key, &ctx, res.cursor_seed, &filter_hash);
                result(json!({"items": res.items, "next_cursor": cursor, "has_more": res.has_more}))
            },
        ),
        tool_with_db(
            "delivery_channels_update",
            "Update a delivery channel. `changes` accepts `enabled` (requires an allowed, unexpired policy review) and `internal_daily_cap`; requires expected_version. Admin only (agents need the org.admin scope).",
            schema::<UpdateChannelInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: UpdateChannelInput = arguments(args)?;
                let req: PatchDeliveryChannelRequest =
                    serde_json::from_value(Value::Object(input.changes)).map_err(|e| {
                        AppError::validation(format!("invalid changes: {e}"), vec![])
                    })?;
                let res = application::mailboxes::patch_delivery_channel(
                    &db,
                    &ctx,
                    input.delivery_channel_id,
                    input.expected_version,
                    &req,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_db(
            "delivery_channels_review",
            "Record a provider-policy review (allowed|blocked|unverified) for a delivery channel with optional policy_url, evidence ref, notes and expiry; the verdict sets policy_status. Admin only (agents need the org.admin scope).",
            schema::<ReviewChannelInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: ReviewChannelInput = arguments(args)?;
                let req = CreateChannelReviewRequest {
                    result: input.result.into(),
                    policy_url: input.policy_url,
                    contract_evidence_ref: input.contract_evidence_ref,
                    notes: input.notes,
                    expires_at: input.expires_at,
                };
                let res = application::mailboxes::review_delivery_channel(
                    &db,
                    &ctx,
                    input.delivery_channel_id,
                    &req,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_services(
            "calendar_connections_request",
            "Prepare a Zoho Calendar OAuth connection for a host (default: caller; another host requires Admin) and return the authorization_url a human must open. No provider call is made.",
            schema::<CalendarConnectionRequestInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: CalendarConnectionRequestInput = arguments(args)?;
                let res = application::mailboxes::request_calendar_connection(
                    &services.db,
                    &ctx,
                    &services.config,
                    input.host_principal_id,
                )
                .await?;
                result(res)
            },
        ),
        tool_with_services(
            "calendar_connections_list",
            "List calendar connections (Admin/Manager/agents: all; others: own) with provider, status and include_in_busy. Paginated with limit/cursor.",
            schema::<PageInput>(),
            page_schema(),
            false,
            |services, ctx, args| async move {
                let input: PageInput = arguments(args)?;
                let limit = page_limit(input.limit)?;
                let key = &services.config.hmac_key;
                let filter_hash = Cursor::hash_filter(&json!({"list": "calendar-connections"}));
                let after = keyset_after(key, &ctx, input.cursor.as_deref(), &filter_hash)?;
                let res = application::mailboxes::list_calendar_connections(
                    &services.db,
                    &ctx,
                    after,
                    limit,
                )
                .await?;
                let cursor = next_cursor(key, &ctx, res.cursor_seed, &filter_hash);
                result(json!({"items": res.items, "next_cursor": cursor, "has_more": res.has_more}))
            },
        ),
        tool_with_db(
            "calendar_connections_connect_local",
            "Create (or reuse) the caller's Office-owned local calendar connection. Requires booking.propose scope; requires idempotency_key.",
            schema::<IdempotentInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: IdempotentInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let request = LocalCalendarConnectionRequest {};
                let request_hash =
                    crypto::sha256_bytes(&serde_json::to_vec(&request).map_err(|e| {
                        AppError::internal(format!("local calendar request encode: {e}"))
                    })?);
                let scope = idempotency_scope(
                    &ctx,
                    "POST /api/v1/calendar-connections/local",
                    input.idempotency_key,
                );
                let outcome =
                    idempotency::run(&db, &scope, &request_hash, move |mut tx| async move {
                        let (body, created) =
                            application::mailboxes::calendar::connect_local_calendar_in_tx(
                                &mut tx, &ctx,
                            )
                            .await?;
                        Ok((
                            tx,
                            Stored {
                                status: if created { 201 } else { 200 },
                                body,
                            },
                        ))
                    })
                    .await?;
                result(outcome.into_inner().body)
            },
        ),
    ]
}

fn smtp_tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
            "smtp_connections_list",
            "List Office SMTP connection profiles (host, sender, status, bound channel); passwords are never returned. Admin only (agents need the org.admin scope).",
            schema::<EmptyInput>(),
            items_schema(),
            false,
            |db, ctx, args| async move {
                let _: EmptyInput = arguments(args)?;
                let items = application::smtp::list_connections(&db, &ctx).await?;
                result(json!({"items": items}))
            },
        ),
        tool_with_services(
            "smtp_connections_create",
            "Create a pending Office SMTP profile (host, port, tls|starttls, username, write-only password, sender) plus a disabled purpose-scoped delivery channel. Admin only (agents need the org.admin scope); requires sender_confirmed and idempotency_key.",
            schema::<CreateSmtpInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: CreateSmtpInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                application::smtp::authorize(&services.db, &ctx, "smtp_connection.create").await?;
                let req = CreateSmtpConnectionRequest {
                    name: input.name,
                    mail_account_id: input.mail_account_id,
                    purpose: input.purpose.or_else(|| Some("marketing".to_owned())),
                    host: input.host,
                    port: input.port,
                    security: input.security,
                    username: input.username,
                    password: input.password,
                    from_address: input.from_address,
                    from_name: input.from_name,
                    reply_to: input.reply_to,
                    custom_headers: input.custom_headers,
                    sender_confirmed: input.sender_confirmed,
                };
                let workspace_id = ctx.workspace_id;
                let id = Uuid::new_v4();
                let hash = smtp_create_request_hash(&services.config.hmac_key, &req);
                let scope =
                    idempotency_scope(&ctx, "POST /smtp-connections", input.idempotency_key);
                let config = services.config.clone();
                let outcome =
                    idempotency::run(&services.db, &scope, &hash, move |mut tx| async move {
                        let body =
                            application::smtp::create_tx(&mut tx, &ctx, &config, id, &req).await?;
                        Ok((tx, Stored { status: 201, body }))
                    })
                    .await;
                match outcome {
                    Ok(saved) => result(saved.into_inner().body),
                    Err(error) => {
                        smtp_cleanup_secret(&services.config, workspace_id, id).await;
                        Err(error)
                    }
                }
            },
        ),
        tool_with_services(
            "smtp_connections_verify",
            "Check an SMTP profile's credentials and TLS against the server (does not prove sender alias authorization). Admin only (agents need the org.admin scope); requires idempotency_key.",
            schema::<SmtpActionInput>(),
            object_schema(),
            true,
            |services, ctx, args| async move {
                let input: SmtpActionInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                application::smtp::authorize(&services.db, &ctx, "smtp_connection.verify").await?;
                let id = input.smtp_connection_id;
                let scope = idempotency_scope(
                    &ctx,
                    "POST /smtp-connections/{id}/verify",
                    input.idempotency_key,
                );
                let hash = smtp_action_hash("smtp-verify-v1", id);
                let config = services.config.clone();
                let outcome =
                    idempotency::run(&services.db, &scope, &hash, move |mut tx| async move {
                        let body = application::smtp::verify_tx(&mut tx, &ctx, &config, id).await?;
                        Ok((tx, Stored { status: 200, body }))
                    })
                    .await?;
                result(outcome.into_inner().body)
            },
        ),
        tool_with_services(
            "smtp_connections_disable",
            "Disable an SMTP profile and its bound delivery channel, and delete the stored credential. Admin only (agents need the org.admin scope); requires idempotency_key.",
            schema::<SmtpActionInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: SmtpActionInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                application::smtp::authorize(&services.db, &ctx, "smtp_connection.disable").await?;
                let id = input.smtp_connection_id;
                let workspace_id = ctx.workspace_id;
                let scope = idempotency_scope(
                    &ctx,
                    "POST /smtp-connections/{id}/disable",
                    input.idempotency_key,
                );
                let hash = smtp_action_hash("smtp-disable-v1", id);
                let outcome =
                    idempotency::run(&services.db, &scope, &hash, move |mut tx| async move {
                        let body = application::smtp::disable_tx(&mut tx, &ctx, id).await?;
                        Ok((tx, Stored { status: 200, body }))
                    })
                    .await?;
                smtp_delete_secret(&services.config, workspace_id, id).await?;
                result(outcome.into_inner().body)
            },
        ),
        tool_with_db(
            "smtp_inbox_get",
            "Read an SMTP profile's IMAP inbox sync configuration, cursor, import count, last error and active job. Admin only (agents need the org.admin scope).",
            schema::<SmtpIdInput>(),
            object_schema(),
            false,
            |db, ctx, args| async move {
                let input: SmtpIdInput = arguments(args)?;
                result(application::smtp_inbox::status(&db, &ctx, input.smtp_connection_id).await?)
            },
        ),
        tool_with_services(
            "smtp_inbox_configure",
            "Enable or disable IMAP inbox sync for an SMTP profile with a public imap_host (port 993 and INBOX are fixed; the profile credential is reused). Admin only (agents need the org.admin scope); requires idempotency_key.",
            schema::<SetInboxSyncInput>(),
            object_schema(),
            false,
            |services, ctx, args| async move {
                let input: SetInboxSyncInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                application::smtp::authorize(&services.db, &ctx, "smtp_inbox.configure").await?;
                let id = input.smtp_connection_id;
                let scope = idempotency_scope(
                    &ctx,
                    "PUT /smtp-connections/{id}/inbox",
                    input.idempotency_key,
                );
                let request_hash = crypto::sha256_bytes(
                    &serde_json::to_vec(&json!({
                        "smtp_connection_id": id,
                        "enabled": input.enabled,
                        "imap_host": input.imap_host,
                    }))
                    .unwrap_or_default(),
                );
                let config = services.config.clone();
                let enabled = input.enabled;
                let imap_host = input.imap_host;
                let outcome = idempotency::run(
                    &services.db,
                    &scope,
                    &request_hash,
                    move |mut tx| async move {
                        let body = application::smtp_inbox::set_settings_tx(
                            &mut tx, &ctx, &config, id, enabled, &imap_host,
                        )
                        .await?;
                        Ok((tx, Stored { status: 200, body }))
                    },
                )
                .await?;
                result(outcome.into_inner().body)
            },
        ),
        tool_with_services(
            "smtp_inbox_request_sync",
            "Queue a bounded IMAP inbox sync job for an SMTP profile with sync enabled; returns job_id and state. Admin only (agents need the org.admin scope); requires idempotency_key.",
            schema::<SmtpActionInput>(),
            object_schema(),
            true,
            |services, ctx, args| async move {
                let input: SmtpActionInput = arguments(args)?;
                validate_idempotency_key(&input.idempotency_key)?;
                application::smtp::authorize(&services.db, &ctx, "smtp_inbox.sync_requested")
                    .await?;
                let id = input.smtp_connection_id;
                let scope = idempotency_scope(
                    &ctx,
                    "POST /smtp-connections/{id}/inbox/sync",
                    input.idempotency_key,
                );
                let request_hash = crypto::sha256_bytes(
                    &serde_json::to_vec(&json!({"smtp_connection_id": id})).unwrap_or_default(),
                );
                let config = services.config.clone();
                let outcome = idempotency::run(
                    &services.db,
                    &scope,
                    &request_hash,
                    move |mut tx| async move {
                        let body =
                            application::smtp_inbox::request_sync_tx(&mut tx, &ctx, &config, id)
                                .await?;
                        Ok((tx, Stored { status: 202, body }))
                    },
                )
                .await?;
                result(outcome.into_inner().body)
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_tools_are_unique_and_keyed_mutations_require_idempotency() {
        let defs = tools();
        let names: std::collections::HashSet<_> = defs.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), defs.len());
        for name in [
            "calendar_connections_connect_local",
            "smtp_connections_create",
            "smtp_connections_verify",
            "smtp_connections_disable",
            "smtp_inbox_configure",
            "smtp_inbox_request_sync",
        ] {
            let def = defs.iter().find(|t| t.name == name).unwrap();
            assert!(
                def.input_schema["required"]
                    .as_array()
                    .is_some_and(|f| f.iter().any(|v| v == "idempotency_key")),
                "{name}"
            );
        }
        let update = defs
            .iter()
            .find(|t| t.name == "delivery_channels_update")
            .unwrap();
        assert!(
            update.input_schema["required"]
                .as_array()
                .is_some_and(|f| f.iter().any(|v| v == "expected_version"))
        );
    }

    #[test]
    fn smtp_create_hash_matches_rest_purpose_default() {
        let base = || CreateSmtpConnectionRequest {
            name: "Office SMTP".into(),
            mail_account_id: Uuid::nil(),
            purpose: Some("marketing".into()),
            host: "smtp.example.test".into(),
            port: 587,
            security: "starttls".into(),
            username: "u".into(),
            password: "p".into(),
            from_address: "a@example.test".into(),
            from_name: "A".into(),
            reply_to: None,
            custom_headers: BTreeMap::new(),
            sender_confirmed: true,
        };
        let mut legacy = base();
        legacy.purpose = None;
        assert_eq!(
            smtp_create_request_hash(b"k", &base()),
            smtp_create_request_hash(b"k", &legacy)
        );
    }
}
