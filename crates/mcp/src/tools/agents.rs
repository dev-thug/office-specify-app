//! Agent tools (B.6): cancellation is delegated to the same application
//! use case and ACL used by the REST endpoint.

use crate::{ToolDef, tool_with_db};
use contracts::agents::CancelAgentRunRequest;
use uuid::Uuid;

/// Tools this module exposes over MCP. Schemas via schemars.
pub fn tools() -> Vec<ToolDef> {
    vec![tool_with_db(
        "agents_cancel_run",
        "Request cancellation of a run owned by the caller or within the caller's management scope. Already-started external actions are reported separately.",
        serde_json::to_value(schemars::schema_for!(CancelRunInput)).expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(CancelRunOutput)).expect("schema serializes"),
        false,
        |db, ctx, args| async move {
            let input = crate::arguments::<CancelRunInput>(args)?;
            if !(16..=128).contains(&input.idempotency_key.len()) {
                return Err(contracts::AppError::validation(
                    "idempotency_key must contain 16..=128 characters",
                    vec![],
                ));
            }
            let body = CancelAgentRunRequest {
                reason: input.reason,
            };
            let hash = infrastructure::crypto::sha256_bytes(
                &serde_json::to_vec(&(input.run_id, &body)).map_err(|error| {
                    contracts::AppError::internal(format!("tool hashing: {error}"))
                })?,
            );
            let scope = infrastructure::idempotency::Scope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: ctx.principal_id.to_string(),
                endpoint: "MCP tools/call agents_cancel_run".into(),
                key: input.idempotency_key,
            };
            let call_ctx = ctx.clone();
            let response =
                infrastructure::idempotency::run(&db, &scope, &hash, move |mut tx| async move {
                    let response =
                        application::agents::cancel_run_tx(&mut tx, &call_ctx, input.run_id, &body)
                            .await?;
                    Ok((
                        tx,
                        infrastructure::idempotency::Stored {
                            status: 200,
                            body: response,
                        },
                    ))
                })
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
    )]
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
