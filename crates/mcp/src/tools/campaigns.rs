//! Campaign MCP tools (B.6.2). Draft creation, preflight, launch approval
//! requests, and pause all delegate to the REST application use cases.

use crate::{ToolDef, arguments, result, tool_with_db};
use contracts::campaigns::{
    CampaignStepInput, CreateCampaignRequest, CreateCampaignResponse,
    LaunchApprovalRequestResponse, LaunchSummary, PreflightRequest, PreflightResponse,
    ScheduleRulesInput,
};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignCreateDraftInput {
    idempotency_key: String,
    name: String,
    objective: Option<String>,
    mail_account_id: Uuid,
    delivery_channel_id: Uuid,
    timezone: String,
    schedule_rules: ScheduleRulesInput,
    steps: Vec<CampaignStepInput>,
    #[serde(default = "default_true")]
    stop_on_reply: bool,
    #[serde(default = "default_true")]
    stop_on_booking: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignPreflightInput {
    campaign_id: Uuid,
    revision_id: Uuid,
    email_address_ids: Vec<Uuid>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignRequestLaunchInput {
    idempotency_key: String,
    launch_id: Uuid,
    expected_manifest_hash: String,
    reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignPauseInput {
    idempotency_key: String,
    launch_id: Uuid,
    expected_version: i64,
    reason: Option<String>,
}

fn scope(ctx: &contracts::Ctx, endpoint: &str, key: String) -> infrastructure::idempotency::Scope {
    infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: endpoint.to_string(),
        key,
    }
}

pub fn tools() -> Vec<ToolDef> {
    vec![
        tool_with_db(
            "campaign_create_draft",
            "Create a campaign and its first immutable revision. This creates no launch and causes no external send. Requires a campaign-compose scope for service credentials and an explicit compose/send grant on the mailbox.",
            serde_json::to_value(schemars::schema_for!(CampaignCreateDraftInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(CreateCampaignResponse))
                .expect("schema serializes"),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignCreateDraftInput>(value)?;
                let request = CreateCampaignRequest {
                    name: input.name,
                    objective: input.objective,
                    mail_account_id: input.mail_account_id,
                    delivery_channel_id: input.delivery_channel_id,
                    timezone: input.timezone,
                    schedule_rules: input.schedule_rules,
                    steps: input.steps,
                    stop_on_reply: input.stop_on_reply,
                    stop_on_booking: input.stop_on_booking,
                };
                let request_hash = infrastructure::crypto::sha256_bytes(
                    &serde_json::to_vec(&request).unwrap_or_default(),
                );
                let scope = scope(
                    &ctx,
                    "MCP tools/call campaign_create_draft",
                    input.idempotency_key,
                );
                let outcome =
                    infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                        let ctx = ctx.clone();
                        let request = request;
                        async move {
                            let body =
                                application::campaigns::create_campaign(&mut tx, &ctx, &request)
                                    .await?;
                            Ok((
                                tx,
                                infrastructure::idempotency::Stored { status: 201, body },
                            ))
                        }
                    })
                    .await?
                    .into_inner();
                result(outcome.body)
            },
        ),
        tool_with_db(
            "campaign_preflight",
            "Check a campaign recipient list against current address, suppression, communication-basis, channel, and emergency-stop state. The result is informational only and never authorizes execution.",
            serde_json::to_value(schemars::schema_for!(CampaignPreflightInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(PreflightResponse))
                .expect("schema serializes"),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignPreflightInput>(value)?;
                let request = PreflightRequest {
                    revision_id: input.revision_id,
                    email_address_ids: input.email_address_ids,
                };
                let mut tx = db.begin(ctx.workspace_id).await?;
                let response =
                    application::campaigns::preflight(&mut tx, &ctx, input.campaign_id, &request)
                        .await?;
                tx.commit().await?;
                result(response)
            },
        ),
        tool_with_db(
            "campaign_request_launch",
            "Request human approval for one already frozen campaign launch and the exact manifest hash you reviewed. This only creates or returns an approval request; it does not submit email.",
            serde_json::to_value(schemars::schema_for!(CampaignRequestLaunchInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(LaunchApprovalRequestResponse))
                .expect("schema serializes"),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignRequestLaunchInput>(value)?;
                let request_hash = infrastructure::crypto::sha256_bytes(
                    &serde_json::to_vec(&serde_json::json!({
                        "launch_id": input.launch_id,
                        "expected_manifest_hash": input.expected_manifest_hash,
                        "reason": input.reason,
                    }))
                    .unwrap_or_default(),
                );
                let launch_id = input.launch_id;
                let manifest_hash = input.expected_manifest_hash;
                let reason = input.reason;
                let scope = scope(
                    &ctx,
                    "MCP tools/call campaign_request_launch",
                    input.idempotency_key,
                );
                let outcome =
                    infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                        let ctx = ctx.clone();
                        let manifest_hash = manifest_hash;
                        let reason = reason;
                        async move {
                            let (body, _created) = application::campaigns::request_launch_approval(
                                &mut tx,
                                &ctx,
                                launch_id,
                                &manifest_hash,
                                reason.as_deref(),
                            )
                            .await?;
                            Ok((
                                tx,
                                infrastructure::idempotency::Stored { status: 201, body },
                            ))
                        }
                    })
                    .await?
                    .into_inner();
                result(outcome.body)
            },
        ),
        tool_with_db(
            "campaign_pause",
            "Pause one campaign launch so future scheduled sends stop. Requires campaign.pause scope or campaign ownership/admin rights; any in-flight submission remains separately tracked.",
            serde_json::to_value(schemars::schema_for!(CampaignPauseInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(LaunchSummary)).expect("schema serializes"),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignPauseInput>(value)?;
                let request_hash = infrastructure::crypto::sha256_bytes(
                    &serde_json::to_vec(&serde_json::json!({
                        "launch_id": input.launch_id,
                        "expected_version": input.expected_version,
                        "reason": input.reason,
                    }))
                    .unwrap_or_default(),
                );
                let launch_id = input.launch_id;
                let expected_version = input.expected_version;
                let reason = input.reason;
                let scope = scope(&ctx, "MCP tools/call campaign_pause", input.idempotency_key);
                let outcome =
                    infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                        let ctx = ctx.clone();
                        let reason = reason;
                        async move {
                            let body = application::campaigns::pause_launch(
                                &mut tx,
                                &ctx,
                                launch_id,
                                expected_version,
                                reason.as_deref(),
                            )
                            .await?;
                            Ok((
                                tx,
                                infrastructure::idempotency::Stored { status: 200, body },
                            ))
                        }
                    })
                    .await?
                    .into_inner();
                result(outcome.body)
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_campaign_tools_with_explicit_mutation_keys() {
        let tools = tools();
        let names: Vec<_> = tools.iter().map(|tool| tool.name).collect();
        assert_eq!(
            names,
            [
                "campaign_create_draft",
                "campaign_preflight",
                "campaign_request_launch",
                "campaign_pause",
            ]
        );
        for tool in tools
            .iter()
            .filter(|tool| tool.name != "campaign_preflight")
        {
            assert!(tool.input_schema["additionalProperties"] == false);
            assert!(
                tool.input_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v == "idempotency_key")
            );
        }
    }
}
