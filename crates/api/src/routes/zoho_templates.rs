//! Authenticated CRUD for templates in the separately prepaid Zoho Email API.

use crate::{
    extractors::{AuthCtx, IdempotencyKey, Json},
    problem::ProblemError,
    state::AppState,
};
use axum::{
    Json as AxumJson,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use contracts::{
    AppError, Envelope, Page, Problem,
    marketing::{
        ZohoTemplateDetailsResponse, ZohoTemplateMutationResponse, ZohoTemplateSummaryResponse,
        ZohoTemplateWriteRequest,
    },
};
use infrastructure::cursor::Cursor;
use serde::Deserialize;
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_templates))
        .routes(routes!(get_template))
        .routes(routes!(create_template))
        .routes(routes!(update_template))
        .routes(routes!(delete_template))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct TemplateListQuery {
    limit: Option<u32>,
    cursor: Option<String>,
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope::new(data, request_id)
}

fn page_start(
    state: &AppState,
    ctx: &contracts::Ctx,
    raw: Option<&str>,
    limit: u32,
) -> Result<u32, AppError> {
    let filter_hash = Cursor::hash_filter(
        &serde_json::json!({"resource":"zoho-email-api-templates","limit":limit}),
    );
    let Some(raw) = raw else { return Ok(1) };
    let cursor = Cursor::decode(raw, &state.config.hmac_key, ctx.principal_id, &filter_hash)?;
    let Some(start) = cursor
        .sort_values
        .first()
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
    else {
        return Err(AppError::validation("invalid template cursor", vec![]));
    };
    Ok(start)
}

fn next_cursor(state: &AppState, ctx: &contracts::Ctx, limit: u32, next_start: u32) -> String {
    Cursor::new(
        vec![serde_json::json!(next_start)],
        Cursor::hash_filter(
            &serde_json::json!({"resource":"zoho-email-api-templates","limit":limit}),
        ),
        ctx.principal_id,
    )
    .encode(&state.config.hmac_key)
}

#[utoipa::path(
    get, path="/zoho-email-api/templates",
    params(TemplateListQuery),
    responses((status=200, description="Remote template page", body=[ZohoTemplateSummaryResponse]), (status=400, body=Problem), (status=403, body=Problem), (status=503, body=Problem))
)]
async fn list_templates(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(query): Query<TemplateListQuery>,
) -> Result<AxumJson<Envelope<Vec<ZohoTemplateSummaryResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    application::marketing::authorize(&state.db, &ctx, "marketing.zoho_email_api.template_list")
        .await?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(AppError::validation("limit must be between 1 and 100", vec![]).into());
    }
    let start = page_start(&state, &ctx, query.cursor.as_deref(), limit)?;
    let end = start
        .checked_add(limit - 1)
        .ok_or_else(|| AppError::validation("template cursor is out of range", vec![]))?;
    let items = application::marketing::email_templates::list_templates(
        &state.db,
        &ctx,
        &state.config,
        state.email_api.as_deref(),
        start,
        end,
    )
    .await?;
    let has_more = items.len() == limit as usize;
    let page = match has_more.then(|| end.checked_add(1)).flatten() {
        Some(next_start) => Page {
            next_cursor: Some(next_cursor(&state, &ctx, limit, next_start)),
            has_more: true,
        },
        None => Page::empty(),
    };
    Ok(AxumJson(envelope(items, &request_id).with_page(page)))
}

#[utoipa::path(
    get, path="/zoho-email-api/templates/{id}",
    params(("id"=String, Path, description="Zoho template identifier")),
    responses((status=200, description="Remote template detail; quoted ETag is the content hash", body=ZohoTemplateDetailsResponse), (status=403, body=Problem), (status=404, body=Problem), (status=503, body=Problem))
)]
async fn get_template(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<String>,
) -> Result<(HeaderMap, AxumJson<Envelope<ZohoTemplateDetailsResponse>>), ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let detail = application::marketing::email_templates::get_template(
        &state.db,
        &ctx,
        &state.config,
        state.email_api.as_deref(),
        &id,
    )
    .await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", detail.content_hash))
            .map_err(|_| AppError::internal("could not encode template content hash"))?,
    );
    Ok((headers, AxumJson(envelope(detail, &request_id))))
}

#[utoipa::path(
    post, path="/zoho-email-api/templates",
    request_body=ZohoTemplateWriteRequest,
    responses((status=201, description="Template created; response contains metadata only", body=ZohoTemplateMutationResponse), (status=403, body=Problem), (status=409, body=Problem), (status=422, body=Problem), (status=503, body=Problem))
)]
async fn create_template(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(request): Json<ZohoTemplateWriteRequest>,
) -> Result<(StatusCode, AxumJson<Envelope<ZohoTemplateMutationResponse>>), ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let result = application::marketing::email_templates::create_template(
        &state.db,
        &ctx,
        &state.config,
        state.email_api.as_deref(),
        &key,
        &request,
    )
    .await?;
    Ok((StatusCode::CREATED, AxumJson(envelope(result, &request_id))))
}

#[utoipa::path(
    put, path="/zoho-email-api/templates/{id}",
    params(("id"=String, Path, description="Zoho template identifier")),
    request_body=ZohoTemplateWriteRequest,
    responses((status=200, description="Template updated; response contains metadata only", body=ZohoTemplateMutationResponse), (status=403, body=Problem), (status=409, body=Problem), (status=412, body=Problem), (status=428, body=Problem), (status=422, body=Problem), (status=503, body=Problem))
)]
async fn update_template(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<String>,
    headers: HeaderMap,
    IdempotencyKey(key): IdempotencyKey,
    Json(request): Json<ZohoTemplateWriteRequest>,
) -> Result<AxumJson<Envelope<ZohoTemplateMutationResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let result = application::marketing::email_templates::update_template(
        &state.db,
        &ctx,
        &state.config,
        state.email_api.as_deref(),
        &key,
        &id,
        if_match,
        &request,
    )
    .await?;
    Ok(AxumJson(envelope(result, &request_id)))
}

#[utoipa::path(
    delete, path="/zoho-email-api/templates/{id}",
    params(("id"=String, Path, description="Zoho template identifier")),
    responses((status=200, description="Template deleted; caller confirms before invoking; response contains metadata only", body=ZohoTemplateMutationResponse), (status=403, body=Problem), (status=409, body=Problem), (status=412, body=Problem), (status=428, body=Problem), (status=503, body=Problem))
)]
async fn delete_template(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<String>,
    headers: HeaderMap,
    IdempotencyKey(key): IdempotencyKey,
) -> Result<AxumJson<Envelope<ZohoTemplateMutationResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let result = application::marketing::email_templates::delete_template(
        &state.db,
        &ctx,
        &state.config,
        state.email_api.as_deref(),
        &key,
        &id,
        if_match,
    )
    .await?;
    Ok(AxumJson(envelope(result, &request_id)))
}
