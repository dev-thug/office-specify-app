//! MCP server — JSON-RPC 2.0 over Streamable HTTP (spec §13.3, B.6.4).
//! Implemented manually (no rmcp): POST `/mcp` handles `initialize`,
//! `notifications/initialized`, `ping`, `tools/list`, `tools/call`.
//! GET `/mcp` → 405. Auth = `Authorization: Bearer osk_…` service credential
//! (deviation X5 — no OAuth discovery; protected-resource metadata → 404).

mod guidance;
pub mod jsonrpc;
pub mod server;
pub mod tools;

use std::sync::Arc;

use contracts::Ctx;

#[derive(Clone)]
pub struct ToolServices {
    pub db: infrastructure::db::Db,
    pub calendar: Arc<dyn infrastructure::providers::CalendarProvider>,
}

/// Protocol version this server speaks.
pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const SERVER_NAME: &str = "office";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A tool exposed over MCP. `input_schema`/`output_schema` are JSON Schemas
/// (schemars-derived by the owning module). The handler receives the
/// verified `Ctx` and the call arguments.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    /// True when the tool causes external side effects (annotated for
    /// clients per MCP spec `destructiveHint`).
    pub external_effect: bool,
    pub handler: Arc<dyn Fn(ToolServices, Ctx, serde_json::Value) -> ToolFuture + Send + Sync>,
}

pub type ToolFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<serde_json::Value, contracts::AppError>> + Send>,
>;

/// Helper for module tool authors.
pub fn tool<F, Fut>(
    name: &'static str,
    description: &'static str,
    input_schema: serde_json::Value,
    output_schema: serde_json::Value,
    external_effect: bool,
    handler: F,
) -> ToolDef
where
    F: Fn(Ctx, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, contracts::AppError>>
        + Send
        + 'static,
{
    ToolDef {
        name,
        description,
        input_schema,
        output_schema: business_output_schema(output_schema),
        external_effect,
        handler: Arc::new(move |_db, ctx, args| Box::pin(handler(ctx, args))),
    }
}

/// Schema matches the business envelope emitted by the transport.
fn business_output_schema(mut data_schema: serde_json::Value) -> serde_json::Value {
    // schemars local references originally resolve at the document root.
    // The transport nests this schema under data, so preserve that scope.
    fn relocate(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(reference)) = map.get_mut("$ref")
                    && let Some(local) = reference.strip_prefix("#/")
                {
                    *reference = format!("#/properties/data/anyOf/0/{local}");
                }
                for value in map.values_mut() {
                    relocate(value);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    relocate(value);
                }
            }
            _ => {}
        }
    }
    relocate(&mut data_schema);
    serde_json::json!({
        "type": "object", "additionalProperties": false,
        "required": ["ok", "data", "error", "request_id"],
        "properties": {
            "ok": {"type": "boolean"},
            "data": {"anyOf": [data_schema, {"type": "null"}]},
            "error": {"anyOf": [{"type": "null"}, {
                "type": "object", "additionalProperties": false,
                "required": ["code", "message", "retryable"],
                "properties": {"code": {"type": "string"}, "message": {"type": "string"}, "retryable": {"type": "boolean"}}
            }]},
            "request_id": {"type": "string"}
        }
    })
}

/// A database-backed tool delegates to the same use cases as REST.
pub fn tool_with_db<F, Fut>(
    name: &'static str,
    description: &'static str,
    input_schema: serde_json::Value,
    output_schema: serde_json::Value,
    external_effect: bool,
    handler: F,
) -> ToolDef
where
    F: Fn(infrastructure::db::Db, Ctx, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, contracts::AppError>>
        + Send
        + 'static,
{
    ToolDef {
        name,
        description,
        input_schema,
        output_schema: business_output_schema(output_schema),
        external_effect,
        handler: Arc::new(move |services, ctx, args| Box::pin(handler(services.db, ctx, args))),
    }
}

/// Provider-aware tools receive process-owned adapters, never client configuration.
pub fn tool_with_services<F, Fut>(
    name: &'static str,
    description: &'static str,
    input_schema: serde_json::Value,
    output_schema: serde_json::Value,
    external_effect: bool,
    handler: F,
) -> ToolDef
where
    F: Fn(ToolServices, Ctx, serde_json::Value) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<serde_json::Value, contracts::AppError>>
        + Send
        + 'static,
{
    ToolDef {
        name,
        description,
        input_schema,
        output_schema: business_output_schema(output_schema),
        external_effect,
        handler: Arc::new(move |services, ctx, args| Box::pin(handler(services, ctx, args))),
    }
}

/// Reject unknown and system-owned input fields through each typed DTO.
pub fn arguments<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
) -> Result<T, contracts::AppError> {
    serde_json::from_value(value).map_err(|e| {
        contracts::AppError::validation(format!("invalid tool arguments: {e}"), vec![])
    })
}

pub fn result<T: serde::Serialize>(value: T) -> Result<serde_json::Value, contracts::AppError> {
    serde_json::to_value(value)
        .map_err(|e| contracts::AppError::internal(format!("tool result: {e}")))
}

/// All registered tools — each module contributes via `tools::<m>::tools()`.
pub fn all_tools() -> Vec<ToolDef> {
    all_tools_with_key(&[])
}

/// Runtime registry binds the configured cursor signing key.
pub fn all_tools_with_key(hmac_key: &[u8]) -> Vec<ToolDef> {
    let mut v = Vec::new();
    v.extend(tools::agents::tools());
    v.extend(tools::approvals::tools());
    v.extend(tools::booking::tools());
    v.extend(tools::campaigns::tools());
    v.extend(tools::sales_campaigns::tools_with_key(hmac_key));
    v.extend(tools::compliance::tools());
    v.extend(tools::crm::tools_with_key(hmac_key));
    v.extend(tools::discovery::tools_with_key(hmac_key));
    v.extend(tools::imports::tools());
    v.extend(tools::inbox::tools_with_key(hmac_key));
    v.extend(tools::mailboxes::tools());
    v.extend(tools::operations::tools());
    v.extend(tools::org::tools());
    v.extend(tools::research::tools());
    v.extend(tools::sending::tools());
    v.extend(tools::templates::tools());
    for tool in &mut v {
        guidance::enrich(tool);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        id: uuid::Uuid,
    }

    #[test]
    fn catalog_contains_every_approved_agent_tool_and_no_privileged_actions() {
        let definitions = all_tools();
        let names: std::collections::HashSet<_> = definitions.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), definitions.len(), "duplicate tool names");
        for expected in [
            "crm_search_companies",
            "crm_get_company",
            "crm_get_contact",
            "mail_get_thread",
            "sales_campaigns_list",
            "sales_campaigns_create",
            "sales_campaigns_get",
            "sales_campaigns_add_target",
            "sales_campaigns_create_message",
            "sales_campaigns_move_stage",
            "sales_campaigns_queue",
            "mail_list_threads",
            "mail_search_messages",
            "mail_update_thread_status",
            "mail_set_thread_read",
            "mail_assign_thread",
            "mail_get_outbound",
            "research_propose_leads",
            "research_add_evidence",
            "research_get_run",
            "crm_propose_update",
            "crm_create_note",
            "mail_create_draft",
            "mail_create_reply_draft",
            "templates_create",
            "mail_submit_approved",
            "mail_update_thread_status",
            "mail_set_thread_read",
            "mail_assign_thread",
            "templates_create_draft_version",
            "campaign_create_draft",
            "campaign_get_launch",
            "campaign_submit_approved",
            "campaign_preflight",
            "campaign_request_launch",
            "campaign_pause",
            "booking_list_slots",
            "booking_propose",
            "booking_get",
            "operations_get_job",
            "approvals_get_request",
            "agents_cancel_run",
        ] {
            assert!(names.contains(expected), "missing B.6 tool: {expected}");
        }
        for forbidden in [
            "mail_submit",
            "approvals_decide",
            "mailbox_grant",
            "suppression_release",
        ] {
            assert!(!names.contains(forbidden));
        }
    }

    #[test]
    fn mutating_tools_require_explicit_idempotency_keys() {
        let definitions = all_tools();
        for name in [
            "sales_campaigns_create",
            "sales_campaigns_add_target",
            "sales_campaigns_create_message",
            "sales_campaigns_move_stage",
            "sales_campaigns_queue",
            "research_propose_leads",
            "research_add_evidence",
            "crm_propose_update",
            "crm_create_note",
            "mail_create_draft",
            "mail_create_reply_draft",
            "templates_create",
            "mail_submit_approved",
            "mail_update_thread_status",
            "mail_set_thread_read",
            "mail_assign_thread",
            "templates_create_draft_version",
            "campaign_create_draft",
            "campaign_submit_approved",
            "campaign_request_launch",
            "campaign_pause",
            "booking_propose",
            "agents_cancel_run",
        ] {
            let definition = definitions.iter().find(|tool| tool.name == name).unwrap();
            assert!(
                definition.input_schema["required"]
                    .as_array()
                    .is_some_and(|fields| fields.iter().any(|field| field == "idempotency_key")),
                "{name} must require an idempotency key"
            );
        }
    }

    #[test]
    fn arguments_reject_system_owned_context() {
        let id = uuid::Uuid::new_v4();
        let parsed: Input = arguments(serde_json::json!({"id": id})).unwrap();
        assert_eq!(parsed.id, id);
        assert!(arguments::<Input>(serde_json::json!({"id": id, "workspace_id": id})).is_err());
        assert!(arguments::<Input>(serde_json::json!({"id": "invalid"})).is_err());
    }

    #[test]
    fn output_schema_describes_transport_envelope() {
        let definition = tool(
            "test",
            "test",
            serde_json::json!({}),
            serde_json::json!({"type": "object"}),
            false,
            |_ctx, _args| async { Ok(serde_json::json!({})) },
        );
        assert_eq!(
            definition.output_schema["required"],
            serde_json::json!(["ok", "data", "error", "request_id"])
        );
        assert_eq!(
            definition.output_schema["properties"]["data"]["anyOf"][0]["type"],
            "object"
        );
        let nested = business_output_schema(serde_json::json!({
            "properties": {"item": {"$ref": "#/definitions/Item"}}, "definitions": {"Item": {"type":"string"}}
        }));
        assert_eq!(
            nested["properties"]["data"]["anyOf"][0]["properties"]["item"]["$ref"],
            "#/properties/data/anyOf/0/definitions/Item"
        );
    }
}
