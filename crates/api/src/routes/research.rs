//! research module routes (spec §6 — owned by the module worker).
//! `/lead-proposals` (+decisions), `/research-evidence` (+reviews) — B.3.12.

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, header};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use contracts::research::{
    CreateEvidenceRequest, CreateLeadProposalsRequest, CreateLeadProposalsResponse,
    EvidenceResponse, EvidenceReviewRequest, LeadProposalDecisionRequest,
    LeadProposalDecisionResponse, LeadProposalDetailResponse, LeadProposalResponse,
};
use contracts::{AppError, Envelope, Page, Problem};
use infrastructure::cursor::Cursor;
use serde::Deserialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{
    AuthCtx, IfMatch, Json, OptionalIdempotencyKey, PageParams, RequestIdHeader,
};
use crate::problem::ProblemError;
use crate::state::AppState;

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_lead_proposals))
        .routes(routes!(list_lead_proposals))
        .routes(routes!(get_lead_proposal))
        .routes(routes!(decide_lead_proposal))
        .routes(routes!(create_research_evidence))
        .routes(routes!(list_research_evidence))
        .routes(routes!(get_research_evidence))
        .routes(routes!(review_research_evidence))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope::new(data, request_id)
}

/// The shared idempotency helper owns the same transaction the use case
/// writes to; storing the replay response and business changes is atomic.
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

fn etag(version: i64) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("\"{version}\"")) {
        h.insert(header::ETAG, v);
    }
    h
}

/// Decode the keyset cursor, bound to this principal + filter set.
fn decode_cursor(
    state: &AppState,
    ctx: &contracts::Ctx,
    params: &PageParams,
    filters: &serde_json::Value,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let fh = Cursor::hash_filter(filters);
    let Some(cur) = params.decode(&state.config.hmac_key, ctx.principal_id, &fh)? else {
        return Ok(None);
    };
    let (ts, id) = (
        cur.sort_values
            .first()
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok()),
        cur.sort_values
            .get(1)
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Uuid>().ok()),
    );
    match (ts, id) {
        (Some(t), Some(i)) => Ok(Some((t, i))),
        _ => Err(AppError::validation("invalid pagination cursor", vec![])),
    }
}

fn encode_cursor(
    state: &AppState,
    ctx: &contracts::Ctx,
    filters: &serde_json::Value,
    last: (DateTime<Utc>, Uuid),
) -> String {
    Cursor::new(
        vec![
            serde_json::json!(last.0.to_rfc3339()),
            serde_json::json!(last.1),
        ],
        Cursor::hash_filter(filters),
        ctx.principal_id,
    )
    .encode(&state.config.hmac_key)
}

#[utoipa::path(
    post, path = "/lead-proposals",
    request_body = CreateLeadProposalsRequest,
    responses(
        (status = 201, description = "Proposals stored with evidence (status pending)", body = CreateLeadProposalsResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Missing research.propose scope", body = Problem),
        (status = 422, description = "Batch rejected — per-item violations", body = Problem),
    )
)]
async fn create_lead_proposals(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    request_id: RequestIdHeader,
    idem: OptionalIdempotencyKey,
    Json(req): Json<CreateLeadProposalsRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    if let Some(key) = idem.0 {
        let run_ctx = ctx.clone();
        let run_req = req.clone();
        let out = run_idem(
            &state.db,
            &ctx,
            "POST /api/v1/lead-proposals",
            key,
            &serde_json::to_value(&req).unwrap_or_default(),
            201,
            move |mut tx| {
                let ctx = run_ctx;
                let req = run_req;
                async move {
                    let body =
                        application::research::create_proposals_tx(&mut tx, &ctx, &req).await?;
                    Ok((tx, body))
                }
            },
        )
        .await?;
        return Ok((
            axum::http::StatusCode::from_u16(out.status).unwrap(),
            axum::Json(envelope(out.body, &request_id.0)),
        )
            .into_response());
    }
    let res = application::research::create_proposals(&state.db, &ctx, &req).await?;
    Ok((
        axum::http::StatusCode::CREATED,
        axum::Json(envelope(res, &request_id.0)),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct ListParams {
    status: Option<String>,
}

#[utoipa::path(
    get, path = "/lead-proposals",
    params(
        ("status" = Option<String>, Query, description = "pending|needs_review|accepted|rejected"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query, description = "Opaque page cursor"),
    ),
    responses(
        (status = 200, description = "Page of proposals", body = Vec<LeadProposalResponse>),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_lead_proposals(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Query(params): Query<ListParams>,
    page: PageParams,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let filters = serde_json::json!({"status": params.status});
    let after = decode_cursor(&state, &ctx, &page, &filters)?;
    let limit = i64::from(page.limit);
    let (rows, next) = application::research::list_proposals(
        &state.db,
        &ctx,
        params.status.as_deref(),
        after,
        limit,
    )
    .await?;
    let has_more = next.is_some();
    let page = Page {
        next_cursor: next.map(|l| encode_cursor(&state, &ctx, &filters, l)),
        has_more,
    };
    Ok(axum::Json(envelope(rows, &ctx.request_id).with_page(page)))
}

#[utoipa::path(
    get, path = "/lead-proposals/{id}",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Proposal + evidence (ETag set)", body = LeadProposalDetailResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_lead_proposal(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::research::get_proposal(&state.db, &ctx, id).await?;
    Ok((
        etag(res.proposal.version),
        axum::Json(envelope(res, &ctx.request_id)),
    ))
}

#[utoipa::path(
    post, path = "/lead-proposals/{id}/decisions",
    params(("id" = Uuid, Path)),
    request_body = LeadProposalDecisionRequest,
    responses(
        (status = 200, description = "Decision applied (new ETag)", body = LeadProposalDecisionResponse),
        (status = 403, description = "Human CRM review required", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 409, description = "Already decided", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn decide_lead_proposal(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<LeadProposalDecisionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::research::decide(&state.db, &ctx, id, if_match.0, &req).await?;
    Ok((
        etag(res.version),
        axum::Json(envelope(res, &ctx.request_id)),
    ))
}

#[utoipa::path(
    post, path = "/research-evidence",
    request_body = CreateEvidenceRequest,
    responses(
        (status = 201, description = "Evidence recorded (pending review)", body = EvidenceResponse),
        (status = 422, description = "Invalid target/URL", body = Problem),
    )
)]
async fn create_research_evidence(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    request_id: RequestIdHeader,
    idem: OptionalIdempotencyKey,
    Json(req): Json<CreateEvidenceRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    if let Some(key) = idem.0 {
        let run_ctx = ctx.clone();
        let run_req = req.clone();
        let out = run_idem(
            &state.db,
            &ctx,
            "POST /api/v1/research-evidence",
            key,
            &serde_json::to_value(&req).unwrap_or_default(),
            201,
            move |mut tx| {
                let ctx = run_ctx;
                let req = run_req;
                async move {
                    let body =
                        application::research::create_evidence_tx(&mut tx, &ctx, &req).await?;
                    Ok((tx, body))
                }
            },
        )
        .await?;
        return Ok((
            axum::http::StatusCode::from_u16(out.status).unwrap(),
            axum::Json(envelope(out.body, &request_id.0)),
        )
            .into_response());
    }
    let res = application::research::create_evidence(&state.db, &ctx, &req).await?;
    Ok((
        axum::http::StatusCode::CREATED,
        axum::Json(envelope(res, &request_id.0)),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct EvidenceListParams {
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    lead_proposal_id: Option<Uuid>,
    review_status: Option<String>,
}

#[utoipa::path(
    get, path = "/research-evidence",
    params(
        ("company_id" = Option<Uuid>, Query),
        ("contact_id" = Option<Uuid>, Query),
        ("lead_proposal_id" = Option<Uuid>, Query),
        ("review_status" = Option<String>, Query, description = "pending|verified|rejected"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, max 200)"),
        ("cursor" = Option<String>, Query),
    ),
    responses(
        (status = 200, description = "Page of evidence rows", body = Vec<EvidenceResponse>),
        (status = 401, description = "Unauthenticated", body = Problem),
    )
)]
async fn list_research_evidence(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Query(params): Query<EvidenceListParams>,
    page: PageParams,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let filters = serde_json::json!({
        "company_id": params.company_id,
        "contact_id": params.contact_id,
        "lead_proposal_id": params.lead_proposal_id,
        "review_status": params.review_status,
    });
    let after = decode_cursor(&state, &ctx, &page, &filters)?;
    let limit = i64::from(page.limit);
    let f = application::research::EvidenceFilters {
        company_id: params.company_id,
        contact_id: params.contact_id,
        lead_proposal_id: params.lead_proposal_id,
        review_status: params.review_status.clone(),
    };
    let (rows, next) =
        application::research::list_evidence(&state.db, &ctx, &f, after, limit).await?;
    let has_more = next.is_some();
    let page = Page {
        next_cursor: next.map(|l| encode_cursor(&state, &ctx, &filters, l)),
        has_more,
    };
    Ok(axum::Json(envelope(rows, &ctx.request_id).with_page(page)))
}

#[utoipa::path(
    get, path = "/research-evidence/{id}",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Evidence row", body = EvidenceResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn get_research_evidence(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::research::get_evidence(&state.db, &ctx, id).await?;
    Ok(axum::Json(envelope(res, &ctx.request_id)))
}

#[utoipa::path(
    post, path = "/research-evidence/{id}/reviews",
    params(("id" = Uuid, Path)),
    request_body = EvidenceReviewRequest,
    responses(
        (status = 200, description = "Review recorded", body = EvidenceResponse),
        (status = 403, description = "Human review required", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 409, description = "Already reviewed", body = Problem),
    )
)]
async fn review_research_evidence(
    axum::extract::State(state): axum::extract::State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<EvidenceReviewRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let res = application::research::review_evidence(&state.db, &ctx, id, &req).await?;
    Ok(axum::Json(envelope(res, &ctx.request_id)))
}
