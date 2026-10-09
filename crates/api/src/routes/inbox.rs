//! inbox module routes (spec §6, source §9 + B.2 rows 3605–3612):
//! `email-threads` list/get/assignment/read-state/patch/reply-drafts,
//! `email-messages` classification reviews, `message-attachments` download
//! capability + download, and `/webhooks/v1` provider mail ingress.
//!
//! Mailbox ACL (§5.4/§5.6) is applied inside the use cases and queries —
//! no thread/message/attachment exists for a principal without `can_read`.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use contracts::inbox::{
    AssignmentRequest, ClassificationReviewRequest, ClassificationReviewResponse,
    DownloadRequestResponse, MessageDraftDto, PatchThreadRequest, ReadStateRequest,
    ReadStateResponse, ReplyDraftRequest, ThreadDetail, ThreadMutationResponse, ThreadSummary,
};
use contracts::{AppError, Ctx, Envelope, Page, Problem};
use serde::Deserialize;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IfMatch, Json, OptionalIdempotencyKey, RequestIdHeader};
use crate::problem::ProblemError;
use crate::state::AppState;

/// Internal /api/v1 routes.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_threads))
        .routes(routes!(get_thread))
        .routes(routes!(put_assignment))
        .routes(routes!(put_read_state))
        .routes(routes!(patch_thread))
        .routes(routes!(create_reply_draft))
        .routes(routes!(review_classification))
        .routes(routes!(request_attachment_download))
        .routes(routes!(download_attachment))
}

/// /webhooks/v1 provider ingress routes (§14.3).
pub fn webhook_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(mail_webhook))
}

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope::new(data, request_id)
}

fn etag(version: i64) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&format!("\"{version}\"")) {
        h.insert(header::ETAG, v);
    }
    h
}

// TODO(rebase): use api::extract::PageParams
/// List query params (all flat scalars — serde_urlencoded limitation).
/// `limit`/`cursor` match the shared `PageParams` contract: default 50,
/// clamp ≤200.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[serde(deny_unknown_fields)]
struct ThreadListParams {
    /// Items per page (default 50, max 200).
    limit: Option<u32>,
    /// Signed opaque cursor from a previous response.
    cursor: Option<String>,
    /// Only threads of this mailbox.
    mail_account_id: Option<Uuid>,
    /// `open` | `pending` | `resolved`.
    status: Option<String>,
    /// `me`, `unassigned`, or a principal UUID.
    assigned_to: Option<String>,
    /// Case-insensitive substring on the latest message subject.
    q: Option<String>,
}

impl ThreadListParams {
    fn limit(&self) -> i64 {
        i64::from(self.limit.unwrap_or(50).clamp(1, 200))
    }
}

/// Latest-message sort keys for the signed keyset cursor.
fn thread_cursor_values(
    row: &infrastructure::repo::inbox::ThreadListRow,
) -> Vec<serde_json::Value> {
    vec![serde_json::json!(row.updated_at), serde_json::json!(row.id)]
}

fn decode_thread_cursor(
    raw: &str,
    state: &AppState,
    ctx: &Ctx,
    filter_hash: &str,
) -> Result<(chrono::DateTime<chrono::Utc>, Uuid), AppError> {
    let cur = infrastructure::cursor::Cursor::decode(
        raw,
        &state.config.hmac_key,
        ctx.principal_id,
        filter_hash,
    )?;
    let [sv, id] = match cur.sort_values.as_slice() {
        [a, b] => [a.clone(), b.clone()],
        _ => return Err(AppError::validation("invalid pagination cursor", vec![])),
    };
    let ts = serde_json::from_value::<chrono::DateTime<chrono::Utc>>(sv)
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    let id = serde_json::from_value::<Uuid>(id)
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    Ok((ts, id))
}

#[utoipa::path(
    get, path = "/email-threads",
    params(ThreadListParams),
    responses(
        (status = 200, description = "Inbox page", body = Vec<ThreadSummary>),
        (status = 400, description = "Invalid cursor/filter", body = Problem),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Missing mail.read scope", body = Problem),
    )
)]
async fn list_threads(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(params): Query<ThreadListParams>,
) -> Result<axum::Json<Envelope<Vec<ThreadSummary>>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();

    if let Some(s) = &params.status
        && contracts::inbox::ThreadStatus::parse(s).is_none()
    {
        return Err(
            AppError::validation("status must be open, pending or resolved", vec![]).into(),
        );
    }
    let assigned_to = match params.assigned_to.as_deref() {
        None => None,
        Some("me") => Some(Some(ctx.principal_id)),
        Some("unassigned") => Some(None),
        Some(other) => Some(Some(other.parse::<Uuid>().map_err(|_| {
            AppError::validation(
                "assigned_to must be me, unassigned or a principal id",
                vec![],
            )
        })?)),
    };
    let filter = infrastructure::repo::inbox::ThreadFilter {
        mail_account_id: params.mail_account_id,
        status: params.status.clone(),
        assigned_to,
        q: params.q.clone(),
    };
    // The cursor is bound to the principal + the whole filter projection —
    // a cursor minted for another filter set is rejected (B.1.5).
    let filter_hash = infrastructure::cursor::Cursor::hash_filter(&serde_json::json!({
        "mail_account_id": filter.mail_account_id,
        "status": filter.status,
        "assigned_to": match filter.assigned_to {
            Some(Some(p)) => serde_json::Value::String(p.to_string()),
            Some(None) => serde_json::Value::String("unassigned".into()),
            None => serde_json::Value::Null,
        },
        "q": filter.q,
    }));
    let after = match &params.cursor {
        Some(raw) => Some(decode_thread_cursor(raw, &state, &ctx, &filter_hash)?),
        None => None,
    };

    let limit = params.limit();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let mut rows =
        application::inbox::threads::list_threads(&mut tx, &ctx, &filter, after, limit + 1).await?;
    tx.commit().await?;

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = if has_more && let Some(last) = rows.last() {
        Some(
            infrastructure::cursor::Cursor::new(
                thread_cursor_values(last),
                filter_hash,
                ctx.principal_id,
            )
            .encode(&state.config.hmac_key),
        )
    } else {
        None
    };

    let data = rows
        .into_iter()
        .map(application::inbox::threads::to_summary)
        .collect();
    Ok(axum::Json(envelope(data, &request_id).with_page(Page {
        next_cursor,
        has_more,
    })))
}

#[utoipa::path(
    get, path = "/email-threads/{id}",
    params(("id" = Uuid, Path, description = "Thread id")),
    responses(
        (status = 200, description = "Thread detail with messages", body = ThreadDetail),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 404, description = "Not found or no mailbox access", body = Problem),
    )
)]
async fn get_thread(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let detail = application::inbox::threads::get_thread(&mut tx, &ctx, id).await?;
    let headers = etag(detail.version);
    tx.commit().await?;
    Ok((headers, axum::Json(envelope(detail, &request_id))))
}

#[utoipa::path(
    put, path = "/email-threads/{id}/assignment",
    params(("id" = Uuid, Path)),
    request_body = AssignmentRequest,
    responses(
        (status = 200, description = "Assignment updated", body = ThreadMutationResponse),
        (status = 400, description = "Malformed If-Match / body", body = Problem),
        (status = 403, description = "Missing mail.assign or can_assign", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 412, description = "Stale version", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn put_assignment(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<AssignmentRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let res =
        application::inbox::threads::set_assignment(&mut tx, &ctx, id, if_match.0, &req).await?;
    let headers = etag(res.version);
    tx.commit().await?;
    Ok((headers, axum::Json(envelope(res, &request_id))))
}

#[utoipa::path(
    put, path = "/email-threads/{id}/read-state",
    params(("id" = Uuid, Path)),
    request_body = ReadStateRequest,
    responses(
        (status = 200, description = "Read mark stored (monotonic)", body = ReadStateResponse),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn put_read_state(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<ReadStateRequest>,
) -> Result<axum::Json<Envelope<ReadStateResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let res =
        application::inbox::threads::set_read_state(&mut tx, &ctx, id, req.last_read_at).await?;
    tx.commit().await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

#[utoipa::path(
    patch, path = "/email-threads/{id}",
    params(("id" = Uuid, Path)),
    request_body = PatchThreadRequest,
    responses(
        (status = 200, description = "Thread updated", body = ThreadMutationResponse),
        (status = 404, description = "Not found", body = Problem),
        (status = 412, description = "Stale version", body = Problem),
        (status = 428, description = "If-Match required", body = Problem),
    )
)]
async fn patch_thread(
    State(state): State<AppState>,
    auth: AuthCtx,
    if_match: IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<PatchThreadRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let res =
        application::inbox::threads::patch_status(&mut tx, &ctx, id, if_match.0, &req).await?;
    let headers = etag(res.version);
    tx.commit().await?;
    Ok((headers, axum::Json(envelope(res, &request_id))))
}

async fn reply_draft_inner<'db>(
    mut tx: infrastructure::db::Tx<'db>,
    ctx: &Ctx,
    thread_id: Uuid,
    req: &ReplyDraftRequest,
) -> Result<
    (
        infrastructure::db::Tx<'db>,
        infrastructure::idempotency::Stored<MessageDraftDto>,
    ),
    AppError,
> {
    let dto = application::inbox::threads::create_reply_draft(&mut tx, ctx, thread_id, req).await?;
    Ok((
        tx,
        infrastructure::idempotency::Stored {
            status: 201,
            body: dto,
        },
    ))
}

/// sha256 of the semantic request (path id + body fields) — the idempotency
/// replay hash. `ReplyDraftRequest` is Deserialize-only, so field-by-field.
fn reply_draft_hash(thread_id: Uuid, req: &ReplyDraftRequest) -> [u8; 32] {
    infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "thread_id": thread_id,
            "expected_last_inbound_id": req.expected_last_inbound_id,
            "body_text": req.body_text,
            "body_html": req.body_html,
            "visibility": req.visibility,
        }))
        .unwrap_or_default(),
    )
}

#[utoipa::path(
    post, path = "/email-threads/{id}/reply-drafts",
    params(("id" = Uuid, Path)),
    request_body = ReplyDraftRequest,
    responses(
        (status = 201, description = "Reply draft created", body = MessageDraftDto),
        (status = 403, description = "Missing compose grant", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 409, description = "REPLY_CONTEXT_CHANGED / idempotency conflict", body = Problem),
        (status = 422, description = "Validation failed", body = Problem),
    )
)]
async fn create_reply_draft(
    State(state): State<AppState>,
    auth: AuthCtx,
    idem: OptionalIdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<ReplyDraftRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();

    // Reply creation derives the new draft's subject and recipient from
    // inbound mail. Re-check read+compose authorization before consulting an
    // idempotency record so a revoked scope/grant cannot replay cached mail
    // content from an earlier successful request.
    let mut authorization_tx = state.db.begin(ctx.workspace_id).await?;
    application::inbox::threads::authorize_reply(&mut authorization_tx, &ctx, id).await?;
    authorization_tx.commit().await?;

    let stored = match idem.0 {
        Some(key) => {
            let request_hash = reply_draft_hash(id, &req);
            let scope = infrastructure::idempotency::Scope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: ctx.principal_id.to_string(),
                endpoint: "POST /api/v1/email-threads/{id}/reply-drafts".into(),
                key,
            };
            let ctx2 = ctx.clone();
            let req2 = req.clone();
            infrastructure::idempotency::run(
                &state.db,
                &scope,
                &request_hash,
                move |tx| async move { reply_draft_inner(tx, &ctx2, id, &req2).await },
            )
            .await?
            .into_inner()
        }
        None => {
            let tx = state.db.begin(ctx.workspace_id).await?;
            let (tx, out) = reply_draft_inner(tx, &ctx, id, &req).await?;
            tx.commit().await?;
            out
        }
    };
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(stored.body, &request_id)),
    ))
}

#[utoipa::path(
    post, path = "/email-messages/{id}/classification-reviews",
    params(("id" = Uuid, Path)),
    request_body = ClassificationReviewRequest,
    responses(
        (status = 200, description = "Classification reviewed", body = ClassificationReviewResponse),
        (status = 403, description = "Humans only / no mailbox access", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn review_classification(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<ClassificationReviewRequest>,
) -> Result<axum::Json<Envelope<ClassificationReviewResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let res = application::inbox::reviews::review_classification(&mut tx, &ctx, id, &req).await?;
    tx.commit().await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

#[utoipa::path(
    post, path = "/message-attachments/{id}/download-requests",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Short-lived download URL", body = DownloadRequestResponse),
        (status = 403, description = "No mailbox access", body = Problem),
        (status = 404, description = "Not found", body = Problem),
        (status = 422, description = "Attachment quarantined", body = Problem),
    )
)]
async fn request_attachment_download(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<axum::Json<Envelope<DownloadRequestResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let request_id = ctx.request_id.clone();
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let res = application::inbox::attachments::request_download(
        &mut tx,
        &ctx,
        id,
        &state.config.hmac_key,
    )
    .await?;
    tx.commit().await?;
    Ok(axum::Json(envelope(res, &request_id)))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
struct DownloadParams {
    /// Capability token from the download-requests response.
    token: Option<String>,
}

#[utoipa::path(
    get, path = "/message-attachments/{id}/download",
    params(("id" = Uuid, Path), DownloadParams),
    responses(
        (status = 200, description = "Attachment bytes", content_type = "application/octet-stream"),
        (status = 403, description = "Invalid/expired token or no access", body = Problem),
        (status = 404, description = "Not found", body = Problem),
    )
)]
async fn download_attachment(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Query(q): Query<DownloadParams>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let token = q
        .token
        .ok_or_else(|| AppError::forbidden("download token required"))?;
    let Some(enc_key) = state.config.secret_store_key else {
        return Err(AppError::dependency_unavailable("object store not configured").into());
    };
    let store = infrastructure::storage::LocalObjectStore::new(
        &state.config.objects_dir,
        enc_key,
        state.config.hmac_key.clone(),
    );
    let mut tx = state.db.begin(ctx.workspace_id).await?;
    let content = application::inbox::attachments::download(
        &mut tx,
        &ctx,
        id,
        &token,
        &state.config.hmac_key,
        &store,
    )
    .await?;
    tx.commit().await?;

    let mut headers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&content.media_type) {
        headers.insert(header::CONTENT_TYPE, v);
    }
    // Filename pinned as an attachment download — never inline (§15.1).
    let safe_name: String = content
        .filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{safe_name}\"")) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((headers, content.bytes))
}

/// `POST /webhooks/v1/mail/{connection_id}` — provider mail ingress
/// (§14.3). Unauthenticated path; the provider signature IS the credential.
/// Raw body bytes go to verification — JSON never round-trips first.
#[utoipa::path(
    post, path = "/mail/{connection_id}",
    params(("connection_id" = Uuid, Path)),
    responses(
        (status = 200, description = "Stored and enqueued (or duplicate ACK)"),
        (status = 400, description = "Malformed payload", body = Problem),
        (status = 401, description = "Signature invalid", body = Problem),
        (status = 404, description = "Unknown connection", body = Problem),
    )
)]
async fn mail_webhook(
    State(state): State<AppState>,
    request_id: RequestIdHeader,
    Path(connection_id): Path<Uuid>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<axum::Json<Envelope<serde_json::Value>>, ProblemError> {
    let signature = headers
        .get("x-webhook-signature")
        .and_then(|v| v.to_str().ok());
    let event_id = headers
        .get("x-webhook-event-id")
        .and_then(|v| v.to_str().ok());
    let Some(enc_key) = state.config.secret_store_key else {
        return Err(AppError::dependency_unavailable("secret store not configured").into());
    };
    let secrets =
        infrastructure::secrets::FileSecretStore::new(&state.config.secrets_file, enc_key);
    let ack = application::inbox::webhooks::ingress(
        &state.db,
        connection_id,
        &body,
        signature,
        event_id,
        &secrets,
        &request_id.0,
    )
    .await?;
    Ok(axum::Json(envelope(
        serde_json::json!({
            "received": true,
            "duplicate": ack.duplicate,
            "event_id": ack.event_id,
        }),
        &request_id.0,
    )))
}
