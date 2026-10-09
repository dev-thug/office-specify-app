//! `imports.commit` job body: replay the validated rows into CRM.
//!
//! - `valid` rows create company (or link an existing one under the
//!   `link` duplicate strategy), contact, email address + contact_emails
//!   link. Imported addresses receive **no** communication basis — a CSV
//!   can never assert marketing consent (B.3.13: "CRM에 새로 등록된 주소에
//!   마케팅 동의를 일괄 부여하지 않는다").
//! - `duplicate` rows: skipped under `review`; under `link` they attach to
//!   the existing company/address — so a re-imported suppressed address
//!   reuses its `email_addresses` row and the suppression keeps applying
//!   (T32).
//! - `invalid` rows: `skip` leaves them untouched, `reject` fails the job
//!   (the request path already 422s; the job rechecks defensively).

use contracts::AppError;
use domain::email_address::{self, LocalPartPolicy};
use infrastructure::db::Tx;
use infrastructure::repo::imports as repo;
use infrastructure::repo::research as research_repo;
use uuid::Uuid;

pub async fn run(
    tx: &mut Tx<'_>,
    import_job_id: Uuid,
    job_id: Option<Uuid>,
) -> Result<(), AppError> {
    let job = repo::lock_import_job(tx, import_job_id)
        .await?
        .ok_or_else(|| AppError::not_found("import job not found"))?;
    if job.kind() != "import" {
        return Ok(());
    }
    // Idempotent: 'completed' already done; anything not 'committing' was
    // not committed through the API path.
    if job.status == "completed" {
        return Ok(());
    }
    if job.status != "committing" {
        return Ok(());
    }

    let actor = super::validate::repo_job_actor_pub(tx, &job).await?;
    let policy = job
        .summary
        .get("invalid_row_policy")
        .and_then(|v| v.as_str())
        .unwrap_or("skip")
        .to_string();
    let duplicate_strategy = job
        .mapping
        .get("duplicate_strategy")
        .and_then(|v| v.as_str())
        .unwrap_or("review")
        .to_string();
    let invalid_rows = job
        .summary
        .get("invalid_rows")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    if policy == "reject" && invalid_rows > 0 {
        repo::merge_summary(
            tx,
            job.id,
            &serde_json::json!({"commit_error": "invalid_rows_present"}),
        )
        .await?;
        repo::transition_status(tx, job.id, &["committing"], "failed").await?;
        super::validate::audit_import(
            tx,
            actor,
            job_id,
            job.id,
            "import.commit_failed",
            &serde_json::json!({"error": "invalid_rows_present"}),
        )
        .await?;
        return Ok(());
    }

    // Rows to import: valid always; duplicates only under `link`.
    let mut work = repo::rows_in_status(tx, job.id, &["valid"]).await?;
    let mut dup_rows = repo::rows_in_status(tx, job.id, &["duplicate"]).await?;
    let (mut imported, mut skipped, mut errors_count) = (0i64, 0i64, 0i64);
    if duplicate_strategy == "link" {
        work.append(&mut dup_rows);
        work.sort_by_key(|r| r.row_no);
    } else {
        for r in &dup_rows {
            repo::set_row_result(tx, r.id, "skipped", None, None).await?;
            skipped += 1;
        }
    }

    for row in work {
        match import_row(tx, &job, &row, &duplicate_strategy).await {
            Ok((company_id, contact_id)) => {
                repo::set_row_result(tx, row.id, "imported", company_id, contact_id).await?;
                imported += 1;
            }
            Err(e) => {
                // Data-level failures shouldn't kill the batch: mark the
                // row skipped + error and continue. SQL errors that abort
                // the tx would surface from execute() itself — this arm
                // only sees mapped validation problems.
                repo::append_row_errors(
                    tx,
                    row.id,
                    &serde_json::json!([{
                        "code": "commit_failed",
                        "message": e.to_string(),
                    }]),
                )
                .await?;
                repo::set_row_result(tx, row.id, "skipped", None, None).await?;
                errors_count += 1;
            }
        }
    }

    repo::merge_summary(
        tx,
        job.id,
        &serde_json::json!({
            "imported_rows": imported,
            "skipped_rows": skipped,
            "error_rows": errors_count,
        }),
    )
    .await?;
    repo::transition_status(tx, job.id, &["committing"], "completed").await?;
    super::validate::audit_import(
        tx,
        actor,
        job_id,
        job.id,
        "import.completed",
        &serde_json::json!({
            "imported_rows": imported,
            "skipped_rows": skipped,
            "error_rows": errors_count,
        }),
    )
    .await?;
    Ok(())
}

/// One row → (company_id, contact_id). Validation guarantees shape; this
/// only does entity resolution + inserts.
async fn import_row(
    tx: &mut Tx<'_>,
    job: &repo::ImportJobRow,
    row: &repo::ImportRowRow,
    duplicate_strategy: &str,
) -> Result<(Option<Uuid>, Option<Uuid>), AppError> {
    let company = row
        .normalized_data
        .get("company")
        .cloned()
        .unwrap_or_default();
    let contact = row
        .normalized_data
        .get("contact")
        .cloned()
        .unwrap_or_default();
    let get = |m: &serde_json::Value, k: &str| {
        m.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };

    // Company resolution.
    let mut company_id: Option<Uuid> = None;
    let dom_norm = get(&company, "domain").and_then(|d| domain::research::normalize_domain(&d));
    if duplicate_strategy == "link"
        && let Some(dom) = &dom_norm
    {
        company_id = research_repo::company_id_by_domain(tx, dom).await?;
    }
    if company_id.is_none()
        && let Some(name) = get(&company, "name")
    {
        company_id = Some(
            research_repo::insert_company(
                tx,
                &name,
                get(&company, "domain").as_deref(),
                get(&company, "industry").as_deref(),
                get(&company, "country_code").as_deref(),
                get(&company, "website_url").as_deref(),
            )
            .await?,
        );
    }

    // Contact: needs a name or an email to exist at all.
    let contact_name = get(&contact, "name");
    let email_norm_key = get(&contact, "email_normalized");
    let email_raw = get(&contact, "email");
    if contact_name.is_none() && email_norm_key.is_none() && email_raw.is_none() {
        return Ok((company_id, None));
    }
    let contact_id = research_repo::insert_contact(
        tx,
        company_id,
        contact_name.as_deref().unwrap_or("(imported)"),
        get(&contact, "job_title").as_deref(),
        get(&contact, "department").as_deref(),
        Some(job.requested_by),
    )
    .await?;

    if let Some(key) = email_norm_key.as_deref().or(email_raw.as_deref()) {
        // email_normalized was written at validation; fall back to
        // re-normalizing defensively.
        let n = email_address::normalize(key, LocalPartPolicy::Lowercase)
            .map_err(|e| AppError::internal(format!("normalized email broke: {e}")))?;
        let addr_id = research_repo::find_or_create_email_address(
            tx,
            email_raw.as_deref().unwrap_or(&n.original),
            &n.normalized_key,
            n.normalization_version,
        )
        .await?;
        research_repo::link_contact_email(
            tx,
            contact_id,
            addr_id,
            get(&contact, "email_label").as_deref(),
            true,
        )
        .await?;
    }
    Ok((company_id, Some(contact_id)))
}
