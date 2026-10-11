//! Mailboxes module routes (B.2 메일함·연동):
//! mail-accounts (list/get), OAuth connection-requests + the public Zoho
//! callback, reconnect, per-mailbox grants (list/put), delivery channels +
//! policy reviews, calendar connections (request/list).

use crate::problem::ProblemError;
use axum::extract::FromRequestParts;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use contracts::mailboxes::{
    CalendarConnectionRequestRequest, CalendarConnectionResponse, ChannelReviewResponse,
    ConnectionRequestRequest, CreateChannelReviewRequest, CreateDeliveryChannelRequest,
    DeliveryChannelResponse, GrantListItem, GrantResponse, LocalCalendarConnectionRequest,
    MailAccountResponse, OAuthStartResponse, PatchDeliveryChannelRequest, PutGrantRequest,
    ZohoOAuthSettingsRequest, ZohoOAuthSettingsResponse,
};
use contracts::{AppError, Ctx, Envelope, ErrorCode, Page, Problem};
use infrastructure::cursor::Cursor;
use serde::Deserialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extract::PageParams;
use crate::extractors::{AuthCtx, IdempotencyKey, IfMatch, Json, RequestIdHeader};
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_mail_accounts))
        .routes(routes!(get_mail_account))
        .routes(routes!(request_connection))
        .routes(routes!(get_oauth_settings))
        .routes(routes!(put_oauth_settings))
        .routes(routes!(zoho_callback))
        .routes(routes!(reconnect_mail_account))
        .routes(routes!(list_grants))
        .routes(routes!(put_grant))
        .routes(routes!(create_delivery_channel))
        .routes(routes!(list_delivery_channels))
        .routes(routes!(patch_delivery_channel))
        .routes(routes!(review_delivery_channel))
        .routes(routes!(request_calendar_connection))
        .routes(routes!(connect_local_calendar))
        .routes(routes!(list_calendar_connections))
}

// --------------------------------------------------------------------------
// shared helpers
// --------------------------------------------------------------------------

/// Decode a signed keyset cursor into `(created_at, id)`; bound to the
/// filter projection + principal (B.1.5).
fn keyset_after(
    state: &AppState,
    ctx: &Ctx,
    cursor: Option<&str>,
    filter_hash: &str,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let Some(raw) = cursor else { return Ok(None) };
    let cur = Cursor::decode(raw, &state.config.hmac_key, ctx.principal_id, filter_hash)?;
    let mut it = cur.sort_values.iter();
    let (Some(serde_json::Value::String(ts)), Some(serde_json::Value::String(id))) =
        (it.next(), it.next())
    else {
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

/// Sign `(created_at, id)` into `page.next_cursor`.
fn next_cursor(
    state: &AppState,
    ctx: &Ctx,
    seed: Option<(DateTime<Utc>, Uuid)>,
    filter_hash: &str,
) -> Option<String> {
    seed.map(|(ts, id)| {
        Cursor::new(
            vec![serde_json::json!(ts.to_rfc3339()), serde_json::json!(id)],
            filter_hash.to_string(),
            ctx.principal_id,
        )
        .encode(&state.config.hmac_key)
    })
}

/// `ETag: "<version>"` (B.1.4 — quoted string).
fn etag(version: i64) -> HeaderValue {
    HeaderValue::from_str(&format!("\"{version}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("\"0\""))
}

// --------------------------------------------------------------------------
// mail accounts (B.2: GET /mail-accounts, GET /mail-accounts/{id})
// --------------------------------------------------------------------------

#[utoipa::path(
    get, path = "/mail-accounts",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Signed cursor"),
    ),
    responses(
        (status = 200, description = "Mail account page (management metadata only — T02)", body = [MailAccountResponse]),
        (status = 400, description = "Invalid cursor", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_mail_accounts(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    page_params: PageParams,
) -> Result<axum::Json<Envelope<Vec<MailAccountResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter_hash = Cursor::hash_filter(&serde_json::json!({"list": "mail-accounts"}));
    let after = keyset_after(&state, &ctx, page_params.cursor.as_deref(), &filter_hash)?;
    let res = application::mailboxes::list_mail_accounts(
        &state.db,
        &ctx,
        after,
        page_params.limit as i64,
    )
    .await?;
    let cursor = next_cursor(&state, &ctx, res.cursor_seed, &filter_hash);
    Ok(axum::Json(Envelope::new(res.items, request_id).with_page(
        Page {
            next_cursor: cursor,
            has_more: res.has_more,
        },
    )))
}

#[utoipa::path(
    get, path = "/mail-accounts/{id}",
    params(("id" = Uuid, Path, description = "Mail account id")),
    responses(
        (status = 200, description = "Mail account detail (ETag = version)", body = MailAccountResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_mail_account(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::mailboxes::get_mail_account(&state.db, &ctx, id).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag(res.version));
    Ok((headers, axum::Json(Envelope::new(res, request_id))))
}

// --------------------------------------------------------------------------
// OAuth connect / callback / reconnect (B.2 연동)
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ZohoCallbackQuery {
    #[serde(default)]
    code: Option<String>,
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default, rename = "accounts-server")]
    accounts_server: Option<String>,
}

#[utoipa::path(
    get, path = "/mail-accounts/oauth-settings",
    responses(
        (status = 200, description = "Effective Zoho Mail OAuth client settings; the secret is never returned", body = ZohoOAuthSettingsResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
    )
)]
async fn get_oauth_settings(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::mailboxes::oauth_settings::get_settings(&state.db, &ctx, &state.config)
        .await?;
    Ok(axum::Json(Envelope::new(res, request_id)))
}

#[utoipa::path(
    put, path = "/mail-accounts/oauth-settings",
    request_body = ZohoOAuthSettingsRequest,
    responses(
        (status = 200, description = "Saved; the client secret is stored encrypted and never returned", body = ZohoOAuthSettingsResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 400, description = "Invalid client ID, secret or callback URI", body = Problem),
    )
)]
async fn put_oauth_settings(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Json(req): Json<ZohoOAuthSettingsRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res =
        application::mailboxes::oauth_settings::put_settings(&state.db, &ctx, &state.config, &req)
            .await?;
    Ok(axum::Json(Envelope::new(res, request_id)))
}

#[utoipa::path(
    post, path = "/mail-accounts/connection-requests",
    request_body = ConnectionRequestRequest,
    responses(
        (status = 201, description = "OAuth redirect prepared; no provider call made", body = OAuthStartResponse),
        (status = 400, description = "Validation failed (bad region)", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
    )
)]
async fn request_connection(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Json(req): Json<ConnectionRequestRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::mailboxes::oauth::request_mail_connection(
        &state.db,
        &ctx,
        &state.config,
        &req.region,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(Envelope::new(res, request_id)),
    ))
}

/// Zoho browser callback. Real Mail OAuth additionally requires the same
/// active administrator browser session that created the one-time state.
#[utoipa::path(
    get, path = "/integrations/zoho/callback",
    params(
        ("code" = Option<String>, Query, description = "OAuth authorization code"),
        ("state" = String, Query, description = "Signed OAuth state"),
        ("error" = Option<String>, Query, description = "Provider error (user denied etc.)"),
    ),
    responses(
        (status = 200, description = "Connection activated (mail account or calendar connection)", body = serde_json::Value),
        (status = 303, description = "Live Zoho Mail connection activated; return to mailbox settings"),
        (status = 400, description = "Invalid/expired state signature", body = Problem),
        (status = 404, description = "Connection request not found", body = Problem),
        (status = 409, description = "Connection disabled", body = Problem),
        (status = 503, description = "Provider denied/failed exchange — reauth required", body = Problem),
    )
)]
async fn zoho_callback(
    axum::extract::State(state): axum::extract::State<AppState>,
    RequestIdHeader(request_id): RequestIdHeader,
    query: Result<
        axum::extract::Query<ZohoCallbackQuery>,
        axum::extract::rejection::QueryRejection,
    >,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProblemError> {
    let axum::extract::Query(q) =
        query.map_err(|_| AppError::validation("invalid callback parameters", vec![]))?;
    if !state.config.zoho.fake_for_tests
        && (q
            .location
            .as_deref()
            .is_some_and(|location| !matches!(location, "us" | "com"))
            || q.accounts_server
                .as_deref()
                .is_some_and(|host| host.trim_end_matches('/') != "https://accounts.zoho.com"))
    {
        return Err(AppError::validation(
            "This Zoho Mail connection supports the US data center only",
            vec![],
        )
        .into());
    }
    let auth = if state.config.zoho.fake_for_tests {
        None
    } else {
        let (mut parts, _body) = request.into_parts();
        Some(<AuthCtx as FromRequestParts<AppState>>::from_request_parts(&mut parts, &state).await?)
    };
    let outcome = application::mailboxes::oauth::zoho_callback(
        &state.db,
        &state.config,
        &request_id,
        &q.state,
        q.code.as_deref(),
        q.error.as_deref(),
        auth.as_ref().map(AuthCtx::ctx),
    )
    .await?;
    let data = match outcome {
        application::mailboxes::oauth::CallbackOutcome::MailConnected { account_id } => {
            serde_json::json!({"flow": "mail", "connected": true, "mail_account_id": account_id})
        }
        application::mailboxes::oauth::CallbackOutcome::CalendarConnected(conn) => {
            serde_json::json!({"flow": "calendar", "connected": true, "connection": *conn})
        }
    };
    if !state.config.zoho.fake_for_tests {
        let workspace_id = auth
            .as_ref()
            .map(|auth| auth.ctx().workspace_id)
            .ok_or_else(|| AppError::dependency_unavailable("Mail callback session is missing"))?;
        let effective = application::mailboxes::oauth_settings::effective_config(
            &state.db,
            workspace_id,
            &state.config,
        )
        .await?;
        let mail = application::mailboxes::oauth::mail_oauth_config(&effective)?;
        let mut target = reqwest::Url::parse(
            mail.redirect_uri
                .as_deref()
                .ok_or_else(|| AppError::dependency_unavailable("Mail callback URI is missing"))?,
        )
        .map_err(|_| AppError::dependency_unavailable("Mail callback URI is invalid"))?;
        target.set_path("/app/settings/mail-accounts");
        target.set_query(Some("zoho_mail=connected"));
        return Ok(axum::response::Redirect::to(target.as_str()).into_response());
    }
    Ok(axum::Json(Envelope::new(data, request_id)).into_response())
}

#[utoipa::path(
    post, path = "/mail-accounts/{id}/reconnect",
    params(("id" = Uuid, Path, description = "Mail account id")),
    responses(
        (status = 201, description = "Fresh OAuth redirect URL for the existing connection", body = OAuthStartResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn reconnect_mail_account(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res =
        application::mailboxes::oauth::reconnect_mail_account(&state.db, &ctx, &state.config, id)
            .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(Envelope::new(res, request_id)),
    ))
}

// --------------------------------------------------------------------------
// grants (B.3.2: GET/PUT /mail-accounts/{id}/grants[/{principal_id}])
// --------------------------------------------------------------------------

#[utoipa::path(
    get, path = "/mail-accounts/{id}/grants",
    params(("id" = Uuid, Path, description = "Mail account id")),
    responses(
        (status = 200, description = "Explicit grants on this mailbox", body = [GrantListItem]),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn list_grants(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::Json<Envelope<Vec<GrantListItem>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::mailboxes::list_grants(&state.db, &ctx, id).await?;
    Ok(axum::Json(
        Envelope::new(res, request_id).with_page(Page::empty()),
    ))
}

/// Grant preconditions (B.3.2): first grant `If-None-Match: *` → create;
/// updates `If-Match: "<grant version>"`. Neither → 428.
fn grant_precondition(
    headers: &HeaderMap,
) -> Result<application::mailboxes::GrantPrecondition, AppError> {
    use application::mailboxes::GrantPrecondition;
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "*")
    {
        return Ok(GrantPrecondition::Create);
    }
    let Some(raw) = headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()) else {
        return Err(AppError::new(
            ErrorCode::VersionPreconditionRequired,
            "If-None-Match: * (create) or If-Match: \"<version>\" (update) is required",
        ));
    };
    let cleaned = raw.trim().trim_start_matches("W/").trim_matches('"');
    cleaned
        .parse::<i64>()
        .map(GrantPrecondition::Update)
        .map_err(|_| AppError::validation("malformed If-Match header", vec![]))
}

#[utoipa::path(
    put, path = "/mail-accounts/{id}/grants/{principal_id}",
    params(
        ("id" = Uuid, Path, description = "Mail account id"),
        ("principal_id" = Uuid, Path, description = "Grantee principal id"),
        ("If-Match" = Option<String>, Header, description = "\"<grant version>\" for updates"),
        ("If-None-Match" = Option<String>, Header, description = "\"*\" for first grant"),
    ),
    request_body = PutGrantRequest,
    responses(
        (status = 200, description = "Grant upserted; permission_version bumped (ETag = grant version)", body = GrantResponse),
        (status = 400, description = "Invalid flag chain / can_approve role violation", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Mailbox or grantee not found", body = Problem),
        (status = 412, description = "Precondition failed (exists on create / stale version)", body = Problem),
        (status = 422, description = "Invalid grant dependency chain or can_approve role", body = Problem),
        (status = 428, description = "Precondition header required", body = Problem),
    )
)]
async fn put_grant(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    headers: HeaderMap,
    axum::extract::Path((id, principal_id)): axum::extract::Path<(Uuid, Uuid)>,
    Json(req): Json<PutGrantRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let precondition = grant_precondition(&headers)?;
    let res =
        application::mailboxes::put_grant(&state.db, &ctx, id, principal_id, precondition, &req)
            .await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag(res.version));
    Ok((headers, axum::Json(Envelope::new(res, request_id))))
}

// --------------------------------------------------------------------------
// delivery channels + policy reviews (B.2: POST/GET /delivery-channels,
// PATCH /delivery-channels/{id}, POST /delivery-channels/{id}/reviews)
// --------------------------------------------------------------------------

#[utoipa::path(
    post, path = "/delivery-channels",
    request_body = CreateDeliveryChannelRequest,
    responses(
        (status = 201, description = "Channel created (policy unverified, disabled)", body = DeliveryChannelResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Connection / account / sender not found", body = Problem),
        (status = 409, description = "Connection or sender not usable", body = Problem),
    )
)]
async fn create_delivery_channel(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Json(req): Json<CreateDeliveryChannelRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::mailboxes::create_delivery_channel(&state.db, &ctx, &req).await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(Envelope::new(res, request_id)),
    ))
}

#[utoipa::path(
    get, path = "/delivery-channels",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Signed cursor"),
    ),
    responses(
        (status = 200, description = "Delivery channel page (metadata — mailbox ACL applies)", body = [DeliveryChannelResponse]),
        (status = 400, description = "Invalid cursor", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_delivery_channels(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    page_params: PageParams,
) -> Result<axum::Json<Envelope<Vec<DeliveryChannelResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter_hash = Cursor::hash_filter(&serde_json::json!({"list": "delivery-channels"}));
    let after = keyset_after(&state, &ctx, page_params.cursor.as_deref(), &filter_hash)?;
    let res = application::mailboxes::list_delivery_channels(
        &state.db,
        &ctx,
        after,
        page_params.limit as i64,
    )
    .await?;
    let cursor = next_cursor(&state, &ctx, res.cursor_seed, &filter_hash);
    Ok(axum::Json(Envelope::new(res.items, request_id).with_page(
        Page {
            next_cursor: cursor,
            has_more: res.has_more,
        },
    )))
}

#[utoipa::path(
    patch, path = "/delivery-channels/{id}",
    params(("id" = Uuid, Path, description = "Delivery channel id")),
    request_body = PatchDeliveryChannelRequest,
    responses(
        (status = 200, description = "Channel updated (new ETag)", body = DeliveryChannelResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 412, description = "Stale version", body = Problem),
        (status = 422, description = "PROVIDER_POLICY_BLOCKED / PROVIDER_POLICY_UNVERIFIED", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_delivery_channel(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    IfMatch(version): IfMatch,
    Json(req): Json<PatchDeliveryChannelRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res =
        application::mailboxes::patch_delivery_channel(&state.db, &ctx, id, version, &req).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag(res.version));
    Ok((headers, axum::Json(Envelope::new(res, request_id))))
}

#[utoipa::path(
    post, path = "/delivery-channels/{id}/reviews",
    params(("id" = Uuid, Path, description = "Delivery channel id")),
    request_body = CreateChannelReviewRequest,
    responses(
        (status = 201, description = "Policy review recorded (verdict moves policy_status)", body = ChannelReviewResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn review_delivery_channel(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(req): Json<CreateChannelReviewRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::mailboxes::review_delivery_channel(&state.db, &ctx, id, &req).await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(Envelope::new(res, request_id)),
    ))
}

// --------------------------------------------------------------------------
// calendar connections (B.2: POST /calendar-connections/connection-requests,
//                          GET /calendar-connections)
// --------------------------------------------------------------------------

#[utoipa::path(
    post, path = "/calendar-connections/connection-requests",
    request_body = CalendarConnectionRequestRequest,
    responses(
        (status = 201, description = "Calendar OAuth redirect prepared; no provider call made", body = OAuthStartResponse),
        (status = 400, description = "Invalid host", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Agent / non-admin naming another host", body = Problem),
    )
)]
async fn request_calendar_connection(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Json(req): Json<CalendarConnectionRequestRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::mailboxes::request_calendar_connection(
        &state.db,
        &ctx,
        &state.config,
        req.host_principal_id,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(Envelope::new(res, request_id)),
    ))
}

#[utoipa::path(
    get, path = "/calendar-connections",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Signed cursor"),
    ),
    responses(
        (status = 200, description = "Calendar connection page (Admin/Manager: all; others: own)", body = [CalendarConnectionResponse]),
        (status = 400, description = "Invalid cursor", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_calendar_connections(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    page_params: PageParams,
) -> Result<axum::Json<Envelope<Vec<CalendarConnectionResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter_hash = Cursor::hash_filter(&serde_json::json!({"list": "calendar-connections"}));
    let after = keyset_after(&state, &ctx, page_params.cursor.as_deref(), &filter_hash)?;
    let res = application::mailboxes::list_calendar_connections(
        &state.db,
        &ctx,
        after,
        page_params.limit as i64,
    )
    .await?;
    let cursor = next_cursor(&state, &ctx, res.cursor_seed, &filter_hash);
    Ok(axum::Json(Envelope::new(res.items, request_id).with_page(
        Page {
            next_cursor: cursor,
            has_more: res.has_more,
        },
    )))
}

#[utoipa::path(
    post, path = "/calendar-connections/local",
    request_body = LocalCalendarConnectionRequest,
    params(("Idempotency-Key" = String, Header, description = "Stable request key")),
    responses(
        (status = 201, description = "Office-owned calendar created", body = CalendarConnectionResponse),
        (status = 200, description = "Existing Office-owned calendar reused", body = CalendarConnectionResponse),
        (status = 400, description = "Missing or invalid Idempotency-Key", body = Problem),
        (status = 401, description = "Human browser session required", body = Problem),
        (status = 403, description = "Human host with booking.propose scope required", body = Problem),
        (status = 422, description = "Invalid request body", body = Problem),
    )
)]
async fn connect_local_calendar(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(request): Json<LocalCalendarConnectionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&request)
            .map_err(|e| AppError::internal(format!("local calendar request encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/calendar-connections/local".into(),
        key,
    };
    let ctx_for_usecase = ctx.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let (body, created) = application::mailboxes::calendar::connect_local_calendar_in_tx(
                &mut tx,
                &ctx_for_usecase,
            )
            .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored {
                    status: if created { 201 } else { 200 },
                    body,
                },
            ))
        },
    )
    .await?
    .into_inner();
    let status = StatusCode::from_u16(outcome.status).unwrap_or(StatusCode::OK);
    Ok((status, axum::Json(Envelope::new(outcome.body, request_id))))
}
