//! operations module MCP tools (spec §6 — owned by the module worker).
//! `ops.ping` is a foundation smoke tool proving the registry end-to-end.

use crate::{ToolDef, tool};

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    vec![tool(
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
    )]
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
