//! REST API for agent runs (B.2). MCP and REST invoke the same application
//! use cases and run visibility checks.

use axum::http::StatusCode;
use contracts::agents::{
    AgentRunCancelResponse, AgentRunDetailResponse, AgentRunFilters, AgentRunResponse,
    CancelAgentRunRequest, CreateAgentRunRequest, FinishAgentRunRequest,
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
struct RunQuery {
    limit: Option<u32>,
    cursor: Option<String>,
    status: Option<String>,
    agent_principal_id: Option<Uuid>,
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

fn idempotency_actor(ctx: &Ctx) -> String {
    match &ctx.auth {
        AuthMethod::ServiceCredential { credential_id } => credential_id.to_string(),
        AuthMethod::Session { .. } | AuthMethod::System { .. } => ctx.principal_id.to_string(),
    }
}

fn cursor_filter(filters: &AgentRunFilters) -> String {
    Cursor::hash_filter(&serde_json::json!({
        "status": filters.status,
        "agent_principal_id": filters.agent_principal_id,
    }))
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
        .routes(routes!(list_agent_runs))
        .routes(routes!(create_agent_run))
        .routes(routes!(get_agent_run))
        .routes(routes!(cancel_agent_run))
        .routes(routes!(finish_agent_run))
}

#[utoipa::path(
    get, path = "/agent-runs",
    params(RunQuery),
    responses(
        (status = 200, description = "Visible agent-run page", body = [AgentRunResponse]),
        (status = 400, description = "Invalid filter or cursor", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Run access denied", body = Problem),
    )
)]
async fn list_agent_runs(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    query: Result<axum::extract::Query<RunQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<axum::Json<Envelope<Vec<AgentRunResponse>>>, ProblemError> {
    let axum::extract::Query(query) =
        query.map_err(|_| AppError::validation("invalid agent-run filters", vec![]))?;
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let filters = AgentRunFilters {
        status: query.status,
        agent_principal_id: query.agent_principal_id,
    };
    let filter_hash = cursor_filter(&filters);
    let after = keyset_after(&state, &ctx, query.cursor.as_deref(), &filter_hash)?;
    let page =
        application::agents::list_runs(&state.db, &ctx, &filters, after, page_limit(query.limit)?)
            .await?;
    Ok(axum::Json(Envelope::new(page.items, request_id).with_page(
        Page {
            next_cursor: next_cursor(&state, &ctx, page.cursor_seed, &filter_hash),
            has_more: page.has_more,
        },
    )))
}

#[utoipa::path(
    post, path = "/agent-runs",
    request_body = CreateAgentRunRequest,
    responses(
        (status = 201, description = "Queued agent run", body = AgentRunResponse),
        (status = 400, description = "Invalid objective or budget", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Admin or Manager required", body = Problem),
        (status = 404, description = "Agent not found", body = Problem),
    )
)]
async fn create_agent_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    Json(req): Json<CreateAgentRunRequest>,
) -> Result<impl axum::response::IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: idempotency_actor(&ctx),
        endpoint: "POST /api/v1/agent-runs".into(),
        key: key.0,
    };
    let hash =
        infrastructure::crypto::sha256_bytes(&serde_json::to_vec(&req).map_err(|error| {
            AppError::internal(format!("run request serialization failed: {error}"))
        })?);
    let outcome = infrastructure::idempotency::run(&state.db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        let req = req.clone();
        async move {
            let body = application::agents::create_run_tx(&mut tx, &ctx, &req).await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 201, body },
            ))
        }
    })
    .await?;
    Ok(stored(outcome.into_inner(), &request_id))
}

#[utoipa::path(
    get, path = "/agent-runs/{id}",
    params(("id" = Uuid, Path, description = "Agent run id")),
    responses(
        (status = 200, description = "Visible run and redacted tool actions", body = AgentRunDetailResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 404, description = "Not found or not visible", body = Problem),
    )
)]
async fn get_agent_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<axum::Json<Envelope<AgentRunDetailResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let data = application::agents::get_run_detail(&state.db, &ctx, id).await?;
    Ok(axum::Json(Envelope::new(data, ctx.request_id)))
}

#[utoipa::path(
    post, path = "/agent-runs/{id}/cancel",
    params(("id" = Uuid, Path, description = "Agent run id")),
    request_body = CancelAgentRunRequest,
    responses(
        (status = 200, description = "Run canceled; already-started external actions are counted separately", body = AgentRunCancelResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 404, description = "Not found or not visible", body = Problem),
        (status = 409, description = "Run is already complete", body = Problem),
    )
)]
async fn cancel_agent_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    key: IdempotencyKey,
    Json(req): Json<CancelAgentRunRequest>,
) -> Result<impl axum::response::IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: idempotency_actor(&ctx),
        endpoint: "POST /api/v1/agent-runs/{id}/cancel".into(),
        key: key.0,
    };
    let hash = infrastructure::crypto::sha256_bytes(&serde_json::to_vec(&(id, &req)).map_err(
        |error| AppError::internal(format!("cancel request serialization failed: {error}")),
    )?);
    let outcome = infrastructure::idempotency::run(&state.db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        let req = req.clone();
        async move {
            let body = application::agents::cancel_run_tx(&mut tx, &ctx, id, &req).await?;
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
    post, path = "/agent-runs/{id}/finish",
    params(("id" = Uuid, Path, description = "Agent run id")),
    request_body = FinishAgentRunRequest,
    responses(
        (status = 200, description = "Run completed or failed", body = AgentRunResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Agent-run scope required", body = Problem),
        (status = 404, description = "Not found or caller is not the owning agent", body = Problem),
        (status = 409, description = "Run canceled, awaiting approval, or has unfinished actions", body = Problem),
    )
)]
async fn finish_agent_run(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    key: IdempotencyKey,
    Json(req): Json<FinishAgentRunRequest>,
) -> Result<impl axum::response::IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: idempotency_actor(&ctx),
        endpoint: "POST /api/v1/agent-runs/{id}/finish".into(),
        key: key.0,
    };
    let hash = infrastructure::crypto::sha256_bytes(&serde_json::to_vec(&(id, &req)).map_err(
        |error| AppError::internal(format!("finish request serialization failed: {error}")),
    )?);
    let outcome = infrastructure::idempotency::run(&state.db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        let req = req.clone();
        async move {
            let body = application::agents::finish_run_tx(&mut tx, &ctx, id, &req).await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 200, body },
            ))
        }
    })
    .await?;
    Ok(stored(outcome.into_inner(), &request_id))
}
