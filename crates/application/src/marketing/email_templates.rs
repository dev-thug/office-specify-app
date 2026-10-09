//! Workspace-bound template access for the separate, prepaid Zoho Email API
//! v2 product. Writes use a durable intent journal because Zoho does not
//! document an idempotency key or compare-and-swap for templates.

use contracts::{
    AppError, Ctx, ErrorCode,
    marketing::{
        ZohoTemplateDetailsResponse, ZohoTemplateMutationResponse, ZohoTemplateSummaryResponse,
        ZohoTemplateWriteRequest,
    },
};
use infrastructure::{
    audit::{self, AuditEntry},
    config::Config,
    crypto,
    db::Db,
    providers::{
        zoho_campaigns::SecretValue,
        zoho_email_api::{
            TemplateContentType, TemplateWriteRequest, ZohoEmailApi, ZohoEmailApiError,
        },
    },
    repo::zoho_templates as repo,
};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Debug, Clone)]
enum ClaimResult {
    New(Uuid),
    Replay(ZohoTemplateMutationResponse),
}

fn provider<'p, 'c>(
    ctx: &Ctx,
    config: &'c Config,
    provider: Option<&'p ZohoEmailApi>,
) -> Result<(&'p ZohoEmailApi, &'c SecretValue), AppError> {
    match config.zoho.email_api.workspace_id {
        Some(workspace_id) if workspace_id == ctx.workspace_id => {}
        Some(_) => {
            return Err(AppError::forbidden(
                "Zoho Email API is available only to its configured workspace",
            ));
        }
        None => {
            return Err(AppError::dependency_unavailable(
                "Zoho Email API workspace binding is not configured",
            ));
        }
    }
    if !config.zoho.email_api.enabled {
        return Err(AppError::dependency_unavailable(
            "Zoho Email API is disabled by the server",
        ));
    }
    let api_key = config
        .zoho
        .email_api
        .api_key
        .as_ref()
        .filter(|key| !key.expose().trim().is_empty())
        .ok_or_else(|| {
            AppError::dependency_unavailable("Zoho Email API credentials are not configured")
        })?;
    let provider = provider
        .ok_or_else(|| AppError::dependency_unavailable("Zoho Email API client is unavailable"))?;
    Ok((provider, api_key))
}

fn content_type(raw: &str) -> Result<TemplateContentType, AppError> {
    match raw {
        "html" => Ok(TemplateContentType::Html),
        "text" => Ok(TemplateContentType::Text),
        _ => Err(AppError::validation(
            "content_type must be html or text",
            vec![contracts::Violation {
                field: "content_type".into(),
                code: "invalid_value".into(),
            }],
        )),
    }
}

fn content_type_name(value: TemplateContentType) -> &'static str {
    match value {
        TemplateContentType::Html => "html",
        TemplateContentType::Text => "text",
    }
}

fn valid_template_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 200
}

fn details_response(
    detail: infrastructure::providers::zoho_email_api::TemplateDetails,
) -> ZohoTemplateDetailsResponse {
    let content_type = content_type_name(detail.content_type);
    let mut canonical = b"zoho-template-content-v1\0".to_vec();
    append_field(&mut canonical, content_type.as_bytes());
    append_field(&mut canonical, detail.name.as_bytes());
    append_field(&mut canonical, detail.content.as_bytes());
    let content_hash = crypto::sha256_hex(&canonical);
    ZohoTemplateDetailsResponse {
        id: detail.id,
        name: detail.name,
        content: detail.content,
        content_type: content_type.into(),
        content_hash,
    }
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_be_bytes());
    output.extend_from_slice(value);
}

fn check_if_match(raw: Option<&str>) -> Result<&str, AppError> {
    let raw = raw.ok_or_else(|| {
        AppError::new(
            ErrorCode::VersionPreconditionRequired,
            "If-Match with the current template content hash is required",
        )
    })?;
    let hash = raw
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(raw);
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AppError::validation(
            "If-Match must contain a quoted template content hash",
            vec![],
        ));
    }
    Ok(hash)
}

fn request_hmac(
    hmac_key: &[u8],
    endpoint: &str,
    template_id: Option<&str>,
    if_match: Option<&str>,
    request: Option<&ZohoTemplateWriteRequest>,
) -> Result<[u8; 32], AppError> {
    #[derive(Serialize)]
    struct RequestMaterial<'a> {
        version: &'static str,
        endpoint: &'a str,
        template_id: Option<&'a str>,
        if_match: Option<&'a str>,
        name: Option<&'a str>,
        content: Option<&'a str>,
        content_type: Option<&'a str>,
    }
    let material = RequestMaterial {
        version: "zoho-template-operation-v1",
        endpoint,
        template_id,
        if_match,
        name: request.map(|body| body.name.as_str()),
        content: request.map(|body| body.content.as_str()),
        content_type: request.map(|body| body.content_type.as_str()),
    };
    let mut canonical = b"zoho-template-request-hmac-v1\0".to_vec();
    canonical.extend_from_slice(
        &serde_json::to_vec(&material)
            .map_err(|_| AppError::internal("could not fingerprint template operation"))?,
    );
    Ok(crypto::hmac_sha256(hmac_key, &canonical))
}

fn key_hmac(hmac_key: &[u8], idempotency_key: &str) -> [u8; 32] {
    let mut material = b"zoho-template-idempotency-key-v1\0".to_vec();
    material.extend_from_slice(idempotency_key.as_bytes());
    crypto::hmac_sha256(hmac_key, &material)
}

fn provider_config_hmac(hmac_key: &[u8], workspace_id: Uuid, api_key: &SecretValue) -> [u8; 32] {
    let mut material = b"zoho-email-api-config-v1\0".to_vec();
    material.extend_from_slice(workspace_id.as_bytes());
    append_field(&mut material, api_key.expose().as_bytes());
    crypto::hmac_sha256(hmac_key, &material)
}

#[allow(clippy::too_many_arguments)] // Explicit durable operation context and preconditions.
async fn claim_operation(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    api_key: &SecretValue,
    endpoint: &'static str,
    idempotency_key: &str,
    template_id: Option<&str>,
    if_match: Option<&str>,
    request: Option<&ZohoTemplateWriteRequest>,
) -> Result<ClaimResult, AppError> {
    let key_hash = key_hmac(&config.hmac_key, idempotency_key);
    let request_hash = request_hmac(&config.hmac_key, endpoint, template_id, if_match, request)?;
    let config_hash = provider_config_hmac(&config.hmac_key, ctx.workspace_id, api_key);
    let mut tx = db.begin(ctx.workspace_id).await?;
    let (inserted, row) = repo::claim(
        &mut tx,
        ctx.principal_id,
        endpoint,
        &key_hash,
        &request_hash,
        &config_hash,
    )
    .await?;
    if !inserted && row.request_hmac.as_slice() != request_hash.as_slice() {
        tx.commit().await?;
        return Err(AppError::new(
            ErrorCode::IdempotencyKeyReused,
            "Idempotency-Key was already used for a different template operation",
        ));
    }
    if !inserted && row.provider_config_hmac.as_slice() != config_hash.as_slice() {
        tx.commit().await?;
        return Err(AppError::conflict(
            "Zoho Email API server credentials changed; reconcile the previous operation before using this key",
        ));
    }
    if inserted {
        audit::record(
            &mut tx,
            audit_entry(
                ctx,
                endpoint,
                row.id,
                "ok",
                json!({"operation_id": row.id, "phase": "intent"}),
            ),
        )
        .await?;
        tx.commit().await?;
        return Ok(ClaimResult::New(row.id));
    }
    match row.state.as_str() {
        "completed" => {
            tx.commit().await?;
            Ok(ClaimResult::Replay(ZohoTemplateMutationResponse {
                operation_id: row.id,
                template_id: row.template_id,
                deleted: row.deleted,
            }))
        }
        "intent" | "unknown" => {
            tx.commit().await?;
            Err(unknown_outcome())
        }
        _ => {
            tx.commit().await?;
            Err(AppError::internal(
                "Zoho template operation journal state is invalid",
            ))
        }
    }
}

fn audit_entry(
    ctx: &Ctx,
    endpoint: &str,
    operation_id: Uuid,
    outcome: &str,
    metadata: serde_json::Value,
) -> AuditEntry {
    let operation = match endpoint {
        "create" => "template_create",
        "update" => "template_update",
        _ => "template_delete",
    };
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        format!("marketing.zoho_email_api.{operation}"),
        "zoho_email_template_operation",
        ctx.request_id.clone(),
    )
    .with_target(operation_id);
    entry.outcome = outcome.to_string();
    entry.metadata = metadata;
    entry
}

async fn release_claim(
    db: &Db,
    ctx: &Ctx,
    endpoint: &str,
    operation_id: Uuid,
    error_code: ErrorCode,
) -> Result<(), AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    repo::release(&mut tx, operation_id).await?;
    audit::record(
        &mut tx,
        audit_entry(
            ctx,
            endpoint,
            operation_id,
            &format!("failed:{}", error_code.as_str()),
            json!({"operation_id": operation_id, "phase": "not_applied", "error_code": error_code.as_str()}),
        ),
    )
    .await?;
    tx.commit().await
}

async fn mark_unknown(db: &Db, ctx: &Ctx, endpoint: &str, operation_id: Uuid) {
    let result = async {
        let mut tx = db.begin(ctx.workspace_id).await?;
        repo::mark_unknown(&mut tx, operation_id).await?;
        audit::record(
            &mut tx,
            audit_entry(
                ctx,
                endpoint,
                operation_id,
                "failed:PROVIDER_SUBMISSION_OUTCOME_UNKNOWN",
                json!({"operation_id": operation_id, "phase": "unknown"}),
            ),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if result.is_err() {
        tracing::error!(operation_id = %operation_id, "could not persist unknown Zoho template operation state");
    }
}

async fn complete(
    db: &Db,
    ctx: &Ctx,
    endpoint: &str,
    operation_id: Uuid,
    template_id: &str,
    deleted: bool,
) -> Result<ZohoTemplateMutationResponse, AppError> {
    let persisted = async {
        let mut tx = db.begin(ctx.workspace_id).await?;
        repo::complete(&mut tx, operation_id, template_id, deleted).await?;
        audit::record(
            &mut tx,
            audit_entry(
                ctx,
                endpoint,
                operation_id,
                "ok",
                json!({"operation_id": operation_id, "template_id": template_id, "deleted": deleted}),
            ),
        )
        .await?;
        tx.commit().await
    }
    .await;
    if persisted.is_err() {
        mark_unknown(db, ctx, endpoint, operation_id).await;
        return Err(unknown_outcome());
    }
    Ok(ZohoTemplateMutationResponse {
        operation_id,
        template_id: Some(template_id.to_string()),
        deleted,
    })
}

fn unknown_outcome() -> AppError {
    AppError::new(
        ErrorCode::ProviderSubmissionOutcomeUnknown,
        "Zoho may have applied this template change. Refresh the template list before proceeding; this operation will not be retried automatically",
    )
}

fn provider_unknown(error: &ZohoEmailApiError) -> bool {
    matches!(
        error,
        ZohoEmailApiError::Transport
            | ZohoEmailApiError::InvalidResponse
            | ZohoEmailApiError::ResponseTooLarge
            | ZohoEmailApiError::HttpStatus { status: 300..=399 | 500..=599 }
    ) || matches!(
        error,
        ZohoEmailApiError::Rejected {
            code: None | Some(200701..=200705)
        }
    )
}

fn provider_error(error: ZohoEmailApiError, read_only: bool) -> AppError {
    match error {
        ZohoEmailApiError::NotConfigured | ZohoEmailApiError::InvalidConfiguration => {
            AppError::dependency_unavailable("Zoho Email API client is unavailable")
        }
        ZohoEmailApiError::RateLimited {
            retry_after_seconds,
        } => {
            let mut error = AppError::new(
                ErrorCode::RateLimited,
                "Zoho Email API rate limited the request",
            );
            error.retry_after = retry_after_seconds;
            error
        }
        ZohoEmailApiError::HttpStatus { status: 404 } => {
            AppError::not_found("Zoho template was not found")
        }
        ZohoEmailApiError::HttpStatus { status: 401 | 403 } => AppError::new(
            ErrorCode::IntegrationReauthRequired,
            "Zoho Email API credentials were rejected; review the prepaid API setup",
        ),
        ZohoEmailApiError::Rejected { .. }
        | ZohoEmailApiError::HttpStatus { status: 400..=499 } => {
            AppError::validation("Zoho rejected the template request", vec![]).with_status(422)
        }
        ZohoEmailApiError::InvalidRequest
        | ZohoEmailApiError::UnsupportedFeature
        | ZohoEmailApiError::RequestTooLarge => AppError::validation(
            "template request is invalid or exceeds provider limits",
            vec![],
        ),
        ZohoEmailApiError::Transport
        | ZohoEmailApiError::InvalidResponse
        | ZohoEmailApiError::ResponseTooLarge
        | ZohoEmailApiError::HttpStatus { .. } => {
            if read_only {
                AppError::dependency_unavailable(
                    "Zoho Email API could not complete the read request",
                )
            } else {
                unknown_outcome()
            }
        }
    }
}

fn template_request(request: &ZohoTemplateWriteRequest) -> Result<TemplateWriteRequest, AppError> {
    Ok(TemplateWriteRequest {
        name: request.name.clone(),
        content: request.content.clone(),
        content_type: content_type(&request.content_type)?,
    })
}

pub async fn list_templates(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider_client: Option<&ZohoEmailApi>,
    start_index: u32,
    end_index: u32,
) -> Result<Vec<ZohoTemplateSummaryResponse>, AppError> {
    super::authorize(db, ctx, "marketing.zoho_email_api.template_list").await?;
    let (provider_client, _) = provider(ctx, config, provider_client)?;
    provider_client
        .list_templates(start_index, end_index)
        .await
        .map(|items| {
            items
                .into_iter()
                .map(|item| ZohoTemplateSummaryResponse {
                    id: item.id,
                    name: item.name,
                })
                .collect()
        })
        .map_err(|error| provider_error(error, true))
}

pub async fn get_template(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider_client: Option<&ZohoEmailApi>,
    template_id: &str,
) -> Result<ZohoTemplateDetailsResponse, AppError> {
    super::authorize(db, ctx, "marketing.zoho_email_api.template_read").await?;
    let (provider_client, _) = provider(ctx, config, provider_client)?;
    let detail = provider_client
        .get_template(template_id)
        .await
        .map_err(|error| provider_error(error, true))?;
    if detail.id != template_id {
        return Err(AppError::dependency_unavailable(
            "Zoho Email API returned an unexpected template",
        ));
    }
    Ok(details_response(detail))
}

pub async fn create_template(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider_client: Option<&ZohoEmailApi>,
    idempotency_key: &str,
    request: &ZohoTemplateWriteRequest,
) -> Result<ZohoTemplateMutationResponse, AppError> {
    super::authorize(db, ctx, "marketing.zoho_email_api.template_create").await?;
    let (provider_client, api_key) = provider(ctx, config, provider_client)?;
    let provider_request = template_request(request)?;
    let operation_id = match claim_operation(
        db,
        ctx,
        config,
        api_key,
        "create",
        idempotency_key,
        None,
        None,
        Some(request),
    )
    .await?
    {
        ClaimResult::New(id) => id,
        ClaimResult::Replay(result) => return Ok(result),
    };
    match provider_client.create_template(provider_request).await {
        Ok(template_id) if valid_template_id(&template_id) => {
            complete(db, ctx, "create", operation_id, &template_id, false).await
        }
        Ok(_) => {
            mark_unknown(db, ctx, "create", operation_id).await;
            Err(unknown_outcome())
        }
        Err(error) if provider_unknown(&error) => {
            mark_unknown(db, ctx, "create", operation_id).await;
            Err(unknown_outcome())
        }
        Err(error) => {
            let mapped = provider_error(error, false);
            release_claim(db, ctx, "create", operation_id, mapped.code).await?;
            Err(mapped)
        }
    }
}

// Each locked write needs a second, short-lived connection for its durable
// journal. Reserve pool capacity (the runtime pool has ten connections) before
// acquiring any database lock, including writers waiting for the same resource.
static TEMPLATE_WRITERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
struct TemplateWriteGuard<'db> {
    _tx: infrastructure::db::Tx<'db>,
    _permit: tokio::sync::SemaphorePermit<'static>,
}

// Transaction-scoped advisory locks serialize Office writes across API processes.
// No business rows are held here; the durable operation intent is committed on
// its own connection before the provider call. Dropping this guard releases the
// lock even when a provider request or its response fails.
async fn lock_resource<'db>(
    db: &'db Db,
    workspace_id: Uuid,
    template_id: &str,
) -> Result<TemplateWriteGuard<'db>, AppError> {
    let permit = TEMPLATE_WRITERS
        .acquire()
        .await
        .map_err(|_| AppError::dependency_unavailable("template writer unavailable"))?;
    let digest =
        crypto::sha256_hex(format!("zoho-template-lock:{workspace_id}:{template_id}").as_bytes());
    let key = u64::from_str_radix(&digest[..16], 16)
        .map_err(|_| AppError::internal("could not derive template lock"))? as i64;
    let mut tx = db.begin(workspace_id).await?;
    sqlx::query("SET LOCAL lock_timeout = '5s'")
        .execute(tx.conn())
        .await
        .map_err(|_| AppError::dependency_unavailable("template writer lock unavailable"))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(key)
        .execute(tx.conn())
        .await
        .map_err(|_| AppError::dependency_unavailable("another template write is in progress"))?;
    Ok(TemplateWriteGuard {
        _tx: tx,
        _permit: permit,
    })
}

async fn validate_current_template(
    db: &Db,
    ctx: &Ctx,
    endpoint: &str,
    operation_id: Uuid,
    provider_client: &ZohoEmailApi,
    template_id: &str,
    if_match: &str,
) -> Result<(), AppError> {
    let detail = match provider_client.get_template(template_id).await {
        Ok(detail) => detail,
        Err(error) => {
            let mapped = provider_error(error, true);
            release_claim(db, ctx, endpoint, operation_id, mapped.code).await?;
            return Err(mapped);
        }
    };
    if detail.id != template_id {
        release_claim(
            db,
            ctx,
            endpoint,
            operation_id,
            ErrorCode::DependencyUnavailable,
        )
        .await?;
        return Err(AppError::dependency_unavailable(
            "Zoho Email API returned an unexpected template",
        ));
    }
    let current = details_response(detail);
    if current.content_hash != if_match {
        release_claim(db, ctx, endpoint, operation_id, ErrorCode::VersionMismatch).await?;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "template changed since it was loaded; refresh it before editing",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)] // Matches create/delete plus optimistic concurrency precondition.
pub async fn update_template(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider_client: Option<&ZohoEmailApi>,
    idempotency_key: &str,
    template_id: &str,
    raw_if_match: Option<&str>,
    request: &ZohoTemplateWriteRequest,
) -> Result<ZohoTemplateMutationResponse, AppError> {
    super::authorize(db, ctx, "marketing.zoho_email_api.template_update").await?;
    let if_match = check_if_match(raw_if_match)?;
    let (provider_client, api_key) = provider(ctx, config, provider_client)?;
    let provider_request = template_request(request)?;
    let _resource_guard = lock_resource(db, ctx.workspace_id, template_id).await?;
    let operation_id = match claim_operation(
        db,
        ctx,
        config,
        api_key,
        "update",
        idempotency_key,
        Some(template_id),
        Some(if_match),
        Some(request),
    )
    .await?
    {
        ClaimResult::New(id) => id,
        ClaimResult::Replay(result) => return Ok(result),
    };
    // The database lock serializes Office editor writes to this id. Zoho
    // exposes no atomic compare-and-swap, so another client can still change
    // the remote resource between the read and the subsequent write.
    validate_current_template(
        db,
        ctx,
        "update",
        operation_id,
        provider_client,
        template_id,
        if_match,
    )
    .await?;
    match provider_client
        .update_template(template_id, provider_request)
        .await
    {
        Ok(()) => complete(db, ctx, "update", operation_id, template_id, false).await,
        Err(error) if provider_unknown(&error) => {
            mark_unknown(db, ctx, "update", operation_id).await;
            Err(unknown_outcome())
        }
        Err(error) => {
            let mapped = provider_error(error, false);
            release_claim(db, ctx, "update", operation_id, mapped.code).await?;
            Err(mapped)
        }
    }
}

pub async fn delete_template(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    provider_client: Option<&ZohoEmailApi>,
    idempotency_key: &str,
    template_id: &str,
    raw_if_match: Option<&str>,
) -> Result<ZohoTemplateMutationResponse, AppError> {
    super::authorize(db, ctx, "marketing.zoho_email_api.template_delete").await?;
    let if_match = check_if_match(raw_if_match)?;
    let (provider_client, api_key) = provider(ctx, config, provider_client)?;
    let _resource_guard = lock_resource(db, ctx.workspace_id, template_id).await?;
    let operation_id = match claim_operation(
        db,
        ctx,
        config,
        api_key,
        "delete",
        idempotency_key,
        Some(template_id),
        Some(if_match),
        None,
    )
    .await?
    {
        ClaimResult::New(id) => id,
        ClaimResult::Replay(result) => return Ok(result),
    };
    validate_current_template(
        db,
        ctx,
        "delete",
        operation_id,
        provider_client,
        template_id,
        if_match,
    )
    .await?;
    match provider_client.delete_template(template_id).await {
        Ok(()) => complete(db, ctx, "delete", operation_id, template_id, true).await,
        Err(error) if provider_unknown(&error) => {
            mark_unknown(db, ctx, "delete", operation_id).await;
            Err(unknown_outcome())
        }
        Err(error) => {
            let mapped = provider_error(error, false);
            release_claim(db, ctx, "delete", operation_id, mapped.code).await?;
            Err(mapped)
        }
    }
}
