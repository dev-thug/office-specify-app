//! SMTP connection profiles. Passwords are write-only and all writes require
//! the existing cookie-session CSRF and idempotency protections.

use axum::{
    Json as AxumJson,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use contracts::{
    Envelope, Problem,
    smtp::{CreateSmtpConnectionRequest, SmtpConnectionResponse},
};
use infrastructure::{
    crypto,
    idempotency::{self, Stored},
    secrets::{FileSecretStore, SecretStore},
};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

use crate::{
    extractors::{AuthCtx, IdempotencyKey, Json},
    problem::ProblemError,
    state::AppState,
};

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_connections))
        .routes(routes!(create_connection))
        .routes(routes!(verify_connection))
        .routes(routes!(disable_connection))
}

#[utoipa::path(
    get, path = "/smtp-connections",
    responses(
        (status = 200, description = "SMTP connection metadata; passwords are never returned", body = [SmtpConnectionResponse]),
        (status = 401, description = "Unauthenticated", body = Problem),
        (status = 403, description = "Human administrator session required", body = Problem),
    )
)]
async fn list_connections(
    State(state): State<AppState>,
    auth: AuthCtx,
) -> Result<AxumJson<Envelope<Vec<SmtpConnectionResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let items = application::smtp::list_connections(&state.db, &ctx).await?;
    Ok(AxumJson(Envelope::new(items, ctx.request_id)))
}

#[utoipa::path(
    post, path = "/smtp-connections",
    request_body = CreateSmtpConnectionRequest,
    params(("Idempotency-Key" = String, Header, description = "Stable request key")),
    responses(
        (status = 201, description = "Pending Office SMTP profile and disabled marketing channel", body = SmtpConnectionResponse),
        (status = 400, description = "Missing or invalid idempotency key", body = Problem),
        (status = 403, description = "Human administrator session or mailbox send permission required", body = Problem),
        (status = 404, description = "Mail account not found", body = Problem),
        (status = 409, description = "Idempotency key reused or sender identity conflicts", body = Problem),
        (status = 422, description = "Invalid SMTP settings or sender attestation", body = Problem),
    )
)]
async fn create_connection(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<CreateSmtpConnectionRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    application::smtp::authorize(&state.db, &ctx, "smtp_connection.create").await?;
    let workspace_id = ctx.workspace_id;
    let request_id = ctx.request_id.clone();
    let id = Uuid::new_v4();
    let hash = create_request_hash(&state.config.hmac_key, &req);
    let scope = idempotency::Scope {
        workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /smtp-connections".into(),
        key,
    };
    let db = state.db.clone();
    let config = state.config.clone();
    let result = idempotency::run(&db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        let config = config.clone();
        async move {
            let body = application::smtp::create_tx(&mut tx, &ctx, &config, id, &req).await?;
            Ok((tx, Stored { status: 201, body }))
        }
    })
    .await;
    let saved = match result {
        Ok(saved) => saved.into_inner(),
        Err(error) => {
            cleanup_secret(&state.config, workspace_id, id).await;
            return Err(error.into());
        }
    };
    Ok((
        StatusCode::CREATED,
        AxumJson(Envelope::new(saved.body, request_id)),
    ))
}

#[utoipa::path(
    post, path = "/smtp-connections/{id}/verify",
    params(("id" = Uuid, Path), ("Idempotency-Key" = String, Header, description = "Stable request key")),
    responses(
        (status = 200, description = "Credential and TLS check result; this does not prove sender alias authorization", body = SmtpConnectionResponse),
        (status = 403, description = "Human administrator session or mailbox send permission required", body = Problem),
        (status = 404, description = "SMTP connection not found", body = Problem),
        (status = 409, description = "Idempotency key reused or connection disabled", body = Problem),
    )
)]
async fn verify_connection(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Path(id): Path<Uuid>,
) -> Result<AxumJson<Envelope<SmtpConnectionResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::smtp::authorize(&state.db, &ctx, "smtp_connection.verify").await?;
    let request_id = ctx.request_id.clone();
    let scope = idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /smtp-connections/{id}/verify".into(),
        key,
    };
    let hash = action_hash("smtp-verify-v1", id);
    let config = state.config.clone();
    let outcome = idempotency::run(&state.db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        let config = config.clone();
        async move {
            let body = application::smtp::verify_tx(&mut tx, &ctx, &config, id).await?;
            Ok((tx, Stored { status: 200, body }))
        }
    })
    .await?
    .into_inner();
    Ok(AxumJson(Envelope::new(outcome.body, request_id)))
}

#[utoipa::path(
    post, path = "/smtp-connections/{id}/disable",
    params(("id" = Uuid, Path), ("Idempotency-Key" = String, Header, description = "Stable request key")),
    responses(
        (status = 200, description = "Disabled SMTP profile and bound channel", body = SmtpConnectionResponse),
        (status = 403, description = "Human administrator session or mailbox send permission required", body = Problem),
        (status = 404, description = "SMTP connection not found", body = Problem),
        (status = 409, description = "Idempotency key reused", body = Problem),
    )
)]
async fn disable_connection(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Path(id): Path<Uuid>,
) -> Result<AxumJson<Envelope<SmtpConnectionResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    application::smtp::authorize(&state.db, &ctx, "smtp_connection.disable").await?;
    let request_id = ctx.request_id.clone();
    let workspace_id = ctx.workspace_id;
    let scope = idempotency::Scope {
        workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /smtp-connections/{id}/disable".into(),
        key,
    };
    let hash = action_hash("smtp-disable-v1", id);
    let outcome = idempotency::run(&state.db, &scope, &hash, move |mut tx| {
        let ctx = ctx.clone();
        async move {
            let body = application::smtp::disable_tx(&mut tx, &ctx, id).await?;
            Ok((tx, Stored { status: 200, body }))
        }
    })
    .await?
    .into_inner();
    delete_secret(&state.config, workspace_id, id).await?;
    Ok(AxumJson(Envelope::new(outcome.body, request_id)))
}

fn create_request_hash(key: &[u8], req: &CreateSmtpConnectionRequest) -> [u8; 32] {
    let mut canonical = b"office.smtp.create.v1\0".to_vec();
    push_field(&mut canonical, req.name.as_bytes());
    push_field(&mut canonical, req.mail_account_id.as_bytes());
    push_field(&mut canonical, req.host.as_bytes());
    push_field(&mut canonical, &req.port.to_be_bytes());
    push_field(&mut canonical, req.security.as_bytes());
    push_field(&mut canonical, req.username.as_bytes());
    // Only the digest is persisted by idempotency; the raw password is never
    // serialized to a record, response, audit row, or log.
    push_field(&mut canonical, req.password.as_bytes());
    push_field(&mut canonical, req.from_address.as_bytes());
    push_field(&mut canonical, req.from_name.as_bytes());
    push_field(
        &mut canonical,
        req.reply_to.as_deref().unwrap_or_default().as_bytes(),
    );
    push_field(&mut canonical, &[u8::from(req.sender_confirmed)]);
    for (name, value) in &req.custom_headers {
        push_field(&mut canonical, name.as_bytes());
        push_field(&mut canonical, value.as_bytes());
    }
    crypto::hmac_sha256(key, &canonical)
}

fn action_hash(tag: &str, id: Uuid) -> [u8; 32] {
    crypto::sha256_bytes(format!("{tag}:{id}").as_bytes())
}

fn push_field(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u64).to_be_bytes());
    out.extend_from_slice(value);
}

async fn cleanup_secret(config: &infrastructure::config::Config, workspace: Uuid, id: Uuid) {
    let Some(key) = config.secret_store_key else {
        return;
    };
    let store = FileSecretStore::new(&config.secrets_file, key);
    let _ = store
        .delete(&application::smtp::secret_ref(workspace, id))
        .await;
}

async fn delete_secret(
    config: &infrastructure::config::Config,
    workspace: Uuid,
    id: Uuid,
) -> Result<(), ProblemError> {
    let Some(key) = config.secret_store_key else {
        return Ok(());
    };
    let store = FileSecretStore::new(&config.secrets_file, key);
    store
        .delete(&application::smtp::secret_ref(workspace, id))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn request(password: &str) -> CreateSmtpConnectionRequest {
        CreateSmtpConnectionRequest {
            name: "Office SMTP".into(),
            mail_account_id: Uuid::nil(),
            host: "smtp.example.test".into(),
            port: 587,
            security: "starttls".into(),
            username: "smtp-user".into(),
            password: password.into(),
            from_address: "news@example.test".into(),
            from_name: "Office News".into(),
            reply_to: None,
            custom_headers: BTreeMap::new(),
            sender_confirmed: true,
        }
    }

    #[test]
    fn create_idempotency_fingerprint_is_keyed_and_covers_password() {
        let original = request("smtp-secret-a");
        let fingerprint = create_request_hash(b"test-hmac-key", &original);
        assert_eq!(
            fingerprint,
            create_request_hash(b"test-hmac-key", &original)
        );
        assert_ne!(
            fingerprint,
            create_request_hash(b"another-hmac-key", &original)
        );
        assert_ne!(
            fingerprint,
            create_request_hash(b"test-hmac-key", &request("smtp-secret-b"))
        );
    }
}
