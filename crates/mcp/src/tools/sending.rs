//! Sending module MCP tools (B.6). Mirrors the REST draft/outbound routes:
//! draft list/read/update/freeze, outbound preflight, approval request,
//! submit, approved submit, status and cancel.

use crate::{ToolDef, arguments, result, tool_with_db, tool_with_services};
use chrono::{DateTime, Utc};
use contracts::{
    AppError, Page, PrincipalKind, Violation,
    sending::{
        CancelOutboundRequest, CreateMessageDraftRequest, CreateOutboundApprovalRequest,
        FreezeMessageDraftRequest, MessageDraftListItem, OutboundMessageResponse,
        PatchMessageDraftRequest, SubmitOutboundRequest, SubmitOutboundResponse,
    },
};
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

/// Same per-principal submit limit as the REST `submit_outbound` handler.
const SUBMIT_RATE_LIMIT_PER_MINUTE: u32 = 60;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailCreateDraftInput {
    /// Stable caller key, 16..=128 characters.
    idempotency_key: String,
    mail_account_id: uuid::Uuid,
    to_email_address_id: uuid::Uuid,
    purpose: String,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    template_version_id: Option<uuid::Uuid>,
    variables: Option<serde_json::Map<String, serde_json::Value>>,
    thread_id: Option<uuid::Uuid>,
    reply_to_message_id: Option<uuid::Uuid>,
    expected_last_inbound_id: Option<uuid::Uuid>,
    visibility: Option<String>,
}

impl MailCreateDraftInput {
    fn into_parts(self) -> (String, CreateMessageDraftRequest) {
        let request = CreateMessageDraftRequest {
            mail_account_id: self.mail_account_id,
            to_email_address_id: self.to_email_address_id,
            purpose: self.purpose,
            subject: self.subject,
            body_text: self.body_text,
            body_html: self.body_html,
            template_version_id: self.template_version_id,
            variables: self.variables,
            thread_id: self.thread_id,
            reply_to_message_id: self.reply_to_message_id,
            expected_last_inbound_id: self.expected_last_inbound_id,
            visibility: self.visibility,
        };
        (self.idempotency_key, request)
    }
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
struct MailCreateDraftOutput {
    draft_id: uuid::Uuid,
    version: i64,
    status: String,
    approval_required: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailSubmitApprovedInput {
    /// Stable ASCII caller key, 16..=128 characters.
    idempotency_key: String,
    outbound_message_id: uuid::Uuid,
    /// Must equal the frozen hash reviewed by the independent human approver.
    expected_payload_hash: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailGetOutboundInput {
    outbound_message_id: uuid::Uuid,
}

fn valid_approved_submit_key(key: &str) -> bool {
    key.is_ascii() && (16..=128).contains(&key.len())
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailListDraftsInput {
    /// Page size 1..=200 (default 50).
    limit: Option<u32>,
    /// Opaque next_cursor from a previous page; reuse with identical filters.
    cursor: Option<String>,
    thread_id: Option<Uuid>,
}

#[derive(Debug, serde::Serialize)]
struct MailListDraftsOutput {
    items: Vec<MessageDraftListItem>,
    next_cursor: Option<String>,
    has_more: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailGetDraftInput {
    draft_id: Uuid,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailUpdateDraftInput {
    draft_id: Uuid,
    /// Current draft version (If-Match).
    expected_version: i64,
    /// Fields to change: subject, body_text, body_html, variables,
    /// to_email_address_id, expected_last_inbound_id, visibility.
    changes: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailFreezeDraftInput {
    /// Stable caller key, 16..=128 characters.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    draft_id: Uuid,
    /// Current draft version (If-Match).
    expected_version: i64,
    delivery_channel_id: Uuid,
    /// Must equal the channel's bound sender identity when supplied.
    sender_identity_id: Option<Uuid>,
    /// Required when the template uses meeting.booking_url.
    meeting_type_id: Option<Uuid>,
    /// Request open/click tracking (workspace settings may disable it).
    #[serde(default)]
    tracking_requested: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailOutboundIdInput {
    outbound_message_id: Uuid,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailRequestApprovalInput {
    /// Stable caller key, 16..=128 characters.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    outbound_message_id: Uuid,
    reason: String,
    /// Must equal the outbound's current payload_hash.
    expected_payload_hash: String,
    /// Defaults to now + 7 days.
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailSubmitInput {
    /// Stable caller key, 16..=128 characters.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    outbound_message_id: Uuid,
    /// Must equal the outbound's current payload_hash.
    expected_payload_hash: String,
    not_before: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MailCancelOutboundInput {
    outbound_message_id: Uuid,
    /// Current outbound version (If-Match).
    expected_version: i64,
    reason: Option<String>,
}

fn object_schema() -> serde_json::Value {
    serde_json::json!({"type": "object"})
}

fn require_idempotency_key(key: &str) -> Result<(), AppError> {
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

/// Mirrors the REST `list_drafts` cursor handling exactly.
async fn list_drafts(
    services: crate::ToolServices,
    ctx: contracts::Ctx,
    input: MailListDraftsInput,
) -> Result<MailListDraftsOutput, AppError> {
    let hmac_key = &services.config.hmac_key;
    let filter = serde_json::json!({
        "kind": "message_drafts",
        "thread_id": input.thread_id,
    });
    let filter_hash = infrastructure::cursor::Cursor::hash_filter(&filter);
    let after = match &input.cursor {
        Some(c) => {
            let cur = infrastructure::cursor::Cursor::decode(
                c,
                hmac_key,
                ctx.principal_id,
                &filter_hash,
            )?;
            let ts = cur
                .sort_values
                .first()
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .ok_or_else(|| AppError::validation("invalid cursor", vec![]))?;
            let id = cur
                .sort_values
                .get(1)
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| AppError::validation("invalid cursor", vec![]))?;
            Some((ts, id))
        }
        None => None,
    };
    let limit = i64::from(input.limit.unwrap_or(50).clamp(1, 200));
    let mut items = application::sending::list_drafts_filtered(
        &services.db,
        &ctx,
        limit,
        input.thread_id,
        after,
    )
    .await?;
    let page = if items.len() as i64 > limit {
        items.truncate(limit as usize);
        let last = items.last().expect("non-empty page");
        let cur = infrastructure::cursor::Cursor::new(
            vec![
                serde_json::json!(last.created_at.to_rfc3339()),
                serde_json::json!(last.draft_id),
            ],
            filter_hash,
            ctx.principal_id,
        );
        Page {
            next_cursor: Some(cur.encode(hmac_key)),
            has_more: true,
        }
    } else {
        Page::empty()
    };
    Ok(MailListDraftsOutput {
        items,
        next_cursor: page.next_cursor,
        has_more: page.has_more,
    })
}

/// Tools this module exposes over MCP.
pub fn tools() -> Vec<ToolDef> {
    let mut tools = vec![tool_with_db(
        "mail_create_draft",
        "Create a mailbox-scoped email draft using a stable 16..=128 character idempotency_key. This never submits or schedules mail; agent-created drafts require human approval before sending.",
        serde_json::to_value(schemars::schema_for!(MailCreateDraftInput))
            .expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(MailCreateDraftOutput))
            .expect("schema serializes"),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailCreateDraftInput>(value)?;
            let (idempotency_key, request) = input.into_parts();
            if !(16..=128).contains(&idempotency_key.len()) {
                return Err(AppError::validation(
                    "idempotency_key must be 16..=128 characters",
                    vec![Violation {
                        field: "idempotency_key".into(),
                        code: "LENGTH".into(),
                    }],
                ));
            }
            let approval_required =
                ctx.principal_kind == PrincipalKind::Agent || request.purpose != "correspondence";
            let request_hash = infrastructure::crypto::sha256_bytes(
                &serde_json::to_vec(&request).unwrap_or_default(),
            );
            let scope = infrastructure::idempotency::Scope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: ctx.principal_id.to_string(),
                endpoint: "MCP mail_create_draft".into(),
                key: idempotency_key,
            };
            let ctx2 = ctx.clone();
            let request2 = request.clone();
            let stored = infrastructure::idempotency::run(
                &db,
                &scope,
                &request_hash,
                move |tx| async move {
                    let (tx, draft) =
                        application::sending::create_draft_in_tx(tx, &ctx2, &request2).await?;
                    Ok((
                        tx,
                        infrastructure::idempotency::Stored {
                            status: 201,
                            body: draft,
                        },
                    ))
                },
            )
            .await?
            .into_inner();
            let draft = stored.body;
            result(MailCreateDraftOutput {
                draft_id: draft.draft_id,
                version: draft.version,
                status: draft.status,
                approval_required,
            })
        },
    )];
    tools.push(tool_with_db(
        "mail_submit_approved",
        "Explicitly queue one frozen outbound snapshot after a current, unexpired independent human approval. Requires mail.submit_approved, a current mailbox can_send grant, and the active source credential that created the snapshot. This never approves mail or accepts an arbitrary outbound.",
        serde_json::to_value(schemars::schema_for!(MailSubmitApprovedInput))
            .expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(SubmitOutboundResponse))
            .expect("schema serializes"),
        true,
        |db, ctx, value| async move {
            let input = arguments::<MailSubmitApprovedInput>(value)?;
            if !valid_approved_submit_key(&input.idempotency_key) {
                return Err(AppError::validation(
                    "idempotency_key must be 16..=128 ASCII characters",
                    vec![Violation {
                        field: "idempotency_key".into(),
                        code: "LENGTH".into(),
                    }],
                ));
            }
            let submitted = application::sending::submit_approved(
                &db,
                &ctx,
                input.outbound_message_id,
                &input.expected_payload_hash,
                &input.idempotency_key,
            )
            .await?;
            result(submitted.body)
        },
    ));
    tools.push(tool_with_db(
        "mail_get_outbound",
        "Read the current submission and provider-confirmed delivery status for one outbound. Requires mail.read and a mailbox read grant. Linked replies and request-based engagement are included when available; accepted does not mean delivered or read.",
        serde_json::to_value(schemars::schema_for!(MailGetOutboundInput))
            .expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(OutboundMessageResponse))
            .expect("schema serializes"),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailGetOutboundInput>(value)?;
            result(application::sending::get_outbound(&db, &ctx, input.outbound_message_id).await?)
        },
    ));
    tools.push(tool_with_services(
        "mail_list_drafts",
        "List message drafts visible to the principal, newest first. Requires mail.read. Optional thread_id filter; follow next_cursor with identical filters.",
        serde_json::to_value(schemars::schema_for!(MailListDraftsInput))
            .expect("schema serializes"),
        object_schema(),
        false,
        |services, ctx, value| async move {
            let input = arguments::<MailListDraftsInput>(value)?;
            result(list_drafts(services, ctx, input).await?)
        },
    ));
    tools.push(tool_with_db(
        "mail_get_draft",
        "Read one message draft (content, recipient, status, version). Private drafts of other principals are not visible.",
        serde_json::to_value(schemars::schema_for!(MailGetDraftInput)).expect("schema serializes"),
        object_schema(),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailGetDraftInput>(value)?;
            result(application::sending::get_draft(&db, &ctx, input.draft_id).await?)
        },
    ));
    tools.push(tool_with_db(
        "mail_update_draft",
        "Update an unfrozen draft at expected_version. Allowed keys in changes: subject, body_text, body_html, variables, to_email_address_id, expected_last_inbound_id, visibility. Requires mail compose access to the draft's mailbox.",
        serde_json::to_value(schemars::schema_for!(MailUpdateDraftInput))
            .expect("schema serializes"),
        object_schema(),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailUpdateDraftInput>(value)?;
            let req: PatchMessageDraftRequest =
                serde_json::from_value(serde_json::Value::Object(input.changes)).map_err(|e| {
                    AppError::validation(format!("invalid changes: {e}"), vec![])
                })?;
            result(
                application::sending::patch_draft(
                    &db,
                    &ctx,
                    input.draft_id,
                    input.expected_version,
                    &req,
                )
                .await?,
            )
        },
    ));
    tools.push(tool_with_db(
        "mail_freeze_draft",
        "Freeze a draft at expected_version into an immutable outbound snapshot on the chosen delivery channel. Returns outbound_message_id and payload_hash for preflight, approval or submit. Requires a stable idempotency_key; does not send.",
        serde_json::to_value(schemars::schema_for!(MailFreezeDraftInput))
            .expect("schema serializes"),
        object_schema(),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailFreezeDraftInput>(value)?;
            require_idempotency_key(&input.idempotency_key)?;
            let id = input.draft_id;
            let expected_version = input.expected_version;
            let req = FreezeMessageDraftRequest {
                delivery_channel_id: input.delivery_channel_id,
                sender_identity_id: input.sender_identity_id,
                meeting_type_id: input.meeting_type_id,
                tracking_requested: input.tracking_requested,
            };
            let request_hash = infrastructure::crypto::sha256_bytes(
                &serde_json::to_vec(&serde_json::json!({
                    "draft_id": id,
                    "if_match": expected_version,
                    "body": &req,
                }))
                .unwrap_or_default(),
            );
            let scope = infrastructure::idempotency::Scope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: ctx.principal_id.to_string(),
                endpoint: "POST /api/v1/message-drafts/{id}/freeze".into(),
                key: input.idempotency_key,
            };
            let ctx2 = ctx.clone();
            let stored = infrastructure::idempotency::run(
                &db,
                &scope,
                &request_hash,
                move |tx| async move {
                    let (tx, body) = application::sending::freeze_draft_in_tx(
                        tx,
                        &ctx2,
                        id,
                        expected_version,
                        &req,
                    )
                    .await?;
                    Ok((
                        tx,
                        infrastructure::idempotency::Stored { status: 201, body },
                    ))
                },
            )
            .await?
            .into_inner();
            result(stored.body)
        },
    ));
    tools.push(tool_with_db(
        "mail_preflight_outbound",
        "Run the non-binding send-policy evaluation for a frozen outbound and return each check with its failure code. Requires a mailbox read grant; does not authorize or send.",
        serde_json::to_value(schemars::schema_for!(MailOutboundIdInput))
            .expect("schema serializes"),
        object_schema(),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailOutboundIdInput>(value)?;
            result(application::sending::preflight(&db, &ctx, input.outbound_message_id).await?)
        },
    ));
    tools.push(tool_with_db(
        "mail_request_approval",
        "Create an approval request bound to a frozen outbound's exact expected_payload_hash, with a reason and optional expires_at. Requires a stable idempotency_key. Returns approval_request_id; does not send.",
        serde_json::to_value(schemars::schema_for!(MailRequestApprovalInput))
            .expect("schema serializes"),
        object_schema(),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailRequestApprovalInput>(value)?;
            require_idempotency_key(&input.idempotency_key)?;
            let id = input.outbound_message_id;
            let req = CreateOutboundApprovalRequest {
                reason: input.reason,
                expected_payload_hash: input.expected_payload_hash,
                expires_at: input.expires_at,
            };
            let request_hash = infrastructure::crypto::sha256_bytes(
                &serde_json::to_vec(&serde_json::json!({"outbound_message_id": id, "body": &req}))
                    .unwrap_or_default(),
            );
            let scope = infrastructure::idempotency::Scope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: ctx.principal_id.to_string(),
                endpoint: "POST /api/v1/outbound-messages/{id}/approval-requests".into(),
                key: input.idempotency_key,
            };
            let ctx2 = ctx.clone();
            let stored = infrastructure::idempotency::run(
                &db,
                &scope,
                &request_hash,
                move |tx| async move {
                    let (tx, body) =
                        application::sending::create_approval_request_in_tx(tx, &ctx2, id, &req)
                            .await?;
                    Ok((
                        tx,
                        infrastructure::idempotency::Stored { status: 201, body },
                    ))
                },
            )
            .await?
            .into_inner();
            result(stored.body)
        },
    ));
    tools.push(tool_with_services(
        "mail_submit",
        "Submit a frozen outbound for dispatch, matching expected_payload_hash, with optional not_before. Requires mail.send, a mailbox can_send grant and a stable idempotency_key; the same suppression, basis and approval policy checks as REST apply. Queued is not delivered.",
        serde_json::to_value(schemars::schema_for!(MailSubmitInput)).expect("schema serializes"),
        serde_json::to_value(schemars::schema_for!(SubmitOutboundResponse))
            .expect("schema serializes"),
        true,
        |services, ctx, value| async move {
            let input = arguments::<MailSubmitInput>(value)?;
            require_idempotency_key(&input.idempotency_key)?;
            if ctx.principal_kind != PrincipalKind::Agent
                && let Some(limiter) = &services.rate_limiter
            {
                limiter
                    .require(
                        &format!("send:submit:{}:{}", ctx.workspace_id, ctx.principal_id),
                        SUBMIT_RATE_LIMIT_PER_MINUTE,
                        Duration::from_secs(60),
                        infrastructure::ratelimit::FailMode::Closed,
                    )
                    .await?;
            }
            let req = SubmitOutboundRequest {
                expected_payload_hash: input.expected_payload_hash,
                not_before: input.not_before,
            };
            let stored = application::sending::submit(
                &services.db,
                &ctx,
                input.outbound_message_id,
                &req,
                &input.idempotency_key,
            )
            .await?;
            result(stored.body)
        },
    ));
    tools.push(tool_with_db(
        "mail_cancel_outbound",
        "Cancel a queued or scheduled outbound at expected_version before dispatch starts. Requires mail.compose; optional reason is audited. In-flight or terminal outbounds cannot be canceled.",
        serde_json::to_value(schemars::schema_for!(MailCancelOutboundInput))
            .expect("schema serializes"),
        object_schema(),
        false,
        |db, ctx, value| async move {
            let input = arguments::<MailCancelOutboundInput>(value)?;
            let req = CancelOutboundRequest {
                reason: input.reason,
            };
            result(
                application::sending::cancel(
                    &db,
                    &ctx,
                    input.outbound_message_id,
                    input.expected_version,
                    &req,
                )
                .await?,
            )
        },
    ));
    tools
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mail_create_draft_requires_idempotency_key_and_rejects_unknown_fields() {
        let tool = tools().remove(0);
        let required = tool.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|name| name == "idempotency_key"));

        let request = serde_json::json!({
            "idempotency_key": "stable-key-123456",
            "mail_account_id": uuid::Uuid::new_v4(),
            "to_email_address_id": uuid::Uuid::new_v4(),
            "purpose": "correspondence",
            "subject": "Hello",
            "body_text": "Hi"
        });
        assert!(arguments::<MailCreateDraftInput>(request.clone()).is_ok());
        assert!(
            arguments::<MailCreateDraftInput>(serde_json::json!({
                "idempotency_key": "stable-key-123456",
                "mail_account_id": uuid::Uuid::new_v4(),
            }))
            .is_err()
        );
        assert!(
            arguments::<MailCreateDraftInput>(serde_json::json!({
                "idempotency_key": "stable-key-123456",
                "workspace_id": uuid::Uuid::new_v4(),
            }))
            .is_err()
        );
        assert!(
            arguments::<MailCreateDraftInput>(serde_json::json!({
                "idempotency_key": "short",
                "unknown": true,
            }))
            .is_err()
        );
    }

    #[test]
    fn approved_submit_requires_hash_and_idempotency_key_and_rejects_unknown_fields() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name == "mail_submit_approved")
            .unwrap();
        let required = tool.input_schema["required"].as_array().unwrap();
        for field in [
            "idempotency_key",
            "outbound_message_id",
            "expected_payload_hash",
        ] {
            assert!(required.iter().any(|name| name == field), "missing {field}");
        }
        assert!(tool.external_effect);
        let input = serde_json::json!({
            "idempotency_key": "approved-send-key-001",
            "outbound_message_id": uuid::Uuid::new_v4(),
            "expected_payload_hash": "ab".repeat(32)
        });
        assert!(arguments::<MailSubmitApprovedInput>(input.clone()).is_ok());
        assert!(
            arguments::<MailSubmitApprovedInput>(serde_json::json!({
                "idempotency_key": "approved-send-key-001",
                "outbound_message_id": uuid::Uuid::new_v4(),
                "expected_payload_hash": "ab".repeat(32),
                "status": "queued"
            }))
            .is_err()
        );
        assert!(valid_approved_submit_key("approved-send-key-001"));
        assert!(!valid_approved_submit_key("abcdefghé1234567"));
        assert!(!valid_approved_submit_key("short"));
    }

    #[test]
    fn exposes_every_rest_sending_route_without_duplicates() {
        let names: Vec<_> = tools().iter().map(|t| t.name).collect();
        for name in [
            "mail_create_draft",
            "mail_submit_approved",
            "mail_get_outbound",
            "mail_list_drafts",
            "mail_get_draft",
            "mail_update_draft",
            "mail_freeze_draft",
            "mail_preflight_outbound",
            "mail_request_approval",
            "mail_submit",
            "mail_cancel_outbound",
        ] {
            assert_eq!(names.iter().filter(|n| **n == name).count(), 1, "{name}");
        }
        let submit = tools()
            .into_iter()
            .find(|t| t.name == "mail_submit")
            .unwrap();
        assert!(submit.external_effect);
        for name in ["mail_freeze_draft", "mail_request_approval", "mail_submit"] {
            let tool = tools().into_iter().find(|t| t.name == name).unwrap();
            let required = tool.input_schema["required"].as_array().unwrap();
            assert!(required.iter().any(|f| f == "idempotency_key"), "{name}");
        }
        assert!(
            arguments::<MailUpdateDraftInput>(serde_json::json!({
                "draft_id": Uuid::new_v4(),
                "expected_version": 1,
                "changes": {"subject": "Hi"}
            }))
            .is_ok()
        );
        assert!(
            serde_json::from_value::<PatchMessageDraftRequest>(serde_json::json!({
                "status": "sent"
            }))
            .is_err()
        );
    }

    #[test]
    fn outbound_status_tool_requires_an_outbound_id_and_is_read_only() {
        let tool = tools()
            .into_iter()
            .find(|tool| tool.name == "mail_get_outbound")
            .unwrap();
        assert!(!tool.external_effect);
        let required = tool.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|name| name == "outbound_message_id"));
        assert!(
            arguments::<MailGetOutboundInput>(serde_json::json!({
                "outbound_message_id": uuid::Uuid::new_v4()
            }))
            .is_ok()
        );
    }
}
