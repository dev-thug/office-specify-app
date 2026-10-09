//! Authenticated export and bulk-send controls for Office-owned Zoho
//! Campaigns content. The public route only serves a signed immutable HTML
//! snapshot to Zoho while its capability remains valid.

use crate::{
    extractors::{AuthCtx, IdempotencyKey, Json},
    problem::ProblemError,
    state::AppState,
};
use axum::{
    Json as AxumJson,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use contracts::{
    AppError, Envelope, Meta, Problem,
    marketing::{
        MarketingExportContentResponse, MarketingExportResponse, PrepareMarketingExportRequest,
        SendMarketingExportRequest,
    },
};
use infrastructure::{crypto, idempotency};
use serde::{Deserialize, Serialize};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeliveryIdempotencyReceipt {
    resource_id: Uuid,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
struct ExportListQuery {
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
struct PublicContentQuery {
    expires: i64,
    content_hash: String,
    signature: String,
}

fn envelope<T: Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope {
        data,
        meta: Meta {
            request_id: request_id.to_string(),
        },
        page: None,
    }
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(prepare_export))
        .routes(routes!(list_exports))
        .routes(routes!(get_export))
        .routes(routes!(get_export_content))
        .routes(routes!(sync_export))
        .routes(routes!(send_export))
        .routes(routes!(refresh_export))
}

pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_public_content))
}

async fn reserve_idempotency(
    state: &AppState,
    ctx: &contracts::Ctx,
    endpoint: &str,
    key: String,
    request_hash: [u8; 32],
    resource_id: Uuid,
) -> Result<(), AppError> {
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.to_string(),
        key,
    };
    idempotency::run(&state.db, &scope, &request_hash, move |tx| async move {
        Ok((
            tx,
            idempotency::Stored {
                status: 202,
                body: DeliveryIdempotencyReceipt { resource_id },
            },
        ))
    })
    .await?;
    Ok(())
}

fn request_hash<T: Serialize>(resource_id: Uuid, body: &T) -> Result<[u8; 32], AppError> {
    let encoded = serde_json::to_vec(&(resource_id, body))
        .map_err(|_| AppError::internal("could not fingerprint marketing export request"))?;
    Ok(crypto::sha256_bytes(&encoded))
}

#[utoipa::path(
    post, path="/marketing/exports", request_body=PrepareMarketingExportRequest,
    responses((status=201, description="Immutable Office export metadata", body=MarketingExportResponse), (status=403, body=Problem), (status=409, body=Problem), (status=422, body=Problem), (status=503, body=Problem))
)]
async fn prepare_export(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(request): Json<PrepareMarketingExportRequest>,
) -> Result<(StatusCode, AxumJson<Envelope<MarketingExportResponse>>), ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.export_prepare").await?;
    reserve_idempotency(
        &state,
        &ctx,
        "POST /api/v1/marketing/exports",
        key,
        request_hash(request.launch_id, &request)?,
        request.launch_id,
    )
    .await?;
    let result = application::marketing::delivery::prepare_export(
        &state.db,
        &ctx,
        &state.config,
        state.campaigns.as_deref(),
        state.operations.as_deref(),
        &request,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        AxumJson(envelope(result, &ctx.request_id)),
    ))
}

#[utoipa::path(
    get, path="/marketing/exports", params(ExportListQuery),
    responses((status=200, body=[MarketingExportResponse]), (status=403, body=Problem))
)]
async fn list_exports(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(query): Query<ExportListQuery>,
) -> Result<AxumJson<Envelope<Vec<MarketingExportResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.export_list").await?;
    let rows =
        application::marketing::delivery::list_exports(&state.db, &ctx, query.limit.unwrap_or(50))
            .await?;
    Ok(AxumJson(envelope(rows, &ctx.request_id)))
}

#[utoipa::path(
    get, path="/marketing/exports/{id}", params(("id"=Uuid, Path)),
    responses((status=200, body=MarketingExportResponse), (status=403, body=Problem), (status=404, body=Problem))
)]
async fn get_export(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<AxumJson<Envelope<MarketingExportResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.export_read").await?;
    let row = application::marketing::delivery::get_export(&state.db, &ctx, id).await?;
    Ok(AxumJson(envelope(row, &ctx.request_id)))
}

#[utoipa::path(
    get, path="/marketing/exports/{id}/content", params(("id"=Uuid, Path)),
    responses((status=200, description="Authorized Office content preview", body=MarketingExportContentResponse), (status=403, body=Problem), (status=404, body=Problem))
)]
async fn get_export_content(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<AxumJson<Envelope<MarketingExportContentResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.export_content_read").await?;
    let content = application::marketing::delivery::get_content(&state.db, &ctx, id).await?;
    Ok(AxumJson(envelope(content, &ctx.request_id)))
}

#[utoipa::path(
    post, path="/marketing/exports/{id}/sync", params(("id"=Uuid, Path)),
    responses((status=200, description="Provider list and draft synchronization metadata", body=MarketingExportResponse), (status=403, body=Problem), (status=409, body=Problem), (status=503, body=Problem))
)]
async fn sync_export(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<AxumJson<Envelope<MarketingExportResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.export_sync").await?;
    reserve_idempotency(
        &state,
        &ctx,
        "POST /api/v1/marketing/exports/{id}/sync",
        key,
        request_hash(id, &"sync-v1")?,
        id,
    )
    .await?;
    let row = application::marketing::delivery::sync_export(
        &state.db,
        &ctx,
        &state.config,
        state.campaigns.as_deref(),
        state.operations.as_deref(),
        id,
    )
    .await?;
    Ok(AxumJson(envelope(row, &ctx.request_id)))
}

#[utoipa::path(
    post, path="/marketing/exports/{id}/send", params(("id"=Uuid, Path)), request_body=SendMarketingExportRequest,
    responses((status=200, description="Single Zoho Campaigns bulk submission status", body=MarketingExportResponse), (status=403, body=Problem), (status=409, body=Problem), (status=422, body=Problem), (status=503, body=Problem))
)]
async fn send_export(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
    Json(request): Json<SendMarketingExportRequest>,
) -> Result<AxumJson<Envelope<MarketingExportResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.export_send").await?;
    reserve_idempotency(
        &state,
        &ctx,
        "POST /api/v1/marketing/exports/{id}/send",
        key,
        request_hash(id, &request)?,
        id,
    )
    .await?;
    let row = application::marketing::delivery::send_export(
        &state.db,
        &ctx,
        &state.config,
        state.campaigns.as_deref(),
        state.operations.as_deref(),
        id,
        &request,
    )
    .await?;
    Ok(AxumJson(envelope(row, &ctx.request_id)))
}

#[utoipa::path(
    post, path="/marketing/exports/{id}/refresh", params(("id"=Uuid, Path)),
    responses((status=200, description="Read-only provider status and partial report metadata", body=MarketingExportResponse), (status=403, body=Problem), (status=404, body=Problem), (status=503, body=Problem))
)]
async fn refresh_export(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<AxumJson<Envelope<MarketingExportResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::marketing::authorize(&state.db, &ctx, "marketing.export_refresh").await?;
    reserve_idempotency(
        &state,
        &ctx,
        "POST /api/v1/marketing/exports/{id}/refresh",
        key,
        request_hash(id, &"refresh-v1")?,
        id,
    )
    .await?;
    let row = application::marketing::delivery::refresh_export(
        &state.db,
        &ctx,
        &state.config,
        state.campaigns.as_deref(),
        state.operations.as_deref(),
        id,
    )
    .await?;
    Ok(AxumJson(envelope(row, &ctx.request_id)))
}

#[utoipa::path(
    get, path="/marketing-content/{workspace_id}/{export_id}", params(("workspace_id"=Uuid, Path), ("export_id"=Uuid, Path), PublicContentQuery),
    responses((status=200, description="Signed, immutable campaign HTML", content_type="text/html"), (status=404, body=Problem))
)]
async fn get_public_content(
    State(state): State<AppState>,
    Path((workspace_id, export_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<PublicContentQuery>,
) -> Result<Response, ProblemError> {
    let html = application::marketing::delivery::get_public_content(
        &state.db,
        &state.config,
        workspace_id,
        export_id,
        query.expires,
        &query.content_hash,
        &query.signature,
    )
    .await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; img-src https: data:; font-src https: data:; connect-src 'none'; form-action 'none'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    Ok((StatusCode::OK, headers, Body::from(html)).into_response())
}
