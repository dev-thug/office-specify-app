//! Approvals MCP tools (B.6.2). Mirrors the REST approvals routes: list,
//! read, decide, and revoke. Every tool is gated by the credential's scopes
//! and the same application-layer checks REST uses.

use crate::{ToolDef, arguments, result, tool_with_db, tool_with_services};
use contracts::approvals::{
    ApprovalRequestDetail, ApprovalRequestSummary, CreateDecisionRequest, DecisionResponse,
};
use contracts::{AppError, ErrorCode, Violation};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::idempotency::{self, Stored};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApprovalsGetRequestInput {
    approval_request_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApprovalsListInput {
    /// Opaque next_cursor from a previous page; reuse with identical filters.
    cursor: Option<String>,
    /// Page size 1..=200 (default 50).
    limit: Option<i64>,
    /// pending | approved | rejected | expired | revoked
    status: Option<String>,
    /// campaign_launch | message_submit | booking_change | suppression_release
    action_type: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
struct ApprovalsListOutput {
    items: Vec<ApprovalRequestSummary>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApprovalsDecideInput {
    /// Stable caller key, 16..=128 characters.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    approval_request_id: Uuid,
    /// Current approval request version (If-Match).
    expected_version: i64,
    /// approved | rejected
    decision: String,
    /// sha256 hex of the snapshot that was actually reviewed.
    expected_snapshot_hash: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApprovalsRevokeInput {
    approval_request_id: Uuid,
    /// Current approval request version (If-Match).
    expected_version: i64,
    reason: Option<String>,
}

fn object_schema() -> serde_json::Value {
    serde_json::json!({"type": "object"})
}

fn validate_idempotency_key(key: &str) -> Result<(), AppError> {
    if (16..=128).contains(&key.len()) {
        Ok(())
    } else {
        Err(AppError::validation(
            "idempotency_key must be 16..=128 characters",
            vec![Violation {
                field: "idempotency_key".into(),
                code: "LENGTH".into(),
            }],
        ))
    }
}

/// Same denied-attempt audit as the REST handler: written in its own tx
/// because the business tx already rolled back.
async fn audit_denied_decision(
    db: &infrastructure::db::Db,
    ctx: &contracts::Ctx,
    approval_request_id: Uuid,
    err: &AppError,
) {
    if !matches!(
        err.code,
        ErrorCode::Forbidden
            | ErrorCode::ScopeRequired
            | ErrorCode::SelfApprovalForbidden
            | ErrorCode::MailboxAccessDenied
            | ErrorCode::PrincipalDisabled
            | ErrorCode::AgentDirectSendForbidden
    ) {
        return;
    }
    if let Ok(mut tx) = db.begin(ctx.workspace_id).await {
        let _ = audit::record(
            &mut tx,
            AuditEntry::new(
                ctx.principal_id,
                "approval.decide",
                "approval_request",
                ctx.request_id.clone(),
            )
            .with_target(approval_request_id)
            .denied(&err.code),
        )
        .await;
        let _ = tx.commit().await;
    }
}

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
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
        ),
        tool_with_services(
            "approvals_list",
            "List approval requests the principal may decide or has requested. Requires approvals.read. Optional status/action_type filters; follow next_cursor with identical filters.",
            serde_json::to_value(schemars::schema_for!(ApprovalsListInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(ApprovalsListOutput))
                .expect("schema serializes"),
            false,
            |services, ctx, value| async move {
                let input = arguments::<ApprovalsListInput>(value)?;
                let (items, page) = application::approvals::list(
                    &services.db,
                    &ctx,
                    input.status.as_deref(),
                    input.action_type.as_deref(),
                    input.cursor.as_deref(),
                    input.limit.unwrap_or(50).clamp(1, 200),
                    &services.config.hmac_key,
                )
                .await?;
                result(ApprovalsListOutput {
                    items,
                    next_cursor: page.next_cursor,
                    has_more: page.has_more,
                })
            },
        ),
        tool_with_db(
            "approvals_decide",
            "Approve or reject a pending approval request against the exact snapshot hash reviewed. Requires an admin/manager role (agents: the mail.approve scope), expected_version and a stable idempotency_key; self-approval is rejected. Approval does not itself execute the action.",
            serde_json::to_value(schemars::schema_for!(ApprovalsDecideInput))
                .expect("schema serializes"),
            object_schema(),
            false,
            |db, ctx, value| async move {
                let input = arguments::<ApprovalsDecideInput>(value)?;
                validate_idempotency_key(&input.idempotency_key)?;
                let id = input.approval_request_id;
                let expected_version = input.expected_version;
                let req = CreateDecisionRequest {
                    decision: input.decision,
                    expected_snapshot_hash: input.expected_snapshot_hash,
                    reason: input.reason,
                };
                let hash = infrastructure::crypto::sha256_bytes(
                    &serde_json::to_vec(&serde_json::json!({
                        "approval_request_id": id,
                        "if_match": expected_version,
                        "body": &req,
                    }))
                    .unwrap_or_default(),
                );
                let scope = idempotency::Scope {
                    workspace_id: ctx.workspace_id,
                    principal_or_capability: ctx.principal_id.to_string(),
                    endpoint: "POST /approvals/{id}/decisions".to_string(),
                    key: input.idempotency_key,
                };
                let outcome = idempotency::run(&db, &scope, &hash, |mut tx| {
                    let ctx = ctx.clone();
                    let req = &req;
                    async move {
                        let body: DecisionResponse = application::approvals::decide(
                            &mut tx,
                            &ctx,
                            id,
                            expected_version,
                            req,
                        )
                        .await?;
                        Ok((tx, Stored { status: 200, body }))
                    }
                })
                .await;
                match outcome {
                    Ok(out) => result(out.into_inner().body),
                    Err(e) => {
                        audit_denied_decision(&db, &ctx, id, &e).await;
                        Err(e)
                    }
                }
            },
        ),
        tool_with_db(
            "approvals_revoke",
            "Revoke a pending approval request you requested before its side effect executes. Requires expected_version of the approval request; optional reason is recorded.",
            serde_json::to_value(schemars::schema_for!(ApprovalsRevokeInput))
                .expect("schema serializes"),
            object_schema(),
            false,
            |db, ctx, value| async move {
                let input = arguments::<ApprovalsRevokeInput>(value)?;
                let mut tx = db.begin(ctx.workspace_id).await?;
                let body = application::approvals::revoke(
                    &mut tx,
                    &ctx,
                    input.approval_request_id,
                    input.expected_version,
                    input.reason.as_deref(),
                )
                .await?;
                tx.commit().await?;
                result(body)
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(name: &str) -> ToolDef {
        tools().into_iter().find(|t| t.name == name).unwrap()
    }

    #[test]
    fn exposes_read_only_approval_request_lookup() {
        let tool = find("approvals_get_request");
        assert!(!tool.external_effect);
        assert_eq!(tool.input_schema["additionalProperties"], false);
        assert_eq!(
            tool.input_schema["required"],
            serde_json::json!(["approval_request_id"])
        );
    }

    #[test]
    fn exposes_list_decide_and_revoke() {
        let names: Vec<_> = tools().iter().map(|t| t.name).collect();
        assert_eq!(
            names,
            vec![
                "approvals_get_request",
                "approvals_list",
                "approvals_decide",
                "approvals_revoke"
            ]
        );
        let decide = find("approvals_decide");
        let required = decide.input_schema["required"].as_array().unwrap();
        for field in [
            "idempotency_key",
            "approval_request_id",
            "expected_version",
            "decision",
            "expected_snapshot_hash",
        ] {
            assert!(required.iter().any(|n| n == field), "missing {field}");
        }
        assert!(
            arguments::<ApprovalsDecideInput>(serde_json::json!({
                "idempotency_key": "decide-key-0000001",
                "approval_request_id": Uuid::new_v4(),
                "expected_version": 1,
                "decision": "approved",
                "expected_snapshot_hash": "ab".repeat(32),
                "workspace_id": Uuid::new_v4()
            }))
            .is_err()
        );
        assert!(validate_idempotency_key("short").is_err());
        let revoke = find("approvals_revoke");
        let required = revoke.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|n| n == "expected_version"));
    }
}
