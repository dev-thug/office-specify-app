//! templates module routes (spec §6, B.2/B.3.5 — owned by the module
//! worker).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use contracts::templates::*;
use contracts::{AppError, Envelope, Meta, Page, Problem};
use serde::Deserialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IfMatch, Json};
use crate::problem::ProblemError;
use crate::state::AppState;

/// Internal /api/v1 routes for this module.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_templates))
        .routes(routes!(create_template))
        .routes(routes!(get_template))
        .routes(routes!(patch_template))
        .routes(routes!(list_template_versions))
        .routes(routes!(create_template_version))
        .routes(routes!(render_template_version))
}

// TODO(rebase): replace with api::extract::PageParams once the shared
// extractor lands (wave-b-common).
#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct PageParams {
    /// Max items (default 50, clamped to 200).
    limit: Option<u32>,
    /// Opaque cursor from the previous page.
    cursor: Option<String>,
}

impl PageParams {
    fn limit(&self) -> i64 {
        i64::from(self.limit.unwrap_or(50).clamp(1, 200))
    }
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct TemplateListParams {
    limit: Option<u32>,
    cursor: Option<String>,
    include_archived: Option<bool>,
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
    get, path = "/email-templates",
    params(TemplateListParams),
    responses(
        (status = 200, description = "Templates page", body = Vec<EmailTemplateListItem>),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_templates(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<TemplateListParams>,
) -> Result<axum::Json<Envelope<Vec<EmailTemplateListItem>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter = serde_json::json!({"include_archived": q.include_archived.unwrap_or(false)});
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
    let limit = PageParams {
        limit: q.limit,
        cursor: None,
    }
    .limit();
    let mut items = application::templates::list(
        &state.db,
        &ctx,
        filter["include_archived"].as_bool().unwrap(),
        limit,
        after,
    )
    .await?;
    let page = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        let last = items.last().unwrap();
        let cur = infrastructure::cursor::Cursor::new(
            vec![
                serde_json::json!(last.created_at.to_rfc3339()),
                serde_json::json!(last.template_id),
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
    post, path = "/email-templates",
    request_body = CreateEmailTemplateRequest,
    responses(
        (status = 201, description = "Template created", body = EmailTemplateResponse),
        (status = 400, description = "Validation failed", body = Problem),
        (status = 403, description = "templates.write scope required", body = Problem),
    )
)]
async fn create_template(
    State(state): State<AppState>,
    auth: AuthCtx,
    Json(req): Json<CreateEmailTemplateRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::templates::create(&state.db, &ctx, &req).await?;
    let version = res.version;
    Ok((
        StatusCode::CREATED,
        etag(version),
        axum::Json(envelope(res, &request_id)),
    ))
}

#[utoipa::path(
    get, path = "/email-templates/{id}",
    params(("id" = Uuid, Path, description = "Template id")),
    responses(
        (status = 200, description = "Template", body = EmailTemplateResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_template(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::templates::get(&state.db, &ctx, id).await?;
    let version = res.version;
    Ok((etag(version), axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    patch, path = "/email-templates/{id}",
    params(("id" = Uuid, Path, description = "Template id")),
    request_body = PatchEmailTemplateRequest,
    responses(
        (status = 200, description = "Template updated", body = EmailTemplateResponse),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_template(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchEmailTemplateRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::templates::patch(&state.db, &ctx, id, if_match.0, &req).await?;
    let version = res.version;
    Ok((etag(version), axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    get, path = "/email-templates/{id}/versions",
    params(("id" = Uuid, Path), PageParams),
    responses(
        (status = 200, description = "Versions page", body = Vec<TemplateVersionListItem>),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn list_template_versions(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<PageParams>,
) -> Result<axum::Json<Envelope<Vec<TemplateVersionListItem>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter = serde_json::json!({"template_id": id});
    let filter_hash = infrastructure::cursor::Cursor::hash_filter(&filter);
    let after_no = match &q.cursor {
        Some(c) => {
            let cur = infrastructure::cursor::Cursor::decode(
                c,
                &state.config.hmac_key,
                ctx.principal_id,
                &filter_hash,
            )?;
            cur.sort_values
                .first()
                .and_then(|v| v.as_i64())
                .map(|n| n as i32)
        }
        None => None,
    };
    let limit = q.limit();
    let mut items =
        application::templates::list_versions(&state.db, &ctx, id, limit, after_no).await?;
    let page = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        let last = items.last().unwrap();
        let cur = infrastructure::cursor::Cursor::new(
            vec![serde_json::json!(last.version_no)],
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
    post, path = "/email-templates/{id}/versions",
    params(("id" = Uuid, Path)),
    request_body = CreateTemplateVersionRequest,
    responses(
        (status = 201, description = "Version created (immutable)", body = CreateTemplateVersionResponse),
        (status = 400, description = "Validation/schema error", body = Problem),
        (status = 404, description = "Template not found", body = Problem),
        (status = 409, description = "Template archived", body = Problem),
    )
)]
async fn create_template_version(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateTemplateVersionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::templates::create_version(&state.db, &ctx, id, &req).await?;
    Ok((StatusCode::CREATED, axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    post, path = "/template-versions/{id}/render",
    params(("id" = Uuid, Path)),
    request_body = RenderTemplateVersionRequest,
    responses(
        (status = 200, description = "Rendered preview", body = RenderTemplateVersionResponse),
        (status = 403, description = "Compose grant required", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 422, description = "Missing/invalid variables", body = Problem),
    )
)]
async fn render_template_version(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<RenderTemplateVersionRequest>,
) -> Result<axum::Json<Envelope<RenderTemplateVersionResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let res = application::templates::render_version(&state.db, &ctx, id, &req).await?;
    Ok(axum::Json(envelope(res, &request_id)))
}
