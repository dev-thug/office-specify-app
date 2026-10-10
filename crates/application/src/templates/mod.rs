//! templates module use cases (spec §6 — owned by the module worker).

use chrono::{DateTime, Utc};
use contracts::templates::*;
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use uuid::Uuid;

use infrastructure::audit::{self, AuditEntry};
use infrastructure::crypto;
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::repo::templates as repo;

use crate::authz;

/// `hex::encode` — application has no `hex` dependency; tiny local encoder
/// for the 32-byte content hashes.
fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    let mut out = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Map renderer errors onto stable API codes (B.5 + §8.1).
fn render_err(e: domain::templates::TemplateError) -> AppError {
    use domain::templates::TemplateError as T;
    match e {
        T::MissingRequired(name) => AppError::new(
            ErrorCode::TemplateVariableMissing,
            format!("required template variable '{name}' has no value"),
        )
        .with_violations(vec![Violation {
            field: format!("variables.{name}"),
            code: "TEMPLATE_VARIABLE_MISSING".into(),
        }]),
        other => AppError::validation(
            other.to_string(),
            vec![Violation {
                field: "variables".into(),
                code: "TEMPLATE_RENDER".into(),
            }],
        ),
    }
}

fn schema_err(e: domain::templates::TemplateError) -> AppError {
    AppError::validation(
        e.to_string(),
        vec![Violation {
            field: "variable_schema".into(),
            code: "SCHEMA_INVALID".into(),
        }],
    )
}

pub fn to_response(r: repo::TemplateRow) -> EmailTemplateResponse {
    EmailTemplateResponse {
        template_id: r.id,
        name: r.name,
        created_by: r.created_by,
        archived_at: r.archived_at,
        version: r.version,
        created_at: r.created_at,
        updated_at: r.created_at,
    }
}

// ---------------------------------------------------------------------------
// email_templates
// ---------------------------------------------------------------------------

/// Cursor page of logical templates (archived hidden unless requested).
pub async fn list(
    db: &Db,
    ctx: &Ctx,
    include_archived: bool,
    limit: i64,
    after: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<EmailTemplateListItem>, AppError> {
    authz::require_scope(ctx, Scope::TemplatesRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_templates(&mut tx, include_archived, limit + 1, after).await?;
    let ids: Vec<_> = rows.iter().map(|row| row.id).collect();
    let latest = repo::latest_version_numbers(&mut tx, &ids).await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|r| EmailTemplateListItem {
            template_id: r.id,
            latest_version_no: latest.get(&r.id).copied(),
            name: r.name,
            archived_at: r.archived_at,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn create(
    db: &Db,
    ctx: &Ctx,
    req: &CreateEmailTemplateRequest,
) -> Result<EmailTemplateResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesWrite)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = create_in_tx(&mut tx, ctx, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-bound form used by MCP's idempotency boundary. The logical
/// template, audit row, outbox event and idempotency response share a commit.
pub async fn create_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateEmailTemplateRequest,
) -> Result<EmailTemplateResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesWrite)?;
    let name = req.name.trim();
    if name.is_empty() || name.chars().count() > 300 {
        return Err(AppError::validation(
            "name must be 1..=300 characters",
            vec![Violation {
                field: "name".into(),
                code: "LENGTH".into(),
            }],
        ));
    }
    let row = repo::create_template(tx, name, ctx.principal_id).await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "template.created",
            "email_template",
            ctx.request_id.clone(),
        )
        .with_target(row.id),
    )
    .await?;
    outbox::emit(
        tx,
        "template.created",
        "email_template",
        row.id,
        row.version,
        serde_json::json!({"template_id": row.id}),
    )
    .await?;
    Ok(to_response(row))
}

pub async fn get(db: &Db, ctx: &Ctx, id: Uuid) -> Result<EmailTemplateResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_template(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("email template not found"))?;
    tx.commit().await?;
    Ok(to_response(row))
}

/// Metadata PATCH (name/archive). Content never changes — new
/// `template_versions` are the only content mutation (§8.1).
pub async fn patch(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &PatchEmailTemplateRequest,
) -> Result<EmailTemplateResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesWrite)?;
    if let Some(n) = &req.name {
        let n = n.trim();
        if n.is_empty() || n.chars().count() > 300 {
            return Err(AppError::validation(
                "name must be 1..=300 characters",
                vec![Violation {
                    field: "name".into(),
                    code: "LENGTH".into(),
                }],
            ));
        }
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(_existing) = repo::get_template_for_update(&mut tx, id).await? else {
        return Err(AppError::not_found("email template not found"));
    };
    match repo::patch_template(
        &mut tx,
        id,
        expected_version,
        req.name.as_deref().map(str::trim),
        req.archived,
    )
    .await?
    {
        Some(row) => {
            audit::record(
                &mut tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "template.updated",
                    "email_template",
                    ctx.request_id.clone(),
                )
                .with_target(id),
            )
            .await?;
            outbox::emit(
                &mut tx,
                "template.updated",
                "email_template",
                id,
                row.version,
                serde_json::json!({"template_id": id, "version": row.version}),
            )
            .await?;
            tx.commit().await?;
            Ok(to_response(row))
        }
        // Either the version moved or nothing changed — reload to tell apart.
        None => {
            let current = repo::get_template(&mut tx, id)
                .await?
                .ok_or_else(|| AppError::not_found("email template not found"))?;
            if current.version != expected_version {
                return Err(AppError::new(
                    ErrorCode::VersionMismatch,
                    "template version changed — reload and retry",
                ));
            }
            // Same version, no-op patch → idempotent success.
            tx.commit().await?;
            Ok(to_response(current))
        }
    }
}

// ---------------------------------------------------------------------------
// template_versions
// ---------------------------------------------------------------------------

pub async fn list_versions(
    db: &Db,
    ctx: &Ctx,
    template_id: Uuid,
    limit: i64,
    after_version_no: Option<i32>,
) -> Result<Vec<TemplateVersionListItem>, AppError> {
    authz::require_scope(ctx, Scope::TemplatesRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    if repo::get_template(&mut tx, template_id).await?.is_none() {
        return Err(AppError::not_found("email template not found"));
    }
    let rows = repo::list_versions(&mut tx, template_id, limit + 1, after_version_no).await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|r| TemplateVersionListItem {
            template_version_id: r.id,
            version_no: i64::from(r.version_no),
            content_hash: hex_encode(r.content_hash),
            created_at: r.created_at,
        })
        .collect())
}

/// Create an immutable content version (B.3.5). The content hash is
/// `sha256(subject‖body_text‖body_html‖canonical schema)`.
pub async fn create_version(
    db: &Db,
    ctx: &Ctx,
    template_id: Uuid,
    req: &CreateTemplateVersionRequest,
) -> Result<CreateTemplateVersionResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesWrite)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = create_version_in_tx(&mut tx, ctx, template_id, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-bound form used by MCP's idempotency boundary. The template
/// version, audit row, outbox event and idempotency response share the same
/// commit.
pub async fn create_version_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    template_id: Uuid,
    req: &CreateTemplateVersionRequest,
) -> Result<CreateTemplateVersionResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesWrite)?;

    // ---- content validation (§8.1) — happens before the tx opens so a
    // schema error costs nothing.
    if req.subject.trim().is_empty() {
        return Err(AppError::validation(
            "subject must not be empty",
            vec![Violation {
                field: "subject".into(),
                code: "REQUIRED".into(),
            }],
        ));
    }
    if domain::templates::contains_header_break(&req.subject) {
        return Err(AppError::validation(
            "subject must not contain CR/LF (header injection)",
            vec![Violation {
                field: "subject".into(),
                code: "HEADER_BREAK".into(),
            }],
        ));
    }
    if req.body_text.trim().is_empty() {
        return Err(AppError::validation(
            "body_text must not be empty",
            vec![Violation {
                field: "body_text".into(),
                code: "REQUIRED".into(),
            }],
        ));
    }
    if let Some(h) = &req.body_html
        && domain::templates::is_dangerous_html(h)
    {
        return Err(AppError::validation(
            "body_html contains disallowed constructs",
            vec![Violation {
                field: "body_html".into(),
                code: "DANGEROUS_HTML".into(),
            }],
        ));
    }
    let specs =
        domain::templates::parse_variable_schema(&req.variable_schema).map_err(schema_err)?;

    // Validate that every placeholder used is declared — render the
    // template once with inert sample values (failures surface as 422 now
    // rather than at send time).
    let mut samples = serde_json::Map::new();
    for s in &specs {
        if s.required && !domain::templates::SYSTEM_VARIABLES.contains(&s.name.as_str()) {
            samples.insert(
                s.name.clone(),
                serde_json::Value::String(if s.format_uri {
                    "https://example.invalid/x".to_string()
                } else {
                    "x".to_string()
                }),
            );
        }
    }
    let mut sys = std::collections::BTreeMap::new();
    sys.insert(
        "compliance.unsubscribe_url".to_string(),
        domain::templates::PREVIEW_UNSUBSCRIBE_URL.to_string(),
    );
    sys.insert(
        "meeting.booking_url".to_string(),
        domain::templates::PREVIEW_BOOKING_URL.to_string(),
    );
    domain::templates::render(
        &specs,
        &req.subject,
        &req.body_text,
        req.body_html.as_deref(),
        &samples,
        &sys,
    )
    .map_err(render_err)?;

    let content_hash = crypto::sha256_bytes(&domain::templates::canonical_fields([
        req.subject.as_str(),
        req.body_text.as_str(),
        req.body_html.as_deref().unwrap_or(""),
        &serde_json::to_string(&req.variable_schema).unwrap_or_default(),
    ]));

    let Some(tpl) = repo::get_template_for_update(tx, template_id).await? else {
        return Err(AppError::not_found("email template not found"));
    };
    if tpl.archived_at.is_some() {
        return Err(AppError::conflict(
            "template is archived — unarchive or create a new template",
        ));
    }
    let version_no = repo::next_version_no(tx, template_id).await?;
    let row = repo::insert_version(
        tx,
        template_id,
        version_no,
        req.subject.trim(),
        &req.body_text,
        req.body_html.as_deref(),
        &req.variable_schema,
        &content_hash,
        ctx.principal_id,
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "template.version.created",
            "template_version",
            ctx.request_id.clone(),
        )
        .with_target(row.id),
    )
    .await?;
    outbox::emit(
        tx,
        "template.version.created",
        "template_version",
        row.id,
        i64::from(row.version_no),
        serde_json::json!({
            "template_id": template_id,
            "template_version_id": row.id,
            "version_no": row.version_no,
            "content_hash": hex_encode(content_hash),
        }),
    )
    .await?;
    Ok(CreateTemplateVersionResponse {
        template_version_id: row.id,
        version_no: i64::from(row.version_no),
        content_hash: hex_encode(content_hash),
    })
}

/// `POST /template-versions/{id}/render` — allowlisted preview render.
/// System variables get deterministic preview values; caller-supplied
/// system variables are rejected inside `render`.
pub async fn render_version(
    db: &Db,
    ctx: &Ctx,
    version_id: Uuid,
    req: &RenderTemplateVersionRequest,
) -> Result<RenderTemplateVersionResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesRead)?;
    // "compose" permission: humans need a compose grant on at least one
    // mailbox; agents need the mail.compose scope (§13.2).
    match ctx.principal_kind {
        contracts::PrincipalKind::Agent => authz::require_scope(ctx, Scope::MailCompose)?,
        _ => {
            let mut tx = db.begin(ctx.workspace_id).await?;
            let any: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM mail_account_grants
                  WHERE workspace_id = $1 AND principal_id = $2 AND can_compose
                  LIMIT 1",
            )
            .bind(ctx.workspace_id)
            .bind(ctx.principal_id)
            .fetch_optional(tx.conn())
            .await
            .map_err(infrastructure::error::map_sqlx)?;
            tx.commit().await?;
            if any.is_none() {
                return Err(AppError::new(
                    ErrorCode::MailboxAccessDenied,
                    "render requires a compose grant",
                ));
            }
        }
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(v) = repo::get_version(&mut tx, version_id).await? else {
        return Err(AppError::not_found("template version not found"));
    };
    tx.commit().await?;

    let specs = domain::templates::parse_variable_schema(&v.variable_schema).map_err(schema_err)?;
    let mut sys = std::collections::BTreeMap::new();
    sys.insert(
        "compliance.unsubscribe_url".to_string(),
        domain::templates::PREVIEW_UNSUBSCRIBE_URL.to_string(),
    );
    sys.insert(
        "meeting.booking_url".to_string(),
        domain::templates::PREVIEW_BOOKING_URL.to_string(),
    );
    let out = domain::templates::render(
        &specs,
        &v.subject,
        &v.body_text,
        v.body_html.as_deref(),
        &req.variables,
        &sys,
    )
    .map_err(render_err)?;
    Ok(RenderTemplateVersionResponse {
        subject: out.subject,
        body_text: out.body_text,
        body_html: out.body_html,
        warnings: out.warnings,
    })
}

/// Read immutable version content under the same scope and workspace boundary
/// as template metadata and preview rendering.
pub async fn get_version(
    db: &Db,
    ctx: &Ctx,
    version_id: Uuid,
) -> Result<TemplateVersionResponse, AppError> {
    authz::require_scope(ctx, Scope::TemplatesRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = repo::get_version(&mut tx, version_id)
        .await?
        .ok_or_else(|| AppError::not_found("template version not found"))?;
    tx.commit().await?;
    Ok(TemplateVersionResponse {
        template_version_id: row.id,
        template_id: row.template_id,
        version_no: i64::from(row.version_no),
        subject: row.subject,
        body_text: row.body_text,
        body_html: row.body_html,
        variable_schema: row.variable_schema,
        content_hash: hex_encode(row.content_hash),
        created_by: row.created_by,
        created_at: row.created_at,
    })
}
