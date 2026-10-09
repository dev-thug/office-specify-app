//! MCP server — JSON-RPC 2.0 over Streamable HTTP (spec §13.3, B.6.4).
//! Implemented manually (no rmcp): POST `/mcp` handles `initialize`,
//! `notifications/initialized`, `ping`, `tools/list`, `tools/call`.
//! GET `/mcp` → 405. Auth = `Authorization: Bearer osk_…` service credential
//! (deviation X5 — no OAuth discovery; protected-resource metadata → 404).

pub mod jsonrpc;
pub mod server;
pub mod tools;

use std::sync::Arc;

use contracts::Ctx;

/// Protocol version this server speaks.
pub const PROTOCOL_VERSION: &str = "2025-11-25";
pub const SERVER_NAME: &str = "outreach-os";
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
    pub handler: Arc<dyn Fn(Ctx, serde_json::Value) -> ToolFuture + Send + Sync>,
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
        output_schema,
        external_effect,
        handler: Arc::new(move |ctx, args| Box::pin(handler(ctx, args))),
    }
}

/// All registered tools — each module contributes via `tools::<m>::tools()`.
pub fn all_tools() -> Vec<ToolDef> {
    let mut v = Vec::new();
    v.extend(tools::agents::tools());
    v.extend(tools::approvals::tools());
    v.extend(tools::booking::tools());
    v.extend(tools::campaigns::tools());
    v.extend(tools::compliance::tools());
    v.extend(tools::crm::tools());
    v.extend(tools::imports::tools());
    v.extend(tools::inbox::tools());
    v.extend(tools::mailboxes::tools());
    v.extend(tools::operations::tools());
    v.extend(tools::org::tools());
    v.extend(tools::research::tools());
    v.extend(tools::sending::tools());
    v.extend(tools::templates::tools());
    v
}
