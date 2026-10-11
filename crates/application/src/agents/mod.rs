//! Agent run use cases (B.2, §13.5). REST and MCP share these ACL checks and
//! mutations so neither surface can see or cancel a broader run.

use chrono::{DateTime, Utc};
use contracts::agents::{
    AgentActionResponse, AgentRunBudget, AgentRunCancelResponse, AgentRunDetailResponse,
    AgentRunFilters, AgentRunResponse, AgentRunStatus, AgentRunTargetScope, CancelAgentRunRequest,
    CreateAgentRunRequest, FinishAgentRunRequest,
};
use contracts::{AppError, Ctx, PrincipalKind, Role};
use infrastructure::db::{Db, Tx};
use infrastructure::repo::agents as repo;
use uuid::Uuid;

#[derive(Debug)]
pub struct RunListResult {
    pub items: Vec<AgentRunResponse>,
    pub has_more: bool,
    pub cursor_seed: Option<(DateTime<Utc>, Uuid)>,
}

fn manager_or_admin(ctx: &Ctx) -> bool {
    (matches!(ctx.role, Role::Admin | Role::Manager) || crate::authz::agent_admin(ctx))
}

fn may_view_run(ctx: &Ctx, run: &repo::AgentRunRow) -> bool {
    manager_or_admin(ctx)
        || (ctx.principal_kind == PrincipalKind::Human && run.requested_by == ctx.principal_id)
        || (ctx.principal_kind == PrincipalKind::Agent
            && (run.principal_id == ctx.principal_id || ctx.agent_run_id == Some(run.id)))
}

fn status(value: &str) -> Result<AgentRunStatus, AppError> {
    AgentRunStatus::parse(value)
        .ok_or_else(|| AppError::internal("agent run row has an invalid state"))
}

fn allowed_actions(ctx: &Ctx, run: &repo::AgentRunRow) -> Vec<String> {
    let can_cancel = (manager_or_admin(ctx)
        || (ctx.principal_kind == PrincipalKind::Human && run.requested_by == ctx.principal_id)
        || (ctx.principal_kind == PrincipalKind::Agent && run.principal_id == ctx.principal_id))
        && AgentRunStatus::parse(&run.status).is_some_and(|s| s.can_cancel());
    if can_cancel {
        vec!["agents.cancel_run".into()]
    } else {
        Vec::new()
    }
}

fn run_response(ctx: &Ctx, run: &repo::AgentRunRow) -> Result<AgentRunResponse, AppError> {
    Ok(AgentRunResponse {
        id: run.id,
        principal_id: run.principal_id,
        requested_by: run.requested_by,
        source_credential_id: run.source_credential_id,
        objective: run.objective.clone(),
        status: status(&run.status)?,
        budget: run.budget.clone(),
        usage: run.usage.clone(),
        started_at: run.started_at,
        finished_at: run.finished_at,
        cancel_requested_at: run.cancel_requested_at,
        created_at: run.created_at,
        allowed_actions: allowed_actions(ctx, run),
    })
}

fn cancel_response(
    ctx: &Ctx,
    run: &repo::AgentRunRow,
    in_flight_external_actions: i64,
) -> Result<AgentRunCancelResponse, AppError> {
    let run = run_response(ctx, run)?;
    Ok(AgentRunCancelResponse {
        id: run.id,
        principal_id: run.principal_id,
        requested_by: run.requested_by,
        source_credential_id: run.source_credential_id,
        objective: run.objective,
        status: run.status,
        budget: run.budget,
        usage: run.usage,
        started_at: run.started_at,
        finished_at: run.finished_at,
        cancel_requested_at: run.cancel_requested_at,
        created_at: run.created_at,
        allowed_actions: run.allowed_actions,
        in_flight_external_actions,
    })
}

fn validate_budget(budget: &AgentRunBudget) -> Result<(), AppError> {
    for (field, value) in [
        ("max_tool_calls", budget.max_tool_calls.map(u64::from)),
        ("max_proposals", budget.max_proposals.map(u64::from)),
        (
            "max_external_searches",
            budget.max_external_searches.map(u64::from),
        ),
        ("max_input_tokens", budget.max_input_tokens),
        ("max_output_tokens", budget.max_output_tokens),
    ] {
        if value == Some(0) {
            return Err(AppError::validation(
                format!("{field} must be greater than zero"),
                vec![contracts::Violation {
                    field: field.into(),
                    code: "out_of_range".into(),
                }],
            ));
        }
    }
    if budget
        .max_cost_usd
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        return Err(AppError::validation(
            "max_cost_usd must be finite and greater than zero",
            vec![contracts::Violation {
                field: "max_cost_usd".into(),
                code: "out_of_range".into(),
            }],
        ));
    }
    Ok(())
}

fn validate_target_scope(scope: &AgentRunTargetScope) -> Result<(), AppError> {
    if scope
        .industry
        .as_deref()
        .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 128)
    {
        return Err(AppError::validation(
            "industry must contain 1 to 128 characters",
            vec![contracts::Violation {
                field: "scope.industry".into(),
                code: "out_of_range".into(),
            }],
        ));
    }
    if scope.country_code.as_deref().is_some_and(|value| {
        let value = value.trim();
        value.len() != 2 || !value.bytes().all(|b| b.is_ascii_alphabetic())
    }) {
        return Err(AppError::validation(
            "country_code must be a two-letter code",
            vec![contracts::Violation {
                field: "scope.country_code".into(),
                code: "invalid_format".into(),
            }],
        ));
    }
    Ok(())
}

/// `POST /agent-runs`. A human Admin or Manager requests a queued run on an
/// active agent. The objective and validated limits are stored atomically
/// with audit and outbox rows. The executing agent/credential is never
/// accepted as client-controlled system metadata.
pub async fn create_run(
    db: &Db,
    ctx: &Ctx,
    req: &CreateAgentRunRequest,
) -> Result<AgentRunResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = create_run_tx(&mut tx, ctx, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-owning variant for the REST idempotency wrapper.
pub async fn create_run_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateAgentRunRequest,
) -> Result<AgentRunResponse, AppError> {
    if !manager_or_admin(ctx) {
        return Err(AppError::forbidden(
            "agent runs require an Admin or Manager",
        ));
    }
    crate::authz::require_scope(ctx, contracts::Scope::AgentsRun)?;
    let objective = req.objective.trim();
    if objective.is_empty() || objective.chars().count() > 1000 {
        return Err(AppError::validation(
            "objective must contain 1 to 1000 characters",
            vec![contracts::Violation {
                field: "objective".into(),
                code: "out_of_range".into(),
            }],
        ));
    }
    let budget = req.budget.clone().unwrap_or_default();
    validate_budget(&budget)?;
    let scope = req.scope.clone().unwrap_or_default();
    validate_target_scope(&scope)?;
    let mut budget_json = serde_json::to_value(&budget).map_err(|error| {
        AppError::internal(format!("agent budget serialization failed: {error}"))
    })?;
    let mut target_scope = serde_json::Map::new();
    if let Some(industry) = scope.industry.as_ref() {
        target_scope.insert(
            "industry".into(),
            serde_json::Value::String(industry.trim().to_string()),
        );
    }
    if let Some(country_code) = scope.country_code.as_ref() {
        target_scope.insert(
            "country_code".into(),
            serde_json::Value::String(country_code.trim().to_ascii_uppercase()),
        );
    }
    if !target_scope.is_empty() {
        budget_json["target_scope"] = serde_json::Value::Object(target_scope);
    }

    if !repo::is_active_agent(tx, req.agent_principal_id).await? {
        return Err(AppError::not_found("active agent not found"));
    }
    let run_id = repo::insert_run(
        tx,
        req.agent_principal_id,
        ctx.principal_id,
        None,
        objective,
        &budget_json,
    )
    .await?;
    let run = repo::get_run(tx, run_id)
        .await?
        .ok_or_else(|| AppError::internal("agent run missing after insert"))?;
    let objective_hash = infrastructure::crypto::sha256_hex(objective.as_bytes());
    let budget_hash = infrastructure::crypto::sha256_hex(budget_json.to_string().as_bytes());
    infrastructure::audit::record(
        tx,
        infrastructure::audit::AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: Some(ctx.principal_id),
            approved_by: None,
            job_id: None,
            action: "agent_run.created".into(),
            target_type: "agent_run".into(),
            target_id: Some(run_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "agent_principal_id": req.agent_principal_id,
                "objective_hash": objective_hash,
                "objective_length": objective.chars().count(),
                "budget_hash": budget_hash,
            }),
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    infrastructure::outbox::emit(
        tx,
        "agent_run.created",
        "agent_run",
        run_id,
        1,
        serde_json::json!({ "agent_run_id": run_id, "principal_id": req.agent_principal_id }),
    )
    .await?;
    run_response(ctx, &run)
}

/// `GET /agent-runs` page. Admin/Manager may view the workspace, Members only
/// their own requests, and Agents only runs belonging to their principal.
pub async fn list_runs(
    db: &Db,
    ctx: &Ctx,
    filters: &AgentRunFilters,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<RunListResult, AppError> {
    if ctx.principal_kind == PrincipalKind::System {
        return Err(AppError::forbidden(
            "system principals cannot list agent runs",
        ));
    }
    if ctx.principal_kind == PrincipalKind::Agent
        && !ctx.scopes.contains(contracts::Scope::AgentsRun)
    {
        return Err(AppError::new(
            contracts::ErrorCode::ScopeRequired,
            "missing scope agents.run",
        ));
    }
    if manager_or_admin(ctx) && !ctx.scopes.contains(contracts::Scope::AgentsRun) {
        return Err(AppError::new(
            contracts::ErrorCode::ScopeRequired,
            "missing scope agents.run",
        ));
    }
    if let Some(value) = &filters.status {
        status(value).map_err(|_| {
            AppError::validation(
                "invalid agent run status",
                vec![contracts::Violation {
                    field: "status".into(),
                    code: "invalid".into(),
                }],
            )
        })?;
    }
    let filter = if manager_or_admin(ctx) {
        repo::RunListFilter {
            principal_id: filters.agent_principal_id,
            status: filters.status.clone(),
            ..Default::default()
        }
    } else if ctx.principal_kind == PrincipalKind::Agent {
        repo::RunListFilter {
            agent_principal: Some(ctx.principal_id),
            status: filters.status.clone(),
            ..Default::default()
        }
    } else {
        repo::RunListFilter {
            requested_by: Some(ctx.principal_id),
            status: filters.status.clone(),
            ..Default::default()
        }
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_runs(&mut tx, &filter, after, limit + 1).await?;
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let cursor_seed = has_more
        .then(|| rows.last().map(|row| (row.created_at, row.id)))
        .flatten();
    let items = rows
        .iter()
        .map(|row| run_response(ctx, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RunListResult {
        items,
        has_more,
        cursor_seed,
    })
}

/// Shared owner/manager ACL for REST and MCP `agents_cancel_run` / run detail.
pub async fn get_run_detail(
    db: &Db,
    ctx: &Ctx,
    run_id: Uuid,
) -> Result<AgentRunDetailResponse, AppError> {
    if ctx.principal_kind == PrincipalKind::System {
        return Err(AppError::forbidden(
            "system principals cannot view agent runs",
        ));
    }
    if (ctx.principal_kind == PrincipalKind::Agent || manager_or_admin(ctx))
        && !ctx.scopes.contains(contracts::Scope::AgentsRun)
    {
        return Err(AppError::new(
            contracts::ErrorCode::ScopeRequired,
            "missing scope agents.run",
        ));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let run = repo::get_run(&mut tx, run_id).await?;
    let Some(run) = run.filter(|row| may_view_run(ctx, row)) else {
        tx.commit().await?;
        return Err(AppError::not_found("agent run not found"));
    };
    let actions = repo::list_actions(&mut tx, run_id).await?;
    tx.commit().await?;
    let actions = actions
        .into_iter()
        .map(|action| AgentActionResponse {
            id: action.id,
            tool_name: action.tool_name,
            arguments_hash: action
                .arguments_hash
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            redacted_arguments: domain::agents::redact_action_arguments(&action.redacted_arguments),
            result_code: action.result_code,
            result_ref: action
                .result_ref
                .filter(|value| Uuid::parse_str(value).is_ok()),
            approval_request_id: action.approval_request_id,
            has_external_effect: action.has_external_effect,
            trace_id: action.trace_id,
            started_at: action.started_at,
            finished_at: action.finished_at,
            created_at: action.created_at,
        })
        .collect();
    let run_data = run_response(ctx, &run)?;
    Ok(AgentRunDetailResponse {
        id: run_data.id,
        principal_id: run_data.principal_id,
        requested_by: run_data.requested_by,
        source_credential_id: run_data.source_credential_id,
        objective: run_data.objective,
        status: run_data.status,
        budget: run_data.budget,
        usage: run_data.usage,
        started_at: run_data.started_at,
        finished_at: run_data.finished_at,
        cancel_requested_at: run_data.cancel_requested_at,
        created_at: run_data.created_at,
        allowed_actions: run_data.allowed_actions,
        actions,
    })
}

/// `POST /agent-runs/{id}/cancel` and MCP cancellation share this function.
/// A repeated cancellation returns the canceled record; completed/failed
/// runs cannot be canceled. In-flight external work remains visible through
/// its own job and provider state.
pub async fn cancel_run(
    db: &Db,
    ctx: &Ctx,
    run_id: Uuid,
    req: &CancelAgentRunRequest,
) -> Result<AgentRunCancelResponse, AppError> {
    if ctx.principal_kind == PrincipalKind::Agent
        && !ctx.scopes.contains(contracts::Scope::AgentsRun)
    {
        return Err(AppError::new(
            contracts::ErrorCode::ScopeRequired,
            "missing scope agents.run",
        ));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = cancel_run_tx(&mut tx, ctx, run_id, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-owning variant for REST idempotency.
pub async fn cancel_run_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    run_id: Uuid,
    req: &CancelAgentRunRequest,
) -> Result<AgentRunCancelResponse, AppError> {
    if ctx.principal_kind == PrincipalKind::Agent
        && !ctx.scopes.contains(contracts::Scope::AgentsRun)
    {
        return Err(AppError::new(
            contracts::ErrorCode::ScopeRequired,
            "missing scope agents.run",
        ));
    }
    if req
        .reason
        .as_deref()
        .is_some_and(|reason| reason.chars().count() > 1000)
    {
        return Err(AppError::validation(
            "reason must be at most 1000 characters",
            vec![contracts::Violation {
                field: "reason".into(),
                code: "too_long".into(),
            }],
        ));
    }
    let run = repo::get_run(tx, run_id)
        .await?
        .filter(|row| may_view_run(ctx, row))
        .ok_or_else(|| AppError::not_found("agent run not found"))?;
    let run_status = status(&run.status)?;
    if run_status == AgentRunStatus::Canceled {
        let in_flight_external_actions = repo::count_in_flight_external_actions(tx, run_id).await?;
        return cancel_response(ctx, &run, in_flight_external_actions);
    }
    if !run_status.can_cancel() {
        return Err(AppError::conflict("agent run is already complete"));
    }
    let may_cancel = manager_or_admin(ctx)
        || (ctx.principal_kind == PrincipalKind::Human && run.requested_by == ctx.principal_id)
        || (ctx.principal_kind == PrincipalKind::Agent && run.principal_id == ctx.principal_id);
    if !may_cancel {
        return Err(AppError::not_found("agent run not found"));
    }
    let updated = repo::cancel_run(tx, run_id)
        .await?
        .ok_or_else(|| AppError::conflict("agent run state changed; reload and retry"))?;
    let reason_hash = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| infrastructure::crypto::sha256_hex(reason.as_bytes()));
    let mut metadata = serde_json::json!({
        "status": "canceled",
        "principal_id": run.principal_id,
    });
    if let Some(hash) = reason_hash {
        metadata["reason_hash"] = serde_json::Value::String(hash);
        metadata["reason_length"] = serde_json::Value::from(
            req.reason
                .as_deref()
                .unwrap_or_default()
                .trim()
                .chars()
                .count() as u64,
        );
    }
    infrastructure::audit::record(
        tx,
        infrastructure::audit::AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: Some(run.requested_by),
            approved_by: None,
            job_id: None,
            action: "agent_run.cancel_requested".into(),
            target_type: "agent_run".into(),
            target_id: Some(run_id),
            outcome: "ok".into(),
            metadata,
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    infrastructure::outbox::emit(
        tx,
        "agent_run.cancel_requested",
        "agent_run",
        run_id,
        1,
        serde_json::json!({ "agent_run_id": run_id }),
    )
    .await?;
    let in_flight_external_actions = repo::count_in_flight_external_actions(tx, run_id).await?;
    cancel_response(ctx, &updated, in_flight_external_actions)
}

fn may_finish_run(ctx: &Ctx, run: &repo::AgentRunRow) -> bool {
    if ctx.scopes.contains(contracts::Scope::AgentsRun)
        && ctx.principal_kind == PrincipalKind::Human
        && (ctx.role == Role::Admin || crate::authz::agent_admin(ctx))
        && matches!(&ctx.auth, contracts::AuthMethod::Session { .. })
    {
        return true;
    }
    ctx.principal_kind == PrincipalKind::Agent
        && ctx.role == Role::Agent
        && ctx.scopes.contains(contracts::Scope::AgentsRun)
        && run.principal_id == ctx.principal_id
        && ctx.agent_run_id.is_none_or(|bound_run| bound_run == run.id)
        && credential_matches(ctx, run)
}

/// External runner completion with a constrained outcome. Human Admins may
/// perform recovery; Agents may finish only their own run and bound
/// credential. Run/action rows, audit, and outbox commit atomically.
pub async fn finish_run(
    db: &Db,
    ctx: &Ctx,
    run_id: Uuid,
    req: &FinishAgentRunRequest,
) -> Result<AgentRunResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = finish_run_tx(&mut tx, ctx, run_id, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-owning variant for REST idempotency.
pub async fn finish_run_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    run_id: Uuid,
    req: &FinishAgentRunRequest,
) -> Result<AgentRunResponse, AppError> {
    let run = repo::get_run_for_update(tx, run_id)
        .await?
        .filter(|run| may_finish_run(ctx, run))
        .ok_or_else(|| AppError::not_found("agent run not found"))?;
    let desired = req.outcome.status();
    let current = status(&run.status)?;
    if current == desired {
        return run_response(ctx, &run);
    }
    if current.is_terminal() {
        return Err(AppError::conflict(
            "agent run already has a different terminal outcome",
        ));
    }
    if current == AgentRunStatus::AwaitingApproval {
        return Err(AppError::conflict("agent run is awaiting approval"));
    }
    let unfinished = repo::count_unfinished_actions(tx, run_id).await?;
    if unfinished > 0 {
        return Err(AppError::conflict(
            "agent run still has unfinished tool actions",
        ));
    }
    let outcome = match req.outcome {
        contracts::agents::AgentRunFinishOutcome::Completed => "completed",
        contracts::agents::AgentRunFinishOutcome::Failed => "failed",
    };
    let updated = repo::finish_run(tx, run_id, outcome)
        .await?
        .ok_or_else(|| AppError::conflict("agent run state changed; reload and retry"))?;
    let action = if outcome == "completed" {
        "agent_run.completed"
    } else {
        "agent_run.failed"
    };
    infrastructure::audit::record(
        tx,
        infrastructure::audit::AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: Some(run.requested_by),
            approved_by: None,
            job_id: None,
            action: action.into(),
            target_type: "agent_run".into(),
            target_id: Some(run_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({ "agent_run_id": run_id }),
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    infrastructure::outbox::emit(
        tx,
        action,
        "agent_run",
        run_id,
        1,
        serde_json::json!({ "agent_run_id": run_id }),
    )
    .await?;
    run_response(ctx, &updated)
}

fn valid_tool_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

fn tool_result_code(value: &str) -> &str {
    if valid_tool_label(value) {
        value
    } else {
        "INTERNAL"
    }
}

fn credential_matches(ctx: &Ctx, run: &repo::AgentRunRow) -> bool {
    let Some(expected) = run.source_credential_id else {
        return true;
    };
    matches!(
        &ctx.auth,
        contracts::AuthMethod::ServiceCredential { credential_id } if *credential_id == expected
    )
}

async fn audit_tool_denial(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    run_id: Option<Uuid>,
    tool_name: &str,
    arguments_hash: &str,
    has_external_effect: bool,
    error: &AppError,
) -> Result<(), AppError> {
    let mut entry = infrastructure::audit::AuditEntry::new(
        ctx.principal_id,
        "agent.tool_call.denied",
        if run_id.is_some() {
            "agent_run"
        } else {
            "agent_tool_call"
        },
        ctx.request_id.clone(),
    )
    .denied(&error.code);
    entry.target_id = run_id;
    entry.metadata = serde_json::json!({
        "tool_name": tool_name,
        "arguments_hash": arguments_hash,
        "has_external_effect": has_external_effect,
        "error_code": error.code.as_str(),
    });
    infrastructure::audit::record(tx, entry).await
}

/// Begin an MCP tool call. If a run is bound to the context, this locks and
/// validates that exact run, enforces its tool-call budget, transitions a
/// queued run to running, increments usage, and records redacted action/audit
/// metadata atomically. Calls without a run are still audited, but have no
/// `agent_actions` row because that table requires a run id.
pub async fn begin_tool_call(
    db: &Db,
    ctx: &mut Ctx,
    args: &serde_json::Value,
    tool_name: &str,
    has_external_effect: bool,
) -> Result<Option<Uuid>, AppError> {
    if !valid_tool_label(tool_name) {
        return Err(AppError::validation("invalid tool name", vec![]));
    }
    let args_hash =
        infrastructure::crypto::sha256_bytes(&serde_json::to_vec(args).map_err(|error| {
            AppError::internal(format!("tool arguments hashing failed: {error}"))
        })?);
    let args_hash_hex = args_hash
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let redacted_args = domain::agents::redact_action_arguments(args);
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(run_id) = ctx.agent_run_id else {
        infrastructure::audit::record(
            &mut tx,
            infrastructure::audit::AuditEntry {
                actor_principal_id: Some(ctx.principal_id),
                requested_by: None,
                approved_by: None,
                job_id: None,
                action: "agent.tool_call.started".into(),
                target_type: "agent_tool_call".into(),
                target_id: None,
                outcome: "ok".into(),
                metadata: serde_json::json!({
                    "tool_name": tool_name,
                    "arguments_hash": args_hash_hex,
                    "has_external_effect": has_external_effect,
                }),
                request_id: ctx.request_id.clone(),
                trace_id: None,
            },
        )
        .await?;
        tx.commit().await?;
        return Ok(None);
    };

    let run = repo::get_run_for_update(&mut tx, run_id).await?;
    let Some(run) = run else {
        let error = AppError::not_found("agent run not found");
        audit_tool_denial(
            &mut tx,
            ctx,
            Some(run_id),
            tool_name,
            &args_hash_hex,
            has_external_effect,
            &error,
        )
        .await?;
        tx.commit().await?;
        return Err(error);
    };
    if ctx.principal_kind != PrincipalKind::Agent
        || run.principal_id != ctx.principal_id
        || !credential_matches(ctx, &run)
    {
        let error = AppError::not_found("agent run not found");
        audit_tool_denial(
            &mut tx,
            ctx,
            Some(run_id),
            tool_name,
            &args_hash_hex,
            has_external_effect,
            &error,
        )
        .await?;
        tx.commit().await?;
        return Err(error);
    }
    let run_status = AgentRunStatus::parse(&run.status)
        .ok_or_else(|| AppError::internal("agent run row has an invalid state"))?;
    if !matches!(run_status, AgentRunStatus::Queued | AgentRunStatus::Running) {
        let error = AppError::conflict("agent run is canceled, terminal, or awaiting approval");
        audit_tool_denial(
            &mut tx,
            ctx,
            Some(run_id),
            tool_name,
            &args_hash_hex,
            has_external_effect,
            &error,
        )
        .await?;
        tx.commit().await?;
        return Err(error);
    }

    let mut usage = run.usage.as_object().cloned().unwrap_or_default();
    let tool_calls = usage
        .get("tool_calls")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let max_tool_calls = run
        .budget
        .get("max_tool_calls")
        .and_then(serde_json::Value::as_u64);
    if max_tool_calls.is_some_and(|maximum| tool_calls >= maximum) {
        let error = AppError::new(
            contracts::ErrorCode::RateLimited,
            "agent run has reached its max_tool_calls budget",
        );
        audit_tool_denial(
            &mut tx,
            ctx,
            Some(run_id),
            tool_name,
            &args_hash_hex,
            has_external_effect,
            &error,
        )
        .await?;
        tx.commit().await?;
        return Err(error);
    }
    let next_tool_calls = tool_calls
        .checked_add(1)
        .filter(|count| i64::try_from(*count).is_ok())
        .ok_or_else(|| AppError::internal("agent tool-call usage overflow"))?;
    let aggregate_version = i64::try_from(next_tool_calls)
        .map_err(|_| AppError::internal("agent tool-call usage overflow"))?;
    usage.insert(
        "tool_calls".into(),
        serde_json::Value::from(next_tool_calls),
    );
    if !repo::update_tool_usage(&mut tx, run_id, &serde_json::Value::Object(usage)).await? {
        let error = AppError::conflict("agent run state changed; reload and retry");
        audit_tool_denial(
            &mut tx,
            ctx,
            Some(run_id),
            tool_name,
            &args_hash_hex,
            has_external_effect,
            &error,
        )
        .await?;
        tx.commit().await?;
        return Err(error);
    }
    let action_id = repo::record_action(
        &mut tx,
        run_id,
        tool_name,
        &args_hash,
        redacted_args,
        None,
        has_external_effect,
    )
    .await?;
    infrastructure::audit::record(
        &mut tx,
        infrastructure::audit::AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: Some(run.requested_by),
            approved_by: None,
            job_id: None,
            action: "agent.tool_call.started".into(),
            target_type: "agent_action".into(),
            target_id: Some(action_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "agent_run_id": run_id,
                "tool_name": tool_name,
                "arguments_hash": args_hash_hex,
                "has_external_effect": has_external_effect,
            }),
            request_id: ctx.request_id.clone(),
            trace_id: None,
        },
    )
    .await?;
    infrastructure::outbox::emit(
        &mut tx,
        "agent.tool_call.started",
        "agent_action",
        action_id,
        aggregate_version,
        serde_json::json!({ "agent_run_id": run_id, "agent_action_id": action_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(action_id))
}

/// Finish a begun MCP call. Successful and failed outcomes are recorded with
/// a stable result code only; arguments and result bodies stay out of logs.
pub async fn finish_tool_call(
    db: &Db,
    ctx: &Ctx,
    action_id: Option<Uuid>,
    result_code: &str,
) -> Result<(), AppError> {
    let result_code = tool_result_code(result_code);
    let Some(action_id) = action_id else {
        let mut tx = db.begin(ctx.workspace_id).await?;
        infrastructure::audit::record(
            &mut tx,
            infrastructure::audit::AuditEntry {
                actor_principal_id: Some(ctx.principal_id),
                requested_by: None,
                approved_by: None,
                job_id: None,
                action: "agent.tool_call.finished".into(),
                target_type: "agent_tool_call".into(),
                target_id: None,
                outcome: if result_code == "ok" {
                    "ok".into()
                } else {
                    format!("failed:{result_code}")
                },
                metadata: serde_json::json!({ "result_code": result_code }),
                request_id: ctx.request_id.clone(),
                trace_id: None,
            },
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    };
    let Some(run_id) = ctx.agent_run_id else {
        return Err(AppError::not_found("agent run not found"));
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let run = repo::get_run_for_update(&mut tx, run_id).await?;
    let Some(run) = run.filter(|run| {
        ctx.principal_kind == PrincipalKind::Agent
            && run.principal_id == ctx.principal_id
            && credential_matches(ctx, run)
    }) else {
        return Err(AppError::not_found("agent run not found"));
    };
    let Some(updated) = repo::finish_action(&mut tx, run_id, action_id, result_code).await? else {
        return Err(AppError::not_found("agent action not found"));
    };
    if !updated {
        tx.commit().await?;
        return Ok(());
    }
    let mut entry = infrastructure::audit::AuditEntry::new(
        ctx.principal_id,
        "agent.tool_call.finished",
        "agent_action",
        ctx.request_id.clone(),
    )
    .with_target(action_id);
    entry.requested_by = Some(run.requested_by);
    entry.outcome = if result_code == "ok" {
        "ok".into()
    } else {
        format!("failed:{result_code}")
    };
    entry.metadata = serde_json::json!({
        "agent_run_id": run_id,
        "agent_action_id": action_id,
        "result_code": result_code,
    });
    infrastructure::audit::record(&mut tx, entry).await?;
    infrastructure::outbox::emit(
        &mut tx,
        "agent.tool_call.finished",
        "agent_action",
        action_id,
        1,
        serde_json::json!({ "agent_run_id": run_id, "agent_action_id": action_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
