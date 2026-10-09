//! agents module repository (spec §6 — owned by the module worker).
//!
//! `agent_runs` + `agent_actions` access. `record_action` is the recording
//! helper MCP tool execution calls in Wave C (task contract — signature is
//! fixed): it stores tool name, argument **hash**, redacted args, result
//! code and the external-effect flag — never raw secrets or model
//! reasoning (§13.5).

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentRunRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub principal_id: Uuid,
    pub requested_by: Uuid,
    pub source_credential_id: Option<Uuid>,
    pub objective: String,
    pub status: String,
    pub budget: serde_json::Value,
    pub usage: serde_json::Value,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

const RUN_COLS: &str = "id, workspace_id, principal_id, requested_by,
        source_credential_id, objective, status, budget, usage,
        started_at, finished_at, cancel_requested_at, created_at";

pub async fn get_run(tx: &mut Tx<'_>, run_id: Uuid) -> Result<Option<AgentRunRow>, AppError> {
    sqlx::query_as::<_, AgentRunRow>(&format!(
        "SELECT {RUN_COLS} FROM agent_runs WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(run_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_run_for_update(
    tx: &mut Tx<'_>,
    run_id: Uuid,
) -> Result<Option<AgentRunRow>, AppError> {
    sqlx::query_as::<_, AgentRunRow>(&format!(
        "SELECT {RUN_COLS} FROM agent_runs WHERE workspace_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tx.workspace_id())
    .bind(run_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Create a queued run (§13.5: 목적·실행 principal·credential·상한 저장).
/// `budget` is already validated by the request DTO — stored verbatim.
#[allow(clippy::too_many_arguments)]
pub async fn insert_run(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    requested_by: Uuid,
    source_credential_id: Option<Uuid>,
    objective: &str,
    budget: &serde_json::Value,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO agent_runs
            (workspace_id, principal_id, requested_by, source_credential_id,
             objective, budget)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .bind(requested_by)
    .bind(source_credential_id)
    .bind(objective)
    .bind(budget)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Target run principal must be an active agent in this workspace.
pub async fn is_active_agent(tx: &mut Tx<'_>, principal_id: Uuid) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM principals
             WHERE workspace_id = $1 AND id = $2
               AND kind = 'agent' AND status = 'active'
        )",
    )
    .bind(tx.workspace_id())
    .bind(principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

#[derive(Debug, Clone, Default)]
pub struct RunListFilter {
    /// Restrict to runs of one executing agent (admin/manager only).
    pub principal_id: Option<Uuid>,
    pub status: Option<String>,
    /// Row-level visibility: `Some(pid)` = only runs requested by pid;
    /// applied for Member callers.
    pub requested_by: Option<Uuid>,
    /// Agent callers: runs executing on their own principal (plus runs they
    /// requested, OR'd by the query).
    pub agent_principal: Option<Uuid>,
}

/// Keyset page ordered `(created_at DESC, id DESC)` (B.1.5 default).
pub async fn list_runs(
    tx: &mut Tx<'_>,
    filter: &RunListFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<AgentRunRow>, AppError> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(&format!(
        "SELECT {RUN_COLS} FROM agent_runs WHERE workspace_id = "
    ));
    qb.push_bind(tx.workspace_id());
    // Visibility: member → own requested; agent → own principal or requested.
    // Admin/Manager pass no restriction.
    if let Some(pid) = filter.requested_by {
        qb.push(" AND requested_by = ");
        qb.push_bind(pid);
    }
    if let Some(pid) = filter.agent_principal {
        qb.push(" AND (principal_id = ");
        qb.push_bind(pid);
        qb.push(" OR requested_by = ");
        qb.push_bind(pid);
        qb.push(")");
    }
    if let Some(pid) = filter.principal_id {
        qb.push(" AND principal_id = ");
        qb.push_bind(pid);
    }
    if let Some(status) = &filter.status {
        qb.push(" AND status = ");
        qb.push_bind(status.clone());
    }
    if let Some((created_at, id)) = after {
        qb.push(" AND (created_at < ");
        qb.push_bind(created_at);
        qb.push(" OR (created_at = ");
        qb.push_bind(created_at);
        qb.push(" AND id < ");
        qb.push_bind(id);
        qb.push("))");
    }
    qb.push(" ORDER BY created_at DESC, id DESC LIMIT ");
    qb.push_bind(limit);
    qb.build_query_as::<AgentRunRow>()
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

/// `POST /agent-runs/{id}/cancel` — atomic transition to `canceled` from a
/// cancelable state (queued/running/awaiting_approval → §16.3 "새 도구 실행
/// 중단"). There is no separate `canceling` status in the DDL CHECK, so a
/// cancel marks the run canceled immediately; in-flight external effects
/// are tracked separately by their own jobs. Returns the updated row or
/// None when the run was already terminal.
pub async fn cancel_run(tx: &mut Tx<'_>, run_id: Uuid) -> Result<Option<AgentRunRow>, AppError> {
    sqlx::query_as::<_, AgentRunRow>(&format!(
        "UPDATE agent_runs
            SET status = 'canceled',
                cancel_requested_at = now(),
                finished_at = now()
          WHERE workspace_id = $1 AND id = $2
            AND status IN ('queued','running','awaiting_approval')
        RETURNING {RUN_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(run_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Atomically start/continue a run and replace its usage object while the
/// caller holds the `FOR UPDATE` lock from `get_run_for_update`.
pub async fn update_tool_usage(
    tx: &mut Tx<'_>,
    run_id: Uuid,
    usage: &serde_json::Value,
) -> Result<bool, AppError> {
    let rows = sqlx::query(
        "UPDATE agent_runs
            SET status = 'running',
                started_at = COALESCE(started_at, now()),
                usage = $3
          WHERE workspace_id = $1 AND id = $2 AND status IN ('queued','running')",
    )
    .bind(tx.workspace_id())
    .bind(run_id)
    .bind(usage)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(rows == 1)
}

pub async fn count_unfinished_actions(tx: &mut Tx<'_>, run_id: Uuid) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM agent_actions
          WHERE workspace_id = $1 AND agent_run_id = $2 AND finished_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(run_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Transition an active run to one of its two server-selected terminal
/// outcomes. Callers hold the run row lock and reject unfinished actions.
pub async fn finish_run(
    tx: &mut Tx<'_>,
    run_id: Uuid,
    outcome: &str,
) -> Result<Option<AgentRunRow>, AppError> {
    sqlx::query_as::<_, AgentRunRow>(&format!(
        "UPDATE agent_runs
            SET status = $3, finished_at = now()
          WHERE workspace_id = $1 AND id = $2
            AND status IN ('queued','running')
        RETURNING {RUN_COLS}"
    ))
    .bind(tx.workspace_id())
    .bind(run_id)
    .bind(outcome)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

// ------------------------------------------------------------- actions

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AgentActionRow {
    pub id: Uuid,
    pub tool_name: String,
    pub arguments_hash: Vec<u8>,
    pub redacted_arguments: serde_json::Value,
    pub result_code: Option<String>,
    pub result_ref: Option<String>,
    pub approval_request_id: Option<Uuid>,
    pub has_external_effect: bool,
    pub trace_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Actions of one run, chronological (§13.5 audit view).
pub async fn list_actions(tx: &mut Tx<'_>, run_id: Uuid) -> Result<Vec<AgentActionRow>, AppError> {
    sqlx::query_as::<_, AgentActionRow>(
        "SELECT id, tool_name, arguments_hash, redacted_arguments,
                result_code, result_ref, approval_request_id,
                has_external_effect, trace_id, started_at, finished_at,
                created_at
           FROM agent_actions
          WHERE workspace_id = $1 AND agent_run_id = $2
          ORDER BY started_at, id",
    )
    .bind(tx.workspace_id())
    .bind(run_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn count_in_flight_external_actions(
    tx: &mut Tx<'_>,
    run_id: Uuid,
) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM agent_actions
          WHERE workspace_id = $1 AND agent_run_id = $2
            AND has_external_effect AND finished_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(run_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Set an action's result exactly once. `None` means missing/wrong-run,
/// `Some(false)` is a harmless duplicate finish, and `Some(true)` performed
/// the transition.
pub async fn finish_action(
    tx: &mut Tx<'_>,
    run_id: Uuid,
    action_id: Uuid,
    result_code: &str,
) -> Result<Option<bool>, AppError> {
    let row: Option<(Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT agent_run_id, finished_at FROM agent_actions
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(action_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some((owner_run_id, finished_at)) = row else {
        return Ok(None);
    };
    if owner_run_id != run_id {
        return Ok(None);
    }
    if finished_at.is_some() {
        return Ok(Some(false));
    }
    let updated = sqlx::query(
        "UPDATE agent_actions
            SET result_code = $4, finished_at = now()
          WHERE workspace_id = $1 AND agent_run_id = $2 AND id = $3 AND finished_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(run_id)
    .bind(action_id)
    .bind(result_code)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(Some(updated == 1))
}

/// Wave C MCP recording helper (task-pinned signature): one tool invocation
/// row inside the caller's transaction. `args_hash` is the raw SHA-256
/// digest (BYTEA — the API renders hex per B.1.1). `redacted_args` must
/// already be redacted by the caller — this function never sees secrets.
/// Returns the action id.
pub async fn record_action(
    tx: &mut Tx<'_>,
    run_id: Uuid,
    tool_name: &str,
    args_hash: &[u8],
    redacted_args: serde_json::Value,
    result_code: Option<&str>,
    has_external_effect: bool,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO agent_actions
            (workspace_id, agent_run_id, tool_name, arguments_hash,
             redacted_arguments, result_code, has_external_effect)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(run_id)
    .bind(tool_name)
    .bind(args_hash)
    .bind(domain::agents::redact_action_arguments(&redacted_args))
    .bind(result_code)
    .bind(has_external_effect)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}
