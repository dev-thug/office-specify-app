//! Campaign MCP tools (B.6.2). Every tool delegates to the same application
//! use case as the matching REST route in `crates/api/src/routes/campaigns.rs`:
//!
//! - `campaign_list` → `GET /campaigns`
//! - `campaign_get` → `GET /campaigns/{id}`
//! - `campaign_create_draft` → `POST /campaigns`
//! - `campaign_create_revision` → `POST /campaigns/{id}/revisions`
//! - `campaign_preflight` → `POST /campaigns/{id}/preflight`
//! - `campaign_create_launch` → `POST /campaigns/{id}/launches`
//! - `campaign_get_launch_detail` → `GET /campaign-launches/{id}`
//! - `campaign_get_launch` → safe launch status summary (no REST twin)
//! - `campaign_submit_approved` → `POST /campaign-launches/{id}/execute`
//! - `campaign_request_launch` → `POST /campaign-launches/{id}/approval-requests`
//! - `campaign_pause` → `POST /campaign-launches/{id}/pause`
//! - `campaign_cancel` → `POST /campaign-launches/{id}/cancel`

use std::collections::BTreeMap;

use crate::{ToolDef, arguments, result, tool_with_db, tool_with_services};
use contracts::campaigns::{
    CampaignLaunchStatus, CampaignStepInput, CreateCampaignRequest, CreateCampaignResponse,
    CreateLaunchRequest, CreateRevisionRequest, ExecuteLaunchRequest, ExecuteLaunchResponse,
    LaunchApprovalRequestResponse, LaunchSummary, PreflightRequest, PreflightResponse,
    ScheduleRulesInput,
};
use schemars::JsonSchema;
use serde::Deserialize;
use uuid::Uuid;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Contract read models without a JsonSchema derive are described loosely.
fn object_schema() -> serde_json::Value {
    serde_json::json!({"type": "object", "additionalProperties": true})
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignListInput {
    /// Opaque cursor returned as next_cursor by the previous call.
    cursor: Option<String>,
    /// Page size (default 50, max 200).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignGetInput {
    campaign_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignCreateRevisionInput {
    campaign_id: Uuid,
    /// Current campaign version (from campaign_get); stale versions are rejected.
    expected_version: i64,
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
struct CampaignCreateLaunchInput {
    idempotency_key: String,
    campaign_id: Uuid,
    revision_id: Uuid,
    email_address_ids: Vec<Uuid>,
    /// Request recipient-bound engagement tracking (only honored when the
    /// workspace has tracking enabled). Defaults to false.
    #[serde(default)]
    tracking_requested: bool,
    /// Personalization values applied to every recipient; system-resolved
    /// variables of the same name win.
    #[serde(default)]
    variables: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignGetLaunchDetailInput {
    launch_id: Uuid,
    /// Messages page cursor returned by the previous call.
    cursor: Option<String>,
    /// Messages page size (default 50, max 200).
    limit: Option<i64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignCancelInput {
    idempotency_key: String,
    launch_id: Uuid,
    expected_version: i64,
    reason: Option<String>,
}

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
struct CampaignSubmitApprovedInput {
    idempotency_key: String,
    launch_id: Uuid,
    expected_manifest_hash: String,
    expected_version: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CampaignGetLaunchInput {
    launch_id: Uuid,
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
        tool_with_services(
            "campaign_list",
            "List legacy common-template bulk campaigns in this workspace, newest first, with cursor pagination. Requires campaign.compose for credential callers; returns {campaigns, next_cursor, has_more}.",
            serde_json::to_value(schemars::schema_for!(CampaignListInput))
                .expect("schema serializes"),
            object_schema(),
            false,
            |services, ctx, value| async move {
                let input = arguments::<CampaignListInput>(value)?;
                let (campaigns, page) = application::campaigns::list_campaigns(
                    &services.db,
                    &ctx,
                    input.cursor.as_deref(),
                    clamp_limit(input.limit),
                    &services.config.hmac_key,
                )
                .await?;
                result(serde_json::json!({
                    "campaigns": campaigns,
                    "next_cursor": page.next_cursor,
                    "has_more": page.has_more,
                }))
            },
        ),
        tool_with_db(
            "campaign_get",
            "Read one campaign with its revisions, steps, and launches, including the campaign version needed by campaign_create_revision. Requires campaign.compose for credential callers and campaign mailbox read access.",
            serde_json::to_value(schemars::schema_for!(CampaignGetInput))
                .expect("schema serializes"),
            object_schema(),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignGetInput>(value)?;
                result(application::campaigns::get_campaign(&db, &ctx, input.campaign_id).await?)
            },
        ),
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
            "campaign_create_revision",
            "Create a new immutable revision (channel, timezone, schedule, steps) for an existing campaign. Requires campaign.compose and the campaign mailbox compose grant; expected_version must equal the current campaign version. Creates no launch and sends nothing.",
            serde_json::to_value(schemars::schema_for!(CampaignCreateRevisionInput))
                .expect("schema serializes"),
            object_schema(),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignCreateRevisionInput>(value)?;
                let request = CreateRevisionRequest {
                    delivery_channel_id: input.delivery_channel_id,
                    timezone: input.timezone,
                    schedule_rules: input.schedule_rules,
                    steps: input.steps,
                    stop_on_reply: input.stop_on_reply,
                    stop_on_booking: input.stop_on_booking,
                };
                let mut tx = db.begin(ctx.workspace_id).await?;
                let body = application::campaigns::create_revision(
                    &mut tx,
                    &ctx,
                    input.campaign_id,
                    input.expected_version,
                    &request,
                )
                .await?;
                tx.commit().await?;
                result(body)
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
            "campaign_create_launch",
            "Freeze a campaign revision and recipient list into a launch that builds asynchronously; poll campaign_get_launch_detail for the frozen manifest hash before campaign_request_launch. Requires campaign.compose and the campaign mailbox compose grant. Does not approve or send.",
            serde_json::to_value(schemars::schema_for!(CampaignCreateLaunchInput))
                .expect("schema serializes"),
            object_schema(),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignCreateLaunchInput>(value)?;
                let campaign_id = input.campaign_id;
                let request = CreateLaunchRequest {
                    revision_id: input.revision_id,
                    email_address_ids: input.email_address_ids,
                    tracking_requested: input.tracking_requested,
                    variables: input.variables,
                };
                let request_hash = infrastructure::crypto::sha256_bytes(
                    &serde_json::to_vec(&serde_json::json!({
                        "campaign_id": campaign_id,
                        "body": &request,
                    }))
                    .unwrap_or_default(),
                );
                let scope = scope(
                    &ctx,
                    "MCP tools/call campaign_create_launch",
                    input.idempotency_key,
                );
                let outcome =
                    infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                        let ctx = ctx.clone();
                        let request = request;
                        async move {
                            let body = application::campaigns::create_launch(
                                &mut tx,
                                &ctx,
                                campaign_id,
                                &request,
                            )
                            .await?;
                            Ok((
                                tx,
                                infrastructure::idempotency::Stored { status: 202, body },
                            ))
                        }
                    })
                    .await?
                    .into_inner();
                result(outcome.body)
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
            "campaign_submit_approved",
            "Queue activation of one human-approved frozen campaign manifest. Requires the narrow campaign.submit_approved scope plus a current campaign mailbox compose or send grant; does not approve or directly send messages.",
            serde_json::to_value(schemars::schema_for!(CampaignSubmitApprovedInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(ExecuteLaunchResponse))
                .expect("schema serializes"),
            true,
            |db, ctx, value| async move {
                let input = arguments::<CampaignSubmitApprovedInput>(value)?;
                let request = ExecuteLaunchRequest {
                    expected_manifest_hash: input.expected_manifest_hash,
                    expected_version: input.expected_version,
                };
                let request_hash = infrastructure::crypto::sha256_bytes(
                    &serde_json::to_vec(&serde_json::json!({
                        "launch_id": input.launch_id,
                        "body": &request,
                    }))
                    .unwrap_or_default(),
                );
                let launch_id = input.launch_id;
                let scope = scope(
                    &ctx,
                    "MCP tools/call campaign_submit_approved",
                    input.idempotency_key,
                );
                let outcome =
                    infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                        let ctx = ctx.clone();
                        let request = request;
                        async move {
                            let body = application::campaigns::execute_launch(
                                &mut tx, &ctx, launch_id, &request,
                            )
                            .await?;
                            Ok((
                                tx,
                                infrastructure::idempotency::Stored { status: 202, body },
                            ))
                        }
                    })
                    .await?
                    .into_inner();
                result(outcome.body)
            },
        ),
        tool_with_db(
            "campaign_get_launch",
            "Read a safe campaign launch summary, execution status, enrollment IDs, and counts. Requires current campaign mailbox read access and campaign.compose for credential callers; message bodies and recipient addresses are omitted.",
            serde_json::to_value(schemars::schema_for!(CampaignGetLaunchInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(CampaignLaunchStatus))
                .expect("schema serializes"),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignGetLaunchInput>(value)?;
                let status =
                    application::campaigns::get_launch_status(&db, &ctx, input.launch_id).await?;
                result(status)
            },
        ),
        tool_with_services(
            "campaign_get_launch_detail",
            "Read full campaign launch detail: launch record (version, manifest hash, status), enrollment counts, and a cursor-paginated page of launch messages. Requires campaign.compose for credential callers and campaign mailbox read access.",
            serde_json::to_value(schemars::schema_for!(CampaignGetLaunchDetailInput))
                .expect("schema serializes"),
            object_schema(),
            false,
            |services, ctx, value| async move {
                let input = arguments::<CampaignGetLaunchDetailInput>(value)?;
                let detail = application::campaigns::get_launch(
                    &services.db,
                    &ctx,
                    input.launch_id,
                    input.cursor.as_deref(),
                    clamp_limit(input.limit),
                    &services.config.hmac_key,
                )
                .await?;
                result(detail)
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
        tool_with_db(
            "campaign_cancel",
            "Cancel one campaign launch permanently so no further scheduled sends occur. Requires campaign.pause scope or campaign ownership/admin rights; expected_version must equal the current launch version.",
            serde_json::to_value(schemars::schema_for!(CampaignCancelInput))
                .expect("schema serializes"),
            serde_json::to_value(schemars::schema_for!(LaunchSummary)).expect("schema serializes"),
            false,
            |db, ctx, value| async move {
                let input = arguments::<CampaignCancelInput>(value)?;
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
                let scope = scope(
                    &ctx,
                    "MCP tools/call campaign_cancel",
                    input.idempotency_key,
                );
                let outcome =
                    infrastructure::idempotency::run(&db, &scope, &request_hash, |mut tx| {
                        let ctx = ctx.clone();
                        let reason = reason;
                        async move {
                            let body = application::campaigns::cancel_launch(
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
                "campaign_list",
                "campaign_get",
                "campaign_create_draft",
                "campaign_create_revision",
                "campaign_preflight",
                "campaign_create_launch",
                "campaign_request_launch",
                "campaign_submit_approved",
                "campaign_get_launch",
                "campaign_get_launch_detail",
                "campaign_pause",
                "campaign_cancel",
            ]
        );
        let without_key = [
            "campaign_list",
            "campaign_get",
            "campaign_create_revision",
            "campaign_preflight",
            "campaign_get_launch",
            "campaign_get_launch_detail",
        ];
        for tool in &tools {
            assert!(tool.input_schema["additionalProperties"] == false);
        }
        for tool in tools
            .iter()
            .filter(|tool| !without_key.contains(&tool.name))
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

    #[test]
    fn approved_submit_is_mutating_and_status_is_safe_read_only() {
        let tools = tools();
        let submit = tools
            .iter()
            .find(|tool| tool.name == "campaign_submit_approved")
            .unwrap();
        assert!(submit.external_effect);
        for field in [
            "idempotency_key",
            "launch_id",
            "expected_manifest_hash",
            "expected_version",
        ] {
            assert!(
                submit.input_schema["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|required| required == field),
                "campaign_submit_approved must require {field}"
            );
        }

        let status = tools
            .iter()
            .find(|tool| tool.name == "campaign_get_launch")
            .unwrap();
        assert!(!status.external_effect);
        assert_eq!(
            status.input_schema["required"].as_array().unwrap(),
            &[serde_json::json!("launch_id")]
        );
    }
}
