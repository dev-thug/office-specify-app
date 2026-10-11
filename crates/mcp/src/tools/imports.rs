//! imports module MCP tools. Mirrors the authenticated REST handlers in
//! `crates/api/src/routes/imports.rs` (imports pipeline and exports). The raw
//! byte upload/download routes are not exposed; `imports_create_from_csv`
//! instead runs upload-request → store → create import for inline CSV text.

use std::collections::BTreeMap;

use contracts::AppError;
use contracts::imports::{
    CommitImportRequest, CreateExportRequest, CreateImportRequest, CreateUploadRequest,
    ExportFilters, ImportMapping, ImportStatus, JobAcceptedResponse,
};
use infrastructure::idempotency::{self, Stored};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::operations::{
    PageOut, decode_keyset, encode_keyset, idempotency_scope, object_output, page_limit,
    page_output, request_hash, schema,
};
use crate::{ToolDef, arguments, result, tool_with_db, tool_with_services};

const IMPORT_DESCRIPTION: &str = "Import/export job: id, kind (import|export), stored_object_id, requested_by, status, validation_hash (needed for commit), summary, download_url (completed exports), version, created_at.";
const ACCEPTED_DESCRIPTION: &str = "Queued job: import_id (poll with imports_get) and status.";

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
            "imports_create",
            "Queue validation of a CSV that was already uploaded as a stored object (POST /imports). Requires crm.import. Provide stored_object_id and a column mapping; poll imports_get until ready, then imports_commit with its validation_hash.",
            schema::<CreateImportInput>(),
            object_output(ACCEPTED_DESCRIPTION),
            false,
            |db, ctx, args| async move {
                let input = arguments::<CreateImportInput>(args)?;
                let req = CreateImportRequest {
                    stored_object_id: input.stored_object_id,
                    mapping: input.mapping.into_contract()?,
                };
                let hash = request_hash(&req)?;
                let scope = idempotency_scope(&ctx, "imports_create", input.idempotency_key)?;
                let body = idempotency::run(&db, &scope, &hash, move |mut tx| async move {
                    let body =
                        application::imports::query::create_import(&mut tx, &ctx, &req).await?;
                    Ok((tx, Stored { status: 202, body }))
                })
                .await?
                .into_inner()
                .body;
                result::<JobAcceptedResponse>(body)
            },
        ),
        tool_with_services(
            "imports_create_from_csv",
            "Upload inline CSV text (UTF-8, total MCP request under 1MB) as a stored object and queue its import validation in one step. Requires crm.import. Provide csv_text and a column mapping; poll imports_get until ready, then imports_commit. Retry with the same idempotency_key and identical arguments.",
            schema::<CreateFromCsvInput>(),
            json!({
                "type": "object",
                "required": ["stored_object_id", "import_id", "status"],
                "properties": {
                    "stored_object_id": {"type": "string", "format": "uuid"},
                    "import_id": {"type": "string", "format": "uuid"},
                    "status": {"type": "string"}
                }
            }),
            false,
            |services, ctx, args| async move {
                let input = arguments::<CreateFromCsvInput>(args)?;
                let mapping = input.mapping.into_contract()?;
                let filename = input.filename.unwrap_or_else(|| "import.csv".into());
                let hash = request_hash(&(&filename, &input.csv_text, &mapping))?;
                let scope =
                    idempotency_scope(&ctx, "imports_create_from_csv", input.idempotency_key)?;
                let upload_db = services.db.clone();
                let config = services.config.clone();
                let csv_text = input.csv_text;
                let body =
                    idempotency::run(&services.db, &scope, &hash, move |mut tx| async move {
                        // Same use cases as POST /imports/upload-requests and
                        // PUT /imports/uploads/{key}; only reached on first use
                        // of the idempotency key, so replays store nothing.
                        let upload = application::imports::upload::request_upload(
                            &ctx,
                            &config,
                            &CreateUploadRequest {
                                filename,
                                media_type: "text/csv".into(),
                                byte_size: csv_text.len() as i64,
                            },
                        )?;
                        let stored = application::imports::upload::complete_upload(
                            &upload_db,
                            &ctx,
                            &config,
                            &upload.storage_key,
                            &upload.upload_token,
                            csv_text.as_bytes(),
                        )
                        .await?;
                        let accepted = application::imports::query::create_import(
                            &mut tx,
                            &ctx,
                            &CreateImportRequest {
                                stored_object_id: stored.object_id,
                                mapping,
                            },
                        )
                        .await?;
                        Ok((
                            tx,
                            Stored {
                                status: 202,
                                body: CreateFromCsvOutput {
                                    stored_object_id: stored.object_id,
                                    import_id: accepted.import_id,
                                    status: accepted.status,
                                },
                            },
                        ))
                    })
                    .await?
                    .into_inner()
                    .body;
                result(body)
            },
        ),
        tool_with_services(
            "imports_list",
            "List import and export jobs visible to the caller, newest first. Requires crm.import or crm.export. Optional import_status filter; follow next_cursor with identical filters.",
            schema::<ListImportsInput>(),
            page_output(IMPORT_DESCRIPTION),
            false,
            |services, ctx, args| async move {
                let input = arguments::<ListImportsInput>(args)?;
                let limit = page_limit(input.limit)?;
                let filters = json!({
                    "endpoint": "GET /api/v1/imports",
                    "status": input.import_status,
                });
                let after = decode_keyset(
                    &services.config.hmac_key,
                    &ctx,
                    input.cursor.as_deref(),
                    &filters,
                )?;
                let (items, next) = application::imports::query::list_imports(
                    &services.db,
                    &ctx,
                    &services.config,
                    input.import_status.as_deref(),
                    after,
                    i64::from(limit),
                )
                .await?;
                result(PageOut {
                    items,
                    next_cursor: encode_keyset(&services.config.hmac_key, &ctx, &filters, next),
                    has_more: next.is_some(),
                })
            },
        ),
        tool_with_services(
            "imports_get",
            "Read one import/export job: status, validation summary, validation_hash for commit and, for completed exports, a short-lived download_url. Requires crm.import or crm.export. No writes.",
            schema::<ImportIdInput>(),
            object_output(IMPORT_DESCRIPTION),
            false,
            |services, ctx, args| async move {
                let input = arguments::<ImportIdInput>(args)?;
                result(
                    application::imports::query::get_import(
                        &services.db,
                        &ctx,
                        &services.config,
                        input.import_id,
                    )
                    .await?,
                )
            },
        ),
        tool_with_db(
            "imports_list_rows",
            "List validated rows of an import with their normalized data and errors, ordered by row_no. Optional row_status filter; page with after_row_no set to the last row_no returned while has_more is true.",
            schema::<ListRowsInput>(),
            json!({
                "type": "object",
                "required": ["items", "has_more"],
                "properties": {
                    "items": {"type": "array", "items": {"type": "object", "description": "Import row: row_no, status, normalized_data, errors, company_id, contact_id."}},
                    "next_after_row_no": {"type": ["integer", "null"]},
                    "has_more": {"type": "boolean"}
                }
            }),
            false,
            |db, ctx, args| async move {
                let input = arguments::<ListRowsInput>(args)?;
                let limit = page_limit(input.limit)?;
                let (items, has_more) = application::imports::query::list_rows(
                    &db,
                    &ctx,
                    input.import_id,
                    input.row_status.as_deref(),
                    input.after_row_no.unwrap_or(0),
                    i64::from(limit),
                )
                .await?;
                let next_after_row_no = items.last().map(|row| row.row_no).filter(|_| has_more);
                result(json!({
                    "items": items,
                    "next_after_row_no": next_after_row_no,
                    "has_more": has_more,
                }))
            },
        ),
        tool_with_db(
            "imports_commit",
            "Queue the commit of a validated import into the CRM (POST /imports/{id}/commit). Requires crm.import, the validation_hash from imports_get, and invalid_row_policy skip|reject. Retry with the same idempotency_key.",
            schema::<CommitImportInput>(),
            object_output(ACCEPTED_DESCRIPTION),
            false,
            |db, ctx, args| async move {
                let input = arguments::<CommitImportInput>(args)?;
                let import_id = input.import_id;
                let req = CommitImportRequest {
                    validation_hash: input.validation_hash,
                    invalid_row_policy: serde_json::from_value(Value::String(
                        input.invalid_row_policy,
                    ))
                    .map_err(|_| {
                        AppError::validation(
                            "invalid_row_policy must be 'skip' or 'reject'",
                            vec![],
                        )
                    })?,
                };
                let hash = request_hash(&json!({"id": import_id, "body": &req}))?;
                let scope = idempotency_scope(&ctx, "imports_commit", input.idempotency_key)?;
                let body = idempotency::run(&db, &scope, &hash, move |mut tx| async move {
                    let body = application::imports::commit::request_commit(
                        &mut tx, &ctx, import_id, &req,
                    )
                    .await?;
                    Ok((tx, Stored { status: 202, body }))
                })
                .await?
                .into_inner()
                .body;
                result::<JobAcceptedResponse>(body)
            },
        ),
        tool_with_services(
            "exports_create",
            "Queue a CRM CSV export (POST /exports). Requires crm.export. Choose fields from the export allowlist and optional company_ids/lifecycle_stage/q filters; poll imports_get for the download_url. Retry with the same idempotency_key.",
            schema::<CreateExportInput>(),
            object_output(ACCEPTED_DESCRIPTION),
            false,
            |services, ctx, args| async move {
                let input = arguments::<CreateExportInput>(args)?;
                let filters = if input.company_ids.is_none()
                    && input.lifecycle_stage.is_none()
                    && input.q.is_none()
                {
                    None
                } else {
                    Some(ExportFilters {
                        company_ids: input.company_ids,
                        lifecycle_stage: input.lifecycle_stage,
                        q: input.q,
                    })
                };
                let req = CreateExportRequest {
                    fields: input.fields,
                    filters,
                };
                let store = application::imports::object_store(&services.config)?;
                let hash = request_hash(&req)?;
                let scope = idempotency_scope(&ctx, "exports_create", input.idempotency_key)?;
                let body =
                    idempotency::run(&services.db, &scope, &hash, move |mut tx| async move {
                        let body = application::imports::query::request_export(
                            &mut tx, &ctx, &store, &req,
                        )
                        .await?;
                        Ok((tx, Stored { status: 202, body }))
                    })
                    .await?
                    .into_inner()
                    .body;
                result::<JobAcceptedResponse>(body)
            },
        ),
    ]
}

#[derive(Serialize, Deserialize)]
struct CreateFromCsvOutput {
    stored_object_id: Uuid,
    import_id: Uuid,
    status: ImportStatus,
}

/// Column mapping mirrored from `contracts::imports::ImportMapping`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MappingInput {
    /// Map of CSV header (or col1, col2, ... when has_header is false) to canonical field:
    /// company.name, company.domain, company.industry, company.country_code, company.website_url,
    /// contact.name, contact.job_title, contact.department, contact.email, contact.email_label.
    columns: BTreeMap<String, String>,
    /// Whether the first CSV row is a header row (default true).
    #[serde(default)]
    has_header: Option<bool>,
    /// Duplicate handling on commit: `review` (default, duplicates are skipped for a human) or
    /// `link` (attach to the existing company/email instead of creating a new one).
    #[serde(default)]
    duplicate_strategy: Option<String>,
}

impl MappingInput {
    fn into_contract(self) -> Result<ImportMapping, AppError> {
        let mut value = json!({ "columns": self.columns });
        if let Some(has_header) = self.has_header {
            value["has_header"] = json!(has_header);
        }
        if let Some(strategy) = self.duplicate_strategy {
            value["duplicate_strategy"] = json!(strategy);
        }
        serde_json::from_value(value)
            .map_err(|error| AppError::validation(format!("invalid mapping: {error}"), vec![]))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateImportInput {
    /// UUID of an already uploaded CSV stored object (from the REST upload flow).
    stored_object_id: Uuid,
    /// Column mapping object: columns (CSV header → canonical field), has_header, duplicate_strategy.
    mapping: MappingInput,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateFromCsvInput {
    /// Full CSV file content as UTF-8 text (header row first when mapping.has_header is true).
    csv_text: String,
    /// Optional display filename recorded with the stored object (default import.csv).
    #[serde(default)]
    filename: Option<String>,
    /// Column mapping object: columns (CSV header → canonical field), has_header, duplicate_strategy.
    mapping: MappingInput,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListImportsInput {
    /// Optional job status filter: uploaded, validating, ready, committing, completed, failed or canceled.
    #[serde(default)]
    import_status: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ImportIdInput {
    /// Import/export job UUID returned by imports_create, imports_create_from_csv, exports_create or imports_list.
    import_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListRowsInput {
    /// Import job UUID returned by imports_create, imports_create_from_csv or imports_list.
    import_id: Uuid,
    /// Optional row status filter: valid, invalid, duplicate, imported or skipped.
    #[serde(default)]
    row_status: Option<String>,
    /// Return rows strictly after this row_no; use next_after_row_no from the previous page.
    #[serde(default)]
    after_row_no: Option<i32>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommitImportInput {
    /// Import job UUID in `ready` status (see imports_get).
    import_id: Uuid,
    /// Hex SHA-256 validation_hash returned by imports_get; pins the validated snapshot.
    validation_hash: String,
    /// How to treat rows still invalid: `skip` them or `reject` the whole commit.
    invalid_row_policy: String,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateExportInput {
    /// Non-empty list of export fields: company.name, company.domain, company.industry,
    /// company.country_code, company.website_url, company.lifecycle_stage, contact.name,
    /// contact.job_title, contact.department, contact.status, contact.email, contact.email_label.
    fields: Vec<String>,
    /// Optional list of company UUIDs (from crm_search_companies) to restrict the export to.
    #[serde(default)]
    company_ids: Option<Vec<Uuid>>,
    /// Optional companies.lifecycle_stage value to restrict the export to.
    #[serde(default)]
    lifecycle_stage: Option<String>,
    /// Optional case-insensitive substring match on company name or domain.
    #[serde(default)]
    q: Option<String>,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}
