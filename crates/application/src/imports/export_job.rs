//! `imports.export` job body: run the manifest's query as a fixed snapshot,
//! render CSV with the §15.1 formula-injection guard, store the result as
//! a `stored_objects` row and point `summary.output_object_id` at it.

use contracts::AppError;
use domain::imports as d;
use infrastructure::crypto;
use infrastructure::db::Tx;
use infrastructure::repo::imports as repo;
use infrastructure::storage::ObjectStore;
use uuid::Uuid;

pub async fn run(
    tx: &mut Tx<'_>,
    store: &impl ObjectStore,
    export_job_id: Uuid,
    job_id: Option<Uuid>,
) -> Result<(), AppError> {
    let job = repo::lock_import_job(tx, export_job_id)
        .await?
        .ok_or_else(|| AppError::not_found("export job not found"))?;
    if job.kind() != "export" {
        return Ok(());
    }
    if job.status != "uploaded" {
        return Ok(()); // already done or canceled — idempotent
    }

    let actor = super::validate::repo_job_actor_pub(tx, &job).await?;
    let fields: Vec<String> = job
        .mapping
        .get("fields")
        .and_then(|f| serde_json::from_value(f.clone()).ok())
        .unwrap_or_default();
    let filters = job.mapping.get("filters").cloned().unwrap_or_default();
    let company_ids: Option<Vec<Uuid>> = filters
        .get("company_ids")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let stage = filters
        .get("lifecycle_stage")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let q = filters
        .get("q")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    // +1 row detects truncation — the snapshot is capped, never partial-
    // silently.
    let records = repo::exportable_records(
        tx,
        company_ids.as_deref(),
        stage.as_deref(),
        q.as_deref(),
        d::MAX_EXPORT_ROWS as i64 + 1,
    )
    .await?;
    let truncated = records.len() > d::MAX_EXPORT_ROWS;
    let records: Vec<_> = records.into_iter().take(d::MAX_EXPORT_ROWS).collect();

    let headers: Vec<String> = fields.clone();
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            fields
                .iter()
                .map(|f| record_field(r, f))
                .collect::<Vec<String>>()
        })
        .collect();
    let csv = d::render_csv(&headers, &rows);
    let bytes = csv.into_bytes();

    let storage_key = Uuid::new_v4().simple().to_string();
    store.put(&storage_key, &bytes).await?;
    let hash = crypto::sha256_bytes(&bytes);
    let object_id = repo::insert_stored_object(
        tx,
        &storage_key,
        "text/csv",
        bytes.len() as i64,
        &hash,
        "clean", // server-generated
        Some(job.requested_by),
    )
    .await?;

    repo::merge_summary(
        tx,
        job.id,
        &serde_json::json!({
            "output_object_id": object_id,
            "row_count": records.len(),
            "truncated": truncated,
            "fields": fields,
        }),
    )
    .await?;
    repo::transition_status(tx, job.id, &["uploaded"], "completed").await?;
    super::validate::audit_import(
        tx,
        actor,
        job_id,
        job.id,
        "export.completed",
        &serde_json::json!({
            "row_count": records.len(),
            "truncated": truncated,
        }),
    )
    .await?;
    Ok(())
}

fn record_field(r: &repo::ExportRecord, field: &str) -> String {
    let opt = |v: &Option<String>| v.clone().unwrap_or_default();
    match field {
        "company.name" => r.company_name.clone(),
        "company.domain" => opt(&r.company_domain),
        "company.industry" => opt(&r.company_industry),
        "company.country_code" => opt(&r.company_country_code),
        "company.website_url" => opt(&r.company_website_url),
        "company.lifecycle_stage" => r.company_lifecycle_stage.clone(),
        "contact.name" => opt(&r.contact_name),
        "contact.job_title" => opt(&r.contact_job_title),
        "contact.department" => opt(&r.contact_department),
        "contact.status" => opt(&r.contact_status),
        "contact.email" => opt(&r.contact_email),
        "contact.email_label" => opt(&r.contact_email_label),
        _ => String::new(),
    }
}
