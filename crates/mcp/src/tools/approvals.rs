//! Approvals MCP tools (B.6.2). The tool exposes read-only request detail;
//! only humans using the reviewed UI can decide or revoke approvals.

use crate::{ToolDef, arguments, result, tool_with_db};
use contracts::approvals::ApprovalRequestDetail;
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApprovalsGetRequestInput {
    approval_request_id: Uuid,
}

pub fn tools() -> Vec<ToolDef> {
    vec![tool_with_db(
        "approvals_get_request",
        "Read the current status, requester, target hash, and allowed next action for an approval request visible to the authenticated principal. This does not decide or execute the request.",
        serde_json::to_value(schemars::schema_for!(ApprovalsGetRequestInput))
            .expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(ApprovalRequestDetail))
            .expect("schema serializes"),
        false,
        |db, ctx, value| async move {
            let input = arguments::<ApprovalsGetRequestInput>(value)?;
            let response =
                application::approvals::get(&db, &ctx, input.approval_request_id).await?;
            result(response)
        },
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_read_only_approval_request_lookup() {
        let tools = tools();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "approvals_get_request");
        assert!(!tools[0].external_effect);
        assert_eq!(tools[0].input_schema["additionalProperties"], false);
        assert_eq!(
            tools[0].input_schema["required"],
            serde_json::json!(["approval_request_id"])
        );
    }
}
