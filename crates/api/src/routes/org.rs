//! Org module routes (spec §6): auth (§4.5/X1/X2) + `/me` + org admin
//! (principals, member-invitations, agents, credentials, disable-requests)
//! + `/operations/emergency-stop` (B.2 조직·연동 / 운영 rows).

use crate::problem::ProblemError;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use contracts::org::{
    AvatarResponse,
    AcceptInvitationRequest, ChangePasswordRequest, CreateAgentRequest, CreateCredentialRequest,
    CreateInvitationRequest, CredentialCreatedResponse, CredentialResponse,
    CredentialRevokedResponse, DisableRequestRequest, DisableRequestResponse, EmergencyStopRequest,
    EmergencyStopResponse, InvitationCreatedResponse, InvitationPreviewResponse,
    InvitationResponse, InvitationRevokedResponse, LoginRequest, MeResponse, PatchPrincipalRequest,
    PrincipalListItem, PrincipalPickerResponse, PrincipalResponse,
};
use contracts::{AppError, AuthMethod, Envelope, ErrorCode, Meta, Page, Problem};
use infrastructure::cursor::Cursor;
use infrastructure::idempotency;
use serde::Deserialize;
use std::net::SocketAddr;
use std::time::Duration;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::cookies;
use crate::extract::PageParams;
use crate::extractors::{AuthCtx, IdempotencyKey, IfMatch, Json, OptionalIdempotencyKey};
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(invitation_preview))
        .routes(routes!(invitation_accept))
        .routes(routes!(me))
        .routes(routes!(list_my_sessions))
        .routes(routes!(revoke_my_session))
        .routes(routes!(change_my_password))
        .routes(routes!(list_principals))
        .routes(routes!(get_principal))
        .routes(routes!(patch_principal))
        .routes(routes!(put_principal_avatar, delete_principal_avatar, get_principal_avatar))
        .routes(routes!(request_disable_principal))
        .routes(routes!(create_invitation))
        .routes(routes!(list_invitations))
        .routes(routes!(revoke_invitation))
        .routes(routes!(create_agent))
        .routes(routes!(create_credential))
        .routes(routes!(list_credentials))
        .routes(routes!(revoke_credential))
        .routes(routes!(get_emergency_stop))
        .routes(routes!(put_emergency_stop))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope {
        data,
        meta: Meta {
            request_id: request_id.to_string(),
        },
        page: None,
    }
}

fn auth_cookies(state: &AppState, session: &str, csrf: &str) -> [axum::http::HeaderValue; 2] {
    let ttl = state.config.session_ttl.as_secs() as i64;
    [
        cookies::session(session, state.config.cookie_secure, ttl),
        cookies::csrf(csrf, state.config.cookie_secure, ttl),
    ]
}

/// Login rate limit (R6): 10 attempts/minute per client IP *and* per
/// normalized email, fail closed when Redis is configured; no limiter
/// configured → skipped. The client IP trusts `X-Forwarded-For` only when
/// the TCP peer is in `TRUSTED_PROXIES` (spec §16.3, §4.10).
async fn login_rate_limit(
    state: &AppState,
    headers: &HeaderMap,
    addr: &SocketAddr,
    email: &str,
) -> Result<(), AppError> {
    if let Some(rl) = &state.rate_limiter {
        let ip = crate::extractors::client_ip(&state.config, headers, addr.ip());
        let email_key = domain::email_address::normalize(
            email,
            domain::email_address::LocalPartPolicy::Lowercase,
        )
        .map(|e| e.normalized_key)
        .unwrap_or_else(|_| email.to_lowercase());
        let hash = infrastructure::crypto::sha256_hex(email_key.as_bytes());
        let window = Duration::from_secs(60);
        for key in [format!("login:ip:{ip}"), format!("login:email:{hash}")] {
            rl.require(
                &key,
                10,
                window,
                infrastructure::ratelimit::FailMode::Closed,
            )
            .await?;
        }
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated; sets outreach_session + outreach_csrf cookies", body = MeResponse),
        (status = 401, description = "Invalid credentials", body = Problem),
        (status = 429, description = "Account locked / rate limited", body = Problem),
    )
)]
async fn login(
    axum::extract::State(state): axum::extract::State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request_id: crate::extractors::RequestIdHeader,
    Json(req): Json<LoginRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    login_rate_limit(&state, &headers, &addr, &req.email).await?;
    let workspace_id = state
        .db
        .default_workspace_id()
        .ok_or_else(|| AppError::internal("DEFAULT_WORKSPACE_ID not configured"))?;
    let out = application::org::auth::login(
        &state.db,
        workspace_id,
        chrono::Duration::from_std(state.config.session_ttl).unwrap(),
        &req.email,
        &req.password,
        &request_id.0,
    )
    .await?;
    let [sc, cc] = auth_cookies(&state, &out.session_token, &out.csrf_token);
    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, sc);
    headers.append(header::SET_COOKIE, cc);
    Ok((headers, axum::Json(envelope(out.me, &request_id.0))))
}

#[utoipa::path(
    post, path = "/auth/logout",
    responses(
        (status = 204, description = "Session revoked"),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "CSRF failure", body = Problem),
    )
)]
async fn logout(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let session_id = match ctx.auth {
        AuthMethod::Session { session_id } => session_id,
        _ => {
            return Err(ProblemError(AppError::validation(
                "logout requires a session",
                vec![],
            )));
        }
    };
    application::org::auth::logout(&state.db, &ctx, session_id).await?;
    let [sc, cc] = cookies::clear(state.config.cookie_secure);
    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, sc);
    headers.append(header::SET_COOKIE, cc);
    Ok((axum::http::StatusCode::NO_CONTENT, headers))
}

#[utoipa::path(
    get, path = "/auth/invitations/{token}",
    params(("token" = String, Path, description = "One-time invitation token")),
    responses(
        (status = 200, description = "Invitation preview", body = InvitationPreviewResponse),
        (status = 404, description = "Not found or expired", body = Problem),
    )
)]
async fn invitation_preview(
    axum::extract::State(state): axum::extract::State<AppState>,
    request_id: crate::extractors::RequestIdHeader,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<axum::Json<Envelope<InvitationPreviewResponse>>, ProblemError> {
    let res = application::org::auth::invitation_preview(&state.db, &token).await?;
    Ok(axum::Json(envelope(res, &request_id.0)))
}

#[utoipa::path(
    post, path = "/auth/invitations/{token}/accept",
    params(("token" = String, Path)),
    request_body = AcceptInvitationRequest,
    responses(
        (status = 200, description = "Account created; session cookies set", body = MeResponse),
        (status = 404, description = "Invitation not found or expired", body = Problem),
        (status = 400, description = "Validation failed", body = Problem),
    )
)]
async fn invitation_accept(
    axum::extract::State(state): axum::extract::State<AppState>,
    request_id: crate::extractors::RequestIdHeader,
    axum::extract::Path(token): axum::extract::Path<String>,
    Json(req): Json<AcceptInvitationRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let out = application::org::auth::accept_invitation(
        &state.db,
        chrono::Duration::from_std(state.config.session_ttl).unwrap(),
        &token,
        &req.display_name,
        &req.password,
        &request_id.0,
    )
    .await?;
    let [sc, cc] = auth_cookies(&state, &out.session_token, &out.csrf_token);
    let mut headers = HeaderMap::new();
    headers.append(header::SET_COOKIE, sc);
    headers.append(header::SET_COOKIE, cc);
    Ok((headers, axum::Json(envelope(out.me, &request_id.0))))
}

#[utoipa::path(
    get, path = "/me",
    responses(
        (status = 200, description = "Current principal", body = MeResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn me(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<axum::Json<Envelope<MeResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let principal = infrastructure::repo::auth::get_principal(&mut tx, ctx.principal_id)
        .await?
        .ok_or_else(|| AppError::not_found("principal not found"))?;
    tx.commit().await?;
    let res = application::org::me::me_for_principal(&state.db, &principal).await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

#[utoipa::path(
    get, path = "/me/sessions",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Signed continuation cursor"),
    ),
    responses(
        (status = 200, description = "Sessions belonging to the authenticated human principal", body = [contracts::org::UserSessionResponse]),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Human session required", body = Problem),
    )
)]
async fn list_my_sessions(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    page: PageParams,
) -> Result<axum::Json<Envelope<Vec<contracts::org::UserSessionResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter_hash = Cursor::hash_filter(&serde_json::json!({"list": "me-sessions"}));
    let after = keyset_after(&state, &ctx, page.cursor.as_deref(), &filter_hash)?;
    let result =
        application::org::security::list_own_sessions(&state.db, &ctx, after, page.limit as i64)
            .await?;
    let cursor = next_cursor(&state, &ctx, result.cursor_seed, &filter_hash);
    Ok(axum::Json(envelope(result.items, &request_id).with_page(
        Page {
            next_cursor: cursor,
            has_more: result.has_more,
        },
    )))
}

#[utoipa::path(
    post, path = "/me/sessions/{id}/revoke",
    params(("id" = Uuid, Path, description = "Own session id")),
    responses(
        (status = 200, description = "Own session revoked", body = contracts::org::UserSessionRevokedResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Human session required", body = Problem),
        (status = 404, description = "Session not found", body = Problem),
        (status = 409, description = "Current session must be ended through logout", body = Problem),
    )
)]
async fn revoke_my_session(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(session_id): axum::extract::Path<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<axum::Json<Envelope<contracts::org::UserSessionRevokedResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    application::org::security::require_human_session(&ctx)?;
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/me/sessions/{id}/revoke".into(),
        key,
    };
    let hash = request_hash(&session_id.to_string(), &());
    let outcome = idempotency::run(&state.db, &scope, &hash, {
        let ctx = ctx.clone();
        move |mut tx| {
            let ctx = ctx.clone();
            Box::pin(async move {
                let response =
                    application::org::security::revoke_own_session_tx(&mut tx, &ctx, session_id)
                        .await?;
                Ok((
                    tx,
                    idempotency::Stored {
                        status: 200,
                        body: response,
                    },
                ))
            })
        }
    })
    .await?;
    let stored = outcome.into_inner();
    Ok(axum::Json(envelope(stored.body, &request_id)))
}

fn password_change_request_hash(key: &[u8], current: &str, new: &str) -> Vec<u8> {
    let mut canonical = b"outreach.password-change.v1\0".to_vec();
    for value in [current, new] {
        canonical.extend_from_slice(&(value.len() as u64).to_be_bytes());
        canonical.extend_from_slice(value.as_bytes());
    }
    infrastructure::crypto::hmac_sha256(key, &canonical).to_vec()
}

async fn password_change_rate_limit(
    state: &AppState,
    ctx: &contracts::Ctx,
) -> Result<(), AppError> {
    if let Some(rl) = &state.rate_limiter {
        rl.require(
            &format!(
                "password-change:principal:{}:{}",
                ctx.workspace_id, ctx.principal_id
            ),
            5,
            Duration::from_secs(15 * 60),
            infrastructure::ratelimit::FailMode::Closed,
        )
        .await?;
    }
    Ok(())
}

#[utoipa::path(
    put, path = "/me/password",
    request_body = ChangePasswordRequest,
    responses(
        (status = 200, description = "Password changed; current session remains active", body = contracts::org::PasswordChangedResponse),
        (status = 400, description = "New password fails policy", body = Problem),
        (status = 401, description = "Current password is invalid", body = Problem),
        (status = 403, description = "Human session required", body = Problem),
        (status = 429, description = "Account/password change throttle", body = Problem),
        (status = 503, description = "Rate limiter unavailable", body = Problem),
    )
)]
async fn change_my_password(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<ChangePasswordRequest>,
) -> Result<axum::Json<Envelope<contracts::org::PasswordChangedResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    application::org::security::require_human_session(&ctx)?;
    password_change_rate_limit(&state, &ctx).await?;

    let mut principal_tx = state.db.begin(ctx.workspace_id).await?;
    let principal = infrastructure::repo::auth::get_principal(&mut principal_tx, ctx.principal_id)
        .await?
        .ok_or_else(|| AppError::not_found("principal not found"))?;
    principal_tx.commit().await?;
    let user_id = principal
        .user_id
        .filter(|_| principal.kind == "human" && principal.status == "active")
        .ok_or_else(|| AppError::forbidden("active human login required"))?;

    let request_hash = password_change_request_hash(
        &state.config.hmac_key,
        &req.current_password,
        &req.new_password,
    );
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "PUT /api/v1/me/password".into(),
        key,
    };
    let current_password = req.current_password;
    let new_password = req.new_password;
    let outcome = idempotency::run(&state.db, &scope, &request_hash, {
        let ctx = ctx.clone();
        move |mut tx| {
            let ctx = ctx.clone();
            let current_password = current_password.clone();
            let new_password = new_password.clone();
            Box::pin(async move {
                let response = application::org::security::change_password_tx(
                    &mut tx,
                    &ctx,
                    &current_password,
                    &new_password,
                )
                .await?;
                Ok((
                    tx,
                    idempotency::Stored {
                        status: 200,
                        body: response,
                    },
                ))
            })
        }
    })
    .await;

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) if error.code == ErrorCode::InvalidCredentials => {
            let locked_until =
                infrastructure::repo::auth::record_login_failure(&state.db, user_id).await?;
            let mut tx = state.db.begin(ctx.workspace_id).await?;
            infrastructure::audit::record(
                &mut tx,
                infrastructure::audit::AuditEntry::new(
                    ctx.principal_id,
                    "user.password_change_failed",
                    "user",
                    ctx.request_id.clone(),
                )
                .denied(&ErrorCode::InvalidCredentials)
                .with_target(user_id),
            )
            .await?;
            tx.commit().await?;
            if let Some(until) = locked_until
                && until > Utc::now()
            {
                return Err(ProblemError(
                    AppError::new(
                        ErrorCode::AccountLocked,
                        "too many failed attempts; try again later",
                    )
                    .with_retry_after((until - Utc::now()).num_seconds().max(1) as u32),
                ));
            }
            return Err(ProblemError(error));
        }
        Err(error) => return Err(ProblemError(error)),
    };
    let stored = outcome.into_inner();
    Ok(axum::Json(envelope(stored.body, &request_id)))
}

// --------------------------------------------------------------------------
// shared helpers for the admin surface
// --------------------------------------------------------------------------

/// Decode a signed keyset cursor into `(created_at, id)`; the cursor binds
/// the filter projection + principal (B.1.5).
fn keyset_after(
    state: &AppState,
    ctx: &contracts::Ctx,
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
    ctx: &contracts::Ctx,
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

/// Base URL for the one-time invitation link (X2). Mirrors
/// `OUTREACH_PUBLIC_BASE_URL` / bound api addr — the link is a preview URL
/// the admin copies; no email is sent.
fn invite_base(state: &AppState) -> String {
    std::env::var("OUTREACH_PUBLIC_BASE_URL")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("http://{}", state.config.api_addr))
}

/// sha256 over the canonical semantic request (path id + body) — the
/// `request_hash` of B.1.3.
fn request_hash<T: serde::Serialize>(tag: &str, body: &T) -> Vec<u8> {
    let mut bytes = tag.as_bytes().to_vec();
    bytes.extend_from_slice(&serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec()));
    infrastructure::crypto::sha256_bytes(&bytes).to_vec()
}

// --------------------------------------------------------------------------
// principals (B.2: GET /principals, GET/PATCH /principals/{id},
//               POST /principals/{id}/disable-requests)
// --------------------------------------------------------------------------

#[utoipa::path(
    get, path = "/principals",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Signed cursor"),
        ("kind" = Option<String>, Query, description = "human|agent|system"),
        ("role" = Option<String>, Query, description = "admin|manager|member|agent|system"),
        ("status" = Option<String>, Query, description = "invited|active|disabled"),
    ),
    responses(
        (status = 200, description = "Admin roster or active-human picker page", body = [PrincipalListItem]),
        (status = 400, description = "Invalid cursor/filter", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Human session required", body = Problem),
    )
)]
async fn list_principals(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    page: PageParams,
    query: Result<axum::extract::Query<PrincipalsQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::Json<Envelope<Vec<PrincipalListItem>>>, ProblemError> {
    let axum::extract::Query(q) =
        query.map_err(|_| AppError::validation("invalid query parameters", vec![]))?;
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    application::org::admin::check_principal_picker_read(&state.db, &ctx).await?;
    let admin = ctx.role == contracts::Role::Admin || application::authz::agent_admin(&ctx);
    let filter = if admin {
        application::org::admin::PrincipalFilter {
            kind: q.kind,
            role: q.role,
            status: q.status,
        }
    } else {
        application::org::admin::PrincipalFilter {
            kind: Some("human".into()),
            role: q.role,
            status: Some("active".into()),
        }
    };
    let filter_hash = Cursor::hash_filter(&serde_json::json!({
        "kind": filter.kind,
        "role": filter.role,
        "status": filter.status,
    }));
    let after = keyset_after(&state, &ctx, page.cursor.as_deref(), &filter_hash)?;
    let limit = page.limit as i64;
    let res =
        application::org::admin::list_principals(&state.db, &ctx, &filter, after, limit).await?;
    let cursor = next_cursor(&state, &ctx, res.cursor_seed, &filter_hash);
    let items = res
        .items
        .into_iter()
        .map(|principal| {
            if admin {
                PrincipalListItem::Detail(principal)
            } else {
                PrincipalListItem::Picker(PrincipalPickerResponse {
                    id: principal.id,
                    kind: principal.kind,
                    name: principal.name,
                    role: principal.role,
                    status: principal.status,
                })
            }
        })
        .collect();
    Ok(axum::Json(envelope(items, &request_id).with_page(Page {
        next_cursor: cursor,
        has_more: res.has_more,
    })))
}

/// Flattened query shape for `GET /principals` (page params + filters).
#[derive(Debug, Deserialize)]
struct PrincipalsQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    status: Option<String>,
}

/// Image response for a stored profile photo. `nosniff` keeps browsers from
/// reinterpreting the bytes; the sniffed type was verified on upload.
pub(crate) fn avatar_image_response(
    content_type: &str,
    bytes: Vec<u8>,
    cache: &'static str,
) -> (HeaderMap, Vec<u8>) {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    (headers, bytes)
}

#[utoipa::path(
    put, path = "/principals/{id}/avatar",
    params(("id" = Uuid, Path, description = "Principal id")),
    request_body(content = Vec<u8>, content_type = "image/png", description = "PNG, JPEG or WebP, at most 512 KB. The type is sniffed from the bytes."),
    responses(
        (status = 200, description = "Photo stored", body = AvatarResponse),
        (status = 400, description = "Not an image or too large", body = Problem),
        (status = 403, description = "Only admins may change another member's photo", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn put_principal_avatar(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    body: axum::body::Bytes,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::org::admin::set_avatar(&state.db, &ctx, id, &body).await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

#[utoipa::path(
    delete, path = "/principals/{id}/avatar",
    params(("id" = Uuid, Path, description = "Principal id")),
    responses(
        (status = 204, description = "Photo removed (idempotent)"),
        (status = 403, description = "Only admins may change another member's photo", body = Problem),
    )
)]
async fn delete_principal_avatar(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    application::org::admin::delete_avatar(&state.db, &ctx, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get, path = "/principals/{id}/avatar",
    params(("id" = Uuid, Path, description = "Principal id")),
    responses(
        (status = 200, description = "Photo bytes", content_type = "image/*"),
        (status = 404, description = "No photo", body = Problem),
    )
)]
async fn get_principal_avatar(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let (content_type, bytes, _) = application::org::admin::get_avatar(&state.db, &ctx, id).await?;
    Ok(avatar_image_response(&content_type, bytes, "private, max-age=300"))
}

#[utoipa::path(
    get, path = "/principals/{id}",
    params(("id" = Uuid, Path, description = "Principal id")),
    responses(
        (status = 200, description = "Principal detail (ETag = version)", body = PrincipalResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Admin session required", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_principal(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    application::org::admin::check_roster_read(&state.db, &ctx, "principal.read", Some(id)).await?;
    let res = application::org::admin::get_principal(&state.db, &ctx, id).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag(res.version));
    Ok((headers, axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    patch, path = "/principals/{id}",
    params(("id" = Uuid, Path, description = "Principal id")),
    request_body = PatchPrincipalRequest,
    responses(
        (status = 200, description = "Updated principal (new ETag)", body = PrincipalResponse),
        (status = 400, description = "Validation failed; use disable-requests to deactivate", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 409, description = "Last-admin or state conflict", body = Problem),
        (status = 412, description = "Stale version", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_principal(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    IfMatch(version): IfMatch,
    Json(req): Json<PatchPrincipalRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::org::admin::patch_principal(&state.db, &ctx, id, version, &req).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag(res.version));
    Ok((headers, axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    post, path = "/principals/{id}/disable-requests",
    params(("id" = Uuid, Path, description = "Principal to disable")),
    request_body = DisableRequestRequest,
    responses(
        (status = 202, description = "Disable accepted; cleanup job enqueued", body = DisableRequestResponse),
        (status = 400, description = "Missing Idempotency-Key / validation", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 409, description = "Already disabled / last admin / idempotency conflict", body = Problem),
    )
)]
async fn request_disable_principal(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<DisableRequestRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    application::org::admin::check_admin(
        &state.db,
        &ctx,
        "principal.disable",
        "principal",
        Some(id),
    )
    .await?;
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/principals/{id}/disable-requests".into(),
        key,
    };
    let hash = request_hash(&id.to_string(), &req);
    let outcome = idempotency::run(&state.db, &scope, &hash, {
        let ctx = ctx.clone();
        let req = req.clone();
        move |mut tx| {
            let ctx = ctx.clone();
            let req = req.clone();
            Box::pin(async move {
                let res =
                    application::org::admin::request_disable_tx(&mut tx, &ctx, id, &req).await?;
                Ok((
                    tx,
                    idempotency::Stored {
                        status: 202,
                        body: res,
                    },
                ))
            })
        }
    })
    .await?;
    let stored = outcome.into_inner();
    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&format!("/api/v1/jobs/{}", stored.body.job_id)) {
        headers.insert(header::LOCATION, v);
    }
    Ok((
        StatusCode::ACCEPTED,
        headers,
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

// --------------------------------------------------------------------------
// member invitations (X2 — link returned once; list; revoke)
// --------------------------------------------------------------------------

#[utoipa::path(
    post, path = "/member-invitations",
    request_body = CreateInvitationRequest,
    responses(
        (status = 201, description = "Invitation created; `invite_url` appears once", body = InvitationCreatedResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 409, description = "Idempotency conflict", body = Problem),
    )
)]
async fn create_invitation(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    idem: OptionalIdempotencyKey,
    Json(req): Json<CreateInvitationRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let invite_base = invite_base(&state);
    let Some(key) = idem.0 else {
        // No key: straight create (one-time URL in the fresh response).
        let res =
            application::org::admin::create_invitation(&state.db, &ctx, &req, &invite_base).await?;
        return Ok((StatusCode::CREATED, axum::Json(envelope(res, &request_id))));
    };
    application::org::admin::check_admin(
        &state.db,
        &ctx,
        "member_invitation.create",
        "member_invitation",
        None,
    )
    .await?;
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/member-invitations".into(),
        key,
    };
    let hash = request_hash("", &req);
    // The stored body never holds the one-time URL (B.1.3) — the secret is
    // spliced back only into the fresh response.
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let outcome = idempotency::run(&state.db, &scope, &hash, {
        let ctx = ctx.clone();
        let req = req.clone();
        let invite_base = invite_base.clone();
        let captured = captured.clone();
        move |mut tx| {
            let ctx = ctx.clone();
            let req = req.clone();
            let invite_base = invite_base.clone();
            let captured = captured.clone();
            Box::pin(async move {
                let mut res = application::org::admin::create_invitation_tx(
                    &mut tx,
                    &ctx,
                    &req,
                    &invite_base,
                )
                .await?;
                *captured.lock().unwrap() = res.invite_url.take();
                Ok((
                    tx,
                    idempotency::Stored {
                        status: 201,
                        body: res,
                    },
                ))
            })
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    let mut stored = outcome.into_inner();
    if !is_replay {
        stored.body.invite_url = captured.lock().unwrap().take();
    }
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/member-invitations",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Signed cursor"),
    ),
    responses(
        (status = 200, description = "Invitation page", body = [InvitationResponse]),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Not admin", body = Problem),
    )
)]
async fn list_invitations(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    page: PageParams,
) -> Result<axum::Json<Envelope<Vec<InvitationResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter_hash = Cursor::hash_filter(&serde_json::json!({"list": "member-invitations"}));
    let after = keyset_after(&state, &ctx, page.cursor.as_deref(), &filter_hash)?;
    let res = application::org::admin::list_invitations(&state.db, &ctx, after, page.limit as i64)
        .await?;
    let cursor = next_cursor(&state, &ctx, res.cursor_seed, &filter_hash);
    Ok(axum::Json(envelope(res.items, &request_id).with_page(
        Page {
            next_cursor: cursor,
            has_more: res.has_more,
        },
    )))
}

#[utoipa::path(
    post, path = "/member-invitations/{id}/revoke",
    params(("id" = Uuid, Path, description = "Invitation id")),
    responses(
        (status = 200, description = "Invitation revoked (idempotent)", body = InvitationRevokedResponse),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn revoke_invitation(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::Json<Envelope<InvitationRevokedResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::org::admin::revoke_invitation(&state.db, &ctx, id).await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

// --------------------------------------------------------------------------
// agents + service credentials
// --------------------------------------------------------------------------

#[utoipa::path(
    post, path = "/agents",
    request_body = CreateAgentRequest,
    responses(
        (status = 201, description = "Agent principal created", body = PrincipalResponse),
        (status = 400, description = "Validation failed (incl. forbidden scopes)", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 409, description = "Idempotency conflict", body = Problem),
    )
)]
async fn create_agent(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    idem: OptionalIdempotencyKey,
    Json(req): Json<CreateAgentRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let Some(key) = idem.0 else {
        let res = application::org::admin::create_agent(&state.db, &ctx, &req).await?;
        return Ok((StatusCode::CREATED, axum::Json(envelope(res, &request_id))));
    };
    application::org::admin::check_admin(&state.db, &ctx, "agent.create", "principal", None)
        .await?;
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/agents".into(),
        key,
    };
    let hash = request_hash("", &req);
    let outcome = idempotency::run(&state.db, &scope, &hash, {
        let ctx = ctx.clone();
        let req = req.clone();
        move |mut tx| {
            let ctx = ctx.clone();
            let req = req.clone();
            Box::pin(async move {
                let res = application::org::admin::create_agent_tx(&mut tx, &ctx, &req).await?;
                Ok((
                    tx,
                    idempotency::Stored {
                        status: 201,
                        body: res,
                    },
                ))
            })
        }
    })
    .await?;
    let stored = outcome.into_inner();
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/agents/{id}/credentials",
    params(("id" = Uuid, Path, description = "Agent principal id")),
    request_body = CreateCredentialRequest,
    responses(
        (status = 201, description = "Credential created; `secret` appears once", body = CredentialCreatedResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Agent not found", body = Problem),
        (status = 409, description = "Agent not active / idempotency conflict", body = Problem),
    )
)]
async fn create_credential(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(agent_id): axum::extract::Path<Uuid>,
    idem: OptionalIdempotencyKey,
    Json(req): Json<CreateCredentialRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let Some(key) = idem.0 else {
        let res =
            application::org::admin::create_credential(&state.db, &ctx, agent_id, &req).await?;
        return Ok((StatusCode::CREATED, axum::Json(envelope(res, &request_id))));
    };
    application::org::admin::check_admin(
        &state.db,
        &ctx,
        "credential.create",
        "service_credential",
        None,
    )
    .await?;
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/agents/{id}/credentials".into(),
        key,
    };
    let hash = request_hash(&agent_id.to_string(), &req);
    // Same one-time-secret handling as invitations: `secret` is stripped
    // before persistence and spliced back only on the fresh response.
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let outcome = idempotency::run(&state.db, &scope, &hash, {
        let ctx = ctx.clone();
        let req = req.clone();
        let captured = captured.clone();
        move |mut tx| {
            let ctx = ctx.clone();
            let req = req.clone();
            let captured = captured.clone();
            Box::pin(async move {
                let mut res =
                    application::org::admin::create_credential_tx(&mut tx, &ctx, agent_id, &req)
                        .await?;
                *captured.lock().unwrap() = res.secret.take();
                Ok((
                    tx,
                    idempotency::Stored {
                        status: 201,
                        body: res,
                    },
                ))
            })
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    let mut stored = outcome.into_inner();
    if !is_replay {
        stored.body.secret = captured.lock().unwrap().take();
    }
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/credentials",
    responses(
        (status = 200, description = "Service credentials (never secrets)", body = [CredentialResponse]),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Not admin", body = Problem),
    )
)]
async fn list_credentials(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<axum::Json<Envelope<Vec<CredentialResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::org::admin::list_credentials(&state.db, &ctx).await?;
    Ok(axum::Json(
        envelope(res, &request_id).with_page(Page::empty()),
    ))
}

#[utoipa::path(
    post, path = "/credentials/{id}/revoke",
    params(("id" = Uuid, Path, description = "Credential id")),
    responses(
        (status = 200, description = "Credential revoked (idempotent)", body = CredentialRevokedResponse),
        (status = 403, description = "Not admin / agent attempt", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn revoke_credential(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::Json<Envelope<CredentialRevokedResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::org::admin::revoke_credential(&state.db, &ctx, id).await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

// --------------------------------------------------------------------------
// emergency stop (B.2 운영: PUT /operations/emergency-stop)
// --------------------------------------------------------------------------

#[utoipa::path(
    get, path = "/operations/emergency-stop",
    responses(
        (status = 200, description = "Emergency-stop state (ETag = policy_version)", body = EmergencyStopResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn get_emergency_stop(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::org::admin::emergency_stop_state(&state.db, &ctx).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag(res.policy_version));
    Ok((headers, axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    put, path = "/operations/emergency-stop",
    request_body = EmergencyStopRequest,
    responses(
        (status = 200, description = "New state (new policy_version ETag)", body = EmergencyStopResponse),
        (status = 400, description = "Reason required to resume", body = Problem),
        (status = 403, description = "ON: admin/manager; OFF: admin only", body = Problem),
        (status = 412, description = "Stale policy_version", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn put_emergency_stop(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    IfMatch(version): IfMatch,
    Json(req): Json<EmergencyStopRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::org::admin::set_emergency_stop(&state.db, &ctx, version, &req).await?;
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag(res.policy_version));
    Ok((headers, axum::Json(envelope(res, &request_id))))
}
