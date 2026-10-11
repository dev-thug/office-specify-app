//! Company use cases (B.2 `/companies`, B.3.3, A.6.1).
//!
//! - `POST /companies` is an idempotent create (Idempotency-Key) returning
//!   201 + `duplicate_candidates` (advisory — same domain or name, never
//!   auto-merged, B.3.3).
//! - PATCH/archive are mutations on a versioned resource → If-Match
//!   (428 missing / 412 stale, B.1.4). Owner changes are a separate
//!   privileged command and are rejected here (A.6.1).
//! - `POST /companies/{id}/merge-requests` enqueues `crm.merge_companies`
//!   (Manager/Admin only, §5.3) and returns 202 + the job id. A repeated
//!   request for the same pair returns the existing job.

use chrono::Utc;
use contracts::crm::{
    ArchiveCompanyRequest, CompanyResponse, CreateCompanyRequest, LifecycleStage,
    MergeCompanyRequest, MergeCompanyResponse, PatchCompanyRequest,
};
use contracts::{AppError, Ctx, ErrorCode, Page, Role};
use domain::crm::{self as dcrm, activity_kind};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::idempotency::Stored;
use infrastructure::jobs;
use infrastructure::repo::crm::{self, CompanyFilter, CompanyInsert, CompanyRow, CompanySort};
use uuid::Uuid;

use super::util;
use super::{require_crm_read, require_crm_write};
use crate::authz;

pub const MERGE_JOB_KIND: &str = "crm.merge_companies";

// ------------------------------------------------------------------- query

#[derive(Debug, Default)]
pub struct CompanyListQuery {
    pub q: Option<String>,
    pub lifecycle_stage: Option<String>,
    pub owner_principal_id: Option<Uuid>,
    /// None/false = non-archived (default view); true = archived only.
    pub archived: bool,
    /// `created_desc` (default) | `updated_desc` | `name_asc`.
    pub sort: Option<String>,
    pub limit: u32,
    pub cursor: Option<String>,
}

fn filters_json(q: &CompanyListQuery, sort: CompanySort) -> serde_json::Value {
    serde_json::json!({
        "endpoint": "GET /api/v1/companies",
        "q": q.q,
        "lifecycle_stage": q.lifecycle_stage,
        "owner_principal_id": q.owner_principal_id,
        "archived": q.archived,
        "sort": sort.as_str(),
        "limit": q.limit,
    })
}

/// `GET /companies` — search/filter/sort + signed cursor page (B.1.5). The
/// cursor is bound to the sort order, so it cannot be replayed under another.
pub async fn list(
    db: &Db,
    ctx: &Ctx,
    hmac_key: &[u8],
    q: &CompanyListQuery,
) -> Result<(Vec<CompanyResponse>, Page), AppError> {
    require_crm_read(ctx)?;
    if let Some(stage) = &q.lifecycle_stage
        && LifecycleStage::parse(stage).is_none()
    {
        return Err(util::bad_request(
            "invalid lifecycle_stage filter",
            vec![util::violation("lifecycle_stage", "ENUM")],
        ));
    }
    let sort = match q.sort.as_deref() {
        None => CompanySort::default(),
        Some(value) => CompanySort::parse(value).ok_or_else(|| {
            util::bad_request("invalid sort", vec![util::violation("sort", "ENUM")])
        })?,
    };
    let filters = filters_json(q, sort);
    let after = util::decode_text_cursor(q.cursor.as_deref(), hmac_key, ctx, &filters)?;
    let filter = CompanyFilter {
        q: q.q.as_deref().map(dcrm::escape_like),
        lifecycle_stage: q.lifecycle_stage.clone(),
        owner_principal_id: q.owner_principal_id,
        archived: q.archived,
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let mut rows = crm::list_companies(&mut tx, &filter, sort, after, q.limit + 1).await?;
    tx.commit().await?;
    let page = util::page_for_text(&mut rows, q.limit, hmac_key, ctx, &filters, |r| {
        (r.sort_key.clone(), r.row.id)
    });
    Ok((
        rows.iter().map(|r| util::company_dto(&r.row)).collect(),
        page,
    ))
}

/// `GET /companies/{id}` — 404 hides other-workspace rows (RLS + T01).
pub async fn get(db: &Db, ctx: &Ctx, id: Uuid) -> Result<CompanyResponse, AppError> {
    require_crm_read(ctx)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = crm::get_company(&mut tx, id).await?;
    tx.commit().await?;
    row.map(|r| util::company_dto(&r))
        .ok_or_else(|| AppError::not_found("company not found"))
}

// ----------------------------------------------------------------- writers

/// Owner override validation for create: assigning a different principal is
/// a reassignment — Manager/Admin only (§5.3, B.3.3).
fn check_owner_assignment(ctx: &Ctx, owner: Option<Uuid>) -> Result<(), AppError> {
    if let Some(owner) = owner
        && owner != ctx.principal_id
        && !matches!(ctx.role, Role::Admin | Role::Manager)
    {
        return Err(AppError::forbidden(
            "assigning another owner requires manager or admin role",
        ));
    }
    Ok(())
}

fn validate_insert(req: &CreateCompanyRequest) -> Result<CompanyInsert, AppError> {
    util::require_name(&req.name, "name")?;
    util::require_country_code(req.country_code.as_deref())?;
    if let Some(stage) = &req.lifecycle_stage
        && matches!(stage, LifecycleStage::Inactive)
    {
        // `inactive` is reachable only through archive, not create.
        return Err(util::bad_request(
            "lifecycle_stage may not be 'inactive' at creation",
            vec![util::violation("lifecycle_stage", "ENUM")],
        ));
    }
    let domain = req.domain.as_deref().and_then(dcrm::normalize_domain);
    if req.domain.is_some() && domain.is_none() {
        return Err(util::bad_request(
            "domain is not a valid host name",
            vec![util::violation("domain", "FORMAT")],
        ));
    }
    Ok(CompanyInsert {
        name: req.name.trim().to_string(),
        domain,
        website_url: req.website_url.clone(),
        industry: req.industry.clone(),
        country_code: req.country_code.clone(),
        lifecycle_stage: req
            .lifecycle_stage
            .unwrap_or(LifecycleStage::Prospect)
            .as_str()
            .to_string(),
        owner_principal_id: req.owner_principal_id,
        description: req.description.clone(),
        custom_fields: req
            .custom_fields
            .clone()
            .map(serde_json::Value::Object)
            .unwrap_or_else(|| serde_json::json!({})),
    })
}

/// `POST /companies` — B.3.3. Agents are rejected (lead-proposals instead).
/// Always called with `Idempotency-Key` (route enforces) → 201 stored.
pub async fn create(
    db: &Db,
    ctx: &Ctx,
    req: &CreateCompanyRequest,
    idem_key: String,
) -> Result<Stored<CompanyResponse>, AppError> {
    require_crm_write(ctx)?;
    // B.3.3: agent direct creation is rejected — use /lead-proposals.
    authz::require_not_agent_for(ctx, "create companies directly")?;
    authz::require_human(ctx)?;
    let insert = validate_insert(req)?;
    check_owner_assignment(ctx, req.owner_principal_id)?;
    let body = serde_json::to_value(req)
        .map_err(|e| AppError::internal(format!("request serialize: {e}")))?;
    let ctx_owned = ctx.clone();
    util::run_maybe_idempotent(
        db,
        ctx,
        "POST /api/v1/companies",
        Some(idem_key),
        &body,
        201,
        move |mut tx| async move {
            let r = create_in_tx(&mut tx, &ctx_owned, insert).await?;
            Ok((tx, r))
        },
    )
    .await
}

async fn create_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mut insert: CompanyInsert,
) -> Result<CompanyResponse, AppError> {
    insert.owner_principal_id = util::resolve_owner(tx, insert.owner_principal_id).await?;
    let row = crm::insert_company(tx, &insert).await?;
    // Advisory duplicates (B.3.3): same domain or case-insensitive name.
    let dupes = crm::company_duplicates(tx, row.id, &row.name, row.domain.as_deref()).await?;
    let mut dto = util::company_dto(&row);
    dto.duplicate_candidates = Some(
        dupes
            .iter()
            .map(|d| {
                let mut matched = Vec::new();
                if row.domain.is_some() && d.domain == row.domain {
                    matched.push("domain".to_string());
                }
                if d.name.eq_ignore_ascii_case(&row.name) {
                    matched.push("name".to_string());
                }
                util::duplicate_dto(d, matched)
            })
            .collect(),
    );
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "company.create",
            "company",
            ctx.request_id.clone(),
        )
        .with_target(row.id),
    )
    .await?;
    util::write_activity(
        tx,
        crm::ActivityInsert {
            company_id: Some(row.id),
            actor_principal_id: Some(ctx.principal_id),
            kind: activity_kind::COMPANY_CREATED,
            source_event_id: row.id,
            occurred_at: row.created_at,
            summary: serde_json::json!({"name": row.name, "domain": row.domain}),
            ..Default::default()
        },
    )
    .await?;
    util::emit_change(tx, "company.created", "company", row.id, row.version).await?;
    Ok(dto)
}

// ------------------------------------------------------------------- patch

fn apply_patch(row: &CompanyRow, req: &PatchCompanyRequest) -> Result<CompanyInsert, AppError> {
    if let Some(name) = &req.name {
        util::require_name(name, "name")?;
    }
    let country = match &req.country_code {
        Some(c) => c.clone(),
        None => row.country_code.clone(),
    };
    util::require_country_code(country.as_deref())?;
    let domain = match &req.domain {
        Some(d) => match d {
            Some(raw) => Some(dcrm::normalize_domain(raw).ok_or_else(|| {
                util::bad_request(
                    "domain is not a valid host name",
                    vec![util::violation("domain", "FORMAT")],
                )
            })?),
            None => None,
        },
        None => row.domain.clone(),
    };
    Ok(CompanyInsert {
        name: req
            .name
            .clone()
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| row.name.clone()),
        domain,
        website_url: match &req.website_url {
            Some(v) => v.clone(),
            None => row.website_url.clone(),
        },
        industry: match &req.industry {
            Some(v) => v.clone(),
            None => row.industry.clone(),
        },
        country_code: country,
        lifecycle_stage: req
            .lifecycle_stage
            .map(|s| s.as_str().to_string())
            .unwrap_or_else(|| row.lifecycle_stage.clone()),
        owner_principal_id: row.owner_principal_id,
        description: match &req.description {
            Some(v) => v.clone(),
            None => row.description.clone(),
        },
        custom_fields: req
            .custom_fields
            .clone()
            .map(serde_json::Value::Object)
            .unwrap_or_else(|| row.custom_fields.clone()),
    })
}

/// `PATCH /companies/{id}` — A.6.1 conditional update + If-Match.
/// A.6.1: on 0 changed rows distinguish 404 (invisible/absent) from 412.
pub async fn patch(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &PatchCompanyRequest,
) -> Result<CompanyResponse, AppError> {
    require_crm_write(ctx)?;
    authz::require_not_agent_for(ctx, "modify companies")?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = crm::get_company(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("company not found"))?;
    authz::crm_can_edit(ctx, row.owner_principal_id)?;
    let merged = apply_patch(&row, req)?;
    let Some(updated) =
        crm::update_company_versioned(&mut tx, id, expected_version, &merged).await?
    else {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "company was modified by another request",
        ));
    };
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "company.patch",
            "company",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    util::emit_change(&mut tx, "company.updated", "company", id, updated.version).await?;
    tx.commit().await?;
    Ok(util::company_dto(&updated))
}

/// `POST /companies/{id}/archive` — If-Match on the versioned resource.
/// Archive permission follows the record-edit rule (crm_can_edit): Member
/// may archive own/unassigned records (§5.3 edit column).
pub async fn archive(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &ArchiveCompanyRequest,
) -> Result<CompanyResponse, AppError> {
    require_crm_write(ctx)?;
    authz::require_not_agent_for(ctx, "archive companies")?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = crm::get_company(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("company not found"))?;
    authz::crm_can_edit(ctx, row.owner_principal_id)?;
    if row.archived_at.is_some() {
        return Err(AppError::conflict("company is already archived"));
    }
    let Some(updated) = crm::archive_company(&mut tx, id, expected_version).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "company was modified by another request",
        ));
    };
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "company.archive",
            "company",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    util::write_activity(
        &mut tx,
        crm::ActivityInsert {
            company_id: Some(id),
            actor_principal_id: Some(ctx.principal_id),
            kind: activity_kind::COMPANY_ARCHIVED,
            source_event_id: updated.id,
            occurred_at: Utc::now(),
            summary: serde_json::json!({"reason": req.reason}),
            ..Default::default()
        },
    )
    .await?;
    util::emit_change(&mut tx, "company.archived", "company", id, updated.version).await?;
    tx.commit().await?;
    Ok(util::company_dto(&updated))
}

// ----------------------------------------------------------------- merging

/// `POST /companies/{id}/merge-requests` → 202 + merge job (Manager/Admin,
/// §5.3). `{id}` is the surviving target; `source_id` is folded into it.
pub async fn request_merge(
    db: &Db,
    ctx: &Ctx,
    target_id: Uuid,
    req: &MergeCompanyRequest,
    idem_key: String,
) -> Result<Stored<MergeCompanyResponse>, AppError> {
    require_crm_write(ctx)?;
    authz::require_not_agent_for(ctx, "merge companies")?;
    authz::require_role(ctx, &[Role::Admin, Role::Manager])?;
    let bad = dcrm::invalid_field_choices(&req.field_choices);
    if !bad.is_empty() {
        return Err(util::bad_request(
            "invalid field_choices entries",
            bad.iter()
                .map(|f| util::violation(f, "INVALID_FIELD_CHOICE"))
                .collect(),
        ));
    }
    if req.source_id == target_id {
        return Err(util::bad_request(
            "source and target must differ",
            vec![util::violation("source_id", "SELF_MERGE")],
        ));
    }
    let body = serde_json::to_value(req)
        .map_err(|e| AppError::internal(format!("request serialize: {e}")))?;
    let ctx_owned = ctx.clone();
    util::run_maybe_idempotent(
        db,
        ctx,
        "POST /api/v1/companies/{id}/merge-requests",
        Some(idem_key),
        &body,
        202,
        move |mut tx| async move {
            let r = request_merge_in_tx(&mut tx, &ctx_owned, target_id, req).await?;
            Ok((tx, r))
        },
    )
    .await
}

async fn request_merge_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    target_id: Uuid,
    req: &MergeCompanyRequest,
) -> Result<MergeCompanyResponse, AppError> {
    let dedupe_key = format!("merge_companies:{}:{}", req.source_id, target_id);
    // Repeated requests for an active source/target pair reuse its job.
    if let Some((job_id, state)) = crm::find_merge_job(tx, req.source_id, target_id).await?
        && !matches!(state.as_str(), "dead" | "canceled")
    {
        return Ok(MergeCompanyResponse {
            job_id,
            source_id: req.source_id,
            target_id,
            status: state,
        });
    }
    for id in [req.source_id, target_id] {
        let Some(company) = crm::get_company(tx, id).await? else {
            return Err(AppError::not_found("company not found"));
        };
        if company.archived_at.is_some() {
            return Err(AppError::conflict("cannot merge an archived company"));
        }
    }
    // A previous terminal merge for this pair gets a fresh dedupe key.
    let count = crm::merge_job_count(tx, req.source_id, target_id).await?;
    let dedupe_key = if count > 0 {
        format!("{dedupe_key}:{count}")
    } else {
        dedupe_key
    };
    let payload = serde_json::json!({
        "merge_request": {
            "source_id": req.source_id,
            "target_id": target_id,
            "field_choices": req.field_choices,
        },
        "requested_by": ctx.principal_id,
    });
    // `requested_by` rides inside the payload (frozen enqueue signature has
    // no principal param; backfilling the jobs column would also break under
    // the post-rebase grant tightening — no UPDATE on jobs for api_runtime).
    let job_id = jobs::enqueue(
        tx,
        MERGE_JOB_KIND,
        1,
        payload,
        Utc::now(),
        Some(&dedupe_key),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "company.merge_request",
            "company",
            ctx.request_id.clone(),
        )
        .with_target(target_id),
    )
    .await?;
    util::emit_event(
        tx,
        "company.merge_requested",
        "company_merge_request",
        job_id,
        1,
        serde_json::json!({
            "job_id": job_id,
            "source_company_id": req.source_id,
            "target_company_id": target_id,
        }),
    )
    .await?;
    Ok(MergeCompanyResponse {
        job_id,
        source_id: req.source_id,
        target_id,
        status: "queued".to_string(),
    })
}
