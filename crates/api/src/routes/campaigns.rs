//! campaigns module routes (spec §6 — B.2 endpoints). Versioned mutations
//! take If-Match (428/412); creations that enqueue work take
//! Idempotency-Key; lists are cursor-paginated (default 50, max 200).

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use contracts::campaigns::{
    CampaignDetail, CampaignSummary, CancelLaunchRequest, CreateCampaignRequest,
    CreateCampaignResponse, CreateLaunchApprovalRequest, CreateLaunchRequest, CreateLaunchResponse,
    CreateRevisionRequest, CreateRevisionResponse, ExecuteLaunchRequest, ExecuteLaunchResponse,
    LaunchApprovalRequestResponse, LaunchDetail, LaunchSummary, PauseLaunchRequest,
    PreflightRequest, PreflightResponse,
};
use contracts::{Envelope, Problem};
use infrastructure::crypto;
use infrastructure::idempotency::{self, Stored};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IdempotencyKey, IfMatch, Json};
use crate::problem::ProblemError;
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_campaigns))
        .routes(routes!(create_campaign))
        .routes(routes!(get_campaign))
        .routes(routes!(create_revision))
        .routes(routes!(preflight))
        .routes(routes!(create_launch))
        .routes(routes!(get_launch))
        .routes(routes!(execute_launch))
        .routes(routes!(create_launch_approval_request))
        .routes(routes!(pause_launch))
        .routes(routes!(cancel_launch))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope::new(data, request_id)
}

/// §4.4: responses for versioned entities carry `ETag: "<version>"`.
fn etag(version: i64) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(
        axum::http::header::ETAG,
        axum::http::HeaderValue::from_str(&format!("\"{version}\"")).unwrap(),
    );
    h
}

#[derive(Debug, Deserialize, IntoParams)]
struct ListParams {
    /// Opaque pagination cursor from a previous page.
    cursor: Option<String>,
    /// Page size (default 50, max 200).
    limit: Option<i64>,
}

fn clamp_limit(l: Option<i64>) -> i64 {
    l.unwrap_or(50).clamp(1, 200)
}

#[utoipa::path(
    get, path = "/campaigns",
    params(ListParams),
    responses(
        (status = 200, description = "Campaign page", body = Vec<CampaignSummary>),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Missing scope", body = Problem),
    )
)]
async fn list_campaigns(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<ListParams>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let (items, page) = application::campaigns::list_campaigns(
        &state.db,
        &ctx,
        q.cursor.as_deref(),
        clamp_limit(q.limit),
        &state.config.hmac_key,
    )
    .await?;
    Ok(axum::Json(
        Envelope::new(items, ctx.request_id.clone()).with_page(page),
    ))
}

#[utoipa::path(
    post, path = "/campaigns",
    request_body = CreateCampaignRequest,
    responses(
        (status = 201, description = "Campaign + initial revision created", body = CreateCampaignResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "Mailbox/scope denied", body = Problem),
        (status = 409, description = "Idempotency-Key reused", body = Problem),
        (status = 422, description = "Semantic validation failed", body = Problem),
    )
)]
async fn create_campaign(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Json(req): Json<CreateCampaignRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(&serde_json::to_vec(&req).unwrap_or_default());
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /campaigns".to_string(),
        key: key.0,
    };
    let out = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        async move {
            let body = application::campaigns::create_campaign(&mut tx, &ctx, &req).await?;
            Ok((tx, Stored { status: 201, body }))
        }
    })
    .await?
    .into_inner();
    Ok((
        axum::http::StatusCode::CREATED,
        etag(out.body.version),
        axum::Json(envelope(out.body, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/campaigns/{id}",
    params(("id" = Uuid, Path, description = "Campaign id")),
    responses(
        (status = 200, description = "Campaign detail", body = CampaignDetail),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_campaign(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let detail = application::campaigns::get_campaign(&state.db, &ctx, id).await?;
    let version = detail.campaign.version;
    Ok((etag(version), axum::Json(envelope(detail, &ctx.request_id))))
}

#[utoipa::path(
    post, path = "/campaigns/{id}/revisions",
    params(("id" = Uuid, Path, description = "Campaign id")),
    request_body = CreateRevisionRequest,
    responses(
        (status = 201, description = "New revision created", body = CreateRevisionResponse),
        (status = 404, description = "Campaign not found", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 422, description = "Semantic validation failed", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn create_revision(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateRevisionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let body = application::campaigns::create_revision(&mut tx, &ctx, id, if_match.0, &req).await?;
    tx.commit().await?;
    Ok((
        axum::http::StatusCode::CREATED,
        axum::Json(envelope(body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/campaigns/{id}/preflight",
    params(("id" = Uuid, Path, description = "Campaign id")),
    request_body = PreflightRequest,
    responses(
        (status = 200, description = "Informational preflight — never an execution authorization", body = PreflightResponse),
        (status = 404, description = "Campaign/revision not found", body = Problem),
    )
)]
async fn preflight(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<PreflightRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let res = application::campaigns::preflight(&mut tx, &ctx, id, &req).await?;
    tx.commit().await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

#[utoipa::path(
    post, path = "/campaigns/{id}/launches",
    params(("id" = Uuid, Path, description = "Campaign id")),
    request_body = CreateLaunchRequest,
    responses(
        (status = 202, description = "Launch building asynchronously; poll GET /campaign-launches/{id}", body = CreateLaunchResponse),
        (status = 404, description = "Campaign/revision not found", body = Problem),
        (status = 409, description = "Idempotency-Key reused / invalid state", body = Problem),
        (status = 422, description = "Semantic validation failed", body = Problem),
    )
)]
async fn create_launch(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateLaunchRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({"campaign_id": id, "body": &req}))
            .unwrap_or_default(),
    );
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /campaigns/{id}/launches".to_string(),
        key: key.0,
    };
    let out = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        async move {
            let body = application::campaigns::create_launch(&mut tx, &ctx, id, &req).await?;
            Ok((tx, Stored { status: 202, body }))
        }
    })
    .await?
    .into_inner();
    // §12.2 — long-running work returns 202 + a Location to poll.
    let mut headers = etag(out.body.version);
    headers.insert(
        axum::http::header::LOCATION,
        axum::http::HeaderValue::from_str(&format!(
            "/api/v1/campaign-launches/{}",
            out.body.launch_id
        ))
        .unwrap(),
    );
    Ok((
        axum::http::StatusCode::ACCEPTED,
        headers,
        axum::Json(envelope(out.body, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/campaign-launches/{id}",
    params(
        ("id" = Uuid, Path, description = "Campaign launch id"),
        ("cursor" = Option<String>, Query, description = "Messages page cursor"),
        ("limit" = Option<i64>, Query, description = "Messages page size (max 200)"),
    ),
    responses(
        (status = 200, description = "Launch detail with enrollment counts and message page", body = LaunchDetail),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_launch(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<ListParams>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let detail = application::campaigns::get_launch(
        &state.db,
        &ctx,
        id,
        q.cursor.as_deref(),
        clamp_limit(q.limit),
        &state.config.hmac_key,
    )
    .await?;
    let version = detail.launch.version;
    Ok((etag(version), axum::Json(envelope(detail, &ctx.request_id))))
}

#[utoipa::path(
    post, path = "/campaign-launches/{id}/execute",
    params(("id" = Uuid, Path, description = "Campaign launch id")),
    request_body = ExecuteLaunchRequest,
    responses(
        (status = 202, description = "Approved launch activation queued", body = ExecuteLaunchResponse),
        (status = 403, description = "Human principal or campaign mailbox grant required", body = Problem),
        (status = 404, description = "Launch not found", body = Problem),
        (status = 409, description = "Approval/snapshot/state mismatch or idempotency key reused", body = Problem),
        (status = 412, description = "Launch version mismatch", body = Problem),
        (status = 422, description = "Approval expired or policy blocked", body = Problem),
    )
)]
async fn execute_launch(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<ExecuteLaunchRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    application::authz::require_operator(&ctx)?;
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({"launch_id": id, "body": &req}))
            .unwrap_or_default(),
    );
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /campaign-launches/{id}/execute".to_string(),
        key: key.0,
    };
    let out = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        async move {
            let body = application::campaigns::execute_launch(&mut tx, &ctx, id, &req).await?;
            Ok((tx, Stored { status: 202, body }))
        }
    })
    .await?
    .into_inner();
    let mut headers = etag(out.body.version);
    headers.insert(
        axum::http::header::LOCATION,
        axum::http::HeaderValue::from_str(&format!(
            "/api/v1/campaign-launches/{}",
            out.body.launch_id
        ))
        .unwrap(),
    );
    Ok((
        axum::http::StatusCode::ACCEPTED,
        headers,
        axum::Json(envelope(out.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/campaign-launches/{id}/approval-requests",
    params(("id" = Uuid, Path, description = "Campaign launch id")),
    request_body = CreateLaunchApprovalRequest,
    responses(
        (status = 200, description = "Existing live approval request returned", body = LaunchApprovalRequestResponse),
        (status = 201, description = "Approval request created", body = LaunchApprovalRequestResponse),
        (status = 404, description = "Launch not found", body = Problem),
        (status = 409, description = "Snapshot mismatch / launch not frozen / key reused", body = Problem),
        (status = 422, description = "Provider policy / emergency stop block", body = Problem),
    )
)]
async fn create_launch_approval_request(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateLaunchApprovalRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({"launch_id": id, "body": &req}))
            .unwrap_or_default(),
    );
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /campaign-launches/{id}/approval-requests".to_string(),
        key: key.0,
    };
    let out = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        async move {
            let (body, created) = application::campaigns::request_launch_approval(
                &mut tx,
                &ctx,
                id,
                &req.expected_manifest_hash,
                req.reason.as_deref(),
            )
            .await?;
            // B.2: the endpoint creates the request (201); a repeated call
            // returns the existing live request (200).
            let status = if created { 201 } else { 200 };
            Ok((tx, Stored { status, body }))
        }
    })
    .await?
    .into_inner();
    let status = axum::http::StatusCode::from_u16(out.status).unwrap_or(axum::http::StatusCode::OK);
    Ok((
        status,
        etag(out.body.version),
        axum::Json(envelope(out.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/campaign-launches/{id}/pause",
    params(("id" = Uuid, Path, description = "Campaign launch id")),
    request_body = PauseLaunchRequest,
    responses(
        (status = 200, description = "Launch paused", body = LaunchSummary),
        (status = 404, description = "Launch not found", body = Problem),
        (status = 409, description = "Invalid state", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn pause_launch(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<PauseLaunchRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let body =
        application::campaigns::pause_launch(&mut tx, &ctx, id, if_match.0, req.reason.as_deref())
            .await?;
    tx.commit().await?;
    let version = body.version;
    Ok((etag(version), axum::Json(envelope(body, &request_id))))
}

#[utoipa::path(
    post, path = "/campaign-launches/{id}/cancel",
    params(("id" = Uuid, Path, description = "Campaign launch id")),
    request_body = CancelLaunchRequest,
    responses(
        (status = 200, description = "Launch canceled", body = LaunchSummary),
        (status = 404, description = "Launch not found", body = Problem),
        (status = 409, description = "Invalid state", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn cancel_launch(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<CancelLaunchRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let body =
        application::campaigns::cancel_launch(&mut tx, &ctx, id, if_match.0, req.reason.as_deref())
            .await?;
    tx.commit().await?;
    let version = body.version;
    Ok((etag(version), axum::Json(envelope(body, &request_id))))
}
