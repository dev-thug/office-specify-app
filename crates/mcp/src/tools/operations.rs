//! operations module MCP tools (B.6).

use crate::{ToolDef, tool, tool_with_db};
use contracts::operations::JobResponse;
use uuid::Uuid;

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    vec![
        tool(
            "ops.ping",
            "Liveness check — returns ok:true for the calling principal.",
            serde_json::to_value(schemars::schema_for!(OpsPingInput)).expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(OpsPingOutput)).expect("schema serializes"),
            false,
            |ctx, _args| async move {
                Ok(serde_json::json!({
                    "ok": true,
                    "workspace_id": ctx.workspace_id,
                    "principal_id": ctx.principal_id,
                }))
            },
        ),
        tool_with_db(
            "operations_get_job",
            "Read progress, safe result references, and retryability for a job in the caller's allowed scope.",
            serde_json::to_value(schemars::schema_for!(GetJobInput)).expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(GetJobOutput)).expect("schema serializes"),
            false,
            |db, ctx, args| async move {
                let input = crate::arguments::<GetJobInput>(args)?;
                let job = application::operations::jobs::get_job(&db, &ctx, input.job_id).await?;
                crate::result(GetJobOutput::from(job))
            },
        ),
    ]
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpsPingInput {}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct OpsPingOutput {
    ok: bool,
    workspace_id: uuid::Uuid,
    principal_id: uuid::Uuid,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetJobInput {
    job_id: Uuid,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct GetJobOutput {
    job_id: Uuid,
    kind: String,
    state: String,
    retryable: bool,
    external_effect: bool,
    result_refs: Vec<JobToolResultRef>,
    lease_owner: Option<String>,
    lease_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    last_error_code: Option<String>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
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
