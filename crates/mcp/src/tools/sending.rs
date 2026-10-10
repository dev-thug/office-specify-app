//! Sending module MCP tools (B.6): drafts stay internal-only. An approved
//! snapshot can be explicitly queued through its narrow approval-bound path.

use crate::{ToolDef, arguments, result, tool_with_db};
use contracts::{
    AppError, PrincipalKind, Violation,
    sending::{CreateMessageDraftRequest, OutboundMessageResponse, SubmitOutboundResponse},
};
use serde::Deserialize;

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
