//! JSON-RPC 2.0 wire types + MCP result shapes (B.6.4).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: Option<String>,
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl RpcRequest {
    /// Notifications carry no `id` member.
    pub fn is_notification(&self) -> bool {
        self.id.is_null()
    }
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// Application errors map to this server-defined range.
pub const APP_ERROR: i64 = -32000;

impl RpcResponse {
    pub fn result(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            result: Some(result),
            error: None,
        }
    }
    pub fn error(id: Option<Value>, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
    pub fn app_error(id: Value, e: &contracts::AppError) -> Self {
        Self {
            jsonrpc: "2.0",
            id: Some(id),
            result: None,
            error: Some(RpcError {
                code: APP_ERROR,
                message: e.code.as_str().to_string(),
                data: Some(serde_json::json!({
                    "code": e.code.as_str(),
                    "detail": e.detail,
                    "status": e.status(),
                })),
            }),
        }
    }
}

/// `tools/call` result per B.6.4: `structuredContent` + matching short text
/// content + `isError`.
pub fn tool_result_ok(structured: Value) -> Value {
    let text = serde_json::to_string(&structured).unwrap_or_else(|_| "{}".into());
    serde_json::json!({
        "structuredContent": structured,
        "content": [{ "type": "text", "text": text }],
        "isError": false,
    })
}

pub fn tool_result_err(code: &str, detail: &str) -> Value {
    serde_json::json!({
        "structuredContent": { "code": code, "detail": detail },
        "content": [{ "type": "text", "text": format!("{code}: {detail}") }],
        "isError": true,
    })
}

/// `initialize` result.
pub fn initialize_result() -> Value {
    serde_json::json!({
        "protocolVersion": crate::PROTOCOL_VERSION,
        "serverInfo": { "name": crate::SERVER_NAME, "version": crate::SERVER_VERSION },
        "capabilities": { "tools": {} },
    })
}
