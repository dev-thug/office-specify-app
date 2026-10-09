//! imports module routes (spec §6 — owned by the module worker).
//! `/imports*` upload→validate→commit pipeline, `/exports`, and the
//! `/objects/{id}/download` capability endpoint (B.3.13, §15.2).

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use contracts::imports::{
    CommitImportRequest, CreateExportRequest, CreateImportRequest, CreateUploadRequest,
    ImportResponse, ImportRowResponse, JobAcceptedResponse, StoredObjectResponse,
    UploadRequestResponse,
};
use contracts::{AppError, Envelope, Page, Problem, Violation};
use infrastructure::cursor::Cursor;
use infrastructure::storage::ObjectStore;
use serde::Deserialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IdempotencyKey, Json, PageParams};
use crate::problem::ProblemError;
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_upload_request))
        .routes(routes!(complete_upload))
        .routes(routes!(create_import))
        .routes(routes!(list_imports))
        .routes(routes!(get_import))
        .routes(routes!(list_import_rows))
        .routes(routes!(commit_import))
        .routes(routes!(create_export))
        .routes(routes!(download_object))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope::new(data, request_id)
}

/// Keep the job row, audit, outbox, and idempotency replay in one transaction.
async fn run_idem<'db, T, F, Fut>(
    db: &'db infrastructure::db::Db,
    ctx: &contracts::Ctx,
    endpoint: &str,
    key: String,
    request_body: &serde_json::Value,
    status: u16,
    f: F,
) -> Result<infrastructure::idempotency::Stored<T>, AppError>
where
    F: FnOnce(infrastructure::db::Tx<'db>) -> Fut,
    Fut: std::future::Future<Output = Result<(infrastructure::db::Tx<'db>, T), AppError>>,
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.to_string(),
        key,
    };
    let request_hash = infrastructure::crypto::sha256_bytes(request_body.to_string().as_bytes());
    let out = infrastructure::idempotency::run(db, &scope, &request_hash, move |tx| async move {
        let (tx, body) = f(tx).await?;
        Ok((tx, infrastructure::idempotency::Stored { status, body }))
    })
    .await?;
    Ok(out.into_inner())
}

#[derive(Debug, Deserialize)]
struct ImportListParams {
    status: Option<String>,
}

fn import_list_filters(status: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "endpoint": "GET /api/v1/imports",
        "status": status,
    })
}

fn decode_import_cursor(
    state: &AppState,
    ctx: &contracts::Ctx,
    params: &PageParams,
    filters: &serde_json::Value,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let filter_hash = Cursor::hash_filter(filters);
    let Some(cursor) = params.decode(&state.config.hmac_key, ctx.principal_id, &filter_hash)?
    else {
        return Ok(None);
    };
    let created_at = cursor
        .sort_values
        .first()
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());
    let id = cursor
        .sort_values
        .get(1)
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<Uuid>().ok());
    match (created_at, id) {
        (Some(ts), Some(id)) => Ok(Some((ts, id))),
        _ => Err(AppError::validation("invalid pagination cursor", vec![])),
    }
}

fn encode_import_cursor(
    state: &AppState,
    ctx: &contracts::Ctx,
    filters: &serde_json::Value,
    position: (DateTime<Utc>, Uuid),
) -> String {
    Cursor::new(
        vec![
            serde_json::json!(position.0.to_rfc3339()),
            serde_json::json!(position.1),
        ],
        Cursor::hash_filter(filters),
        ctx.principal_id,
    )
    .encode(&state.config.hmac_key)
}

#[utoipa::path(
    post, path = "/imports/upload-requests",
    request_body = CreateUploadRequest,
    responses(
        (status = 200, description = "Upload capability (PUT path + token)", body = UploadRequestResponse),
        (status = 403, description = "Missing crm.import scope", body = Problem),
        (status = 422, description = "Invalid filename/media type/size", body = Problem),
    )
)]
async fn create_upload_request(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Json(req): Json<CreateUploadRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::imports::upload::request_upload(&ctx, &state.config, &req)?;
    Ok(axum::Json(envelope(res, &ctx.request_id)))
}

#[derive(Debug, Deserialize)]
struct UploadToken {
    token: String,
}

/// `PUT /imports/uploads/{key}?token=…` — raw CSV bytes. Not in the B.2
/// endpoint table but required by the presigned-style upload flow; the
/// capability token binds workspace+principal+key+size+expiry (§15.2).
/// The body is read with an explicit limit rather than the app-wide 1MB
/// `DefaultBodyLimit` (CSV may be up to 20MB).
#[utoipa::path(
    put, path = "/imports/uploads/{key}",
    params(
        ("key" = String, Path, description = "Storage key from upload-request"),
        ("token" = String, Query, description = "Upload capability token"),
    ),
    responses(
        (status = 201, description = "Object stored", body = StoredObjectResponse),
        (status = 403, description = "Invalid/expired token", body = Problem),
        (status = 413, description = "Exceeds declared/allowed size", body = Problem),
    )
)]
async fn complete_upload(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Path(key): Path<String>,
    Query(q): Query<UploadToken>,
    request: axum::extract::Request,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let max = domain::imports::MAX_IMPORT_BYTES as usize;
    let body = axum::body::to_bytes(request.into_body(), max + 1)
        .await
        .map_err(|_| {
            ProblemError(AppError::validation(
                "upload body exceeds the 20MB limit",
                vec![Violation {
                    field: "body".into(),
                    code: "out_of_range".into(),
                }],
            ))
        })?;
    if body.len() > max {
        return Err(ProblemError(AppError::validation(
            "upload body exceeds the 20MB limit",
            vec![Violation {
                field: "body".into(),
                code: "out_of_range".into(),
            }],
        )));
    }
    let res = application::imports::upload::complete_upload(
        &state.db,
        &ctx,
        &state.config,
        &key,
        &q.token,
        &body,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(res, &ctx.request_id)),
    ))
}

#[utoipa::path(
    post, path = "/imports",
    request_body = CreateImportRequest,
    responses(
        (status = 202, description = "Validation job queued", body = JobAcceptedResponse),
        (status = 403, description = "Missing crm.import scope", body = Problem),
        (status = 422, description = "Invalid mapping/object", body = Problem),
    )
)]
async fn create_import(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    idem: IdempotencyKey,
    Json(req): Json<CreateImportRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let run_ctx = ctx.clone();
    let run_req = req.clone();
    let out = run_idem(
        &state.db,
        &ctx,
        "POST /api/v1/imports",
        idem.0,
        &serde_json::to_value(&req).unwrap_or_default(),
        202,
        move |mut tx| {
            let ctx = run_ctx;
            let req = run_req;
            async move {
                let body = application::imports::query::create_import(&mut tx, &ctx, &req).await?;
                Ok((tx, body))
            }
        },
    )
    .await?;
    Ok((
        StatusCode::from_u16(out.status).unwrap(),
        axum::Json(envelope(out.body, &ctx.request_id)),
    ))
}

#[utoipa::path(
    get, path = "/imports",
    params(
        ("status" = Option<String>, Query, description = "uploaded|validating|ready|committing|completed|failed|canceled"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Principal- and filter-bound signed cursor"),
    ),
    responses(
        (status = 200, description = "Page of import and export jobs visible to the caller", body = Vec<ImportResponse>),
        (status = 403, description = "Missing crm.import or crm.export scope", body = Problem),
        (status = 422, description = "Invalid status filter or cursor", body = Problem),
    )
)]
async fn list_imports(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Query(params): Query<ImportListParams>,
    page_params: PageParams,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let filters = import_list_filters(params.status.as_deref());
    let after = decode_import_cursor(&state, &ctx, &page_params, &filters)?;
    let limit = i64::from(page_params.limit);
    let (rows, next) = application::imports::query::list_imports(
        &state.db,
        &ctx,
        &state.config,
        params.status.as_deref(),
        after,
        limit,
    )
    .await?;
    let has_more = next.is_some();
    let page = Page {
        next_cursor: next.map(|position| encode_import_cursor(&state, &ctx, &filters, position)),
        has_more,
    };
    Ok(axum::Json(envelope(rows, &ctx.request_id).with_page(page)))
}

#[utoipa::path(
    get, path = "/imports/{id}",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Import/export job state (ETag set)", body = ImportResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_import(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::imports::query::get_import(&state.db, &ctx, &state.config, id).await?;
    let mut headers = HeaderMap::new();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("\"{}\"", res.version)) {
        headers.insert(header::ETAG, v);
    }
    Ok((headers, axum::Json(envelope(res, &ctx.request_id))))
}

#[derive(Debug, Deserialize)]
struct RowsParams {
    status: Option<String>,
    /// Keyset position: last seen `row_no` (import rows are gap-free — the
    /// opaque-cursor requirement is relaxed to a plain integer here, B.1.5).
    after_row_no: Option<i32>,
    limit: Option<u32>,
}

#[utoipa::path(
    get, path = "/imports/{id}/rows",
    params(
        ("id" = Uuid, Path),
        ("status" = Option<String>, Query, description = "valid|invalid|duplicate|imported|skipped"),
        ("after_row_no" = Option<i32>, Query, description = "Rows strictly after this row_no"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
    ),
    responses(
        (status = 200, description = "Page of import rows", body = Vec<ImportRowResponse>),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn list_import_rows(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Query(params): Query<RowsParams>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let limit = params.limit.unwrap_or(50);
    if limit == 0 || limit > 200 {
        return Err(ProblemError(AppError::validation(
            "limit must be 1..=200",
            vec![],
        )));
    }
    let (rows, has_more) = application::imports::query::list_rows(
        &state.db,
        &ctx,
        id,
        params.status.as_deref(),
        params.after_row_no.unwrap_or(0),
        i64::from(limit),
    )
    .await?;
    let next_cursor = rows
        .last()
        .map(|r| r.row_no.to_string())
        .filter(|_| has_more);
    Ok(axum::Json(envelope(rows, &ctx.request_id).with_page(
        Page {
            next_cursor,
            has_more,
        },
    )))
}

#[utoipa::path(
    post, path = "/imports/{id}/commit",
    params(("id" = Uuid, Path)),
    request_body = CommitImportRequest,
    responses(
        (status = 202, description = "Commit job queued", body = JobAcceptedResponse),
        (status = 403, description = "Missing crm.import scope", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 409, description = "SNAPSHOT_MISMATCH / wrong state", body = Problem),
        (status = 422, description = "Invalid rows present with reject policy", body = Problem),
    )
)]
async fn commit_import(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    idem: IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<CommitImportRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    // Path param joins the hash — same body on a different import is a
    // different request.
    let request_body = serde_json::json!({"id": id, "body": req});
    let run_ctx = ctx.clone();
    let run_req = req.clone();
    let out = run_idem(
        &state.db,
        &ctx,
        "POST /api/v1/imports/{id}/commit",
        idem.0,
        &request_body,
        202,
        move |mut tx| {
            let ctx = run_ctx;
            let req = run_req;
            async move {
                let body =
                    application::imports::commit::request_commit(&mut tx, &ctx, id, &req).await?;
                Ok((tx, body))
            }
        },
    )
    .await?;
    Ok((
        StatusCode::from_u16(out.status).unwrap(),
        axum::Json(envelope(out.body, &ctx.request_id)),
    ))
}

#[utoipa::path(
    post, path = "/exports",
    request_body = CreateExportRequest,
    responses(
        (status = 202, description = "Export job queued", body = JobAcceptedResponse),
        (status = 403, description = "Missing crm.export scope", body = Problem),
        (status = 422, description = "Invalid fields/filters", body = Problem),
    )
)]
async fn create_export(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    idem: IdempotencyKey,
    Json(req): Json<CreateExportRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let store = application::imports::object_store(&state.config)?;
    let run_ctx = ctx.clone();
    let run_req = req.clone();
    let out = run_idem(
        &state.db,
        &ctx,
        "POST /api/v1/exports",
        idem.0,
        &serde_json::to_value(&req).unwrap_or_default(),
        202,
        move |mut tx| {
            let ctx = run_ctx;
            let req = run_req;
            async move {
                let body = application::imports::query::request_export(&mut tx, &ctx, &store, &req)
                    .await?;
                Ok((tx, body))
            }
        },
    )
    .await?;
    Ok((
        StatusCode::from_u16(out.status).unwrap(),
        axum::Json(envelope(out.body, &ctx.request_id)),
    ))
}

#[derive(Debug, Deserialize)]
struct DownloadToken {
    token: String,
}

/// `GET /objects/{id}/download?token=…` — capability token *and* session
/// ACL are both required (§15.2 mint-time re-check; served via the API so
/// no object-store path ever leaks).
#[utoipa::path(
    get, path = "/objects/{id}/download",
    params(
        ("id" = Uuid, Path),
        ("token" = String, Query),
    ),
    responses(
        (status = 200, description = "Object bytes (content-disposition: attachment)"),
        (status = 403, description = "Invalid/expired token or no access", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn download_object(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<DownloadToken>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let (storage_key, media_type) =
        application::imports::query::resolve_download(&state.db, &ctx, &state.config, id, &q.token)
            .await?;
    let store = application::imports::object_store(&state.config)?;
    let bytes = store.get(&storage_key).await?;
    let mut headers = HeaderMap::new();
    if let Ok(v) = media_type.parse() {
        headers.insert(header::CONTENT_TYPE, v);
    }
    headers.insert(
        header::CONTENT_DISPOSITION,
        axum::http::HeaderValue::from_static("attachment; filename=\"export.csv\""),
    );
    Ok((headers, bytes))
}
