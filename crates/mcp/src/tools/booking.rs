//! Booking MCP tools (B.6.2). Slot reads are provider-aware; proposals only
//! create approval requests and never write to a calendar.

use chrono::NaiveDate;
use contracts::booking::McpBookingProposalRequest;
use infrastructure::idempotency::{self, Scope as IdempotencyScope, Stored};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{ToolDef, arguments, result, tool_with_db, tool_with_services};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListSlotsArgs {
    #[serde(default)]
    meeting_type_id: Option<Uuid>,
    #[serde(default)]
    booking_link_id: Option<Uuid>,
    from: NaiveDate,
    to: NaiveDate,
    timezone: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetBookingArgs {
    booking_id: Uuid,
}

fn input_schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|_| serde_json::json!({"type":"object"}))
}

pub fn tools() -> Vec<ToolDef> {
    let slots = tool_with_services(
        "booking_list_slots",
        "List fresh candidate meeting slots in an allowed meeting type or booking link. Internal calendar details are never returned.",
        input_schema::<ListSlotsArgs>(),
        serde_json::json!({
            "type":"object","additionalProperties":false,
            "required":["timezone","availability_checked_at","slots","reservation_guaranteed"],
            "properties":{
                "timezone":{"type":"string"},
                "availability_checked_at":{"type":"string","format":"date-time"},
                "slots":{"type":"array","items":{"type":"object","required":["starts_at","ends_at"],"properties":{"starts_at":{"type":"string","format":"date-time"},"ends_at":{"type":"string","format":"date-time"}}}},
                "reservation_guaranteed":{"type":"boolean"}
            }
        }),
        false,
        |services, ctx, args| async move {
            let args: ListSlotsArgs = arguments(args)?;
            if args.meeting_type_id.is_some() == args.booking_link_id.is_some() {
                return Err(contracts::AppError::validation(
                    "provide exactly one of meeting_type_id or booking_link_id",
                    vec![],
                ));
            }
            let response = if let Some(meeting_id) = args.meeting_type_id {
                application::booking::list_meeting_slots(
                    &services.db,
                    services.calendar.as_ref(),
                    &ctx,
                    meeting_id,
                    args.from,
                    args.to,
                    &args.timezone,
                )
                .await?
            } else {
                application::booking::list_booking_link_slots(
                    &services.db,
                    services.calendar.as_ref(),
                    &ctx,
                    args.booking_link_id.unwrap(),
                    args.from,
                    args.to,
                    &args.timezone,
                )
                .await?
            };
            result(response)
        },
    );

    let propose = tool_with_db(
        "booking_propose",
        "Propose a meeting at a desired time and request human approval. This tool creates no calendar event.",
        input_schema::<McpBookingProposalRequest>(),
        serde_json::json!({"type":"object","required":["id","booking_id","kind","status","approval_request_id"],"properties":{"id":{"type":"string","format":"uuid"},"booking_id":{"type":["string","null"],"format":"uuid"},"kind":{"type":"string"},"status":{"type":"string"},"approval_request_id":{"type":["string","null"],"format":"uuid"}}}),
        false,
        |db, ctx, args| async move {
            let request: McpBookingProposalRequest = arguments(args)?;
            if request.idempotency_key.len() < 16 || request.idempotency_key.len() > 128 {
                return Err(contracts::AppError::validation(
                    "idempotency_key must be 16..=128 characters",
                    vec![],
                ));
            }
            let request_hash =
                infrastructure::crypto::sha256_bytes(&serde_json::to_vec(&request).map_err(
                    |e| contracts::AppError::internal(format!("booking proposal encode: {e}")),
                )?);
            let key = request.idempotency_key.clone();
            let scope = IdempotencyScope {
                workspace_id: ctx.workspace_id,
                principal_or_capability: format!(
                    "{}:{}",
                    ctx.principal_id,
                    ctx.agent_run_id.unwrap_or(ctx.principal_id)
                ),
                endpoint: "MCP booking_propose".into(),
                key,
            };
            let ctx_for_usecase = ctx.clone();
            let outcome = idempotency::run(&db, &scope, &request_hash, move |mut tx| async move {
                let body = application::booking::create_mcp_booking_proposal_in_tx(
                    &mut tx,
                    &ctx_for_usecase,
                    &request,
                )
                .await?;
                Ok((tx, Stored { status: 201, body }))
            })
            .await?;
            result(outcome.into_inner().body)
        },
    );

    let get = tool_with_db(
        "booking_get",
        "Get an allowed booking status and synchronization state.",
        input_schema::<GetBookingArgs>(),
        serde_json::json!({"type":"object","additionalProperties":false}),
        false,
        |db, ctx, args| async move {
            let args: GetBookingArgs = arguments(args)?;
            let row = application::booking::get_booking(&db, &ctx, args.booking_id).await?;
            result(row)
        },
    );

    vec![slots, propose, get]
}
