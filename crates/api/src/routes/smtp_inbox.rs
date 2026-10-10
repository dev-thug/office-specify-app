//! Admin-only configuration and status for the opt-in SMTP-profile IMAP
//! inbox sync. It shares the profile's already encrypted credential and
//! never accepts or returns a second password.

use axum::{
    Json as AxumJson,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use contracts::{Envelope, Problem};
use infrastructure::{
    crypto,
    idempotency::{self, Stored},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    extractors::{AuthCtx, IdempotencyKey, Json},
    problem::ProblemError,
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_inbox_sync))
        .routes(routes!(put_inbox_sync))
        .routes(routes!(request_inbox_sync))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
struct InboxSyncResponse {
    smtp_connection_id: Uuid,
    mail_account_id: Uuid,
    enabled: bool,
    /// Empty until an administrator explicitly configures the IMAP host.
    imap_host: String,
    imap_port: u16,
    folder: String,
    cursor_uidvalidity: Option<u32>,
    cursor_uid: u32,
    imported_messages: u64,
    last_success_at: Option<chrono::DateTime<chrono::Utc>>,
    last_error_code: Option<String>,
    next_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    active_job_id: Option<Uuid>,
}

impl From<application::smtp_inbox::InboxSyncStatus> for InboxSyncResponse {
    fn from(status: application::smtp_inbox::InboxSyncStatus) -> Self {
        Self {
            smtp_connection_id: status.smtp_connection_id,
            mail_account_id: status.mail_account_id,
            enabled: status.enabled,
            imap_host: status.imap_host,
            imap_port: status.imap_port,
            folder: status.folder,
            cursor_uidvalidity: status.cursor_uidvalidity,
            cursor_uid: status.cursor_uid,
            imported_messages: status.imported_messages,
            last_success_at: status.last_success_at,
            last_error_code: status.last_error_code,
            next_sync_at: status.next_sync_at,
            active_job_id: status.active_job_id,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
struct SetInboxSyncRequest {
    enabled: bool,
    /// Public IMAP hostname; IMAP TLS port 993 and INBOX are server-fixed.
    imap_host: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
struct InboxSyncQueuedResponse {
    job_id: Uuid,
    state: String,
}

#[utoipa::path(
    get, path = "/smtp-connections/{id}/inbox",
    params(("id" = Uuid, Path)),
    responses(
        (status = 200, description = "Inbox sync configuration and cursor", body = InboxSyncResponse),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Human administrator session and mailbox permission required", body = Problem),
        (status = 404, description = "SMTP connection not found", body = Problem),
    )
)]
async fn get_inbox_sync(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<AxumJson<Envelope<InboxSyncResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let status = application::smtp_inbox::status(&state.db, &ctx, id).await?;
    Ok(AxumJson(Envelope::new(status.into(), ctx.request_id)))
}

#[utoipa::path(
    put, path = "/smtp-connections/{id}/inbox",
    request_body = SetInboxSyncRequest,
    params(("id" = Uuid, Path), ("Idempotency-Key" = String, Header, description = "Stable request key")),
    responses(
        (status = 200, description = "Inbox sync configuration", body = InboxSyncResponse),
        (status = 400, description = "Missing or invalid idempotency key", body = Problem),
        (status = 403, description = "Human administrator session and mailbox permission required", body = Problem),
        (status = 404, description = "SMTP connection not found", body = Problem),
        (status = 409, description = "SMTP profile or mailbox is not available", body = Problem),
        (status = 422, description = "IMAP host is invalid or resolves to a non-public address", body = Problem),
    )
)]
async fn put_inbox_sync(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<SetInboxSyncRequest>,
) -> Result<AxumJson<Envelope<InboxSyncResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::smtp::authorize(&state.db, &ctx, "smtp_inbox.configure").await?;
    let request_id = ctx.request_id.clone();
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "PUT /smtp-connections/{id}/inbox".into(),
        key,
    };
    let request_hash = crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({
            "smtp_connection_id": id,
            "enabled": req.enabled,
            "imap_host": req.imap_host,
        }))
        .unwrap_or_default(),
    );
    let config = state.config.clone();
    let outcome = idempotency::run(&state.db, &scope, &request_hash, move |mut tx| {
        let ctx = ctx.clone();
        let config = config.clone();
        async move {
            let body = application::smtp_inbox::set_settings_tx(
                &mut tx,
                &ctx,
                &config,
                id,
                req.enabled,
                &req.imap_host,
            )
            .await?;
            Ok((tx, Stored { status: 200, body }))
        }
    })
    .await?
    .into_inner();
    Ok(AxumJson(Envelope::new(outcome.body.into(), request_id)))
}

#[utoipa::path(
    post, path = "/smtp-connections/{id}/inbox/sync",
    params(("id" = Uuid, Path), ("Idempotency-Key" = String, Header, description = "Stable request key")),
    responses(
        (status = 202, description = "Bounded inbox page job queued", body = InboxSyncQueuedResponse),
        (status = 400, description = "Missing or invalid idempotency key", body = Problem),
        (status = 403, description = "Human administrator session and mailbox permission required", body = Problem),
        (status = 404, description = "SMTP connection not found", body = Problem),
        (status = 409, description = "Enable inbox sync before requesting a poll", body = Problem),
    )
)]
async fn request_inbox_sync(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    application::smtp::authorize(&state.db, &ctx, "smtp_inbox.sync_requested").await?;
    let request_id = ctx.request_id.clone();
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /smtp-connections/{id}/inbox/sync".into(),
        key,
    };
    let request_hash = crypto::sha256_bytes(
        &serde_json::to_vec(&serde_json::json!({"smtp_connection_id": id})).unwrap_or_default(),
    );
    let config = state.config.clone();
    let outcome = idempotency::run(&state.db, &scope, &request_hash, move |mut tx| {
        let ctx = ctx.clone();
        let config = config.clone();
        async move {
            let body = application::smtp_inbox::request_sync_tx(&mut tx, &ctx, &config, id).await?;
            Ok((tx, Stored { status: 202, body }))
        }
    })
    .await?
    .into_inner();
    let response: InboxSyncQueuedResponse = InboxSyncQueuedResponse {
        job_id: outcome.body.job_id,
        state: outcome.body.state,
    };
    Ok((
        StatusCode::ACCEPTED,
        AxumJson(Envelope::new(response, request_id)),
    ))
}
