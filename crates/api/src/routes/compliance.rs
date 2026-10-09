//! compliance module routes (spec §6 — owned by the module worker).
//!
//! Internal: `/communication-bases` (+reviews, +revoke), `/suppressions`
//! (+release-requests). Public: `/unsubscribe/{token}` GET/POST and the
//! RFC 8058 one-click endpoint (gate off → documented disabled response).

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use contracts::compliance::{
    CommunicationBasisResponse, CreateCommunicationBasisRequest, CreateSuppressionRequest,
    ReleaseRequestResponse, ReleaseSuppressionRequest, ReviewBasisRequest, SuppressionResponse,
    UnsubscribeConfirmationResponse, UnsubscribeResultResponse,
};
use contracts::{AppError, Envelope, Meta, Page, Problem};
use infrastructure::cursor::Cursor;
use infrastructure::repo::compliance::{BasisFilter, SuppressionFilter};
use serde::Deserialize;
use std::net::SocketAddr;
use std::time::Duration;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IfMatch, Json, PageParams, RequestIdHeader};
use crate::problem::ProblemError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
struct BasisListQuery {
    email_address_id: Option<Uuid>,
    purpose: Option<String>,
    status: Option<String>,
    jurisdiction: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SuppressionListQuery {
    email_address_id: Option<Uuid>,
    purpose_scope: Option<String>,
    reason: Option<String>,
    /// `active` (default view shows both) | `released`.
    status: Option<String>,
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

fn etag_header(version: i64) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&format!("\"{version}\"")) {
        h.insert(header::ETAG, v);
    }
    h
}

/// Decode the opaque cursor: binds sort values + filter hash + principal
/// (B.1.5). Returns the keyset `(created_at, id)` start point.
fn decode_after(
    cursor: Option<&str>,
    state: &AppState,
    principal: Uuid,
    filter: &serde_json::Value,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let Some(raw) = cursor else {
        return Ok(None);
    };
    let invalid = || AppError::validation("invalid pagination cursor", vec![]);
    let cur = Cursor::decode(
        raw,
        &state.config.hmac_key,
        principal,
        &Cursor::hash_filter(filter),
    )?;
    let ts = cur
        .sort_values
        .first()
        .and_then(|v| v.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or_else(invalid)?;
    let id = cur
        .sort_values
        .get(1)
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(invalid)?;
    Ok(Some((ts, id)))
}

fn next_cursor(
    state: &AppState,
    principal: Uuid,
    filter: &serde_json::Value,
    last: Option<(DateTime<Utc>, Uuid)>,
) -> Option<String> {
    let (ts, id) = last?;
    Some(
        Cursor::new(
            vec![serde_json::json!(ts.to_rfc3339()), serde_json::json!(id)],
            Cursor::hash_filter(filter),
            principal,
        )
        .encode(&state.config.hmac_key),
    )
}

// -------------------------------------------------------- internal API

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_bases))
        .routes(routes!(create_basis))
        .routes(routes!(get_basis))
        .routes(routes!(review_basis))
        .routes(routes!(revoke_basis))
        .routes(routes!(list_suppressions))
        .routes(routes!(create_suppression))
        .routes(routes!(get_suppression))
        .routes(routes!(request_release))
}

#[utoipa::path(
    get, path = "/communication-bases",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Opaque continuation cursor"),
        ("email_address_id" = Option<Uuid>, Query),
        ("purpose" = Option<String>, Query, description = "marketing|correspondence|transactional"),
        ("status" = Option<String>, Query, description = "pending|valid|rejected|revoked|expired"),
        ("jurisdiction" = Option<String>, Query),
    ),
    responses(
        (status = 200, description = "Basis list", body = [CommunicationBasisResponse]),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Forbidden", body = Problem),
    )
)]
async fn list_bases(
    State(state): State<AppState>,
    auth: AuthCtx,
    page_params: PageParams,
    Query(q): Query<BasisListQuery>,
) -> Result<axum::Json<Envelope<Vec<CommunicationBasisResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let filter_json = serde_json::json!({
        "kind": "communication-bases",
        "email_address_id": q.email_address_id,
        "purpose": q.purpose.clone(),
        "status": q.status.clone(),
        "jurisdiction": q.jurisdiction.clone(),
    });
    let after = decode_after(
        page_params.cursor.as_deref(),
        &state,
        ctx.principal_id,
        &filter_json,
    )?;
    let filter = BasisFilter {
        email_address_id: q.email_address_id,
        purpose: q.purpose,
        status: q.status,
        jurisdiction: q.jurisdiction,
        after,
    };
    let (rows, last) =
        application::compliance::bases::list_bases(&state.db, &ctx, filter, page_params.limit)
            .await?;
    let page = Page {
        next_cursor: next_cursor(&state, ctx.principal_id, &filter_json, last),
        has_more: last.is_some(),
    };
    Ok(axum::Json(envelope(rows, &ctx.request_id).with_page(page)))
}

#[utoipa::path(
    post, path = "/communication-bases",
    request_body = CreateCommunicationBasisRequest,
    responses(
        (status = 201, description = "Basis registered (pending)", body = CommunicationBasisResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 404, description = "Email address not found", body = Problem),
        (status = 422, description = "Invalid basis fields", body = Problem),
    )
)]
async fn create_basis(
    State(state): State<AppState>,
    auth: AuthCtx,
    Json(req): Json<CreateCommunicationBasisRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::compliance::bases::create_basis(&state.db, &ctx, req).await?;
    let headers = etag_header(res.version);
    Ok((
        StatusCode::CREATED,
        headers,
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/communication-bases/{id}",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Basis", body = CommunicationBasisResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_basis(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::compliance::bases::get_basis(&state.db, &ctx, id).await?;
    Ok((
        etag_header(res.version),
        axum::Json(envelope(res, &ctx.request_id)),
    ))
}

#[utoipa::path(
    post, path = "/communication-bases/{id}/reviews",
    params(("id" = Uuid, Path), ("if-match" = String, Header, description = "Expected version, e.g. \"3\"")),
    request_body = ReviewBasisRequest,
    responses(
        (status = 200, description = "Reviewed basis", body = CommunicationBasisResponse),
        (status = 409, description = "Already decided", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn review_basis(
    State(state): State<AppState>,
    auth: AuthCtx,
    IfMatch(version): IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<ReviewBasisRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res =
        application::compliance::bases::review_basis(&state.db, &ctx, id, version, req).await?;
    Ok((
        etag_header(res.version),
        axum::Json(envelope(res, &ctx.request_id)),
    ))
}

#[utoipa::path(
    post, path = "/communication-bases/{id}/revoke",
    params(("id" = Uuid, Path), ("if-match" = String, Header)),
    responses(
        (status = 200, description = "Revoked basis", body = CommunicationBasisResponse),
        (status = 409, description = "Already revoked", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn revoke_basis(
    State(state): State<AppState>,
    auth: AuthCtx,
    IfMatch(version): IfMatch,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::compliance::bases::revoke_basis(&state.db, &ctx, id, version).await?;
    Ok((
        etag_header(res.version),
        axum::Json(envelope(res, &ctx.request_id)),
    ))
}

#[utoipa::path(
    get, path = "/suppressions",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Opaque continuation cursor"),
        ("email_address_id" = Option<Uuid>, Query),
        ("purpose_scope" = Option<String>, Query, description = "marketing|all"),
        ("reason" = Option<String>, Query, description = "unsubscribe|complaint|hard_bounce|manual"),
        ("status" = Option<String>, Query, description = "active|released"),
    ),
    responses(
        (status = 200, description = "Suppression list", body = [SuppressionResponse]),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Forbidden", body = Problem),
    )
)]
async fn list_suppressions(
    State(state): State<AppState>,
    auth: AuthCtx,
    page_params: PageParams,
    Query(q): Query<SuppressionListQuery>,
) -> Result<axum::Json<Envelope<Vec<SuppressionResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let released = match q.status.as_deref() {
        None => None,
        Some("active") => Some(false),
        Some("released") => Some(true),
        Some(_) => {
            return Err(ProblemError(AppError::validation(
                "status must be 'active' or 'released'",
                vec![],
            )));
        }
    };
    let filter_json = serde_json::json!({
        "kind": "suppressions",
        "email_address_id": q.email_address_id,
        "purpose_scope": q.purpose_scope.clone(),
        "reason": q.reason.clone(),
        "status": q.status.clone(),
    });
    let after = decode_after(
        page_params.cursor.as_deref(),
        &state,
        ctx.principal_id,
        &filter_json,
    )?;
    let filter = SuppressionFilter {
        email_address_id: q.email_address_id,
        purpose_scope: q.purpose_scope,
        reason: q.reason,
        released,
        after,
    };
    let (rows, last) = application::compliance::suppressions::list_suppressions(
        &state.db,
        &ctx,
        filter,
        page_params.limit,
    )
    .await?;
    let page = Page {
        next_cursor: next_cursor(&state, ctx.principal_id, &filter_json, last),
        has_more: last.is_some(),
    };
    Ok(axum::Json(envelope(rows, &ctx.request_id).with_page(page)))
}

#[utoipa::path(
    post, path = "/suppressions",
    request_body = CreateSuppressionRequest,
    responses(
        (status = 200, description = "Existing active suppression reaffirmed", body = SuppressionResponse),
        (status = 201, description = "Suppression created", body = SuppressionResponse),
        (status = 404, description = "Email address not found", body = Problem),
        (status = 422, description = "Invalid fields", body = Problem),
    )
)]
async fn create_suppression(
    State(state): State<AppState>,
    auth: AuthCtx,
    Json(req): Json<CreateSuppressionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let out = application::compliance::suppressions::create_suppression(
        &state.db,
        &ctx,
        state.rate_limiter.as_ref(),
        req,
    )
    .await?;
    let headers = etag_header(out.response.version);
    let status = if out.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        status,
        headers,
        axum::Json(envelope(out.response, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/suppressions/{id}",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Suppression", body = SuppressionResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_suppression(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::compliance::suppressions::get_suppression(&state.db, &ctx, id).await?;
    Ok((
        etag_header(res.version),
        axum::Json(envelope(res, &ctx.request_id)),
    ))
}

#[utoipa::path(
    post, path = "/suppressions/{id}/release-requests",
    params(("id" = Uuid, Path)),
    request_body = ReleaseSuppressionRequest,
    responses(
        (status = 201, description = "Pending release approval request", body = ReleaseRequestResponse),
        (status = 404, description = "Suppression not found", body = Problem),
        (status = 409, description = "Already released or request pending", body = Problem),
    )
)]
async fn request_release(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<ReleaseSuppressionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res =
        application::compliance::suppressions::request_release(&state.db, &ctx, id, req).await?;
    Ok((StatusCode::CREATED, axum::Json(envelope(res, &request_id))))
}

// ---------------------------------------------------------- public API

/// Public endpoints get `Cache-Control: no-store`, `Referrer-Policy:
/// no-referrer`, `X-Robots-Tag: noindex` (spec §4.4) on every response —
/// including problem+json, which is why this lives in a layer, not in
/// the success-path headers.
async fn public_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    res
}

/// `/public/v1` unsubscribe capability routes. The URL token is the
/// capability — no session, credential or CSRF applies (§11.2, B.4).
pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(unsubscribe_confirmation))
        .routes(routes!(unsubscribe))
        .routes(routes!(unsubscribe_one_click))
        .layer(axum::middleware::from_fn(public_headers))
}

fn public_client_ip(state: &AppState, headers: &HeaderMap, addr: &SocketAddr) -> std::net::IpAddr {
    crate::extractors::client_ip(&state.config, headers, addr.ip())
}

/// Abuse limit for public unsubscribe writes — per-IP, fail closed when
/// Redis is configured (spec §16.3). Unconfigured limiter → skipped.
async fn public_write_limit(
    state: &AppState,
    headers: &HeaderMap,
    addr: &SocketAddr,
) -> Result<(), AppError> {
    if let Some(rl) = &state.rate_limiter {
        rl.require(
            &format!("unsub:post:{}", public_client_ip(state, headers, addr)),
            30,
            Duration::from_secs(60),
            infrastructure::ratelimit::FailMode::Closed,
        )
        .await?;
    }
    Ok(())
}

/// Lighter limit for the read-only confirmation — prefetch storms are
/// expected here and writes are the sensitive path.
async fn public_read_limit(
    state: &AppState,
    headers: &HeaderMap,
    addr: &SocketAddr,
) -> Result<(), AppError> {
    if let Some(rl) = &state.rate_limiter {
        rl.require(
            &format!("unsub:get:{}", public_client_ip(state, headers, addr)),
            120,
            Duration::from_secs(60),
            infrastructure::ratelimit::FailMode::Closed,
        )
        .await?;
    }
    Ok(())
}

#[utoipa::path(
    get, path = "/unsubscribe/{token}",
    params(("token" = String, Path, description = "Unsubscribe capability token")),
    responses(
        (status = 200, description = "Confirmation data — never a state change", body = UnsubscribeConfirmationResponse),
        (status = 404, description = "Unknown or revoked token", body = Problem),
    )
)]
async fn unsubscribe_confirmation(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request_id: RequestIdHeader,
    Path(token): Path<String>,
) -> Result<axum::Json<Envelope<UnsubscribeConfirmationResponse>>, ProblemError> {
    public_read_limit(&state, &headers, &addr).await?;
    let res = application::compliance::public_unsubscribe::confirmation(&state.db, &token).await?;
    Ok(axum::Json(envelope(res, &request_id.0)))
}

#[utoipa::path(
    post, path = "/unsubscribe/{token}",
    params(("token" = String, Path)),
    responses(
        (status = 200, description = "Marketing suppression recorded (idempotent)", body = UnsubscribeResultResponse),
        (status = 400, description = "Body must be empty or {}", body = Problem),
        (status = 404, description = "Unknown or revoked token", body = Problem),
        (status = 429, description = "Rate limited", body = Problem),
    )
)]
async fn unsubscribe(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request_id: RequestIdHeader,
    Path(token): Path<String>,
    body: Bytes,
) -> Result<axum::Json<Envelope<UnsubscribeResultResponse>>, ProblemError> {
    // B.4.4: the body is an empty object or empty.
    if !body.is_empty() {
        let ok = serde_json::from_slice::<serde_json::Value>(&body)
            .map(|v| v.as_object().is_some_and(|o| o.is_empty()))
            .unwrap_or(false);
        if !ok {
            return Err(ProblemError(AppError::validation(
                "body must be empty or an empty JSON object",
                vec![],
            )));
        }
    }
    public_write_limit(&state, &headers, &addr).await?;
    let res =
        application::compliance::public_unsubscribe::unsubscribe(&state.db, &token, &request_id.0)
            .await?;
    Ok(axum::Json(envelope(res, &request_id.0)))
}

#[utoipa::path(
    post, path = "/unsubscribe/{token}/one-click",
    params(("token" = String, Path)),
    responses(
        (status = 404, description = "Unknown or revoked token", body = Problem),
        (status = 422, description = "One-click capability not verified for this channel", body = Problem),
    )
)]
async fn unsubscribe_one_click(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request_id: RequestIdHeader,
    Path(token): Path<String>,
    // RFC 8058 posts `List-Unsubscribe=One-Click` as a form body; while the
    // capability gate is off the body is irrelevant and left unread.
    _body: Body,
) -> Result<axum::Json<Envelope<UnsubscribeResultResponse>>, ProblemError> {
    public_write_limit(&state, &headers, &addr).await?;
    let res = application::compliance::public_unsubscribe::one_click(&state.db, &token).await?;
    Ok(axum::Json(envelope(res, &request_id.0)))
}
