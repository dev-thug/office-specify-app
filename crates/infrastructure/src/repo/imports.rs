//! imports module repository (spec §6 — owned by the module worker).
//!
//! SQL for `import_jobs` / `import_rows` / `stored_objects`, the HMAC
//! upload-capability used by `/imports/upload-requests` (§15.2), CSV
//! parsing for the validate job, and the export source query.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredObjectRow {
    pub id: Uuid,
    pub storage_key: String,
    pub media_type: String,
    pub byte_size: i64,
    pub content_hash: Vec<u8>,
    pub scan_status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ImportJobRow {
    pub id: Uuid,
    pub stored_object_id: Uuid,
    pub requested_by: Uuid,
    pub mapping: serde_json::Value,
    pub status: String,
    pub validation_hash: Option<Vec<u8>>,
    pub summary: serde_json::Value,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl ImportJobRow {
    /// Export jobs reuse `import_jobs`; `mapping.job_kind == "export"`
    /// distinguishes them (integration-contracts `imports.export`).
    pub fn kind(&self) -> &'static str {
        match self.mapping.get("job_kind").and_then(|k| k.as_str()) {
            Some("export") => "export",
            _ => "import",
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ImportRowRow {
    pub id: Uuid,
    pub row_no: i32,
    pub normalized_data: serde_json::Value,
    pub status: String,
    pub errors: serde_json::Value,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

const JOB_COLS: &str = "id, stored_object_id, requested_by, mapping, status,
         validation_hash, summary, version, created_at";

// ---------------------------------------------------------- stored objects

#[allow(clippy::too_many_arguments)]
pub async fn insert_stored_object(
    tx: &mut Tx<'_>,
    storage_key: &str,
    media_type: &str,
    byte_size: i64,
    content_hash: &[u8],
    scan_status: &str,
    created_by: Option<Uuid>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO stored_objects
            (workspace_id, storage_key, media_type, byte_size, content_hash,
             scan_status, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(storage_key)
    .bind(media_type)
    .bind(byte_size)
    .bind(content_hash)
    .bind(scan_status)
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_stored_object(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<StoredObjectRow>, AppError> {
    sqlx::query_as::<_, StoredObjectRow>(
        "SELECT id, storage_key, media_type, byte_size, content_hash,
                scan_status, created_by, created_at
           FROM stored_objects
          WHERE workspace_id = $1 AND id = $2 AND deleted_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The import job (if any) that references this object — as CSV source
/// (`stored_object_id`) or as export output (`summary.output_object_id`).
/// Download authorization is decided from this link (§15.2 re-check).
#[derive(Debug, Clone)]
pub struct ObjectRef {
    pub import_job_id: Uuid,
    pub requested_by: Uuid,
    /// true when the object is the job's *output* (export result).
    pub is_output: bool,
}

pub async fn find_object_ref(
    tx: &mut Tx<'_>,
    object_id: Uuid,
) -> Result<Option<ObjectRef>, AppError> {
    let row: Option<(Uuid, Uuid, bool)> = sqlx::query_as(
        "SELECT id, requested_by,
                COALESCE(summary->>'output_object_id' = $2::text, false) AS is_output
           FROM import_jobs
          WHERE workspace_id = $1
            AND (stored_object_id = $2 OR summary->>'output_object_id' = $2::text)
          ORDER BY created_at
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(object_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(
        row.map(|(import_job_id, requested_by, is_output)| ObjectRef {
            import_job_id,
            requested_by,
            is_output,
        }),
    )
}

// ------------------------------------------------------------ import jobs

pub async fn insert_import_job(
    tx: &mut Tx<'_>,
    stored_object_id: Uuid,
    requested_by: Uuid,
    mapping: &serde_json::Value,
    status: &str,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO import_jobs
            (workspace_id, stored_object_id, requested_by, mapping, status)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(stored_object_id)
    .bind(requested_by)
    .bind(mapping)
    .bind(status)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_import_job(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ImportJobRow>, AppError> {
    sqlx::query_as::<_, ImportJobRow>(&format!(
        "SELECT {JOB_COLS} FROM import_jobs
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Keyset page over workspace import/export jobs, newest first. A caller
/// without manager/admin visibility supplies `requested_by` to restrict rows
/// to its own jobs.
pub async fn list_import_jobs(
    tx: &mut Tx<'_>,
    requested_by: Option<Uuid>,
    status: Option<&str>,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<ImportJobRow>, AppError> {
    sqlx::query_as::<_, ImportJobRow>(&format!(
        "SELECT {JOB_COLS} FROM import_jobs
          WHERE workspace_id = $1
            AND ($2::uuid IS NULL OR requested_by = $2)
            AND ($3::text IS NULL OR status = $3)
            AND ($4::timestamptz IS NULL OR (created_at, id) < ($4, $5))
          ORDER BY created_at DESC, id DESC
          LIMIT $6"
    ))
    .bind(tx.workspace_id())
    .bind(requested_by)
    .bind(status)
    .bind(after.map(|position| position.0))
    .bind(after.map(|position| position.1).unwrap_or(Uuid::nil()))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn lock_import_job(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ImportJobRow>, AppError> {
    sqlx::query_as::<_, ImportJobRow>(&format!(
        "SELECT {JOB_COLS} FROM import_jobs
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Status transition with optimistic source-state guard — returns false
/// when the job is not in `expected`.
pub async fn transition_status(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected: &[&str],
    new_status: &str,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE import_jobs SET status = $4, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND status = ANY($3)",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected)
    .bind(new_status)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Validation finished: store status + hash + summary atomically.
pub async fn set_validation_result(
    tx: &mut Tx<'_>,
    id: Uuid,
    status: &str,
    validation_hash: Option<&[u8]>,
    summary: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE import_jobs
            SET status = $3, validation_hash = $4, summary = $5,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(status)
    .bind(validation_hash)
    .bind(summary)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Merge keys into `summary` (JSONB concat) — used to stash the commit
/// policy and the commit/export results.
pub async fn merge_summary(
    tx: &mut Tx<'_>,
    id: Uuid,
    patch: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE import_jobs SET summary = summary || $3, version = version + 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(patch)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// ------------------------------------------------------------ import rows

pub async fn insert_import_row(
    tx: &mut Tx<'_>,
    import_job_id: Uuid,
    row_no: i32,
    normalized_data: &serde_json::Value,
    status: &str,
    errors: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO import_rows
            (workspace_id, import_job_id, row_no, normalized_data, status, errors)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tx.workspace_id())
    .bind(import_job_id)
    .bind(row_no)
    .bind(normalized_data)
    .bind(status)
    .bind(errors)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Keyset page over `row_no` (stable, gap-free per job).
pub async fn list_import_rows(
    tx: &mut Tx<'_>,
    import_job_id: Uuid,
    status: Option<&str>,
    after_row_no: i32,
    limit: i64,
) -> Result<Vec<ImportRowRow>, AppError> {
    sqlx::query_as::<_, ImportRowRow>(
        "SELECT id, row_no, normalized_data, status, errors, company_id,
                contact_id, created_at
           FROM import_rows
          WHERE workspace_id = $1 AND import_job_id = $2
            AND ($3::text IS NULL OR status = $3)
            AND row_no > $4
          ORDER BY row_no
          LIMIT $5",
    )
    .bind(tx.workspace_id())
    .bind(import_job_id)
    .bind(status)
    .bind(after_row_no)
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// All rows in one of `statuses` — the commit job's work set.
pub async fn rows_in_status(
    tx: &mut Tx<'_>,
    import_job_id: Uuid,
    statuses: &[&str],
) -> Result<Vec<ImportRowRow>, AppError> {
    sqlx::query_as::<_, ImportRowRow>(
        "SELECT id, row_no, normalized_data, status, errors, company_id,
                contact_id, created_at
           FROM import_rows
          WHERE workspace_id = $1 AND import_job_id = $2 AND status = ANY($3)
          ORDER BY row_no",
    )
    .bind(tx.workspace_id())
    .bind(import_job_id)
    .bind(statuses)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Duplicate check during validation: does the normalized email already
/// exist as an `email_addresses` row?
pub async fn email_address_id_by_key(
    tx: &mut Tx<'_>,
    normalized_key: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM email_addresses
          WHERE workspace_id = $1 AND normalized_key = $2",
    )
    .bind(tx.workspace_id())
    .bind(normalized_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Mark a validated row's commit outcome.
pub async fn set_row_result(
    tx: &mut Tx<'_>,
    row_id: Uuid,
    status: &str,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE import_rows
            SET status = $3, company_id = $4, contact_id = $5
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(row_id)
    .bind(status)
    .bind(company_id)
    .bind(contact_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Append row-level errors without touching status.
pub async fn append_row_errors(
    tx: &mut Tx<'_>,
    row_id: Uuid,
    errors: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE import_rows SET errors = errors || $3
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(row_id)
    .bind(errors)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// ---------------------------------------------------------------- export

/// Flattened export record: company LEFT JOIN contacts LEFT JOIN
/// contact_emails/email_addresses — one CSV line per (company, contact,
/// address) combination. Mail-derived columns do not exist here (§5.6).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExportRecord {
    pub company_name: String,
    pub company_domain: Option<String>,
    pub company_industry: Option<String>,
    pub company_country_code: Option<String>,
    pub company_website_url: Option<String>,
    pub company_lifecycle_stage: String,
    pub contact_name: Option<String>,
    pub contact_job_title: Option<String>,
    pub contact_department: Option<String>,
    pub contact_status: Option<String>,
    pub contact_email: Option<String>,
    pub contact_email_label: Option<String>,
}

pub async fn exportable_records(
    tx: &mut Tx<'_>,
    company_ids: Option<&[Uuid]>,
    lifecycle_stage: Option<&str>,
    q: Option<&str>,
    limit: i64,
) -> Result<Vec<ExportRecord>, AppError> {
    sqlx::query_as::<_, ExportRecord>(
        "SELECT c.name              AS company_name,
                c.domain            AS company_domain,
                c.industry          AS company_industry,
                c.country_code      AS company_country_code,
                c.website_url       AS company_website_url,
                c.lifecycle_stage   AS company_lifecycle_stage,
                ct.name             AS contact_name,
                ct.job_title        AS contact_job_title,
                ct.department       AS contact_department,
                ct.status           AS contact_status,
                e.address_original  AS contact_email,
                ce.label            AS contact_email_label
           FROM companies c
           LEFT JOIN contacts ct
                  ON ct.workspace_id = c.workspace_id
                 AND ct.company_id = c.id
                 AND ct.archived_at IS NULL
           LEFT JOIN contact_emails ce
                  ON ce.workspace_id = ct.workspace_id
                 AND ce.contact_id = ct.id
           LEFT JOIN email_addresses e
                  ON e.workspace_id = ce.workspace_id
                 AND e.id = ce.email_address_id
          WHERE c.workspace_id = $1 AND c.archived_at IS NULL
            AND ($2::uuid[] IS NULL OR c.id = ANY($2))
            AND ($3::text IS NULL OR c.lifecycle_stage = $3)
            AND ($4::text IS NULL OR c.name ILIKE '%' || $4 || '%'
                                   OR c.domain ILIKE '%' || $4 || '%')
          ORDER BY c.created_at, c.id, ct.created_at, ct.id
          LIMIT $5",
    )
    .bind(tx.workspace_id())
    .bind(company_ids)
    .bind(lifecycle_stage)
    .bind(q)
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ---------------------------------------------------- upload capability

/// Contents of an upload token: binds the capability to (workspace,
/// principal, object key, declared media type, declared max size, expiry).
/// Serialized then HMAC-signed with the server key — §15.2 says knowing an
/// object key must grant nothing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadGrant {
    pub storage_key: String,
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub media_type: String,
    pub max_bytes: i64,
    pub expires_at: DateTime<Utc>,
}

pub fn sign_upload_token(hmac_key: &[u8], grant: &UploadGrant) -> String {
    let body =
        crate::crypto::b64_encode(&serde_json::to_vec(grant).unwrap_or_else(|_| b"{}".to_vec()));
    let sig = crate::crypto::b64_encode(&crate::crypto::hmac_sha256(hmac_key, body.as_bytes()));
    format!("{body}.{sig}")
}

/// Returns the grant when signature, binding and expiry all check out.
pub fn verify_upload_token(
    hmac_key: &[u8],
    workspace_id: Uuid,
    principal_id: Uuid,
    storage_key: &str,
    token: &str,
) -> Option<UploadGrant> {
    let (body, sig) = token.split_once('.')?;
    let sig_bytes = crate::crypto::b64_decode(sig)?;
    if !crate::crypto::hmac_sha256_verify(hmac_key, body.as_bytes(), &sig_bytes) {
        return None;
    }
    let grant: UploadGrant = serde_json::from_slice(&crate::crypto::b64_decode(body)?).ok()?;
    if grant.workspace_id != workspace_id
        || grant.principal_id != principal_id
        || grant.storage_key != storage_key
        || grant.expires_at <= Utc::now()
    {
        return None;
    }
    Some(grant)
}

/// Short-lived download token for `GET /objects/{id}/download` — binds
/// object id + expiry (§15.2). The endpoint re-checks ACL before serving.
pub fn sign_download_token(hmac_key: &[u8], object_id: Uuid, expires_at: DateTime<Utc>) -> String {
    let body = crate::crypto::b64_encode(
        serde_json::json!({"object_id": object_id, "exp": expires_at.timestamp()})
            .to_string()
            .as_bytes(),
    );
    let sig = crate::crypto::b64_encode(&crate::crypto::hmac_sha256(hmac_key, body.as_bytes()));
    format!("{body}.{sig}")
}

pub fn verify_download_token(hmac_key: &[u8], object_id: Uuid, token: &str) -> bool {
    let Some((body, sig)) = token.split_once('.') else {
        return false;
    };
    let Some(sig_bytes) = crate::crypto::b64_decode(sig) else {
        return false;
    };
    if !crate::crypto::hmac_sha256_verify(hmac_key, body.as_bytes(), &sig_bytes) {
        return false;
    }
    let Some(raw) = crate::crypto::b64_decode(body) else {
        return false;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return false;
    };
    let matches =
        v.get("object_id").and_then(|x| x.as_str()) == Some(object_id.to_string().as_str());
    let unexpired = v
        .get("exp")
        .and_then(|x| x.as_i64())
        .is_some_and(|exp| exp > Utc::now().timestamp());
    matches && unexpired
}

// ------------------------------------------------------------ CSV parse

/// RFC 4180 parse via the `csv` crate. Returns (headers, data rows); when
/// `has_header` is false synthetic `col1..colN` headers are produced so
/// `mapping.columns` can address them.
pub fn parse_csv(
    bytes: &[u8],
    has_header: bool,
) -> Result<(Vec<String>, Vec<Vec<String>>), AppError> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(has_header)
        .flexible(true)
        .from_reader(bytes);
    let headers = if has_header {
        rdr.headers()
            .map_err(|e| AppError::validation(format!("CSV header parse: {e}"), vec![]))?
            .iter()
            .map(|h| h.trim().to_string())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let mut rows = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| AppError::validation(format!("CSV row parse: {e}"), vec![]))?;
        rows.push(rec.iter().map(|c| c.to_string()).collect::<Vec<_>>());
    }
    let headers = if has_header {
        headers
    } else {
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        (1..=width).map(|i| format!("col{i}")).collect()
    };
    Ok((headers, rows))
}
