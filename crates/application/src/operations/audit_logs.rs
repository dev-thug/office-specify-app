//! Audited, access-scoped audit log queries.

use chrono::{DateTime, Utc};
use contracts::operations::{AuditLogEntry, AuditLogFilters};
use contracts::{AppError, Ctx, PrincipalKind, Role, Scope};
use infrastructure::db::Db;
use infrastructure::repo::operations as repo;
use uuid::Uuid;

#[derive(Debug)]
pub struct AuditLogPage {
    pub items: Vec<AuditLogEntry>,
    pub has_more: bool,
    pub cursor_seed: Option<(DateTime<Utc>, Uuid)>,
}

fn visibility(ctx: &Ctx) -> Result<repo::AuditVisibility, AppError> {
    if !ctx.scopes.contains(Scope::OperationsRead) {
        return Err(AppError::new(
            contracts::ErrorCode::ScopeRequired,
            "missing scope operations.read",
        ));
    }
    Ok(match (ctx.principal_kind, ctx.role) {
        (PrincipalKind::Human, Role::Admin) => repo::AuditVisibility::All,
        (PrincipalKind::Human, Role::Manager) => repo::AuditVisibility::BusinessOnly,
        (PrincipalKind::Human, _) => repo::AuditVisibility::OwnActivity(ctx.principal_id),
        (PrincipalKind::Agent, _) => repo::AuditVisibility::AgentRuns {
            principal_id: ctx.principal_id,
        },
        (PrincipalKind::System, _) => {
            return Err(AppError::forbidden(
                "system principals cannot read audit logs",
            ));
        }
    })
}

fn safe_filter(value: &Option<String>, field: &'static str) -> Result<Option<String>, AppError> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim();
    if value.is_empty() || value.len() > 96 {
        return Err(AppError::validation(
            format!("{field} must contain 1 to 96 characters"),
            vec![contracts::Violation {
                field: field.into(),
                code: "out_of_range".into(),
            }],
        ));
    }
    Ok(Some(value.to_string()))
}

/// `GET /audit-logs`. Visibility is selected from the verified context;
/// filters only narrow it. Details are projected through a strict read-time
/// allowlist so legacy values cannot reveal bodies, addresses or tokens.
pub async fn list_audit_logs(
    db: &Db,
    ctx: &Ctx,
    filters: &AuditLogFilters,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<AuditLogPage, AppError> {
    if filters
        .from
        .zip(filters.to)
        .is_some_and(|(from, to)| from >= to)
    {
        return Err(AppError::validation(
            "from must be earlier than to",
            vec![contracts::Violation {
                field: "from".into(),
                code: "invalid_range".into(),
            }],
        ));
    }
    let visibility = visibility(ctx)?;
    let filter = repo::AuditLogFilter {
        action: safe_filter(&filters.action, "action")?,
        target_type: safe_filter(&filters.target_type, "target_type")?,
        target_id: filters.target_id,
        actor_principal_id: filters.actor_principal_id,
        occurred_from: filters.from,
        occurred_to: filters.to,
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::audit_page(&mut tx, &filter, &visibility, after, limit + 1).await?;
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let cursor_seed = has_more
        .then(|| rows.last().map(|row| (row.created_at, row.id)))
        .flatten();
    let items = rows
        .into_iter()
        .map(|row| {
            let details = domain::operations::redact_audit_details(&row.details);
            let outcome = details
                .get("outcome")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            AuditLogEntry {
                id: row.id,
                occurred_at: row.occurred_at,
                action: row.action,
                target_type: row.target_type,
                target_id: row.target_id,
                actor_principal_id: row.actor_principal_id,
                requested_by: row.requested_by,
                approved_by: row.approved_by,
                job_id: row.job_id,
                request_id: row.request_id,
                trace_id: row.trace_id.filter(|trace| {
                    (16..=64).contains(&trace.len())
                        && trace.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                }),
                outcome,
                details,
            }
        })
        .collect();
    Ok(AuditLogPage {
        items,
        has_more,
        cursor_seed,
    })
}
