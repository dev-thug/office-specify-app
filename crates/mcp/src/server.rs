//! Axum transport for MCP (spec §13.3, B.6.4).
//!
//! - POST `/mcp` — JSON-RPC requests and notifications.
//!   Requests → JSON response; notifications → 202 with empty body.
//!   A JSON array body is a batch; notifications inside batches are dropped
//!   per JSON-RPC 2.0.
//! - GET `/mcp` → 405 (Streamable HTTP SSE stream is not implemented).
//! - Auth: `Authorization: Bearer osk_…` service credential. Origin is
//!   checked against `Config.mcp_allowed_origins` when the header is
//!   present (browser safety; non-browser clients may omit it).
//! - `.well-known/oauth-protected-resource` → 404 (X5 — no OAuth).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use contracts::{AuthMethod, Ctx, PrincipalKind, ScopeSet};
use serde_json::Value;

use crate::jsonrpc::*;
use crate::{ToolDef, all_tools};

use infrastructure::db::Db;
use infrastructure::repo::auth as auth_repo;

#[derive(Clone)]
pub struct McpState {
    pub db: Db,
    pub allowed_origins: Arc<Vec<String>>,
    pub tools: Arc<Vec<ToolDef>>,
}

pub fn build_router(state: McpState) -> Router {
    Router::new()
        .route("/mcp", post(handle_post).get(mcp_get_not_allowed))
        .route(
            "/.well-known/oauth-protected-resource",
            get(|| async { StatusCode::NOT_FOUND }),
        )
        .with_state(state)
}

async fn mcp_get_not_allowed() -> impl IntoResponse {
    (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed")
}

fn check_origin(state: &McpState, headers: &HeaderMap) -> Result<(), (StatusCode, &'static str)> {
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !state.allowed_origins.iter().any(|o| o == origin)
    {
        return Err((StatusCode::FORBIDDEN, "origin not allowed"));
    }
    Ok(())
}

async fn authenticate(state: &McpState, headers: &HeaderMap) -> Result<Ctx, (StatusCode, String)> {
    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "bearer credential required".into(),
        ));
    };
    let cred = auth_repo::lookup_service_credential(&state.db, auth)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::UNAUTHORIZED,
                "invalid or expired credential".into(),
            )
        })?;
    if cred.principal.status != "active" {
        return Err((StatusCode::FORBIDDEN, "PRINCIPAL_DISABLED".into()));
    }
    let mut scopes = ScopeSet::from_strings(cred.scopes.iter());
    if cred.principal.principal_kind() == PrincipalKind::Agent {
        // Agents never get forbidden scopes even if issued (§5.4).
        let kept: Vec<_> = scopes
            .iter()
            .filter(|s| !contracts::Scope::AGENT_FORBIDDEN.contains(s))
            .collect();
        scopes = ScopeSet::of(kept);
    }
    Ok(Ctx {
        request_id: uuid::Uuid::new_v4().to_string(),
        workspace_id: cred.principal.workspace_id,
        principal_id: cred.principal.id,
        principal_kind: cred.principal.principal_kind(),
        role: cred.principal.role(),
        scopes,
        permission_version: cred.principal.permission_version,
        auth: AuthMethod::ServiceCredential {
            credential_id: cred.credential_id,
        },
        agent_run_id: None,
    })
}

async fn handle_post(
    State(state): State<McpState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    if let Err(e) = check_origin(&state, &headers) {
        return e.into_response();
    }
    let ctx = match authenticate(&state, &headers).await {
        Ok(c) => c,
        Err((status, msg)) => {
            // MCP auth failure → 401 JSON-RPC error body (B.6.4).
            return (status, Json(RpcResponse::error(None, APP_ERROR, msg))).into_response();
        }
    };

    let parsed: Result<Value, _> = serde_json::from_slice(&body);
    let Ok(value) = parsed else {
        return (
            StatusCode::BAD_REQUEST,
            Json(RpcResponse::error(None, PARSE_ERROR, "parse error")),
        )
            .into_response();
    };

    // Batch support.
    if let Value::Array(items) = value {
        let mut out = Vec::new();
        for item in items {
            let Ok(req) = serde_json::from_value::<RpcRequest>(item) else {
                out.push(RpcResponse::error(None, INVALID_REQUEST, "invalid request"));
                continue;
            };
            if req.is_notification() {
                dispatch_notification(&req.method);
                continue;
            }
            out.push(dispatch(&state, &ctx, req).await);
        }
        if out.is_empty() {
            return StatusCode::ACCEPTED.into_response();
        }
        return Json(out).into_response();
    }

    let Ok(req) = serde_json::from_value::<RpcRequest>(value) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(RpcResponse::error(None, INVALID_REQUEST, "invalid request")),
        )
            .into_response();
    };
    if req.jsonrpc.as_deref() != Some("2.0") {
        return (
            StatusCode::BAD_REQUEST,
            Json(RpcResponse::error(
                Some(req.id),
                INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            )),
        )
            .into_response();
    }
    if req.is_notification() {
        dispatch_notification(&req.method);
        return StatusCode::ACCEPTED.into_response();
    }
    Json(dispatch(&state, &ctx, req).await).into_response()
}

fn dispatch_notification(method: &str) {
    // `notifications/initialized` and friends are acknowledged; nothing to do.
    let _ = method;
}

async fn dispatch(state: &McpState, ctx: &Ctx, req: RpcRequest) -> RpcResponse {
    match req.method.as_str() {
        "initialize" => RpcResponse::result(req.id, initialize_result()),
        "ping" => RpcResponse::result(req.id, serde_json::json!({})),
        "tools/list" => {
            let tools: Vec<Value> = state
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "inputSchema": t.input_schema,
                        "outputSchema": t.output_schema,
                        "annotations": { "destructiveHint": t.external_effect },
                    })
                })
                .collect();
            RpcResponse::result(req.id, serde_json::json!({ "tools": tools }))
        }
        "tools/call" => {
            let name = req.params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or(Value::Object(Default::default()));
            let Some(tool) = state.tools.iter().find(|t| t.name == name) else {
                return RpcResponse::error(
                    Some(req.id),
                    METHOD_NOT_FOUND,
                    format!("unknown tool: {name}"),
                );
            };
            match (tool.handler)(ctx.clone(), args).await {
                Ok(v) => RpcResponse::result(req.id, tool_result_ok(v)),
                Err(e) => RpcResponse::result(req.id, tool_result_err(e.code.as_str(), &e.detail)),
            }
        }
        // Notifications sent as requests are acknowledged silently.
        m if m.starts_with("notifications/") => RpcResponse::result(req.id, Value::Null),
        _ => RpcResponse::error(
            Some(req.id),
            METHOD_NOT_FOUND,
            format!("method not found: {}", req.method),
        ),
    }
}

/// Exported tool catalog for `outreach-cli mcp-schema` → contracts/mcp/tools.json.
pub fn tools_document() -> serde_json::Value {
    let tools: Vec<Value> = all_tools()
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
                "outputSchema": t.output_schema,
                "externalEffect": t.external_effect,
            })
        })
        .collect();
    serde_json::json!({
        "protocolVersion": crate::PROTOCOL_VERSION,
        "server": { "name": crate::SERVER_NAME, "version": crate::SERVER_VERSION },
        "tools": tools,
    })
}
