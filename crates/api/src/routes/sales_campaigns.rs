//! REST routes for personalized sales campaigns and the workspace SMTP default.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
};
use contracts::{
    Envelope, Problem,
    sales_campaigns::{
        AddSalesCampaignTargetRequest, CreateSalesCampaignDraftRequest, CreateSalesCampaignRequest,
        QueueSalesCampaignTargetRequest, QueueSalesCampaignTargetResponse, SalesCampaignResponse,
        SalesCampaignSenderResponse, SalesCampaignSummary, SalesCampaignTargetResponse,
        SetSalesCampaignSenderRequest, UpdateSalesCampaignTargetRequest,
    },
};
use infrastructure::{
    crypto,
    idempotency::{self, Stored},
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    extractors::{AuthCtx, IdempotencyKey, Json},
    problem::ProblemError,
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_sales_campaigns))
        .routes(routes!(create_sales_campaign))
        .routes(routes!(get_sales_campaign))
        .routes(routes!(refresh_sales_campaign_sender))
        .routes(routes!(get_sales_campaign_sender))
        .routes(routes!(set_sales_campaign_sender))
        .routes(routes!(add_sales_campaign_target))
        .routes(routes!(update_sales_campaign_target_stage))
        .routes(routes!(create_sales_campaign_target_draft))
        .routes(routes!(queue_sales_campaign_target))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope::new(data, request_id)
}

fn etag(version: i64) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::ETAG,
        HeaderValue::from_str(&format!("\"{version}\""))
            .expect("version always serializes as a header"),
    );
    headers
}

#[derive(Debug, Deserialize, IntoParams)]
struct ListParams {
    /// Opaque cursor returned in the previous list response page.
    cursor: Option<String>,
    /// Page size (default 50, maximum 200).
    limit: Option<i64>,
}

fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 200)
}

async fn cached_campaign_create_id(
    db: &infrastructure::db::Db,
    scope: &idempotency::Scope,
    request_hash: &[u8],
) -> Result<Option<Uuid>, contracts::AppError> {
    let response = idempotency::completed_response(db, scope, request_hash).await?;
    Ok(response
        .as_ref()
        .and_then(|response| response.get("id"))
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok()))
}

async fn cached_sender_profile_id(
    db: &infrastructure::db::Db,
    scope: &idempotency::Scope,
    request_hash: &[u8],
) -> Result<Option<Uuid>, contracts::AppError> {
    let response = idempotency::completed_response(db, scope, request_hash).await?;
    Ok(response
        .as_ref()
        .and_then(|response| response.get("sender"))
        .and_then(|sender| sender.get("smtp_profile_id"))
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok()))
}

#[utoipa::path(
    get, path = "/sales-campaigns",
    params(ListParams),
    responses(
        (status = 200, description = "Readable personalized sales campaigns", body = Vec<SalesCampaignSummary>),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Missing scope", body = Problem),
    )
)]
async fn list_sales_campaigns(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(query): Query<ListParams>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let (items, page) = application::sales_campaigns::list_sales_campaigns(
        &state.db,
        &ctx,
        query.cursor.as_deref(),
        clamp_limit(query.limit),
        &state.config.hmac_key,
    )
    .await?;
    Ok(axum::Json(
        Envelope::new(items, ctx.request_id.clone()).with_page(page),
    ))
}

#[utoipa::path(
    post, path = "/sales-campaigns",
    request_body = CreateSalesCampaignRequest,
    responses(
        (status = 201, description = "Personalized sales campaign created", body = SalesCampaignSummary),
        (status = 403, description = "Mailbox/scope denied", body = Problem),
        (status = 409, description = "Idempotency or sender conflict", body = Problem),
        (status = 422, description = "Validation failed", body = Problem),
    )
)]
async fn create_sales_campaign(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Json(request): Json<CreateSalesCampaignRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(&serde_json::to_vec(&request).unwrap_or_default());
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /sales-campaigns".to_string(),
        key: key.0,
    };
    if let Some(campaign_id) = cached_campaign_create_id(&state.db, &scope, &hash).await? {
        application::sales_campaigns::preauthorize_sales_campaign_compose(
            &state.db,
            &ctx,
            campaign_id,
        )
        .await?;
    } else {
        application::sales_campaigns::preauthorize_sales_campaign_create_identity(&ctx)?;
    }
    let outcome = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        let request = request.clone();
        async move {
            let body = application::sales_campaigns::create_sales_campaign(&mut tx, &ctx, &request)
                .await?;
            Ok((tx, Stored { status: 201, body }))
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    let stored = outcome.into_inner();
    if is_replay {
        application::sales_campaigns::preauthorize_sales_campaign_compose(
            &state.db,
            &ctx,
            stored.body.id,
        )
        .await?;
    }
    Ok((
        StatusCode::CREATED,
        etag(stored.body.version),
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/sales-campaigns/{id}/sender/refresh",
    params(("id" = Uuid, Path, description = "Sales campaign id")),
    responses(
        (status = 200, description = "Campaign re-pinned to the current default sender", body = SalesCampaignSummary),
        (status = 403, description = "Mailbox/scope denied", body = Problem),
        (status = 404, description = "Campaign not found", body = Problem),
        (status = 409, description = "Messages already queued/sent, or a different mailbox", body = Problem),
    )
)]
async fn refresh_sales_campaign_sender(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let summary =
        application::sales_campaigns::refresh_sales_campaign_sender(&state.db, &ctx, id).await?;
    let version = summary.version;
    Ok((
        etag(version),
        axum::Json(envelope(summary, &ctx.request_id)),
    ))
}

#[utoipa::path(
    get, path = "/sales-campaigns/{id}",
    params(("id" = Uuid, Path, description = "Sales campaign id")),
    responses(
        (status = 200, description = "Campaign with all target cards", body = SalesCampaignResponse),
        (status = 403, description = "Mailbox/scope denied", body = Problem),
        (status = 404, description = "Campaign not found", body = Problem),
    )
)]
async fn get_sales_campaign(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let detail = application::sales_campaigns::get_sales_campaign(&state.db, &ctx, id).await?;
    let version = detail.campaign.version;
    Ok((etag(version), axum::Json(envelope(detail, &ctx.request_id))))
}

#[utoipa::path(
    get, path = "/sales-campaigns/sender",
    responses(
        (status = 200, description = "Workspace default SMTP sender or null", body = SalesCampaignSenderResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Scope denied", body = Problem),
    )
)]
async fn get_sales_campaign_sender(
    State(state): State<AppState>,
    auth: AuthCtx,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let sender = application::sales_campaigns::get_sales_campaign_sender(&state.db, &ctx).await?;
    Ok(axum::Json(envelope(sender, &ctx.request_id)))
}

#[utoipa::path(
    put, path = "/sales-campaigns/sender",
    request_body = SetSalesCampaignSenderRequest,
    responses(
        (status = 200, description = "Workspace default SMTP sender updated", body = SalesCampaignSenderResponse),
        (status = 403, description = "Admin/mailbox access denied", body = Problem),
        (status = 409, description = "Idempotency conflict", body = Problem),
        (status = 422, description = "SMTP profile is not eligible", body = Problem),
    )
)]
async fn set_sales_campaign_sender(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Json(request): Json<SetSalesCampaignSenderRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(&serde_json::to_vec(&request).unwrap_or_default());
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "PUT /sales-campaigns/sender".to_string(),
        key: key.0,
    };
    if let Some(smtp_profile_id) = cached_sender_profile_id(&state.db, &scope, &hash).await? {
        application::sales_campaigns::preauthorize_sales_campaign_sender_access(
            &state.db,
            &ctx,
            smtp_profile_id,
        )
        .await?;
    } else {
        application::sales_campaigns::preauthorize_sales_campaign_sender_identity(&ctx)?;
    }
    let outcome = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        let request = request.clone();
        async move {
            let body =
                application::sales_campaigns::set_sales_campaign_sender(&mut tx, &ctx, &request)
                    .await?;
            Ok((tx, Stored { status: 200, body }))
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    if is_replay {
        application::sales_campaigns::preauthorize_sales_campaign_sender_access(
            &state.db,
            &ctx,
            request.smtp_profile_id,
        )
        .await?;
    }
    let stored = outcome.into_inner();
    Ok(axum::Json(envelope(stored.body, &request_id)))
}

#[utoipa::path(
    post, path = "/sales-campaigns/{id}/targets",
    params(("id" = Uuid, Path, description = "Sales campaign id")),
    request_body = AddSalesCampaignTargetRequest,
    responses(
        (status = 201, description = "Verified CRM target added", body = SalesCampaignTargetResponse),
        (status = 403, description = "Mailbox/CRM scope denied", body = Problem),
        (status = 409, description = "Idempotency or duplicate-recipient conflict", body = Problem),
        (status = 422, description = "CRM binding or target limit invalid", body = Problem),
    )
)]
async fn add_sales_campaign_target(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(request): Json<AddSalesCampaignTargetRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(&serde_json::to_vec(&request).unwrap_or_default());
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: format!("POST /sales-campaigns/{id}/targets"),
        key: key.0,
    };
    application::sales_campaigns::preauthorize_add_sales_campaign_target(
        &state.db, &ctx, id, &request,
    )
    .await?;
    let outcome = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        let request = request.clone();
        async move {
            let body = application::sales_campaigns::add_sales_campaign_target(
                &mut tx, &ctx, id, &request,
            )
            .await?;
            Ok((tx, Stored { status: 201, body }))
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    if is_replay {
        application::sales_campaigns::preauthorize_add_sales_campaign_target(
            &state.db, &ctx, id, &request,
        )
        .await?;
    }
    let stored = outcome.into_inner();
    Ok((
        StatusCode::CREATED,
        etag(stored.body.version),
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    patch, path = "/sales-campaigns/{id}/targets/{target_id}",
    params(
        ("id" = Uuid, Path, description = "Sales campaign id"),
        ("target_id" = Uuid, Path, description = "Sales campaign target id")
    ),
    request_body = UpdateSalesCampaignTargetRequest,
    responses(
        (status = 200, description = "Editable stage updated", body = SalesCampaignTargetResponse),
        (status = 403, description = "Mailbox/scope denied", body = Problem),
        (status = 409, description = "Idempotency or derived-stage conflict", body = Problem),
        (status = 412, description = "Target version changed", body = Problem),
        (status = 422, description = "Stage is invalid", body = Problem),
    )
)]
async fn update_sales_campaign_target_stage(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path((id, target_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateSalesCampaignTargetRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(&serde_json::to_vec(&request).unwrap_or_default());
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: format!("PATCH /sales-campaigns/{id}/targets/{target_id}"),
        key: key.0,
    };
    application::sales_campaigns::preauthorize_sales_campaign_target(
        &state.db, &ctx, id, target_id,
    )
    .await?;
    let outcome = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        let request = request.clone();
        async move {
            let body = application::sales_campaigns::update_sales_campaign_target_stage(
                &mut tx, &ctx, id, target_id, &request,
            )
            .await?;
            Ok((tx, Stored { status: 200, body }))
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    if is_replay {
        application::sales_campaigns::preauthorize_sales_campaign_target(
            &state.db, &ctx, id, target_id,
        )
        .await?;
    }
    let stored = outcome.into_inner();
    Ok((
        etag(stored.body.version),
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/sales-campaigns/{id}/targets/{target_id}/drafts",
    params(
        ("id" = Uuid, Path, description = "Sales campaign id"),
        ("target_id" = Uuid, Path, description = "Sales campaign target id")
    ),
    request_body = CreateSalesCampaignDraftRequest,
    responses(
        (status = 201, description = "Company-specific mailbox-visible marketing draft created", body = contracts::sending::MessageDraftResponse),
        (status = 403, description = "Mailbox/scope denied", body = Problem),
        (status = 409, description = "Idempotency/current-send conflict", body = Problem),
        (status = 422, description = "Draft content or CRM binding invalid", body = Problem),
    )
)]
async fn create_sales_campaign_target_draft(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path((id, target_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<CreateSalesCampaignDraftRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(&serde_json::to_vec(&request).unwrap_or_default());
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: format!("POST /sales-campaigns/{id}/targets/{target_id}/drafts"),
        key: key.0,
    };
    application::sales_campaigns::preauthorize_sales_campaign_target(
        &state.db, &ctx, id, target_id,
    )
    .await?;
    let outcome = idempotency::run(&state.db, &scope, &hash, |tx| {
        let ctx = ctx.clone();
        let request = request.clone();
        async move {
            let (tx, body) = application::sales_campaigns::create_sales_campaign_target_draft(
                tx, &ctx, id, target_id, &request,
            )
            .await?;
            Ok((tx, Stored { status: 201, body }))
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    if is_replay {
        application::sales_campaigns::preauthorize_sales_campaign_target(
            &state.db, &ctx, id, target_id,
        )
        .await?;
    }
    let stored = outcome.into_inner();
    Ok((
        StatusCode::CREATED,
        etag(stored.body.version),
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/sales-campaigns/{id}/targets/{target_id}/queue",
    params(
        ("id" = Uuid, Path, description = "Sales campaign id"),
        ("target_id" = Uuid, Path, description = "Sales campaign target id")
    ),
    request_body = QueueSalesCampaignTargetRequest,
    responses(
        (status = 202, description = "Approved outbound message scheduled for the next ten-minute batch", body = QueueSalesCampaignTargetResponse),
        (status = 403, description = "Human approval/send or mailbox scope required", body = Problem),
        (status = 409, description = "Idempotency/current-draft/approval conflict", body = Problem),
        (status = 412, description = "Target version changed", body = Problem),
        (status = 422, description = "Outbound binding or payload hash invalid", body = Problem),
    )
)]
async fn queue_sales_campaign_target(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Path((id, target_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<QueueSalesCampaignTargetRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(&serde_json::to_vec(&request).unwrap_or_default());
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: format!("POST /sales-campaigns/{id}/targets/{target_id}/queue"),
        key: key.0,
    };
    application::sales_campaigns::preauthorize_queue_sales_campaign_target(
        &state.db,
        &ctx,
        id,
        target_id,
        request.outbound_message_id,
    )
    .await?;
    let outcome = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        let request = request.clone();
        async move {
            let body = application::sales_campaigns::queue_sales_campaign_target(
                &mut tx, &ctx, id, target_id, &request,
            )
            .await?;
            Ok((tx, Stored { status: 202, body }))
        }
    })
    .await?;
    let is_replay = outcome.is_replay();
    if is_replay {
        application::sales_campaigns::preauthorize_queue_sales_campaign_target(
            &state.db,
            &ctx,
            id,
            target_id,
            request.outbound_message_id,
        )
        .await?;
    }
    let stored = outcome.into_inner();
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(envelope(stored.body, &request_id)),
    ))
}
