//! `POST /imports` (→ validate job), `GET /imports/{id}`, rows,
//! `POST /exports` (→ export job), object download.

use chrono::{DateTime, Duration, Utc};
use contracts::imports::{
    CreateExportRequest, CreateImportRequest, ImportResponse, ImportRowResponse,
    JobAcceptedResponse,
};
use contracts::{AppError, Ctx, ErrorCode, Role, Scope, Violation};
use domain::imports as d;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::config::Config;
use infrastructure::db::{Db, Tx};
use infrastructure::jobs;
use infrastructure::repo::imports as repo;
use uuid::Uuid;

use crate::authz;

/// Download token TTL for export outputs (§15.2 — short-lived URL).
const DOWNLOAD_TTL: Duration = Duration::minutes(10);

/// `POST /imports` — inside the caller's idempotency tx. Validates the
/// mapping + source object, inserts the job `validating`, enqueues
/// `imports.validate`.
pub async fn create_import(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateImportRequest,
) -> Result<JobAcceptedResponse, AppError> {
    authz::require_scope(ctx, Scope::CrmImport)?;

    let object = repo::get_stored_object(tx, req.stored_object_id)
        .await?
        .ok_or_else(|| {
            AppError::validation(
                "stored_object_id does not exist",
                vec![Violation {
                    field: "stored_object_id".into(),
                    code: "not_found".into(),
                }],
            )
            .with_status(422)
        })?;
    if object.created_by != Some(ctx.principal_id) {
        return Err(AppError::forbidden(
            "the uploaded file belongs to another principal",
        ));
    }
    if !super::IMPORT_MEDIA_TYPES.contains(&object.media_type.as_str()) {
        return Err(AppError::validation(
            "stored object is not an accepted CSV media type",
            vec![Violation {
                field: "stored_object_id".into(),
                code: "unsupported_media_type".into(),
            }],
        )
        .with_status(422));
    }

    let violations = d::validate_mapping(&req.mapping.columns);
    if !violations.is_empty() {
        return Err(
            AppError::new(ErrorCode::ValidationFailed, "invalid mapping")
                .with_status(422)
                .with_violations(violations),
        );
    }

    let mapping = serde_json::json!({
        "job_kind": "import",
        "columns": req.mapping.columns,
        "has_header": req.mapping.has_header,
        "duplicate_strategy": req.mapping.duplicate_strategy.as_str(),
    });
    let job_id =
        repo::insert_import_job(tx, object.id, ctx.principal_id, &mapping, "validating").await?;
    jobs::enqueue(
        tx,
        "imports.validate",
        1,
        serde_json::json!({"import_job_id": job_id}),
        Utc::now(),
        Some(&format!("imports.validate:{job_id}")),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "import.created",
            "import_job",
            ctx.request_id.clone(),
        )
        .with_target(job_id),
    )
    .await?;
    Ok(JobAcceptedResponse {
        import_id: job_id,
        status: contracts::imports::ImportStatus::Validating,
    })
}

/// `POST /exports` — snapshot spec goes to the object store (so the job is
/// reproducible/auditable), the job row references it and the `imports
/// .export` job produces the CSV (integration-contracts).
pub async fn request_export(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    store: &impl infrastructure::storage::ObjectStore,
    req: &CreateExportRequest,
) -> Result<JobAcceptedResponse, AppError> {
    authz::require_scope(ctx, Scope::CrmExport)?;

    let mut violations = Vec::new();
    if req.fields.is_empty() {
        violations.push(Violation {
            field: "fields".into(),
            code: "required".into(),
        });
    }
    for f in &req.fields {
        if !d::EXPORTABLE_FIELDS.contains(&f.as_str()) {
            violations.push(Violation {
                field: "fields".into(),
                code: format!("unknown_field:{f}"),
            });
        }
    }
    if let Some(f) = &req.filters
        && let Some(stage) = &f.lifecycle_stage
        && !["prospect", "engaged", "qualified", "customer", "inactive"].contains(&stage.as_str())
    {
        violations.push(Violation {
            field: "filters.lifecycle_stage".into(),
            code: "invalid".into(),
        });
    }
    if !violations.is_empty() {
        return Err(AppError::new(ErrorCode::ValidationFailed, "invalid export")
            .with_status(422)
            .with_violations(violations));
    }

    // The manifest *is* the fixed snapshot of the request (B.1.5 — exports
    // use a dedicated job rather than a live query).
    let manifest = serde_json::json!({
        "job_kind": "export",
        "v": 1,
        "fields": req.fields,
        "filters": req.filters,
    });
    let manifest_bytes = serde_json::to_vec(&manifest)
        .map_err(|e| AppError::internal(format!("manifest serialize: {e}")))?;
    let storage_key = Uuid::new_v4().simple().to_string();
    store.put(&storage_key, &manifest_bytes).await?;
    let manifest_hash = infrastructure::crypto::sha256_bytes(&manifest_bytes);
    let object_id = repo::insert_stored_object(
        tx,
        &storage_key,
        "application/json",
        manifest_bytes.len() as i64,
        &manifest_hash,
        // Server-generated content — no scan needed.
        "clean",
        Some(ctx.principal_id),
    )
    .await?;

    let job_id =
        repo::insert_import_job(tx, object_id, ctx.principal_id, &manifest, "uploaded").await?;
    jobs::enqueue(
        tx,
        "imports.export",
        1,
        serde_json::json!({"export_job_id": job_id}),
        Utc::now(),
        Some(&format!("imports.export:{job_id}")),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "export.requested",
            "import_job",
            ctx.request_id.clone(),
        )
        .with_target(job_id),
    )
    .await?;
    Ok(JobAcceptedResponse {
        import_id: job_id,
        status: contracts::imports::ImportStatus::Uploaded,
    })
}

/// Read authz for an import/export job: the requester or Manager/Admin.
fn can_read_job(ctx: &Ctx, job: &repo::ImportJobRow) -> Result<(), AppError> {
    if job.requested_by == ctx.principal_id || can_read_all_jobs(ctx) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "only the requester or a manager may read this import",
        ))
    }
}

fn can_read_all_jobs(ctx: &Ctx) -> bool {
    (matches!(ctx.role, Role::Manager | Role::Admin) || crate::authz::agent_admin(ctx))
}

/// `GET /imports` — newest-first signed-cursor page. Requesters see only
/// their own import/export jobs; human Managers/Admins can see the workspace.
pub async fn list_imports(
    db: &Db,
    ctx: &Ctx,
    cfg: &Config,
    status: Option<&str>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<(Vec<ImportResponse>, Option<(DateTime<Utc>, Uuid)>), AppError> {
    if !ctx.scopes.contains(Scope::CrmImport) && !ctx.scopes.contains(Scope::CrmExport) {
        return Err(AppError::new(
            ErrorCode::ScopeRequired,
            "missing scope crm.import or crm.export",
        ));
    }
    if let Some(value) = status
        && ![
            "uploaded",
            "validating",
            "ready",
            "committing",
            "completed",
            "failed",
            "canceled",
        ]
        .contains(&value)
    {
        return Err(AppError::validation(
            "unknown import status filter",
            vec![Violation {
                field: "status".into(),
                code: "invalid".into(),
            }],
        )
        .with_status(422));
    }
    let requested_by = (!can_read_all_jobs(ctx)).then_some(ctx.principal_id);
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_import_jobs(&mut tx, requested_by, status, after, limit + 1).await?;
    for row in &rows {
        can_read_job(ctx, row)?;
    }
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    let next = if has_more {
        rows.last().map(|row| (row.created_at, row.id))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|job| {
            let url = export_download_url(cfg, &job);
            job_response(job, url)
        })
        .collect();
    tx.commit().await?;
    Ok((items, next))
}

pub async fn get_import(
    db: &Db,
    ctx: &Ctx,
    cfg: &Config,
    id: Uuid,
) -> Result<ImportResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let job = repo::get_import_job(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("import not found"))?;
    can_read_job(ctx, &job)?;
    // A completed export exposes a short-lived download URL (the token is
    // minted here — minting re-checks ACL per §15.2).
    let download_url = export_download_url(cfg, &job);
    tx.commit().await?;
    Ok(job_response(job, download_url))
}

fn export_download_url(cfg: &Config, job: &repo::ImportJobRow) -> Option<String> {
    if job.kind() != "export" || job.status != "completed" {
        return None;
    }
    job.summary
        .get("output_object_id")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse::<Uuid>().ok())
        .map(|oid| {
            let token = repo::sign_download_token(&cfg.hmac_key, oid, Utc::now() + DOWNLOAD_TTL);
            format!("/api/v1/objects/{oid}/download?token={token}")
        })
}

fn job_response(j: repo::ImportJobRow, download_url: Option<String>) -> ImportResponse {
    ImportResponse {
        id: j.id,
        kind: j.kind().to_string(),
        stored_object_id: j.stored_object_id,
        requested_by: j.requested_by,
        status: contracts::imports::ImportStatus::parse(&j.status),
        validation_hash: j.validation_hash.map(|h| super::hex_lower(&h)),
        summary: j.summary,
        download_url,
        version: j.version,
        created_at: j.created_at,
    }
}

/// `GET /imports/{id}/rows` — keyset over `row_no`.
pub async fn list_rows(
    db: &Db,
    ctx: &Ctx,
    import_id: Uuid,
    status: Option<&str>,
    after_row_no: i32,
    limit: i64,
) -> Result<(Vec<ImportRowResponse>, bool), AppError> {
    if let Some(s) = status
        && !["valid", "invalid", "duplicate", "imported", "skipped"].contains(&s)
    {
        return Err(AppError::validation(
            "unknown row status filter",
            vec![Violation {
                field: "status".into(),
                code: "invalid".into(),
            }],
        )
        .with_status(422));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let job = repo::get_import_job(&mut tx, import_id)
        .await?
        .ok_or_else(|| AppError::not_found("import not found"))?;
    can_read_job(ctx, &job)?;
    let rows = repo::list_import_rows(&mut tx, import_id, status, after_row_no, limit + 1).await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<_> = rows.into_iter().take(limit as usize).collect();
    tx.commit().await?;
    Ok((
        rows.into_iter()
            .map(|r| ImportRowResponse {
                row_no: r.row_no,
                status: contracts::imports::ImportRowStatus::parse(&r.status),
                normalized_data: r.normalized_data,
                errors: r.errors,
                company_id: r.company_id,
                contact_id: r.contact_id,
            })
            .collect(),
        has_more,
    ))
}

/// Resolve an object download: the object must be referenced by an import
/// job in this workspace (source or output), the caller must pass the job's
/// read authz, plus the matching scope (`crm.import` for sources,
/// `crm.export` for outputs). Returns the storage key + media type for the
/// route to stream via `ObjectStore`.
pub async fn resolve_download(
    db: &Db,
    ctx: &Ctx,
    cfg: &Config,
    object_id: Uuid,
    token: &str,
) -> Result<(String, String), AppError> {
    if !repo::verify_download_token(&cfg.hmac_key, object_id, token) {
        return Err(AppError::forbidden("invalid or expired download token"));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let object = repo::get_stored_object(&mut tx, object_id)
        .await?
        .ok_or_else(|| AppError::not_found("object not found"))?;
    let oref = repo::find_object_ref(&mut tx, object_id)
        .await?
        .ok_or_else(|| AppError::not_found("object not found"))?;
    let job = repo::get_import_job(&mut tx, oref.import_job_id)
        .await?
        .ok_or_else(|| AppError::not_found("object not found"))?;
    can_read_job(ctx, &job)?;
    let needed = if oref.is_output {
        Scope::CrmExport
    } else {
        Scope::CrmImport
    };
    authz::require_scope(ctx, needed)?;
    tx.commit().await?;
    Ok((object.storage_key, object.media_type))
}
