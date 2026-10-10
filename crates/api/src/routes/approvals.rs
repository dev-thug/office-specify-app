//! approvals module routes (B.2/B.3.9). Decisions require If-Match +
//! Idempotency-Key; revocation requires If-Match. Denied decision attempts
//! write `denied:<code>` audit rows in a separate transaction (the failed
//! business tx rolls back).

use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use contracts::approvals::{
    ApprovalRequestDetail, ApprovalRequestSummary, CreateDecisionRequest, DecisionResponse,
    RevokeApprovalRequest, RevokeResponse,
};
use contracts::{AppError, Envelope, ErrorCode, Problem};
use infrastructure::audit::{self, AuditEntry};
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
        .routes(routes!(list_approvals))
        .routes(routes!(get_approval))
        .routes(routes!(decide_approval))
        .routes(routes!(revoke_approval))
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
    cursor: Option<String>,
    limit: Option<i64>,
    /// pending | approved | rejected | expired | revoked
    status: Option<String>,
    /// campaign_launch | message_submit | booking_change | suppression_release
    action_type: Option<String>,
}

#[utoipa::path(
    get, path = "/approvals",
    params(ListParams),
    responses(
        (status = 200, description = "Approval request page (approver or own requests)", body = Vec<ApprovalRequestSummary>),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Missing scope", body = Problem),
    )
)]
async fn list_approvals(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<ListParams>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let (items, page) = application::approvals::list(
        &state.db,
        &ctx,
        q.status.as_deref(),
        q.action_type.as_deref(),
        q.cursor.as_deref(),
        q.limit.unwrap_or(50).clamp(1, 200),
        &state.config.hmac_key,
    )
    .await?;
    Ok(axum::Json(
        Envelope::new(items, ctx.request_id.clone()).with_page(page),
    ))
}

#[utoipa::path(
    get, path = "/approvals/{id}",
    params(("id" = Uuid, Path, description = "Approval request id")),
    responses(
        (status = 200, description = "Approval detail incl. frozen snapshot + target state", body = ApprovalRequestDetail),
        (status = 404, description = "Not found or not visible", body = Problem),
    )
)]
async fn get_approval(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let detail = application::approvals::get(&state.db, &ctx, id).await?;
    let version = detail.request.version;
    Ok((
        etag(version),
        axum::Json(Envelope::new(detail, ctx.request_id.clone())),
    ))
}

/// Write the denied-attempt audit in its own tx (the business tx already
/// rolled back — a denied audit must survive independently, §5.5).
async fn audit_denied_decision(
    state: &AppState,
    ctx: &contracts::Ctx,
    approval_request_id: Uuid,
    err: &AppError,
) {
    // Only authz-class denials are audited (A.6.3 decision trail).
    if !matches!(
        err.code,
        ErrorCode::Forbidden
            | ErrorCode::ScopeRequired
            | ErrorCode::SelfApprovalForbidden
            | ErrorCode::MailboxAccessDenied
            | ErrorCode::PrincipalDisabled
            | ErrorCode::AgentDirectSendForbidden
    ) {
        return;
    }
    if let Ok(mut tx) = state.db.begin(ctx.workspace_id).await {
        let _ = audit::record(
            &mut tx,
            AuditEntry::new(
                ctx.principal_id,
                "approval.decide",
                "approval_request",
                ctx.request_id.clone(),
            )
            .with_target(approval_request_id)
            .denied(&err.code),
        )
        .await;
        let _ = tx.commit().await;
    }
}

#[utoipa::path(
    post, path = "/approvals/{id}/decisions",
    params(("id" = Uuid, Path, description = "Approval request id")),
    request_body = CreateDecisionRequest,
    responses(
        (status = 200, description = "Decision recorded; approval may require a separate explicit execution action", body = DecisionResponse),
        (status = 403, description = "Agent / non-approver / self-approval", body = Problem),
        (status = 404, description = "Request not found", body = Problem),
        (status = 409, description = "Expired / snapshot mismatch / already decided / key reused", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn decide_approval(
    State(state): State<AppState>,
    auth: AuthCtx,
    key: IdempotencyKey,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<CreateDecisionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let hash = crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "approval_request_id": id,
            "if_match": if_match.0,
            "body": &req,
        }))
        .unwrap_or_default(),
    );
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /approvals/{id}/decisions".to_string(),
        key: key.0,
    };
    let result = idempotency::run(&state.db, &scope, &hash, |mut tx| {
        let ctx = ctx.clone();
        async move {
            let body = application::approvals::decide(&mut tx, &ctx, id, if_match.0, &req).await?;
            Ok((tx, Stored { status: 200, body }))
        }
    })
    .await;
    match result {
        Ok(out) => {
            let body = out.into_inner().body;
            Ok((
                etag(body.version),
                axum::Json(Envelope::new(body, request_id)),
            ))
        }
        Err(e) => {
            audit_denied_decision(&state, &ctx, id, &e).await;
            Err(ProblemError(e))
        }
    }
}

#[utoipa::path(
    post, path = "/approvals/{id}/revoke",
    params(("id" = Uuid, Path, description = "Approval request id")),
    request_body = RevokeApprovalRequest,
    responses(
        (status = 200, description = "Request revoked", body = RevokeResponse),
        (status = 403, description = "Not requester / agent", body = Problem),
        (status = 404, description = "Request not found", body = Problem),
        (status = 409, description = "Already settled / side effect executed", body = Problem),
        (status = 412, description = "Version mismatch", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn revoke_approval(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<RevokeApprovalRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let body = application::approvals::revoke(&mut tx, &ctx, id, if_match.0, req.reason.as_deref())
        .await?;
    tx.commit().await?;
    let version = body.version;
    Ok((etag(version), axum::Json(Envelope::new(body, request_id))))
}
