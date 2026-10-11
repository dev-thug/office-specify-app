//! operations module MCP tools (B.6): jobs, audit logs, dashboard summary,
//! operational alerts and the T30 send-workers restore lock. Every tool
//! mirrors the REST handler in `crates/api/src/routes/operations.rs`.

use crate::{ToolDef, arguments, result, tool, tool_with_db, tool_with_services};
use chrono::{DateTime, Utc};
use contracts::operations::{
    AuditLogFilters, DashboardFilters, JobResponse, JobRetryRequest, ReleaseSendWorkersLockRequest,
    SendWorkersLockResponse,
};
use contracts::{AppError, Ctx, Violation};
use infrastructure::cursor::Cursor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

// ------------------------------------------------ shared tool helpers
// Used by the sibling compliance/imports tool modules as well.

pub(crate) const PAGE_DEFAULT_LIMIT: u32 = 50;
pub(crate) const PAGE_MAX_LIMIT: u32 = 200;
const IDEMPOTENCY_KEY_MIN_LEN: usize = 16;

pub(crate) fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}

/// Output schema for a single REST response object (same shape as the
/// REST envelope's `data`).
pub(crate) fn object_output(description: &str) -> Value {
    json!({"type": "object", "description": description})
}

/// Output schema for a keyset page: `{items, next_cursor, has_more}`.
pub(crate) fn page_output(item_description: &str) -> Value {
    json!({
        "type": "object",
        "required": ["items", "has_more"],
        "properties": {
            "items": {"type": "array", "items": {"type": "object", "description": item_description}},
            "next_cursor": {"type": ["string", "null"]},
            "has_more": {"type": "boolean"}
        }
    })
}

#[derive(Serialize)]
pub(crate) struct PageOut<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Same rule as the REST `PageParams` extractor: default 50, 1..=200.
pub(crate) fn page_limit(limit: Option<u32>) -> Result<u32, AppError> {
    match limit {
        None => Ok(PAGE_DEFAULT_LIMIT),
        Some(n) if (1..=PAGE_MAX_LIMIT).contains(&n) => Ok(n),
        Some(_) => Err(AppError::validation(
            "limit must be between 1 and 200",
            vec![Violation {
                field: "limit".into(),
                code: "out_of_range".into(),
            }],
        )),
    }
}

/// Decode a signed `(created_at, id)` keyset cursor bound to the caller and
/// the filter set (B.1.5).
pub(crate) fn decode_keyset(
    hmac_key: &[u8],
    ctx: &Ctx,
    cursor: Option<&str>,
    filters: &Value,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let invalid = || AppError::validation("invalid pagination cursor", vec![]);
    let cursor = Cursor::decode(
        cursor,
        hmac_key,
        ctx.principal_id,
        &Cursor::hash_filter(filters),
    )?;
    let [timestamp, id] = <[Value; 2]>::try_from(cursor.sort_values).map_err(|_| invalid())?;
    let timestamp = DateTime::parse_from_rfc3339(timestamp.as_str().unwrap_or_default())
        .map_err(|_| invalid())?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id.as_str().unwrap_or_default()).map_err(|_| invalid())?;
    Ok(Some((timestamp, id)))
}

pub(crate) fn encode_keyset(
    hmac_key: &[u8],
    ctx: &Ctx,
    filters: &Value,
    position: Option<(DateTime<Utc>, Uuid)>,
) -> Option<String> {
    position.map(|(timestamp, id)| {
        Cursor::new(
            vec![json!(timestamp.to_rfc3339()), json!(id)],
            Cursor::hash_filter(filters),
            ctx.principal_id,
        )
        .encode(hmac_key)
    })
}

pub(crate) fn idempotency_scope(
    ctx: &Ctx,
    tool_name: &str,
    key: String,
) -> Result<infrastructure::idempotency::Scope, AppError> {
    if !(IDEMPOTENCY_KEY_MIN_LEN..=infrastructure::idempotency::KEY_MAX_LEN).contains(&key.len()) {
        return Err(AppError::validation(
            "idempotency_key must be 16..=128 characters",
            vec![Violation {
                field: "idempotency_key".into(),
                code: "LENGTH".into(),
            }],
        ));
    }
    Ok(infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: format!("MCP tools/call {tool_name}"),
        key,
    })
}

pub(crate) fn request_hash<T: Serialize>(value: &T) -> Result<Vec<u8>, AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| AppError::internal(format!("tool request hashing: {error}")))?;
    Ok(infrastructure::crypto::sha256_bytes(&bytes).to_vec())
}

// ------------------------------------------------------------ tools

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool(
            "ops.ping",
            "Liveness check — returns ok:true for the calling principal.",
            schema::<OpsPingInput>(),
            schema::<OpsPingOutput>(),
            false,
            |ctx, _args| async move {
                Ok(json!({
                    "ok": true,
                    "workspace_id": ctx.workspace_id,
                    "principal_id": ctx.principal_id,
                }))
            },
        ),
        tool_with_db(
            "operations_get_job",
            "Read progress, safe result references, and retryability for a job in the caller's allowed scope.",
            schema::<GetJobInput>(),
            schema::<GetJobOutput>(),
            false,
            |db, ctx, args| async move {
                let input = arguments::<GetJobInput>(args)?;
                let job = application::operations::jobs::get_job(&db, &ctx, input.job_id).await?;
                result(GetJobOutput::from(job))
            },
        ),
        tool_with_services(
            "operations_list_jobs",
            "List background jobs visible to the caller (redacted summaries), newest first. Requires operations.read. Optional state/job_kind filters; follow next_cursor with identical filters.",
            schema::<ListJobsInput>(),
            page_output(
                "Job summary: id, kind, state, attempt_count, last_error_code, retryable, external_effect, timestamps.",
            ),
            false,
            |services, ctx, args| async move {
                let input = arguments::<ListJobsInput>(args)?;
                let limit = page_limit(input.limit)?;
                let filters = json!({"state": input.state, "kind": input.job_kind});
                let after = decode_keyset(
                    &services.config.hmac_key,
                    &ctx,
                    input.cursor.as_deref(),
                    &filters,
                )?;
                let page = application::operations::jobs::list_jobs(
                    &services.db,
                    &ctx,
                    input.state.as_deref(),
                    input.job_kind.as_deref(),
                    after,
                    i64::from(limit),
                )
                .await?;
                result(PageOut {
                    next_cursor: encode_keyset(
                        &services.config.hmac_key,
                        &ctx,
                        &filters,
                        page.cursor_seed,
                    ),
                    has_more: page.has_more,
                    items: page.items,
                })
            },
        ),
        tool_with_db(
            "operations_retry_job",
            "Request a safe retry of a failed/dead job (POST /jobs/{id}/retry-requests). Requires the operations role checks REST applies; external-effect jobs require a reason. Retry with the same idempotency_key.",
            schema::<RetryJobInput>(),
            object_output(
                "Updated job: id, kind, state, attempts, retryable, external_effect, result_refs.",
            ),
            false,
            |db, ctx, args| async move {
                let input = arguments::<RetryJobInput>(args)?;
                let job_id = input.job_id;
                let req = JobRetryRequest {
                    reason: input.reason,
                };
                let hash = request_hash(&(job_id, &req))?;
                let scope = idempotency_scope(&ctx, "operations_retry_job", input.idempotency_key)?;
                let body = infrastructure::idempotency::run(&db, &scope, &hash, move |mut tx| {
                    let ctx = ctx.clone();
                    async move {
                        let body = application::operations::jobs::request_retry_tx(
                            &mut tx, &ctx, job_id, &req,
                        )
                        .await?;
                        Ok((
                            tx,
                            infrastructure::idempotency::Stored { status: 200, body },
                        ))
                    }
                })
                .await?
                .into_inner()
                .body;
                result::<JobResponse>(body)
            },
        ),
        tool_with_services(
            "operations_list_audit_logs",
            "List redacted audit log entries visible to the caller, newest first. Requires audit access (operations.read). Filter by action, target_type, target_id, actor_principal_id and an occurred_from/occurred_to time window; follow next_cursor with identical filters.",
            schema::<ListAuditLogsInput>(),
            page_output(
                "Audit entry: id, occurred_at, action, target_type, target_id, actor_principal_id, request_id, outcome, details.",
            ),
            false,
            |services, ctx, args| async move {
                let input = arguments::<ListAuditLogsInput>(args)?;
                let limit = page_limit(input.limit)?;
                let filter = AuditLogFilters {
                    action: input.action,
                    target_type: input.target_type,
                    target_id: input.target_id,
                    actor_principal_id: input.actor_principal_id,
                    from: input.occurred_from,
                    to: input.occurred_to,
                };
                let filters = json!({
                    "action": filter.action.clone(),
                    "target_type": filter.target_type.clone(),
                    "target_id": filter.target_id,
                    "actor_principal_id": filter.actor_principal_id,
                    "from": filter.from,
                    "to": filter.to,
                });
                let after = decode_keyset(
                    &services.config.hmac_key,
                    &ctx,
                    input.cursor.as_deref(),
                    &filters,
                )?;
                let page = application::operations::audit_logs::list_audit_logs(
                    &services.db,
                    &ctx,
                    &filter,
                    after,
                    i64::from(limit),
                )
                .await?;
                result(PageOut {
                    next_cursor: encode_keyset(
                        &services.config.hmac_key,
                        &ctx,
                        &filters,
                        page.cursor_seed,
                    ),
                    has_more: page.has_more,
                    items: page.items,
                })
            },
        ),
        tool_with_db(
            "operations_dashboard_summary",
            "Read the KPI dashboard summary (queues, reply/meeting ratios, operational counts) with definitions and freshness. Requires operations.read. Optional period_from/period_to window and scope team|mine.",
            schema::<DashboardSummaryInput>(),
            object_output(
                "Dashboard summary: period, generated_at, scope, queues, kpis, operational counts.",
            ),
            false,
            |db, ctx, args| async move {
                let input = arguments::<DashboardSummaryInput>(args)?;
                let data = application::operations::dashboard::summary(
                    &db,
                    &ctx,
                    &DashboardFilters {
                        from: input.period_from,
                        to: input.period_to,
                        scope: input.scope,
                    },
                )
                .await?;
                result(data)
            },
        ),
        tool_with_db(
            "operations_list_alerts",
            "Read current operational alerts: emergency stop, send-worker lock, outbox backlog, unknown outbound, dead jobs and mailboxes needing reauthorization. Requires operations.read and the same role checks as REST. No writes.",
            schema::<ListAlertsInput>(),
            object_output(
                "Alerts: generated_at, emergency_stop, send_workers_locked, outbox_backlog, unknown_outbound, dead_jobs, reauth_mailboxes.",
            ),
            false,
            |db, ctx, args| async move {
                let _ = arguments::<ListAlertsInput>(args)?;
                let data = application::operations::alerts::list(&db, &ctx).await?;
                result(data)
            },
        ),
        tool_with_db(
            "operations_release_send_workers_lock",
            "Release the post-restore send-workers lock after outbound state has been reconciled (POST /operations/send-workers-lock/release). Requires the Admin role checks REST applies and a reconcile_reason. Retry with the same idempotency_key.",
            schema::<ReleaseLockInput>(),
            object_output("Lock state: send_workers_locked (false after release)."),
            false,
            |db, ctx, args| async move {
                let input = arguments::<ReleaseLockInput>(args)?;
                let req = ReleaseSendWorkersLockRequest {
                    reason: input.reconcile_reason,
                };
                let hash = request_hash(&req)?;
                let scope = idempotency_scope(
                    &ctx,
                    "operations_release_send_workers_lock",
                    input.idempotency_key,
                )?;
                let body = infrastructure::idempotency::run(&db, &scope, &hash, move |mut tx| {
                    let ctx = ctx.clone();
                    async move {
                        application::operations::send_workers::release_lock_tx(&mut tx, &ctx, &req)
                            .await?;
                        Ok((
                            tx,
                            infrastructure::idempotency::Stored {
                                status: 200,
                                body: SendWorkersLockResponse {
                                    send_workers_locked: false,
                                },
                            },
                        ))
                    }
                })
                .await?
                .into_inner()
                .body;
                result(body)
            },
        ),
    ]
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpsPingInput {}

#[derive(Serialize, JsonSchema)]
struct OpsPingOutput {
    ok: bool,
    workspace_id: Uuid,
    principal_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetJobInput {
    job_id: Uuid,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListJobsInput {
    /// Optional job state filter : queued, running, retry_wait, completed, dead, canceled or unknown.
    #[serde(default)]
    state: Option<String>,
    /// Optional queue kind filter, e.g. the job `kind` returned by operations_get_job.
    #[serde(default)]
    job_kind: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RetryJobInput {
    job_id: Uuid,
    /// Operator reason, required when the job has external effects; recorded in the audit details.
    #[serde(default)]
    reason: Option<String>,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListAuditLogsInput {
    /// Optional exact audit action filter, e.g. `import.object_uploaded`.
    #[serde(default)]
    action: Option<String>,
    /// Optional audit target type filter, e.g. `stored_object`, `company`, `job`.
    #[serde(default)]
    target_type: Option<String>,
    /// Optional UUID of the audited target record.
    #[serde(default)]
    target_id: Option<Uuid>,
    /// Optional UUID of the principal that performed the audited action.
    #[serde(default)]
    actor_principal_id: Option<Uuid>,
    /// Optional inclusive lower bound on occurred_at, RFC3339 timestamp (e.g. 2026-10-01T00:00:00Z).
    #[serde(default)]
    occurred_from: Option<DateTime<Utc>>,
    /// Optional upper bound on occurred_at, RFC3339 timestamp (e.g. 2026-10-31T23:59:59Z).
    #[serde(default)]
    occurred_to: Option<DateTime<Utc>>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DashboardSummaryInput {
    /// Optional start of the KPI period, RFC3339 timestamp (e.g. 2026-10-01T00:00:00Z).
    #[serde(default)]
    period_from: Option<DateTime<Utc>>,
    /// Optional end of the KPI period, RFC3339 timestamp (e.g. 2026-10-31T23:59:59Z).
    #[serde(default)]
    period_to: Option<DateTime<Utc>>,
    /// Optional dashboard scope: `team` or `mine`; the server picks the default for the caller's role.
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListAlertsInput {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReleaseLockInput {
    /// Required reconciliation evidence/reason explaining why outbound state is safe; recorded in audit details.
    reconcile_reason: String,
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
}

#[derive(Serialize, JsonSchema)]
struct GetJobOutput {
    job_id: Uuid,
    kind: String,
    state: String,
    retryable: bool,
    external_effect: bool,
    result_refs: Vec<JobToolResultRef>,
    lease_owner: Option<String>,
    lease_expires_at: Option<DateTime<Utc>>,
    last_error_code: Option<String>,
}

#[derive(Serialize, JsonSchema)]
struct JobToolResultRef {
    #[serde(rename = "type")]
    reference_type: String,
    id: Uuid,
}

impl From<JobResponse> for GetJobOutput {
    fn from(job: JobResponse) -> Self {
        let state = serde_json::to_value(job.state)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".into());
        Self {
            job_id: job.id,
            kind: job.kind,
            state,
            retryable: job.retryable,
            external_effect: job.external_effect,
            result_refs: job
                .result_refs
                .into_iter()
                .map(|reference| JobToolResultRef {
                    reference_type: reference.reference_type,
                    id: reference.id,
                })
                .collect(),
            lease_owner: job.lease_owner,
            lease_expires_at: job.lease_expires_at,
            last_error_code: job.last_error_code,
        }
    }
}

#[cfg(test)]
mod tests {
    fn parity_tools() -> Vec<crate::ToolDef> {
        crate::all_tools()
            .into_iter()
            .filter(|tool| {
                ["compliance_", "imports_", "exports_", "operations_"]
                    .iter()
                    .any(|prefix| tool.name.starts_with(prefix))
            })
            .collect()
    }

    #[test]
    fn compliance_imports_operations_expose_rest_parity_tools() {
        let names: Vec<_> = parity_tools().iter().map(|tool| tool.name).collect();
        for expected in [
            "compliance_list_bases",
            "compliance_create_basis",
            "compliance_get_basis",
            "compliance_review_basis",
            "compliance_revoke_basis",
            "compliance_list_suppressions",
            "compliance_create_suppression",
            "compliance_get_suppression",
            "compliance_request_suppression_release",
            "imports_create",
            "imports_create_from_csv",
            "imports_list",
            "imports_get",
            "imports_list_rows",
            "imports_commit",
            "exports_create",
            "operations_get_job",
            "operations_list_jobs",
            "operations_retry_job",
            "operations_list_audit_logs",
            "operations_dashboard_summary",
            "operations_list_alerts",
            "operations_release_send_workers_lock",
        ] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn every_input_is_described_and_mutations_take_idempotency_keys() {
        for tool in parity_tools() {
            for (field, schema) in tool.input_schema["properties"]
                .as_object()
                .into_iter()
                .flatten()
            {
                assert!(
                    schema["description"]
                        .as_str()
                        .is_some_and(|text| text.len() > 20),
                    "{}.{field} lacks a description",
                    tool.name
                );
            }
        }
        let tools = parity_tools();
        for name in [
            "imports_create",
            "imports_create_from_csv",
            "imports_commit",
            "exports_create",
            "operations_retry_job",
            "operations_release_send_workers_lock",
        ] {
            let tool = tools.iter().find(|tool| tool.name == name).unwrap();
            assert!(
                tool.input_schema["required"]
                    .as_array()
                    .is_some_and(|fields| fields.iter().any(|field| field == "idempotency_key")),
                "{name} must require an idempotency key"
            );
        }
    }
}
