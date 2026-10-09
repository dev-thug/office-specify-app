//! sending module routes (spec §6, B.2/B.3.10/B.3.11 — owned by the module
//! worker).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use contracts::sending::*;
use contracts::{AppError, Envelope, Meta, Page, Problem};
use serde::Deserialize;
use std::time::Duration;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IdempotencyKey, IfMatch, Json};
use crate::problem::ProblemError;
use crate::state::AppState;

const SUBMIT_RATE_LIMIT_PER_MINUTE: u32 = 60;

/// Internal /api/v1 routes for this module.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_drafts))
        .routes(routes!(create_draft))
        .routes(routes!(get_draft))
        .routes(routes!(patch_draft))
        .routes(routes!(freeze_draft))
        .routes(routes!(preflight_outbound))
        .routes(routes!(create_outbound_approval_request))
        .routes(routes!(submit_outbound))
        .routes(routes!(get_outbound))
        .routes(routes!(cancel_outbound))
}

// TODO(rebase): replace with api::extract::PageParams once the shared
// extractor lands (wave-b-common).
#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct PageParams {
    limit: Option<u32>,
    cursor: Option<String>,
    thread_id: Option<Uuid>,
}

impl PageParams {
    fn limit(&self) -> i64 {
        i64::from(self.limit.unwrap_or(50).clamp(1, 200))
    }
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

fn etag(version: i64) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::ETAG, format!("\"{version}\"").parse().unwrap());
    h
}

#[utoipa::path(
    get, path = "/message-drafts",
    params(PageParams),
    responses(
        (status = 200, description = "Drafts page", body = Vec<MessageDraftListItem>),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_drafts(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<PageParams>,
) -> Result<axum::Json<Envelope<Vec<MessageDraftListItem>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter = serde_json::json!({
        "kind": "message_drafts",
        "thread_id": q.thread_id,
    });
    let filter_hash = infrastructure::cursor::Cursor::hash_filter(&filter);
    let after = match &q.cursor {
        Some(c) => {
            let cur = infrastructure::cursor::Cursor::decode(
                c,
                &state.config.hmac_key,
                ctx.principal_id,
                &filter_hash,
            )?;
            let ts = cur
                .sort_values
                .first()
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .ok_or_else(|| AppError::validation("invalid cursor", vec![]))?;
            let id = cur
                .sort_values
                .get(1)
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| AppError::validation("invalid cursor", vec![]))?;
            Some((ts, id))
        }
        None => None,
    };
    let limit = q.limit();
    let mut items =
        application::sending::list_drafts_filtered(&state.db, &ctx, limit, q.thread_id, after)
            .await?;
    let page = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        let last = items.last().unwrap();
        let cur = infrastructure::cursor::Cursor::new(
            vec![
                serde_json::json!(last.created_at.to_rfc3339()),
                serde_json::json!(last.draft_id),
            ],
            filter_hash,
            ctx.principal_id,
        );
        Page {
            next_cursor: Some(cur.encode(&state.config.hmac_key)),
            has_more: true,
        }
    } else {
        Page::empty()
    };
    Ok(axum::Json(envelope(items, &request_id).with_page(page)))
}

#[utoipa::path(
    post, path = "/message-drafts",
    params(("Idempotency-Key" = String, Header, description = "Stable request key")),
    request_body = CreateMessageDraftRequest,
    responses(
        (status = 201, description = "Draft created", body = MessageDraftResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "No compose grant", body = Problem),
    )
)]
async fn create_draft(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Json(req): Json<CreateMessageDraftRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let request_hash =
        infrastructure::crypto::sha256_bytes(&serde_json::to_vec(&req).unwrap_or_default());
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/message-drafts".into(),
        key: key.0,
    };
    let ctx2 = ctx.clone();
    let req2 = req.clone();
    let stored =
        infrastructure::idempotency::run(&state.db, &scope, &request_hash, move |tx| async move {
            let (tx, body) = application::sending::create_draft_in_tx(tx, &ctx2, &req2).await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 201, body },
            ))
        })
        .await?
        .into_inner();
    let version = stored.body.version;
    Ok((
        StatusCode::CREATED,
        etag(version),
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/message-drafts/{id}",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Draft", body = MessageDraftResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_draft(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::sending::get_draft(&state.db, &ctx, id).await?;
    let version = res.version;
    Ok((etag(version), axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    patch, path = "/message-drafts/{id}",
    params(("id" = Uuid, Path)),
    request_body = PatchMessageDraftRequest,
    responses(
        (status = 200, description = "Draft updated", body = MessageDraftResponse),
        (status = 409, description = "Draft frozen/discarded", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_draft(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchMessageDraftRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::sending::patch_draft(&state.db, &ctx, id, if_match.0, &req).await?;
    let version = res.version;
    Ok((etag(version), axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    post, path = "/message-drafts/{id}/freeze",
    params(("id" = Uuid, Path)),
    params(("If-Match" = String, Header), ("Idempotency-Key" = String, Header)),
    request_body = FreezeMessageDraftRequest,
    responses(
        (status = 201, description = "Immutable outbound snapshot", body = FreezeMessageDraftResponse),
        (status = 409, description = "Draft already frozen / reply context changed", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 422, description = "Validation / template variables / channel mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn freeze_draft(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<FreezeMessageDraftRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let expected_version = if_match.0;
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "draft_id": id,
            "if_match": expected_version,
            "body": &req,
        }))
        .unwrap_or_default(),
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/message-drafts/{id}/freeze".into(),
        key: key.0,
    };
    let ctx2 = ctx.clone();
    let req2 = req.clone();
    let stored =
        infrastructure::idempotency::run(&state.db, &scope, &request_hash, move |tx| async move {
            let (tx, body) =
                application::sending::freeze_draft_in_tx(tx, &ctx2, id, expected_version, &req2)
                    .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 201, body },
            ))
        })
        .await?
        .into_inner();
    Ok((
        StatusCode::CREATED,
        etag(stored.body.version),
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/outbound-messages/{id}/preflight",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Non-binding policy evaluation", body = OutboundPreflightResponse),
        (status = 403, description = "No mailbox read grant", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn preflight_outbound(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<axum::Json<Envelope<OutboundPreflightResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::sending::preflight(&state.db, &ctx, id).await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

#[utoipa::path(
    post, path = "/outbound-messages/{id}/approval-requests",
    params(("id" = Uuid, Path)),
    params(("Idempotency-Key" = String, Header, description = "Stable request key")),
    request_body = CreateOutboundApprovalRequest,
    responses(
        (status = 201, description = "Approval request created", body = OutboundApprovalRequestResponse),
        (status = 409, description = "Active request exists / snapshot mismatch", body = Problem),
        (status = 422, description = "Provider policy gate not satisfied", body = Problem),
    )
)]
async fn create_outbound_approval_request(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateOutboundApprovalRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({"outbound_message_id": id, "body": &req}))
            .unwrap_or_default(),
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/outbound-messages/{id}/approval-requests".into(),
        key: key.0,
    };
    let ctx2 = ctx.clone();
    let req2 = req.clone();
    let stored =
        infrastructure::idempotency::run(&state.db, &scope, &request_hash, move |tx| async move {
            let (tx, body) =
                application::sending::create_approval_request_in_tx(tx, &ctx2, id, &req2).await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 201, body },
            ))
        })
        .await?
        .into_inner();
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/outbound-messages/{id}/submit",
    params(("id" = Uuid, Path)),
    request_body = SubmitOutboundRequest,
    responses(
        (status = 202, description = "Queued for dispatch", body = SubmitOutboundResponse),
        (status = 403, description = "Agent direct send / no send grant", body = Problem),
        (status = 409, description = "Already submitted / emergency stop / snapshot mismatch", body = Problem),
        (status = 422, description = "Recipient suppressed / basis unverified", body = Problem),
    )
)]
async fn submit_outbound(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<SubmitOutboundRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    if ctx.principal_kind != contracts::PrincipalKind::Agent
        && let Some(limiter) = &state.rate_limiter
    {
        limiter
            .require(
                &format!("send:submit:{}:{}", ctx.workspace_id, ctx.principal_id),
                SUBMIT_RATE_LIMIT_PER_MINUTE,
                Duration::from_secs(60),
                infrastructure::ratelimit::FailMode::Closed,
            )
            .await?;
    }
    let stored = application::sending::submit(&state.db, &ctx, id, &req, &key.0).await?;
    let status = StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK);
    Ok((status, axum::Json(envelope(stored.body, &request_id))))
}

#[utoipa::path(
    get, path = "/outbound-messages/{id}",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Outbound status (delivery_status is provider-evidence only)", body = OutboundMessageResponse),
        (status = 403, description = "No mailbox read grant", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_outbound(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::sending::get_outbound(&state.db, &ctx, id).await?;
    let version = res.version;
    Ok((etag(version), axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    post, path = "/outbound-messages/{id}/cancel",
    params(("id" = Uuid, Path)),
    request_body = CancelOutboundRequest,
    responses(
        (status = 200, description = "Canceled; response omits outbound content", body = CancelOutboundResponse),
        (status = 409, description = "In-flight or terminal", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn cancel_outbound(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<CancelOutboundRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::sending::cancel(&state.db, &ctx, id, if_match.0, &req).await?;
    Ok((etag(res.version), axum::Json(envelope(res, &request_id))))
}
