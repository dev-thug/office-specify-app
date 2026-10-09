//! Operations REST routes (B.2): jobs, audit logs, dashboard summary,
//! operational alerts and T30 restore-lock release.

use axum::http::StatusCode;
use contracts::operations::{
    AlertsResponse, AuditLogFilters, DashboardFilters, DashboardSummaryResponse, JobResponse,
    JobRetryRequest, JobSummaryResponse, ReleaseSendWorkersLockRequest, SendWorkersLockResponse,
};
use contracts::{AppError, AuthMethod, Ctx, Envelope, Page, Problem};
use infrastructure::cursor::Cursor;
use serde::{Deserialize, Serialize};
use utoipa::IntoParams;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IdempotencyKey, Json};
use crate::problem::ProblemError;
use crate::state::AppState;

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
struct AuditQuery {
    limit: Option<u32>,
    cursor: Option<String>,
    action: Option<String>,
    target_type: Option<String>,
    target_id: Option<Uuid>,
    actor_principal_id: Option<Uuid>,
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
struct DashboardQuery {
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
    scope: Option<String>,
}

#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(default, deny_unknown_fields)]
struct JobFiltersQuery {
    limit: Option<u32>,
    cursor: Option<String>,
    state: Option<String>,
    kind: Option<String>,
}

fn page_limit(limit: Option<u32>) -> Result<i64, AppError> {
    match limit {
        None => Ok(50),
        Some(0) => Err(AppError::validation(
            "limit must be between 1 and 200",
            vec![contracts::Violation {
                field: "limit".into(),
                code: "out_of_range".into(),
            }],
        )),
        Some(n) => Ok(n.min(200) as i64),
    }
}

fn filter_hash(value: &serde_json::Value) -> String {
    Cursor::hash_filter(value)
}

fn idempotency_actor(ctx: &Ctx) -> String {
    match &ctx.auth {
        AuthMethod::ServiceCredential { credential_id } => credential_id.to_string(),
        AuthMethod::Session { .. } | AuthMethod::System { .. } => ctx.principal_id.to_string(),
    }
}

fn keyset_after(
    state: &AppState,
    ctx: &contracts::Ctx,
    cursor: Option<&str>,
    filter_hash: &str,
) -> Result<Option<(chrono::DateTime<chrono::Utc>, Uuid)>, AppError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let cursor = Cursor::decode(
        cursor,
        &state.config.hmac_key,
        ctx.principal_id,
        filter_hash,
    )?;
    let [timestamp, id] = <[serde_json::Value; 2]>::try_from(cursor.sort_values)
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp.as_str().unwrap_or_default())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?
        .with_timezone(&chrono::Utc);
    let id = Uuid::parse_str(id.as_str().unwrap_or_default())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    Ok(Some((timestamp, id)))
}

fn next_cursor(
    state: &AppState,
    ctx: &contracts::Ctx,
    seed: Option<(chrono::DateTime<chrono::Utc>, Uuid)>,
    filter_hash: &str,
) -> Option<String> {
    seed.map(|(timestamp, id)| {
        Cursor::new(
            vec![
                serde_json::json!(timestamp.to_rfc3339()),
                serde_json::json!(id),
            ],
            filter_hash.to_string(),
            ctx.principal_id,
        )
        .encode(&state.config.hmac_key)
    })
}

fn stored<T: Serialize>(
    stored: infrastructure::idempotency::Stored<T>,
    request_id: &str,
) -> (StatusCode, axum::Json<Envelope<T>>) {
    (
        StatusCode::from_u16(stored.status).unwrap_or(StatusCode::OK),
        axum::Json(Envelope::new(stored.body, request_id.to_string())),
    )
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_jobs))
        .routes(routes!(get_job))
        .routes(routes!(request_job_retry))
        .routes(routes!(list_audit_logs))
        .routes(routes!(dashboard_summary))
        .routes(routes!(list_alerts))
        .routes(routes!(release_send_workers_lock))
}

#[utoipa::path(
    get, path = "/jobs",
    params(
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Signed cursor, bound to filters and principal"),
        ("state" = Option<String>, Query, description = "DDL job state filter"),
        ("kind" = Option<String>, Query, description = "Queue kind filter"),
    ),
    responses(
        (status = 200, description = "Redacted, access-scoped job page", body = [JobSummaryResponse]),
        (status = 400, description = "Invalid filter or cursor", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Operations read scope required", body = Problem),
    )
)]
async fn list_jobs(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    page: crate::extract::PageParams,
    query: Result<axum::extract::Query<JobFiltersQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::Json<Envelope<Vec<JobSummaryResponse>>>, ProblemError> {
    let axum::extract::Query(query) =
        query.map_err(|_| AppError::validation("invalid job filters", vec![]))?;
    let requested_limit = query
        .limit
        .unwrap_or(crate::extract::PAGE_DEFAULT_LIMIT)
        .min(200);
    if page.limit != requested_limit || page.cursor != query.cursor {
        return Err(AppError::validation("invalid pagination parameters", vec![]).into());
    }
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter_value = serde_json::json!({
        "state": query.state,
        "kind": query.kind,
    });
    let filter_hash = filter_hash(&filter_value);
    let cursor = page.decode(&state.config.hmac_key, ctx.principal_id, &filter_hash)?;
    let after = cursor
        .map(
            |cursor| -> Result<(chrono::DateTime<chrono::Utc>, Uuid), AppError> {
                let [timestamp, id] = <[serde_json::Value; 2]>::try_from(cursor.sort_values)
                    .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
                let timestamp =
                    chrono::DateTime::parse_from_rfc3339(timestamp.as_str().unwrap_or_default())
                        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?
                        .with_timezone(&chrono::Utc);
                let id = Uuid::parse_str(id.as_str().unwrap_or_default())
                    .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
                Ok((timestamp, id))
            },
        )
        .transpose()?;
    let result = application::operations::jobs::list_jobs(
        &state.db,
        &ctx,
        query.state.as_deref(),
        query.kind.as_deref(),
        after,
        i64::from(page.limit),
    )
    .await?;
    Ok(axum::Json(
        Envelope::new(result.items, request_id).with_page(Page {
            next_cursor: next_cursor(&state, &ctx, result.cursor_seed, &filter_hash),
            has_more: result.has_more,
        }),
    ))
}

#[utoipa::path(
    get, path = "/jobs/{id}",
    params(("id" = Uuid, Path, description = "Job id")),
    responses(
        (status = 200, description = "Workspace-scoped job progress", body = JobResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Operations read scope required", body = Problem),
        (status = 404, description = "Not found or not visible", body = Problem),
    )
)]
async fn get_job(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::Json<Envelope<JobResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let data = application::operations::jobs::get_job(&state.db, &ctx, id).await?;
    Ok(axum::Json(Envelope::new(data, ctx.request_id)))
}

#[utoipa::path(
    post, path = "/jobs/{id}/retry-requests",
    params(("id" = Uuid, Path, description = "Job id")),
    request_body = JobRetryRequest,
    responses(
        (status = 200, description = "Safe retry request accepted", body = JobResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Operations role required", body = Problem),
        (status = 409, description = "Job state is not retryable or outcome is unknown", body = Problem),
        (status = 422, description = "External-effect retry requires a reason", body = Problem),
    )
)]
async fn request_job_retry(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    key: IdempotencyKey,
    Json(req): Json<JobRetryRequest>,
) -> Result<impl axum::response::IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: idempotency_actor(&ctx),
        endpoint: "POST /api/v1/jobs/{id}/retry-requests".into(),
        key: key.0,
    };
    let bytes = serde_json::to_vec(&(id, &req)).map_err(|error| {
        AppError::internal(format!("retry request serialization failed: {error}"))
    })?;
    let hash = infrastructure::crypto::sha256_bytes(&bytes);
    let outcome = infrastructure::idempotency::run(&state.db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        let req = req.clone();
        async move {
            let body =
                application::operations::jobs::request_retry_tx(&mut tx, &ctx, id, &req).await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 200, body },
            ))
        }
    })
    .await?;
    Ok(stored(outcome.into_inner(), &request_id))
}

#[utoipa::path(
    get, path = "/audit-logs",
    params(AuditQuery),
    responses(
        (status = 200, description = "Redacted audit log page", body = [contracts::operations::AuditLogEntry]),
        (status = 400, description = "Invalid filters or cursor", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Audit access denied", body = Problem),
    )
)]
async fn list_audit_logs(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    query: Result<axum::extract::Query<AuditQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::Json<Envelope<Vec<contracts::operations::AuditLogEntry>>>, ProblemError> {
    let axum::extract::Query(query) =
        query.map_err(|_| AppError::validation("invalid audit log filters", vec![]))?;
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filter = AuditLogFilters {
        action: query.action.clone(),
        target_type: query.target_type.clone(),
        target_id: query.target_id,
        actor_principal_id: query.actor_principal_id,
        from: query.from,
        to: query.to,
    };
    let filter_value = serde_json::json!({
        "action": filter.action.clone(),
        "target_type": filter.target_type.clone(),
        "target_id": filter.target_id,
        "actor_principal_id": filter.actor_principal_id,
        "from": filter.from,
        "to": filter.to,
    });
    let filter_hash = filter_hash(&filter_value);
    let after = keyset_after(&state, &ctx, query.cursor.as_deref(), &filter_hash)?;
    let page = application::operations::audit_logs::list_audit_logs(
        &state.db,
        &ctx,
        &filter,
        after,
        page_limit(query.limit)?,
    )
    .await?;
    Ok(axum::Json(Envelope::new(page.items, request_id).with_page(
        Page {
            next_cursor: next_cursor(&state, &ctx, page.cursor_seed, &filter_hash),
            has_more: page.has_more,
        },
    )))
}

#[utoipa::path(
    get, path = "/dashboard/summary",
    params(DashboardQuery),
    responses(
        (status = 200, description = "KPI summary with definitions and freshness", body = DashboardSummaryResponse),
        (status = 400, description = "Invalid period or scope", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Dashboard scope denied", body = Problem),
    )
)]
async fn dashboard_summary(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    query: Result<axum::extract::Query<DashboardQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::Json<Envelope<DashboardSummaryResponse>>, ProblemError> {
    let axum::extract::Query(query) =
        query.map_err(|_| AppError::validation("invalid dashboard filters", vec![]))?;
    let ctx = auth.into_inner();
    let data = application::operations::dashboard::summary(
        &state.db,
        &ctx,
        &DashboardFilters {
            from: query.from,
            to: query.to,
            scope: query.scope,
        },
    )
    .await?;
    Ok(axum::Json(Envelope::new(data, ctx.request_id)))
}

#[utoipa::path(
    get, path = "/operations/alerts",
    responses(
        (status = 200, description = "Current operational alerts", body = AlertsResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Admin or Manager required", body = Problem),
    )
)]
async fn list_alerts(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
) -> Result<axum::Json<Envelope<AlertsResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let data = application::operations::alerts::list(&state.db, &ctx).await?;
    Ok(axum::Json(Envelope::new(data, ctx.request_id)))
}

#[utoipa::path(
    post, path = "/operations/send-workers-lock/release",
    request_body = ReleaseSendWorkersLockRequest,
    responses(
        (status = 200, description = "Restore lock released after reconciliation", body = SendWorkersLockResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Admin required", body = Problem),
        (status = 409, description = "Workers are not restore-locked", body = Problem),
    )
)]
async fn release_send_workers_lock(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Json(req): Json<ReleaseSendWorkersLockRequest>,
) -> Result<impl axum::response::IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: idempotency_actor(&ctx),
        endpoint: "POST /api/v1/operations/send-workers-lock/release".into(),
        key: key.0,
    };
    let hash = infrastructure::crypto::sha256_bytes(
        serde_json::to_string(&req)
            .map_err(|error| {
                AppError::internal(format!("lock request serialization failed: {error}"))
            })?
            .as_bytes(),
    );
    let outcome = infrastructure::idempotency::run(&state.db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        let req = req.clone();
        async move {
            application::operations::send_workers::release_lock_tx(&mut tx, &ctx, &req).await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored {
                    status: 200,
                    body: SendWorkersLockResponse {
                        send_workers_locked: false,
                    },
                },
            ))
        }
    })
    .await?;
    Ok(stored(outcome.into_inner(), &request_id))
}
