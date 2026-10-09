//! `imports.validate` job body: fetch object → parse CSV → normalize +
//! per-row errors → duplicate checks → `import_rows` + `ready`/`failed`
//! + `validation_hash` (B.3.13).
//!
//! Business failures (bad CSV, too many rows, missing headers) mark the
//! import `failed` and finish the job — they are not transient. Object
//! store / DB errors bubble up as job errors (retry per §16.4).

use contracts::AppError;
use domain::email_address::LocalPartPolicy;
use domain::imports as d;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::crypto;
use infrastructure::db::Tx;
use infrastructure::repo::imports as repo;
use infrastructure::repo::research as research_repo;
use infrastructure::storage::ObjectStore;
use uuid::Uuid;

/// Run inside the worker's claimed-job tx. `job_id` is the durable `jobs`
/// row id (audit correlation).
pub async fn run(
    tx: &mut Tx<'_>,
    store: &impl ObjectStore,
    import_job_id: Uuid,
    job_id: Option<Uuid>,
) -> Result<(), AppError> {
    let job = repo::lock_import_job(tx, import_job_id)
        .await?
        .ok_or_else(|| AppError::not_found("import job not found"))?;
    if job.kind() != "import" {
        return Ok(()); // not ours — ignore
    }
    // Idempotent re-entry: only 'validating' does work; anything later is
    // already past this stage.
    if job.status != "validating" {
        return Ok(());
    }

    let object = repo::get_stored_object(tx, job.stored_object_id)
        .await?
        .ok_or_else(|| AppError::internal("import source object missing"))?;
    let bytes = store.get(&object.storage_key).await?;

    let columns = mapping_columns(&job.mapping);
    let has_header = job
        .mapping
        .get("has_header")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let duplicate_strategy = job
        .mapping
        .get("duplicate_strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("review")
        .to_string();

    let actor = repo_job_actor_pub(tx, &job).await?;
    async fn fail(
        tx: &mut Tx<'_>,
        import_job: Uuid,
        actor: Option<Uuid>,
        job_id: Option<Uuid>,
        summary: serde_json::Value,
    ) -> Result<(), AppError> {
        repo::set_validation_result(tx, import_job, "failed", None, &summary).await?;
        audit_import(
            tx,
            actor,
            job_id,
            import_job,
            "import.validate_failed",
            &summary,
        )
        .await?;
        Ok(())
    }

    let (headers, records) = match repo::parse_csv(&bytes, has_header) {
        Ok(x) => x,
        Err(e) => {
            return fail(
                tx,
                job.id,
                actor,
                job_id,
                serde_json::json!({"error": "csv_parse", "detail": e.detail}),
            )
            .await;
        }
    };
    if headers.is_empty() || records.is_empty() {
        return fail(
            tx,
            job.id,
            actor,
            job_id,
            serde_json::json!({"error": "empty_csv"}),
        )
        .await;
    }
    if records.len() > d::MAX_IMPORT_ROWS {
        return fail(
            tx,
            job.id,
            actor,
            job_id,
            serde_json::json!({"error": "too_many_rows", "max": d::MAX_IMPORT_ROWS}),
        )
        .await;
    }

    // Existing duplicate anchors — queried per row (the row set is ≤100k;
    // point queries keep it simple and exact).
    let mut counts = serde_json::Map::new();
    let (mut valid, mut invalid, mut duplicate) = (0i64, 0i64, 0i64);
    let mut fp_input = Vec::new();
    for (i, record) in records.iter().enumerate() {
        let row_no = (i + 1) as i32;
        let mut normalized = d::apply_mapping(&headers, record, &columns);
        let mut errors = d::validate_row(&mut normalized, LocalPartPolicy::Lowercase);
        let mut status = if errors.is_empty() {
            "valid"
        } else {
            "invalid"
        };

        if status == "valid" {
            // Duplicate detection: normalized email exists OR company
            // domain matches an existing company.
            let email_key = normalized["contact"]["email_normalized"].as_str();
            let domain_norm = normalized["company"]["domain"]
                .as_str()
                .and_then(domain::research::normalize_domain);
            let mut dup_notes: Vec<d::RowError> = Vec::new();
            if let Some(key) = email_key
                && repo::email_address_id_by_key(tx, key).await?.is_some()
            {
                dup_notes.push(d::RowError::new(
                    Some("contact.email"),
                    "duplicate_email",
                    "address already exists in this workspace",
                ));
            }
            if let Some(dom) = domain_norm.as_deref()
                && research_repo::company_id_by_domain(tx, dom)
                    .await?
                    .is_some()
            {
                dup_notes.push(d::RowError::new(
                    Some("company.domain"),
                    "duplicate_company_domain",
                    "an existing company has this domain",
                ));
            }
            if !dup_notes.is_empty() {
                status = "duplicate";
                // Duplicate markers are informational, not errors — but
                // the rows GET surfaces them through `errors`.
                errors.extend(dup_notes);
            }
        }
        match status {
            "valid" => valid += 1,
            "invalid" => invalid += 1,
            _ => duplicate += 1,
        }
        let errors_json = serde_json::to_value(&errors).unwrap_or_else(|_| serde_json::json!([]));
        repo::insert_import_row(tx, job.id, row_no, &normalized, status, &errors_json).await?;
        fp_input.extend_from_slice(row_no.to_be_bytes().as_slice());
        fp_input.extend_from_slice(status.as_bytes());
        fp_input
            .extend_from_slice(crypto::sha256_bytes(normalized.to_string().as_bytes()).as_slice());
    }
    counts.insert("total_rows".into(), (records.len() as i64).into());
    counts.insert("valid_rows".into(), valid.into());
    counts.insert("invalid_rows".into(), invalid.into());
    counts.insert("duplicate_rows".into(), duplicate.into());
    counts.insert("duplicate_strategy".into(), duplicate_strategy.into());

    // validation_hash pins: object content hash + mapping + row outcomes.
    let mut h_input = Vec::new();
    h_input.extend_from_slice(&object.content_hash);
    h_input.extend_from_slice(job.mapping.to_string().as_bytes());
    h_input.extend_from_slice(&fp_input);
    let validation_hash = crypto::sha256_bytes(&h_input);

    repo::set_validation_result(
        tx,
        job.id,
        "ready",
        Some(&validation_hash),
        &serde_json::Value::Object(counts.clone()),
    )
    .await?;
    audit_import(
        tx,
        actor,
        job_id,
        job.id,
        "import.validated",
        &serde_json::json!({
            "total_rows": counts["total_rows"],
            "valid_rows": valid,
            "duplicate_rows": duplicate,
            "invalid_rows": invalid,
        }),
    )
    .await?;
    Ok(())
}

fn mapping_columns(mapping: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    mapping
        .get("columns")
        .and_then(|c| {
            serde_json::from_value::<std::collections::BTreeMap<String, String>>(c.clone()).ok()
        })
        .unwrap_or_default()
}

/// Audit actor for job-side actions: the workspace system principal when
/// present, else the requester (§5.1 — actor vs requested_by).
pub(crate) async fn repo_job_actor_pub(
    tx: &mut Tx<'_>,
    job: &repo::ImportJobRow,
) -> Result<Option<Uuid>, AppError> {
    Ok(match research_repo::system_principal_id(tx).await? {
        Some(system) => Some(system),
        None => Some(job.requested_by),
    })
}

pub(crate) async fn audit_import(
    tx: &mut Tx<'_>,
    actor: Option<Uuid>,
    job_id: Option<Uuid>,
    import_job_id: Uuid,
    action: &str,
    details: &serde_json::Value,
) -> Result<(), AppError> {
    let mut e = AuditEntry::new(actor.unwrap_or_else(Uuid::nil), action, "import_job", "")
        .with_target(import_job_id);
    e.job_id = job_id;
    e.metadata = audit::details(details);
    audit::record(tx, e).await
}
