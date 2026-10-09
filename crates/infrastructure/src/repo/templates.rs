//! templates module repository (spec §6 — owned by the module worker).
//!
//! Tables: `email_templates` (logical template) + `template_versions`
//! (immutable content). Versions are insert-only — no UPDATE exists here by
//! design (§8.1 immutability).

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TemplateRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub created_by: Uuid,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TemplateVersionRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub template_id: Uuid,
    pub version_no: i32,
    pub subject: String,
    pub body_text: String,
    pub body_html: Option<String>,
    pub variable_schema: serde_json::Value,
    pub content_hash: Vec<u8>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

const TEMPLATE_COLS: &str = "id, workspace_id, name, created_by, archived_at, version, created_at";

/// Cursor: (created_at DESC, id DESC) after-key.
pub async fn list_templates(
    tx: &mut Tx<'_>,
    include_archived: bool,
    limit: i64,
    after: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<TemplateRow>, AppError> {
    sqlx::query_as::<_, TemplateRow>(&format!(
        "SELECT {TEMPLATE_COLS} FROM email_templates
          WHERE workspace_id = $1
            AND ($2::bool OR archived_at IS NULL)
            AND ($3::timestamptz IS NULL
                 OR (created_at, id) < ($3, $4))
          ORDER BY created_at DESC, id DESC
          LIMIT $5"
    ))
    .bind(tx.workspace_id())
    .bind(include_archived)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1))
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_template(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<TemplateRow>, AppError> {
    sqlx::query_as::<_, TemplateRow>(&format!(
        "SELECT {TEMPLATE_COLS} FROM email_templates
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// `FOR UPDATE` — used by version creation (serializes version_no).
pub async fn get_template_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<TemplateRow>, AppError> {
    sqlx::query_as::<_, TemplateRow>(&format!(
        "SELECT {TEMPLATE_COLS} FROM email_templates
          WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn create_template(
    tx: &mut Tx<'_>,
    name: &str,
    created_by: Uuid,
) -> Result<TemplateRow, AppError> {
    sqlx::query_as::<_, TemplateRow>(&format!(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, $2, $3)
         RETURNING {TEMPLATE_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(name)
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Patch metadata (`name`, `archived`). Version bumps only when something
/// changed; returns the current row.
pub async fn patch_template(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    name: Option<&str>,
    archived: Option<bool>,
) -> Result<Option<TemplateRow>, AppError> {
    sqlx::query_as::<_, TemplateRow>(&format!(
        "UPDATE email_templates
            SET name = COALESCE($4, name),
                archived_at = CASE $5
                                  WHEN true THEN COALESCE(archived_at, now())
                                  WHEN false THEN NULL
                                  ELSE archived_at
                              END,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $3
            AND (($4::text IS NOT NULL AND name IS DISTINCT FROM $4)
                 OR ($5::bool IS NOT NULL AND (archived_at IS NULL) = $5))
         RETURNING {TEMPLATE_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .bind(name)
    .bind(archived)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

const VERSION_COLS: &str = "id, workspace_id, template_id, version_no, subject, body_text, body_html, \
     variable_schema, content_hash, created_by, created_at";

pub async fn list_versions(
    tx: &mut Tx<'_>,
    template_id: Uuid,
    limit: i64,
    after_version_no: Option<i32>,
) -> Result<Vec<TemplateVersionRow>, AppError> {
    sqlx::query_as::<_, TemplateVersionRow>(&format!(
        "SELECT {VERSION_COLS} FROM template_versions
          WHERE workspace_id = $1 AND template_id = $2
            AND ($3::int IS NULL OR version_no < $3)
          ORDER BY version_no DESC
          LIMIT $4"
    ))
    .bind(tx.workspace_id())
    .bind(template_id)
    .bind(after_version_no)
    .bind(limit)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_version(
    tx: &mut Tx<'_>,
    version_id: Uuid,
) -> Result<Option<TemplateVersionRow>, AppError> {
    sqlx::query_as::<_, TemplateVersionRow>(&format!(
        "SELECT {VERSION_COLS} FROM template_versions
          WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(version_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Latest `version_no` under the template row lock (caller holds
/// `get_template_for_update`), so concurrent creates serialize.
pub async fn next_version_no(tx: &mut Tx<'_>, template_id: Uuid) -> Result<i32, AppError> {
    let n: Option<i32> = sqlx::query_scalar(
        "SELECT max(version_no) FROM template_versions
          WHERE workspace_id = $1 AND template_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(template_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n.unwrap_or(0) + 1)
}

/// Insert-only by design — template versions are immutable (§8.1).
#[allow(clippy::too_many_arguments)]
pub async fn insert_version(
    tx: &mut Tx<'_>,
    template_id: Uuid,
    version_no: i32,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
    variable_schema: &serde_json::Value,
    content_hash: &[u8; 32],
    created_by: Uuid,
) -> Result<TemplateVersionRow, AppError> {
    sqlx::query_as::<_, TemplateVersionRow>(&format!(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text,
             body_html, variable_schema, content_hash, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING {VERSION_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(template_id)
    .bind(version_no)
    .bind(subject)
    .bind(body_text)
    .bind(body_html)
    .bind(variable_schema)
    .bind(&content_hash[..])
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}
