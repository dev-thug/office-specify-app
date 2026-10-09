//! `crm.merge_companies` job body (integration contracts). Applies the merge
//! requested via `POST /companies/{id}/merge-requests`: chosen field values
//! onto the target, every shared-table `company_id` re-pointed, source
//! archived — all in the handler's business tx with audit + timeline row.
//!
//! Idempotent by construction: a re-delivered job sees the source already
//! archived and returns early; `insert_activity` dedupes on
//! `(source_event_id, kind)` where the anchor is the job id.

use contracts::AppError;
use domain::crm::{MergePick, activity_kind};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Tx;
use infrastructure::repo::crm::{self, CompanyInsert};
use serde::Deserialize;
use uuid::Uuid;

use super::util;

#[derive(Debug, Deserialize)]
pub struct MergeRequestPayload {
    pub source_id: Uuid,
    pub target_id: Uuid,
    #[serde(default)]
    pub field_choices: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
pub struct MergeJobPayload {
    pub merge_request: MergeRequestPayload,
    pub requested_by: Uuid,
}

/// Handler entry point. `job_id` anchors audit rows and the timeline
/// `source_event_id`.
pub async fn apply(
    tx: &mut Tx<'_>,
    job_id: Uuid,
    payload: serde_json::Value,
) -> Result<(), AppError> {
    let p: MergeJobPayload = serde_json::from_value(payload)
        .map_err(|e| AppError::internal(format!("merge payload: {e}")))?;
    let mr = &p.merge_request;
    if mr.source_id == mr.target_id {
        return Err(AppError::internal("merge source == target"));
    }
    // Lock in id order — the reverse-direction merge can't deadlock us.
    let (first, second) = if mr.source_id < mr.target_id {
        (mr.source_id, mr.target_id)
    } else {
        (mr.target_id, mr.source_id)
    };
    crm::lock_company(tx, first).await?;
    crm::lock_company(tx, second).await?;
    let source = crm::get_company(tx, mr.source_id)
        .await?
        .ok_or_else(|| AppError::not_found("merge source company not found"))?;
    let target = crm::get_company(tx, mr.target_id)
        .await?
        .ok_or_else(|| AppError::not_found("merge target company not found"))?;
    if source.archived_at.is_some() {
        // Already merged (retry/replay) — nothing to do.
        return Ok(());
    }
    if target.archived_at.is_some() {
        return Err(AppError::conflict("merge target is archived"));
    }
    // Resolve the winning column set for the target.
    let pick = |field: &str| {
        matches!(
            mr.field_choices
                .get(field)
                .and_then(|v| MergePick::parse(v)),
            Some(MergePick::Source)
        )
    };
    let merged = CompanyInsert {
        name: if pick("name") {
            source.name.clone()
        } else {
            target.name.clone()
        },
        domain: if pick("domain") {
            source.domain.clone()
        } else {
            target.domain.clone()
        },
        website_url: if pick("website_url") {
            source.website_url.clone()
        } else {
            target.website_url.clone()
        },
        industry: if pick("industry") {
            source.industry.clone()
        } else {
            target.industry.clone()
        },
        country_code: if pick("country_code") {
            source.country_code.clone()
        } else {
            target.country_code.clone()
        },
        lifecycle_stage: if pick("lifecycle_stage") {
            source.lifecycle_stage.clone()
        } else {
            target.lifecycle_stage.clone()
        },
        owner_principal_id: if pick("owner_principal_id") {
            source.owner_principal_id
        } else {
            target.owner_principal_id
        },
        description: if pick("description") {
            source.description.clone()
        } else {
            target.description.clone()
        },
        custom_fields: if pick("custom_fields") {
            source.custom_fields.clone()
        } else {
            target.custom_fields.clone()
        },
    };
    let moved = crm::repoint_company_refs(tx, mr.source_id, mr.target_id).await?;
    if crm::merge_finalize(
        tx,
        mr.target_id,
        target.version,
        &merged,
        merged.owner_principal_id,
        mr.source_id,
    )
    .await?
    .is_none()
    {
        return Err(AppError::conflict(
            "merge target changed concurrently; merge not applied",
        ));
    }
    let actor = util::system_principal(tx).await?;
    let mut entry = AuditEntry::new(
        actor.unwrap_or(p.requested_by),
        "company.merge",
        "company",
        job_id.to_string(),
    )
    .with_target(mr.target_id);
    entry.requested_by = Some(p.requested_by);
    entry.job_id = Some(job_id);
    entry.metadata = serde_json::json!({
        "source_id": mr.source_id,
        "moved_rows": moved,
    });
    audit::record(tx, entry).await?;
    // Timeline row on the surviving company; source_event_id = job id makes
    // handler retries idempotent.
    util::write_activity(
        tx,
        crm::ActivityInsert {
            company_id: Some(mr.target_id),
            actor_principal_id: actor,
            kind: activity_kind::COMPANY_MERGED,
            source_event_id: job_id,
            occurred_at: chrono::Utc::now(),
            summary: serde_json::json!({
                "source_company_id": mr.source_id,
                "field_choices": mr.field_choices,
            }),
            ..Default::default()
        },
    )
    .await?;
    let merged_target = crm::get_company(tx, mr.target_id)
        .await?
        .ok_or_else(|| AppError::internal("merge target missing after update"))?;
    util::emit_event(
        tx,
        "company.merged",
        "company",
        mr.target_id,
        merged_target.version,
        serde_json::json!({
            "job_id": job_id,
            "source_company_id": mr.source_id,
            "target_company_id": mr.target_id,
            "version": merged_target.version,
        }),
    )
    .await?;
    Ok(())
}
