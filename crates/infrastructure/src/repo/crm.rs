//! crm module repository (spec §6 — owned by the module worker).
//!
//! Tables: companies, contacts, email_addresses, contact_emails, notes,
//! tasks, activities (module-map §crm). All functions run inside the
//! caller's workspace `Tx` (RLS); optimistic locking follows A.6.1 —
//! `UPDATE … WHERE workspace_id AND id AND version`, 0 rows ⇒ caller
//! distinguishes 404 vs 412.
//!
//! §5.6 activity ACL is implemented locally in SQL: rows carrying a
//! `mail_account_id` are visible only when `mail_account_grants` gives the
//! requesting principal `can_read`.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

// ------------------------------------------------------------------ rows

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CompanyRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub domain: Option<String>,
    pub website_url: Option<String>,
    pub industry: Option<String>,
    pub country_code: Option<String>,
    pub lifecycle_stage: String,
    pub owner_principal_id: Option<Uuid>,
    pub description: Option<String>,
    pub custom_fields: serde_json::Value,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

const COMPANY_COLS: &str = "id, workspace_id, name, domain, website_url, industry,
    country_code, lifecycle_stage, owner_principal_id, description, custom_fields,
    archived_at, version, updated_at, created_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ContactRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub owner_principal_id: Option<Uuid>,
    pub status: String,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

const CONTACT_COLS: &str = "id, workspace_id, company_id, name, job_title, department,
    owner_principal_id, status, archived_at, version, updated_at, created_at";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ContactEmailRow {
    pub email_address_id: Uuid,
    pub contact_id: Uuid,
    pub address_original: String,
    pub label: Option<String>,
    pub is_primary: bool,
    pub verification_status: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NoteRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub mail_account_id: Option<Uuid>,
    pub created_by: Uuid,
    pub body: String,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub title: String,
    pub owner_principal_id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub status: String,
    pub due_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ActivityRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub mail_account_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
    pub kind: String,
    pub source_event_id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub summary: serde_json::Value,
}

// ------------------------------------------------------------- principals

/// Principal lookup for owner assignment: must exist, be a human and be
/// active in this workspace.
pub async fn principal_is_active_human(tx: &mut Tx<'_>, id: Uuid) -> Result<bool, AppError> {
    let n: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM principals
          WHERE workspace_id = $1 AND id = $2 AND kind = 'human' AND status = 'active'",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n.is_some())
}

/// `workspaces.settings` object (drives the local-part policy, §6.3).
pub async fn workspace_settings(tx: &mut Tx<'_>) -> Result<Option<serde_json::Value>, AppError> {
    sqlx::query_scalar("SELECT settings FROM workspaces WHERE id = $1")
        .bind(tx.workspace_id())
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
}

// --------------------------------------------------------------- companies

#[derive(Debug, Default)]
pub struct CompanyFilter {
    /// ILIKE match on name/domain (pre-escaped by the caller).
    pub q: Option<String>,
    pub lifecycle_stage: Option<String>,
    pub owner_principal_id: Option<Uuid>,
    /// false → `archived_at IS NULL` (default), true → archived only.
    pub archived: bool,
}

pub async fn insert_company(tx: &mut Tx<'_>, c: &CompanyInsert) -> Result<CompanyRow, AppError> {
    sqlx::query_as::<_, CompanyRow>(
        "INSERT INTO companies
            (workspace_id, name, domain, website_url, industry, country_code,
             lifecycle_stage, owner_principal_id, description, custom_fields)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
         RETURNING *",
    )
    .bind(tx.workspace_id())
    .bind(&c.name)
    .bind(&c.domain)
    .bind(&c.website_url)
    .bind(&c.industry)
    .bind(&c.country_code)
    .bind(&c.lifecycle_stage)
    .bind(c.owner_principal_id)
    .bind(&c.description)
    .bind(&c.custom_fields)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Default)]
pub struct CompanyInsert {
    pub name: String,
    pub domain: Option<String>,
    pub website_url: Option<String>,
    pub industry: Option<String>,
    pub country_code: Option<String>,
    pub lifecycle_stage: String,
    pub owner_principal_id: Option<Uuid>,
    pub description: Option<String>,
    pub custom_fields: serde_json::Value,
}

pub async fn get_company(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<CompanyRow>, AppError> {
    sqlx::query_as::<_, CompanyRow>(&format!(
        "SELECT {COMPANY_COLS} FROM companies WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `FOR UPDATE` variant — used by merge/patch paths that re-check state.
pub async fn lock_company(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<CompanyRow>, AppError> {
    sqlx::query_as::<_, CompanyRow>(&format!(
        "SELECT {COMPANY_COLS} FROM companies
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Cursor page: `(created_at DESC, id DESC)` keyset (B.1.5). `after` carries
/// the (created_at, id) of the last row of the previous page.
pub async fn list_companies(
    tx: &mut Tx<'_>,
    f: &CompanyFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<Vec<CompanyRow>, AppError> {
    let (after_ts, after_id) = after
        .map(|(t, i)| (Some(t), Some(i)))
        .unwrap_or((None, None));
    sqlx::query_as::<_, CompanyRow>(&format!(
        "SELECT {COMPANY_COLS} FROM companies
          WHERE workspace_id = $1
            AND (($2::bool) = (archived_at IS NOT NULL))
            AND ($3::text IS NULL OR name ILIKE '%' || $3 || '%' ESCAPE '\\'
                              OR domain ILIKE '%' || $3 || '%' ESCAPE '\\')
            AND ($4::text IS NULL OR lifecycle_stage = $4)
            AND ($5::uuid IS NULL OR owner_principal_id = $5)
            AND ($6::timestamptz IS NULL OR (created_at, id) < ($6, $7))
          ORDER BY created_at DESC, id DESC
          LIMIT $8"
    ))
    .bind(tx.workspace_id())
    .bind(f.archived)
    .bind(&f.q)
    .bind(&f.lifecycle_stage)
    .bind(f.owner_principal_id)
    .bind(after_ts)
    .bind(after_id)
    .bind(i64::from(limit))
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// A.6.1 conditional update. Caller already read the row, checked authz and
/// resolved null-vs-absent semantics into `c` (the full new column set).
/// Returns None when `expected_version` is stale (412 territory).
pub async fn update_company_versioned(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    c: &CompanyInsert,
) -> Result<Option<CompanyRow>, AppError> {
    sqlx::query_as::<_, CompanyRow>(&format!(
        "UPDATE companies SET
            name = $3, domain = $4, website_url = $5, industry = $6,
            country_code = $7, lifecycle_stage = $8, description = $9,
            custom_fields = $10, version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $11
          RETURNING {COMPANY_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(&c.name)
    .bind(&c.domain)
    .bind(&c.website_url)
    .bind(&c.industry)
    .bind(&c.country_code)
    .bind(&c.lifecycle_stage)
    .bind(&c.description)
    .bind(&c.custom_fields)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Conditional archive — None on stale version or already archived.
pub async fn archive_company(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
) -> Result<Option<CompanyRow>, AppError> {
    sqlx::query_as::<_, CompanyRow>(&format!(
        "UPDATE companies SET archived_at = now(), version = version + 1,
            updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $3
            AND archived_at IS NULL
          RETURNING {COMPANY_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Duplicate candidates (B.3.3): same normalized domain or same name
/// (case-insensitive), excluding `exclude_id`, non-archived.
pub async fn company_duplicates(
    tx: &mut Tx<'_>,
    exclude_id: Uuid,
    name: &str,
    domain: Option<&str>,
) -> Result<Vec<CompanyRow>, AppError> {
    sqlx::query_as::<_, CompanyRow>(&format!(
        "SELECT {COMPANY_COLS} FROM companies
          WHERE workspace_id = $1 AND id <> $2 AND archived_at IS NULL
            AND (($3::text IS NOT NULL AND domain IS NOT NULL AND domain = $3)
              OR lower(name) = lower($4))
          ORDER BY created_at DESC, id DESC
          LIMIT 10"
    ))
    .bind(tx.workspace_id())
    .bind(exclude_id)
    .bind(domain)
    .bind(name)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Re-point every shared-table `company_id` reference from `source` to
/// `target` (merge job). Returns per-table moved counts for audit metadata.
pub async fn repoint_company_refs(
    tx: &mut Tx<'_>,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<BTreeMap<String, u64>, AppError> {
    let mut moved = BTreeMap::new();
    // (table name used in audit metadata, UPDATE statement). Table names are
    // compile-time constants — never user input.
    let stmts: &[(&str, &str)] = &[
        (
            "contacts",
            "UPDATE contacts SET company_id = $3, updated_at = now()
              WHERE workspace_id = $1 AND company_id = $2",
        ),
        (
            "notes",
            "UPDATE notes SET company_id = $3, updated_at = now()
              WHERE workspace_id = $1 AND company_id = $2",
        ),
        (
            "tasks",
            "UPDATE tasks SET company_id = $3
              WHERE workspace_id = $1 AND company_id = $2",
        ),
        (
            "activities",
            "UPDATE activities SET company_id = $3
              WHERE workspace_id = $1 AND company_id = $2",
        ),
        (
            "research_evidence",
            "UPDATE research_evidence SET company_id = $3
              WHERE workspace_id = $1 AND company_id = $2",
        ),
        (
            "lead_proposals",
            "UPDATE lead_proposals SET accepted_company_id = $3
              WHERE workspace_id = $1 AND accepted_company_id = $2",
        ),
        (
            "import_rows",
            "UPDATE import_rows SET company_id = $3
              WHERE workspace_id = $1 AND company_id = $2",
        ),
    ];
    for (name, sql) in stmts {
        let n = sqlx::query(sql)
            .bind(tx.workspace_id())
            .bind(source_id)
            .bind(target_id)
            .execute(tx.conn())
            .await
            .map_err(map_sqlx)?
            .rows_affected();
        moved.insert((*name).to_string(), n);
    }
    Ok(moved)
}

/// Finalize a merge: write the chosen field values onto `target` (bumping
/// its version) and archive `source`. Caller has locked both rows.
/// `assignee` rewrites target columns the field choices picked from source.
pub async fn merge_finalize(
    tx: &mut Tx<'_>,
    target_id: Uuid,
    expected_version: i64,
    merged: &CompanyInsert,
    owner_principal_id: Option<Uuid>,
    source_id: Uuid,
) -> Result<Option<CompanyRow>, AppError> {
    let row = sqlx::query_as::<_, CompanyRow>(&format!(
        "UPDATE companies SET
            name = $3, domain = $4, website_url = $5, industry = $6,
            country_code = $7, lifecycle_stage = $8, description = $9,
            custom_fields = $10, owner_principal_id = $11,
            version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $12
          RETURNING {COMPANY_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(target_id)
    .bind(&merged.name)
    .bind(&merged.domain)
    .bind(&merged.website_url)
    .bind(&merged.industry)
    .bind(&merged.country_code)
    .bind(&merged.lifecycle_stage)
    .bind(&merged.description)
    .bind(&merged.custom_fields)
    .bind(owner_principal_id)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some(row) = row else { return Ok(None) };
    sqlx::query(
        "UPDATE companies SET archived_at = now(), version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND archived_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(source_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(Some(row))
}

// ---------------------------------------------------------------- contacts

#[derive(Debug, Default)]
pub struct ContactFilter {
    pub q: Option<String>,
    pub company_id: Option<Uuid>,
    pub status: Option<String>,
    pub owner_principal_id: Option<Uuid>,
    pub archived: bool,
}

#[derive(Debug, Default)]
pub struct ContactInsert {
    pub name: String,
    pub company_id: Option<Uuid>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub owner_principal_id: Option<Uuid>,
    pub status: String,
}

pub async fn insert_contact(tx: &mut Tx<'_>, c: &ContactInsert) -> Result<ContactRow, AppError> {
    sqlx::query_as::<_, ContactRow>(
        "INSERT INTO contacts
            (workspace_id, company_id, name, job_title, department, owner_principal_id, status)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         RETURNING *",
    )
    .bind(tx.workspace_id())
    .bind(c.company_id)
    .bind(&c.name)
    .bind(&c.job_title)
    .bind(&c.department)
    .bind(c.owner_principal_id)
    .bind(&c.status)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_contact(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ContactRow>, AppError> {
    sqlx::query_as::<_, ContactRow>(&format!(
        "SELECT {CONTACT_COLS} FROM contacts WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn lock_contact(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<ContactRow>, AppError> {
    sqlx::query_as::<_, ContactRow>(&format!(
        "SELECT {CONTACT_COLS} FROM contacts
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn list_contacts(
    tx: &mut Tx<'_>,
    f: &ContactFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<Vec<ContactRow>, AppError> {
    let (after_ts, after_id) = after
        .map(|(t, i)| (Some(t), Some(i)))
        .unwrap_or((None, None));
    sqlx::query_as::<_, ContactRow>(&format!(
        "SELECT {CONTACT_COLS} FROM contacts
          WHERE workspace_id = $1
            AND (($2::bool) = (archived_at IS NOT NULL))
            AND ($3::text IS NULL OR name ILIKE '%' || $3 || '%' ESCAPE '\\')
            AND ($4::uuid IS NULL OR company_id = $4)
            AND ($5::text IS NULL OR status = $5)
            AND ($6::uuid IS NULL OR owner_principal_id = $6)
            AND ($7::timestamptz IS NULL OR (created_at, id) < ($7, $8))
          ORDER BY created_at DESC, id DESC
          LIMIT $9"
    ))
    .bind(tx.workspace_id())
    .bind(f.archived)
    .bind(&f.q)
    .bind(f.company_id)
    .bind(&f.status)
    .bind(f.owner_principal_id)
    .bind(after_ts)
    .bind(after_id)
    .bind(i64::from(limit))
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// A.6.1 conditional update on the full resolved column set.
pub async fn update_contact_versioned(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    c: &ContactInsert,
) -> Result<Option<ContactRow>, AppError> {
    sqlx::query_as::<_, ContactRow>(&format!(
        "UPDATE contacts SET
            name = $3, company_id = $4, job_title = $5, department = $6,
            status = $7, version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $8
          RETURNING {CONTACT_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(&c.name)
    .bind(c.company_id)
    .bind(&c.job_title)
    .bind(&c.department)
    .bind(&c.status)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// A.6.1 version-guarded bump for If-Match-guarded link mutations — false
/// when the expected version is stale (412 territory).
pub async fn touch_contact_versioned(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE contacts SET version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2 AND version = $3",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Bump a contact's version (email link/unlink/primary changes are
/// contact-level mutations).
pub async fn touch_contact(tx: &mut Tx<'_>, id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE contacts SET version = version + 1, updated_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

// -------------------------------------------------------- email addresses

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EmailAddressRow {
    pub id: Uuid,
    pub address_original: String,
    pub normalized_key: String,
    pub kind: String,
    pub verification_status: String,
}

/// Workspace-wide dedupe identity is `(workspace_id, normalized_key)`
/// (§6.3). Upserts bump `last_seen_at`; the first writer's `address_original`
/// wins. Never writes `verified`/`consented` state (B.3.4).
pub async fn upsert_email_address(
    tx: &mut Tx<'_>,
    address_original: &str,
    normalized_key: &str,
    normalization_version: i16,
    kind: &str,
) -> Result<EmailAddressRow, AppError> {
    sqlx::query_as::<_, EmailAddressRow>(
        "INSERT INTO email_addresses
            (workspace_id, address_original, normalized_key, normalization_version, kind, last_seen_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (workspace_id, normalized_key)
         DO UPDATE SET last_seen_at = now()
         RETURNING id, address_original, normalized_key, kind, verification_status",
    )
    .bind(tx.workspace_id())
    .bind(address_original)
    .bind(normalized_key)
    .bind(normalization_version)
    .bind(kind)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Link `contact_id` ↔ `email_address_id`. Returns `(row_id, created)` —
/// an existing link is reported, not duplicated (B.3.4).
pub async fn link_contact_email(
    tx: &mut Tx<'_>,
    contact_id: Uuid,
    email_address_id: Uuid,
    label: Option<&str>,
    is_primary: bool,
) -> Result<(Uuid, bool), AppError> {
    if is_primary {
        clear_primary_email(tx, contact_id).await?;
    }
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM contact_emails
          WHERE workspace_id = $1 AND contact_id = $2 AND email_address_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if let Some(id) = existing {
        if is_primary {
            set_primary_email(tx, contact_id, email_address_id).await?;
        }
        return Ok((id, false));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO contact_emails
            (workspace_id, contact_id, email_address_id, label, is_primary)
         VALUES ($1,$2,$3,$4,$5)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .bind(email_address_id)
    .bind(label)
    .bind(is_primary)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok((id, true))
}

/// Links of one contact joined to their `email_addresses` rows.
pub async fn contact_emails(
    tx: &mut Tx<'_>,
    contact_id: Uuid,
) -> Result<Vec<ContactEmailRow>, AppError> {
    sqlx::query_as::<_, ContactEmailRow>(
        "SELECT ce.email_address_id, ce.contact_id, ea.address_original,
                ce.label, ce.is_primary, ea.verification_status
           FROM contact_emails ce
           JOIN email_addresses ea
             ON ea.workspace_id = ce.workspace_id AND ea.id = ce.email_address_id
          WHERE ce.workspace_id = $1 AND ce.contact_id = $2
          ORDER BY ce.is_primary DESC, ce.created_at, ce.id",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Links of many contacts (list endpoint) — one query, grouped in Rust.
pub async fn contact_emails_batch(
    tx: &mut Tx<'_>,
    contact_ids: &[Uuid],
) -> Result<Vec<ContactEmailRow>, AppError> {
    sqlx::query_as::<_, ContactEmailRow>(
        "SELECT ce.email_address_id, ce.contact_id, ea.address_original,
                ce.label, ce.is_primary, ea.verification_status
           FROM contact_emails ce
           JOIN email_addresses ea
             ON ea.workspace_id = ce.workspace_id AND ea.id = ce.email_address_id
          WHERE ce.workspace_id = $1 AND ce.contact_id = ANY($2)
          ORDER BY ce.contact_id, ce.is_primary DESC, ce.created_at, ce.id",
    )
    .bind(tx.workspace_id())
    .bind(contact_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Other contacts linked to an address (role-address shared links, §6.3).
pub async fn contacts_for_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Vec<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT contact_id FROM contact_emails
          WHERE workspace_id = $1 AND email_address_id = $2
          ORDER BY created_at, id",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

async fn clear_primary_email(tx: &mut Tx<'_>, contact_id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE contact_emails SET is_primary = false
          WHERE workspace_id = $1 AND contact_id = $2 AND is_primary",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Set the primary link; false when the link does not exist (422
/// territory — B.3.4 lets callers explain the missing link).
pub async fn set_primary_email(
    tx: &mut Tx<'_>,
    contact_id: Uuid,
    email_address_id: Uuid,
) -> Result<bool, AppError> {
    clear_primary_email(tx, contact_id).await?;
    let n = sqlx::query(
        "UPDATE contact_emails SET is_primary = true
          WHERE workspace_id = $1 AND contact_id = $2 AND email_address_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .bind(email_address_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Remove a contact↔address link. False when the link does not exist.
pub async fn unlink_contact_email(
    tx: &mut Tx<'_>,
    contact_id: Uuid,
    email_address_id: Uuid,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "DELETE FROM contact_emails
          WHERE workspace_id = $1 AND contact_id = $2 AND email_address_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(contact_id)
    .bind(email_address_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Marketing send-eligibility preview per address (B.3.4). Reason codes are
/// the B.5 wire codes: `RECIPIENT_SUPPRESSED` for any active suppression
/// covering marketing, `SEND_BASIS_UNVERIFIED` when no valid marketing
/// communication basis exists (any jurisdiction — a display hint, not the
/// §8.4 send decision).
pub async fn marketing_eligibility(
    tx: &mut Tx<'_>,
    email_address_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<String>>, AppError> {
    let mut out: HashMap<Uuid, Vec<String>> = HashMap::new();
    if email_address_ids.is_empty() {
        return Ok(out);
    }
    let suppressed: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT email_address_id FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = ANY($2)
            AND released_at IS NULL AND purpose_scope IN ('marketing','all')",
    )
    .bind(tx.workspace_id())
    .bind(email_address_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let based: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT email_address_id FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = ANY($2)
            AND purpose = 'marketing' AND status = 'valid'
            AND (valid_until IS NULL OR valid_until > now())",
    )
    .bind(tx.workspace_id())
    .bind(email_address_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    for id in email_address_ids {
        let mut codes = Vec::new();
        if suppressed.contains(id) {
            codes.push("RECIPIENT_SUPPRESSED".to_string());
        }
        if !based.contains(id) {
            codes.push("SEND_BASIS_UNVERIFIED".to_string());
        }
        out.insert(*id, codes);
    }
    Ok(out)
}

// ------------------------------------------------------------------- notes

#[derive(Debug)]
pub struct NoteInsert {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub mail_account_id: Option<Uuid>,
    pub created_by: Uuid,
    pub body: String,
}

pub async fn insert_note(tx: &mut Tx<'_>, n: &NoteInsert) -> Result<NoteRow, AppError> {
    sqlx::query_as::<_, NoteRow>(
        "INSERT INTO notes
            (workspace_id, company_id, contact_id, thread_id, mail_account_id, created_by, body)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         RETURNING id, workspace_id, company_id, contact_id, thread_id,
                   mail_account_id, created_by, body, version, updated_at, created_at",
    )
    .bind(tx.workspace_id())
    .bind(n.company_id)
    .bind(n.contact_id)
    .bind(n.thread_id)
    .bind(n.mail_account_id)
    .bind(n.created_by)
    .bind(&n.body)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Notes on a target — `mail_account_id`-carrying rows are filtered by the
/// mailbox grant exactly like activities (§5.6: even existence is hidden).
pub async fn list_notes(
    tx: &mut Tx<'_>,
    company_id: Option<Uuid>,
    contact_id: Option<Uuid>,
    thread_id: Option<Uuid>,
    principal_id: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<Vec<NoteRow>, AppError> {
    let (after_ts, after_id) = after
        .map(|(t, i)| (Some(t), Some(i)))
        .unwrap_or((None, None));
    sqlx::query_as::<_, NoteRow>(
        "SELECT id, workspace_id, company_id, contact_id, thread_id,
                mail_account_id, created_by, body, version, updated_at, created_at
           FROM notes
          WHERE workspace_id = $1
            AND ($2::uuid IS NULL OR company_id = $2)
            AND ($3::uuid IS NULL OR contact_id = $3)
            AND ($4::uuid IS NULL OR thread_id = $4)
            AND (mail_account_id IS NULL OR EXISTS (
                SELECT 1 FROM mail_account_grants g
                 WHERE g.workspace_id = $1 AND g.mail_account_id = notes.mail_account_id
                   AND g.principal_id = $5 AND g.can_read))
            AND ($6::timestamptz IS NULL OR (created_at, id) < ($6, $7))
          ORDER BY created_at DESC, id DESC
          LIMIT $8",
    )
    .bind(tx.workspace_id())
    .bind(company_id)
    .bind(contact_id)
    .bind(thread_id)
    .bind(principal_id)
    .bind(after_ts)
    .bind(after_id)
    .bind(i64::from(limit))
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------------------------- tasks

#[derive(Debug, Default)]
pub struct TaskFilter {
    pub status: Option<String>,
    /// List-scoping resolved by the use case: Some(self) for Member.
    pub owner_principal_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
}

#[derive(Debug)]
pub struct TaskInsert {
    pub title: String,
    pub owner_principal_id: Uuid,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub due_at: Option<DateTime<Utc>>,
}

pub async fn insert_task(tx: &mut Tx<'_>, t: &TaskInsert) -> Result<TaskRow, AppError> {
    sqlx::query_as::<_, TaskRow>(
        "INSERT INTO tasks
            (workspace_id, title, owner_principal_id, company_id, contact_id, thread_id, due_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         RETURNING id, workspace_id, title, owner_principal_id, company_id,
                   contact_id, thread_id, status, due_at, version, created_at",
    )
    .bind(tx.workspace_id())
    .bind(&t.title)
    .bind(t.owner_principal_id)
    .bind(t.company_id)
    .bind(t.contact_id)
    .bind(t.thread_id)
    .bind(t.due_at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_task(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<TaskRow>, AppError> {
    sqlx::query_as::<_, TaskRow>(
        "SELECT id, workspace_id, title, owner_principal_id, company_id,
                contact_id, thread_id, status, due_at, version, created_at
           FROM tasks WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn list_tasks(
    tx: &mut Tx<'_>,
    f: &TaskFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<Vec<TaskRow>, AppError> {
    let (after_ts, after_id) = after
        .map(|(t, i)| (Some(t), Some(i)))
        .unwrap_or((None, None));
    sqlx::query_as::<_, TaskRow>(
        "SELECT id, workspace_id, title, owner_principal_id, company_id,
                contact_id, thread_id, status, due_at, version, created_at
           FROM tasks
          WHERE workspace_id = $1
            AND ($2::text IS NULL OR status = $2)
            AND ($3::uuid IS NULL OR owner_principal_id = $3)
            AND ($4::uuid IS NULL OR company_id = $4)
            AND ($5::uuid IS NULL OR contact_id = $5)
            AND ($6::timestamptz IS NULL OR (created_at, id) < ($6, $7))
          ORDER BY created_at DESC, id DESC
          LIMIT $8",
    )
    .bind(tx.workspace_id())
    .bind(&f.status)
    .bind(f.owner_principal_id)
    .bind(f.company_id)
    .bind(f.contact_id)
    .bind(after_ts)
    .bind(after_id)
    .bind(i64::from(limit))
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// A.6.1 conditional update; `due_at` uses `Option<Option<…>>` semantics —
/// outer Some means write (inner None clears), outer None keeps.
pub async fn update_task_versioned(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    title: &str,
    status: &str,
    due_at: Option<DateTime<Utc>>,
    owner_principal_id: Uuid,
) -> Result<Option<TaskRow>, AppError> {
    sqlx::query_as::<_, TaskRow>(
        "UPDATE tasks SET title = $3, status = $4, due_at = $5,
            owner_principal_id = $6, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $7
          RETURNING id, workspace_id, title, owner_principal_id, company_id,
                    contact_id, thread_id, status, due_at, version, created_at",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(title)
    .bind(status)
    .bind(due_at)
    .bind(owner_principal_id)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// -------------------------------------------------------------- activities

/// Idempotent timeline insert — `UNIQUE (workspace_id, source_event_id,
/// kind)` makes projector re-delivery and job retry no-ops (contract §activities).
/// Returns the new row id, or None when it already existed.
#[allow(clippy::too_many_arguments)]
pub async fn insert_activity(
    tx: &mut Tx<'_>,
    a: &ActivityInsert,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "INSERT INTO activities
            (workspace_id, company_id, contact_id, mail_account_id, thread_id,
             booking_id, actor_principal_id, kind, source_event_id, occurred_at, summary)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
         ON CONFLICT (workspace_id, source_event_id, kind) DO NOTHING
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(a.company_id)
    .bind(a.contact_id)
    .bind(a.mail_account_id)
    .bind(a.thread_id)
    .bind(a.booking_id)
    .bind(a.actor_principal_id)
    .bind(a.kind)
    .bind(a.source_event_id)
    .bind(a.occurred_at)
    .bind(&a.summary)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Default)]
pub struct ActivityInsert {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub mail_account_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub booking_id: Option<Uuid>,
    pub actor_principal_id: Option<Uuid>,
    pub kind: &'static str,
    pub source_event_id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub summary: serde_json::Value,
}

/// Company timeline: company rows plus rows of the company's contacts.
/// §5.6 — mail-derived rows (`mail_account_id` set) are visible only to
/// principals holding `can_read` on that mailbox; no grant hides the row
/// entirely (existence is not revealed either).
pub async fn list_company_activities(
    tx: &mut Tx<'_>,
    company_id: Uuid,
    principal_id: Uuid,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: u32,
) -> Result<Vec<ActivityRow>, AppError> {
    let (after_ts, after_id) = after
        .map(|(t, i)| (Some(t), Some(i)))
        .unwrap_or((None, None));
    sqlx::query_as::<_, ActivityRow>(
        "SELECT id, workspace_id, company_id, contact_id, mail_account_id,
                thread_id, booking_id, actor_principal_id, kind, source_event_id,
                occurred_at, summary
           FROM activities
          WHERE workspace_id = $1
            AND (company_id = $2 OR contact_id IN (
                SELECT id FROM contacts WHERE workspace_id = $1 AND company_id = $2))
            AND (mail_account_id IS NULL OR EXISTS (
                SELECT 1 FROM mail_account_grants g
                 WHERE g.workspace_id = $1 AND g.mail_account_id = activities.mail_account_id
                   AND g.principal_id = $3 AND g.can_read))
            AND ($4::timestamptz IS NULL OR (occurred_at, id) < ($4, $5))
          ORDER BY occurred_at DESC, id DESC
          LIMIT $6",
    )
    .bind(tx.workspace_id())
    .bind(company_id)
    .bind(principal_id)
    .bind(after_ts)
    .bind(after_id)
    .bind(i64::from(limit))
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ----------------------------------------------- projector resolution (§5.6
// ids only — summaries never carry subject/body)

/// First contact (deterministic order) linked to an address + its company.
pub async fn contact_refs_for_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Option<(Uuid, Option<Uuid>)>, AppError> {
    sqlx::query_as::<_, (Uuid, Option<Uuid>)>(
        "SELECT ce.contact_id, c.company_id
           FROM contact_emails ce
           JOIN contacts c ON c.workspace_id = ce.workspace_id AND c.id = ce.contact_id
          WHERE ce.workspace_id = $1 AND ce.email_address_id = $2
          ORDER BY ce.created_at, ce.id
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Outbound snapshot ids needed for the timeline row (never the body).
pub async fn outbound_refs(
    tx: &mut Tx<'_>,
    outbound_message_id: Uuid,
) -> Result<Option<(Uuid, Option<Uuid>, Uuid, Uuid)>, AppError> {
    sqlx::query_as::<_, (Uuid, Option<Uuid>, Uuid, Uuid)>(
        "SELECT mail_account_id, thread_id, to_email_address_id, requested_by
           FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Thread ids needed for a `message.received` row.
pub async fn thread_refs(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
) -> Result<Option<(Uuid, Option<Uuid>)>, AppError> {
    sqlx::query_as::<_, (Uuid, Option<Uuid>)>(
        "SELECT mail_account_id, contact_id FROM email_threads
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(thread_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `approval_decisions.decided_by` for `approval.decided` actor attribution.
pub async fn approval_decided_by(
    tx: &mut Tx<'_>,
    approval_request_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT decided_by FROM approval_decisions
          WHERE workspace_id = $1 AND approval_request_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(approval_request_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Suppression row ids for `suppression.created` attribution.
pub async fn suppression_actor(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose_scope: &str,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT created_by FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2 AND purpose_scope = $3
            AND released_at IS NULL
          ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose_scope)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `lead_proposals` ids for `lead.accepted` (accepted company + reviewer).
pub async fn lead_proposal_refs(
    tx: &mut Tx<'_>,
    lead_proposal_id: Uuid,
) -> Result<Option<(Option<Uuid>, Option<Uuid>)>, AppError> {
    sqlx::query_as::<_, (Option<Uuid>, Option<Uuid>)>(
        "SELECT accepted_company_id, reviewed_by FROM lead_proposals
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(lead_proposal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `bookings` guest address for `booking.confirmed` contact resolution.
pub async fn booking_guest_address(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT guest_email_address_id FROM bookings
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `bookings` ids for `booking.confirmed` attribution + guest resolution:
/// (host_principal_id, guest_email_address_id).
pub async fn booking_refs(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
) -> Result<Option<(Uuid, Uuid)>, AppError> {
    sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT host_principal_id, guest_email_address_id FROM bookings
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `suppressions` ids for a `suppression_release` approval target:
/// (email_address_id, purpose_scope).
pub async fn suppression_target(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
) -> Result<Option<(Uuid, String)>, AppError> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT email_address_id, purpose_scope FROM suppressions
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `email_threads.mail_account_id` for note ACL resolution (notes carry the
/// mailbox id because of the composite thread FK).
pub async fn thread_mail_account(
    tx: &mut Tx<'_>,
    thread_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT mail_account_id FROM email_threads
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(thread_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Enqueue de-dup: find the job previously enqueued for a merge pair so a
/// repeated request returns the same job (idempotent enqueue lands in the
/// foundation rebase; this localizes the behaviour now).
pub async fn find_merge_job(
    tx: &mut Tx<'_>,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<Option<(Uuid, String)>, AppError> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, state FROM jobs
          WHERE workspace_id = $1 AND kind = 'crm.merge_companies'
            AND dedupe_key = $4
          ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(source_id)
    .bind(target_id)
    .bind(format!("merge_companies:{source_id}:{target_id}"))
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Count prior merge jobs for a pair — terminal-state duplicates get a
/// suffixed dedupe key so a re-request is honored (not silently dropped).
pub async fn merge_job_count(
    tx: &mut Tx<'_>,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id = $1 AND kind = 'crm.merge_companies'
            AND dedupe_key LIKE $4",
    )
    .bind(tx.workspace_id())
    .bind(source_id)
    .bind(target_id)
    .bind(format!("merge_companies:{source_id}:{target_id}%"))
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}
