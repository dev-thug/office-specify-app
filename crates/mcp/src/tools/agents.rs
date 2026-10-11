//! Agent-run tools (B.6): list, create, finish and cancel runs through the
//! same application use cases and ACL as the REST `/agent-runs` endpoints.
//! Reading a single run is `research_get_run`.

use crate::{ToolDef, tool_with_db, tool_with_services};
use chrono::{DateTime, Utc};
use contracts::AppError;
use contracts::agents::{
    AgentRunBudget, AgentRunFilters, AgentRunFinishOutcome, AgentRunResponse, AgentRunTargetScope,
    CancelAgentRunRequest, CreateAgentRunRequest, FinishAgentRunRequest,
};
use infrastructure::cursor::Cursor;
use serde_json::Value;
use uuid::Uuid;

fn schema<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes")
}

fn check_key(key: &str) -> Result<(), AppError> {
    if (16..=128).contains(&key.len()) {
        Ok(())
    } else {
        Err(AppError::validation(
            "idempotency_key must contain 16..=128 characters",
            vec![],
        ))
    }
}

fn idempotency_scope(
    ctx: &contracts::Ctx,
    endpoint: &str,
    key: String,
) -> infrastructure::idempotency::Scope {
    infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.into(),
        key,
    }
}

fn hash<T: serde::Serialize>(value: &T) -> Result<[u8; 32], AppError> {
    Ok(infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(value)
            .map_err(|error| AppError::internal(format!("tool hashing: {error}")))?,
    ))
}

/// Tools this module exposes over MCP. Schemas via schemars.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_services(
            "agents_list_runs",
            "List agent runs visible to the caller, newest first, with a signed cursor. Requires agents.run for agents, managers and admins. Optional filters: status (queued|running|awaiting_approval|completed|failed|canceled) and agent_principal_id; use research_get_run for one run's action history.",
            schema::<ListRunsInput>(),
            schema::<RunPage>(),
            false,
            |services, ctx, args| async move {
                let input = crate::arguments::<ListRunsInput>(args)?;
                let limit = match input.limit {
                    None => 50,
                    Some(n @ 1..=200) => i64::from(n),
                    Some(_) => {
                        return Err(AppError::validation(
                            "limit must be between 1 and 200",
                            vec![],
                        ));
                    }
                };
                let filters = AgentRunFilters {
                    status: input.status,
                    agent_principal_id: input.agent_principal_id,
                };
                let filter_hash = Cursor::hash_filter(&serde_json::json!({
                    "tool": "agents_list_runs",
                    "status": filters.status,
                    "agent_principal_id": filters.agent_principal_id,
                }));
                let key = &services.config.hmac_key;
                let after = input
                    .cursor
                    .as_deref()
                    .map(|raw| {
                        let cursor = Cursor::decode(raw, key, ctx.principal_id, &filter_hash)?;
                        let ts = cursor
                            .sort_values
                            .first()
                            .and_then(Value::as_str)
                            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                            .map(|t| t.with_timezone(&Utc));
                        let id = cursor
                            .sort_values
                            .get(1)
                            .and_then(Value::as_str)
                            .and_then(|s| Uuid::parse_str(s).ok());
                        match (ts, id) {
                            (Some(ts), Some(id)) => Ok((ts, id)),
                            _ => Err(AppError::validation("invalid pagination cursor", vec![])),
                        }
                    })
                    .transpose()?;
                let page =
                    application::agents::list_runs(&services.db, &ctx, &filters, after, limit)
                        .await?;
                let next_cursor = page.cursor_seed.map(|(ts, id)| {
                    Cursor::new(
                        vec![serde_json::json!(ts.to_rfc3339()), serde_json::json!(id)],
                        filter_hash.clone(),
                        ctx.principal_id,
                    )
                    .encode(key)
                });
                crate::result(RunPage {
                    runs: page.items.into_iter().map(RunOutput::from).collect(),
                    has_more: page.has_more,
                    next_cursor,
                })
            },
        ),
        tool_with_db(
            "agents_create_run",
            "Queue a new agent run for an active agent principal with an objective, optional target scope (industry, country_code) and optional budget caps. Requires agents.run and an Admin, Manager or Agent role. The scope filters work; it grants no access.",
            schema::<CreateRunInput>(),
            schema::<RunOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<CreateRunInput>(args)?;
                check_key(&input.idempotency_key)?;
                let request = CreateAgentRunRequest {
                    agent_principal_id: input.agent_principal_id,
                    objective: input.objective,
                    scope: input.scope.map(|scope| AgentRunTargetScope {
                        industry: scope.industry,
                        country_code: scope.country_code,
                    }),
                    budget: input.budget.map(|budget| AgentRunBudget {
                        max_tool_calls: budget.max_tool_calls,
                        max_proposals: budget.max_proposals,
                        max_external_searches: budget.max_external_searches,
                        max_input_tokens: budget.max_input_tokens,
                        max_output_tokens: budget.max_output_tokens,
                        max_cost_usd: budget.max_cost_usd,
                    }),
                };
                let request_hash = hash(&request)?;
                let scope = idempotency_scope(
                    &ctx,
                    "MCP tools/call agents_create_run",
                    input.idempotency_key,
                );
                let call_ctx = ctx.clone();
                let response = infrastructure::idempotency::run(
                    &db,
                    &scope,
                    &request_hash,
                    move |mut tx| async move {
                        let body = application::agents::create_run_tx(&mut tx, &call_ctx, &request)
                            .await?;
                        Ok((
                            tx,
                            infrastructure::idempotency::Stored { status: 201, body },
                        ))
                    },
                )
                .await?
                .into_inner()
                .body;
                crate::result(RunOutput::from(response))
            },
        ),
        tool_with_db(
            "agents_finish_run",
            "Finish the caller's own agent run as completed or failed. Requires agents.run; only the owning agent can finish a run, and runs that are canceled, awaiting approval or have unfinished actions are rejected.",
            schema::<FinishRunInput>(),
            schema::<RunOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<FinishRunInput>(args)?;
                check_key(&input.idempotency_key)?;
                let request = FinishAgentRunRequest {
                    outcome: match input.outcome {
                        FinishOutcome::Completed => AgentRunFinishOutcome::Completed,
                        FinishOutcome::Failed => AgentRunFinishOutcome::Failed,
                    },
                };
                let run_id = input.run_id;
                let request_hash = hash(&(run_id, &request))?;
                let scope = idempotency_scope(
                    &ctx,
                    "MCP tools/call agents_finish_run",
                    input.idempotency_key,
                );
                let call_ctx = ctx.clone();
                let response = infrastructure::idempotency::run(
                    &db,
                    &scope,
                    &request_hash,
                    move |mut tx| async move {
                        let body = application::agents::finish_run_tx(
                            &mut tx, &call_ctx, run_id, &request,
                        )
                        .await?;
                        Ok((
                            tx,
                            infrastructure::idempotency::Stored { status: 200, body },
                        ))
                    },
                )
                .await?
                .into_inner()
                .body;
                crate::result(RunOutput::from(response))
            },
        ),
        tool_with_db(
            "agents_cancel_run",
            "Request cancellation of a run owned by the caller or within the caller's management scope. Already-started external actions are reported separately.",
            schema::<CancelRunInput>(),
            schema::<CancelRunOutput>(),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<CancelRunInput>(args)?;
                check_key(&input.idempotency_key)?;
                let body = CancelAgentRunRequest {
                    reason: input.reason,
                };
                let run_id = input.run_id;
                let request_hash = hash(&(run_id, &body))?;
                let scope = idempotency_scope(
                    &ctx,
                    "MCP tools/call agents_cancel_run",
                    input.idempotency_key,
                );
                let call_ctx = ctx.clone();
                let response = infrastructure::idempotency::run(
                    &db,
                    &scope,
                    &request_hash,
                    move |mut tx| async move {
                        let response =
                            application::agents::cancel_run_tx(&mut tx, &call_ctx, run_id, &body)
                                .await?;
                        Ok((
                            tx,
                            infrastructure::idempotency::Stored {
                                status: 200,
                                body: response,
                            },
                        ))
                    },
                )
                .await?
                .into_inner()
                .body;
                let status = serde_json::to_value(response.status)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "unknown".into());
                crate::result(CancelRunOutput {
                    run_id: response.id,
                    status,
                    in_flight_external_actions: response.in_flight_external_actions,
                })
            },
        ),
    ]
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListRunsInput {
    /// `queued`, `running`, `awaiting_approval`, `completed`, `failed`, or `canceled`.
    status: Option<String>,
    agent_principal_id: Option<Uuid>,
    /// Page size, 1..=200. Defaults to 50.
    limit: Option<u32>,
    /// Signed cursor from the previous page; bound to this caller and filter.
    cursor: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunScopeInput {
    industry: Option<String>,
    country_code: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunBudgetInput {
    max_tool_calls: Option<u32>,
    max_proposals: Option<u32>,
    max_external_searches: Option<u32>,
    max_input_tokens: Option<u64>,
    max_output_tokens: Option<u64>,
    /// Estimated cost cap in USD.
    max_cost_usd: Option<f64>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateRunInput {
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    /// Active agent principal that will execute the run.
    agent_principal_id: Uuid,
    /// Purpose of the run, 1..=1000 characters.
    objective: String,
    #[serde(default)]
    scope: Option<RunScopeInput>,
    #[serde(default)]
    budget: Option<RunBudgetInput>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum FinishOutcome {
    Completed,
    Failed,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FinishRunInput {
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    run_id: Uuid,
    outcome: FinishOutcome,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CancelRunInput {
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    run_id: Uuid,
    reason: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct CancelRunOutput {
    run_id: Uuid,
    status: String,
    in_flight_external_actions: i64,
}

/// Mirror of `contracts::agents::AgentRunResponse` (no JsonSchema there).
#[derive(serde::Serialize, schemars::JsonSchema)]
struct RunOutput {
    id: Uuid,
    principal_id: Uuid,
    requested_by: Uuid,
    source_credential_id: Option<Uuid>,
    objective: String,
    /// `queued`, `running`, `awaiting_approval`, `completed`, `failed`, or `canceled`.
    status: String,
    budget: Value,
    usage: Value,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    cancel_requested_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    allowed_actions: Vec<String>,
}

impl From<AgentRunResponse> for RunOutput {
    fn from(value: AgentRunResponse) -> Self {
        Self {
            id: value.id,
            principal_id: value.principal_id,
            requested_by: value.requested_by,
            source_credential_id: value.source_credential_id,
            objective: value.objective,
            status: serde_json::to_value(value.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".into()),
            budget: value.budget,
            usage: value.usage,
            started_at: value.started_at,
            finished_at: value.finished_at,
            cancel_requested_at: value.cancel_requested_at,
            created_at: value.created_at,
            allowed_actions: value.allowed_actions,
        }
    }
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct RunPage {
    runs: Vec<RunOutput>,
    has_more: bool,
    next_cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_agent_run_tools_with_idempotent_mutations() {
        let tools = tools();
        let names: Vec<_> = tools.iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            [
                "agents_list_runs",
                "agents_create_run",
                "agents_finish_run",
                "agents_cancel_run"
            ]
        );
        for tool in &tools[1..] {
            let required = tool.input_schema["required"].as_array().unwrap();
            assert!(
                required.iter().any(|f| f == "idempotency_key"),
                "{}",
                tool.name
            );
        }
    }
}
