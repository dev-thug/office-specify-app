//! Booking MCP tools (B.6.2). Slot reads are provider-aware; proposals only
//! create approval requests and never write to a calendar.

use chrono::{DateTime, NaiveDate, Utc};
use contracts::booking::{
    AvailabilityExceptionRequest, AvailabilityRuleRequest, BookingAttendanceRequest,
    BookingChangeProposalRequest, BookingLinkCreatedResponse, CreateBookingLinkRequest,
    CreateMeetingTypeRequest, McpBookingProposalRequest, PublicRescheduleRequest,
    UpdateAvailabilityRequest, UpdateMeetingTypeRequest,
};
use infrastructure::idempotency::{self, Scope as IdempotencyScope, Stored};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
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

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateMeetingTypeArgs {
    /// Idempotency key (16..=128 characters); reuse it to safely retry.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    /// Active calendar connection owned by the calling principal (the host).
    calendar_connection_id: Uuid,
    /// Display name of the meeting type shown to guests.
    name: String,
    /// Optional guest-facing description of the meeting.
    #[serde(default)]
    description: Option<String>,
    /// Meeting length in minutes; defaults to 30.
    #[serde(default)]
    duration_minutes: Option<i32>,
    /// Buffer before each meeting in minutes; defaults to 10.
    #[serde(default)]
    buffer_before_minutes: Option<i32>,
    /// Buffer after each meeting in minutes; defaults to 10.
    #[serde(default)]
    buffer_after_minutes: Option<i32>,
    /// Minimum notice before a bookable slot, in minutes; defaults to 1440.
    #[serde(default)]
    minimum_notice_minutes: Option<i32>,
    /// How many days ahead guests may book; server default when omitted.
    #[serde(default)]
    booking_horizon_days: Option<i32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateMeetingTypeArgs {
    /// Meeting type UUID to update.
    meeting_type_id: Uuid,
    /// Current meeting type version (optimistic concurrency, REST If-Match).
    expected_version: i64,
    /// Fields to change: name, description (null clears), duration_minutes,
    /// buffer_before_minutes, buffer_after_minutes, minimum_notice_minutes,
    /// booking_horizon_days, status.
    changes: Map<String, Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HostArgs {
    /// Principal UUID of the meeting host whose availability is read.
    host_principal_id: Uuid,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AvailabilityRuleArgs {
    /// IANA timezone, e.g. `Asia/Seoul`.
    timezone: String,
    /// Day of week for this rule: 0 = Sunday .. 6 = Saturday.
    weekday: i16,
    /// Local time in `HH:MM` format.
    start_local: String,
    /// Local time in `HH:MM` format.
    end_local: String,
    /// Optional first date (YYYY-MM-DD) the rule applies.
    #[serde(default)]
    valid_from: Option<NaiveDate>,
    /// Optional last date (YYYY-MM-DD) the rule applies.
    #[serde(default)]
    valid_until: Option<NaiveDate>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AvailabilityExceptionArgs {
    /// Exception start as an RFC 3339 timestamp.
    starts_at: DateTime<Utc>,
    /// Exception end as an RFC 3339 timestamp.
    ends_at: DateTime<Utc>,
    /// Exception type: `blocked` or `available`.
    exception_kind: String,
    /// Optional note explaining the exception.
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PutAvailabilityArgs {
    /// Principal UUID of the meeting host whose availability is replaced.
    host_principal_id: Uuid,
    /// Full replacement set of weekly availability rules.
    rules: Vec<AvailabilityRuleArgs>,
    /// Full replacement set of dated availability exceptions.
    #[serde(default)]
    exceptions: Vec<AvailabilityExceptionArgs>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateBookingLinkArgs {
    /// Idempotency key (16..=128 characters); reuse it to safely retry.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    meeting_type_id: Uuid,
    /// Optional email address UUID the link is bound to (only that guest may book).
    #[serde(default)]
    bound_email_address_id: Option<Uuid>,
    /// Optional campaign enrollment UUID to attribute bookings to.
    #[serde(default)]
    campaign_enrollment_id: Option<Uuid>,
    /// Maximum number of bookings through this link; defaults to 1.
    #[serde(default)]
    max_uses: Option<i32>,
    /// Optional future RFC 3339 expiry timestamp for the link.
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListBookingsArgs {
    /// Optional RFC 3339 lower bound on booking start time.
    #[serde(default)]
    starts_from: Option<DateTime<Utc>>,
    /// Optional RFC 3339 upper bound on booking start time; must be after starts_from.
    #[serde(default)]
    starts_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProposeChangeArgs {
    /// Idempotency key (16..=128 characters); reuse it to safely retry.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    /// Change type: `create`, `reschedule` or `cancel`.
    change_kind: String,
    #[serde(default)]
    booking_id: Option<Uuid>,
    /// Proposed new start as an RFC 3339 timestamp (create/reschedule).
    #[serde(default)]
    starts_at: Option<DateTime<Utc>>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RescheduleArgs {
    /// Idempotency key (16..=128 characters); reuse it to safely retry.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    booking_id: Uuid,
    /// Current booking version (REST If-Match).
    expected_version: i64,
    /// New start time as an RFC 3339 timestamp.
    starts_at: DateTime<Utc>,
    /// IANA timezone.
    timezone: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CancelArgs {
    /// Idempotency key (16..=128 characters); reuse it to safely retry.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    booking_id: Uuid,
    /// Current booking version (REST If-Match).
    expected_version: i64,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AttendanceArgs {
    /// Idempotency key (16..=128 characters); reuse it to safely retry.
    #[schemars(length(min = 16, max = 128))]
    idempotency_key: String,
    booking_id: Uuid,
    /// Current booking version (REST If-Match).
    expected_version: i64,
    /// Attendance outcome: `attended` or `no_show`.
    attendance_status: String,
    /// Optional internal note about the meeting outcome.
    #[serde(default)]
    outcome_note: Option<String>,
}

fn check_key(key: &str) -> Result<(), contracts::AppError> {
    if key.len() < 16 || key.len() > 128 {
        return Err(contracts::AppError::validation(
            "idempotency_key must be 16..=128 characters",
            vec![],
        ));
    }
    Ok(())
}

fn idem_scope(ctx: &contracts::Ctx, endpoint: &str, key: String) -> IdempotencyScope {
    IdempotencyScope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: format!(
            "{}:{}",
            ctx.principal_id,
            ctx.agent_run_id.unwrap_or(ctx.principal_id)
        ),
        endpoint: endpoint.into(),
        key,
    }
}

fn hash_of<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, contracts::AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| contracts::AppError::internal(format!("booking request encode: {e}")))?;
    Ok(infrastructure::crypto::sha256_bytes(&bytes).to_vec())
}

/// Build a contract request from present fields only so contract defaults apply.
fn from_present<T: serde::de::DeserializeOwned>(
    fields: Vec<(&str, Value)>,
) -> Result<T, contracts::AppError> {
    let map: Map<String, Value> = fields
        .into_iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    serde_json::from_value(Value::Object(map)).map_err(|e| {
        contracts::AppError::validation(format!("invalid tool arguments: {e}"), vec![])
    })
}

fn object_schema() -> Value {
    json!({"type":"object"})
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

    let create_meeting_type = tool_with_db(
        "booking_create_meeting_type",
        "Create a meeting type hosted by the calling principal on one of its active calendar connections. Requires booking.propose and an idempotency_key; duration, buffers, notice and horizon fall back to defaults when omitted.",
        input_schema::<CreateMeetingTypeArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: CreateMeetingTypeArgs = arguments(args)?;
            check_key(&args.idempotency_key)?;
            let req: CreateMeetingTypeRequest = from_present(vec![
                ("calendar_connection_id", json!(args.calendar_connection_id)),
                ("name", json!(args.name)),
                ("description", json!(args.description)),
                ("duration_minutes", json!(args.duration_minutes)),
                ("buffer_before_minutes", json!(args.buffer_before_minutes)),
                ("buffer_after_minutes", json!(args.buffer_after_minutes)),
                ("minimum_notice_minutes", json!(args.minimum_notice_minutes)),
                ("booking_horizon_days", json!(args.booking_horizon_days)),
            ])?;
            let request_hash = hash_of(&req)?;
            let scope = idem_scope(
                &ctx,
                "MCP booking_create_meeting_type",
                args.idempotency_key,
            );
            let ctx_for_usecase = ctx.clone();
            let outcome = idempotency::run(&db, &scope, &request_hash, move |mut tx| async move {
                let body = application::booking::create_meeting_type_in_tx(
                    &mut tx,
                    &ctx_for_usecase,
                    &req,
                )
                .await?;
                Ok((tx, Stored { status: 201, body }))
            })
            .await?;
            result(outcome.into_inner().body)
        },
    );

    let update_meeting_type = tool_with_db(
        "booking_update_meeting_type",
        "Update a meeting type with optimistic concurrency (expected_version). Requires booking.propose. `changes` may contain name, description (null clears), duration_minutes, buffer_before_minutes, buffer_after_minutes, minimum_notice_minutes, booking_horizon_days, status.",
        input_schema::<UpdateMeetingTypeArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: UpdateMeetingTypeArgs = arguments(args)?;
            let req: UpdateMeetingTypeRequest = serde_json::from_value(Value::Object(args.changes))
                .map_err(|e| {
                    contracts::AppError::validation(format!("invalid changes: {e}"), vec![])
                })?;
            let row = application::booking::update_meeting_type(
                &db,
                &ctx,
                args.meeting_type_id,
                args.expected_version,
                req,
            )
            .await?;
            result(row)
        },
    );

    let get_availability = tool_with_db(
        "booking_get_availability",
        "Read a host's weekly availability rules and exceptions. Requires access to the host.",
        input_schema::<HostArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: HostArgs = arguments(args)?;
            let row =
                application::booking::get_availability(&db, &ctx, args.host_principal_id).await?;
            result(row)
        },
    );

    let put_availability = tool_with_db(
        "booking_set_availability",
        "Replace a host's weekly availability rules and exceptions (full replacement). Requires booking.propose and access to the host.",
        input_schema::<PutAvailabilityArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: PutAvailabilityArgs = arguments(args)?;
            let req = UpdateAvailabilityRequest {
                rules: args
                    .rules
                    .into_iter()
                    .map(|r| AvailabilityRuleRequest {
                        timezone: r.timezone,
                        weekday: r.weekday,
                        start_local: r.start_local,
                        end_local: r.end_local,
                        valid_from: r.valid_from,
                        valid_until: r.valid_until,
                    })
                    .collect(),
                exceptions: args
                    .exceptions
                    .into_iter()
                    .map(|e| AvailabilityExceptionRequest {
                        starts_at: e.starts_at,
                        ends_at: e.ends_at,
                        kind: e.exception_kind,
                        reason: e.reason,
                    })
                    .collect(),
            };
            let row =
                application::booking::put_availability(&db, &ctx, args.host_principal_id, req)
                    .await?;
            result(row)
        },
    );

    let create_link = tool_with_services(
        "booking_create_link",
        "Create a guest booking link for a meeting type and return its public token_url. Requires booking.propose and an idempotency_key; max_uses defaults to 1.",
        input_schema::<CreateBookingLinkArgs>(),
        json!({
            "type":"object",
            "required":["id","token_url","max_uses"],
            "properties":{
                "id":{"type":"string","format":"uuid"},
                "token_url":{"type":"string"},
                "expires_at":{"type":["string","null"],"format":"date-time"},
                "max_uses":{"type":"integer"}
            }
        }),
        false,
        |services, ctx, args| async move {
            let args: CreateBookingLinkArgs = arguments(args)?;
            check_key(&args.idempotency_key)?;
            let req: CreateBookingLinkRequest = from_present(vec![
                ("meeting_type_id", json!(args.meeting_type_id)),
                ("bound_email_address_id", json!(args.bound_email_address_id)),
                ("campaign_enrollment_id", json!(args.campaign_enrollment_id)),
                ("max_uses", json!(args.max_uses)),
                ("expires_at", json!(args.expires_at)),
            ])?;
            let request_hash = hash_of(&req)?;
            let key = args.idempotency_key;
            let scope = idem_scope(&ctx, "MCP booking_create_link", key.clone());
            // Same derivation as REST: reproducible on replay, only its hash is
            // stored. The MCP prefix keeps it distinct from REST-created links.
            let seed = format!(
                "booking-link:mcp:{}:{}:{}",
                ctx.workspace_id, scope.principal_or_capability, key
            );
            let raw = infrastructure::crypto::b64_encode(&infrastructure::crypto::hmac_sha256(
                &services.config.hmac_key,
                seed.as_bytes(),
            ));
            let raw_for_closure = raw.clone();
            let ctx_for_usecase = ctx.clone();
            let outcome = idempotency::run(
                &services.db,
                &scope,
                &request_hash,
                move |mut tx| async move {
                    let body = application::booking::create_booking_link_in_tx(
                        &mut tx,
                        &ctx_for_usecase,
                        &req,
                        &raw_for_closure,
                    )
                    .await?;
                    Ok((tx, Stored { status: 201, body }))
                },
            )
            .await?;
            let stored = outcome.into_inner().body;
            result(BookingLinkCreatedResponse {
                id: stored.id,
                token_url: format!("/public/book/{raw}"),
                expires_at: stored.expires_at,
                max_uses: stored.max_uses,
            })
        },
    );

    let list = tool_with_db(
        "booking_list",
        "List bookings visible to the caller, optionally bounded by from/to (RFC 3339). Requires booking.read.",
        input_schema::<ListBookingsArgs>(),
        json!({"type":"object","required":["bookings"],"properties":{"bookings":{"type":"array","items":{"type":"object"}}}}),
        false,
        |db, ctx, args| async move {
            let args: ListBookingsArgs = arguments(args)?;
            let rows =
                application::booking::list_bookings(&db, &ctx, args.starts_from, args.starts_to)
                    .await?;
            result(json!({ "bookings": rows }))
        },
    );

    let propose_change = tool_with_db(
        "booking_propose_change",
        "Request a booking change with change_kind create, reschedule or cancel (booking_id, starts_at, timezone, reason as applicable). Requires booking.propose and an idempotency_key; the change goes through approval.",
        input_schema::<ProposeChangeArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: ProposeChangeArgs = arguments(args)?;
            check_key(&args.idempotency_key)?;
            let req = BookingChangeProposalRequest {
                kind: args.change_kind,
                booking_id: args.booking_id,
                starts_at: args.starts_at,
                timezone: args.timezone,
                reason: args.reason,
            };
            let request_hash = hash_of(&req)?;
            let scope = idem_scope(&ctx, "MCP booking_propose_change", args.idempotency_key);
            let ctx_for_usecase = ctx.clone();
            let outcome = idempotency::run(&db, &scope, &request_hash, move |mut tx| async move {
                let body =
                    application::booking::propose_change_in_tx(&mut tx, &ctx_for_usecase, req)
                        .await?;
                Ok((tx, Stored { status: 201, body }))
            })
            .await?;
            result(outcome.into_inner().body)
        },
    );

    let reschedule = tool_with_db(
        "booking_reschedule",
        "Reschedule a booking to a new start time. Requires booking.propose, the current booking version as expected_version, and an idempotency_key.",
        input_schema::<RescheduleArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: RescheduleArgs = arguments(args)?;
            check_key(&args.idempotency_key)?;
            let id = args.booking_id;
            let version = args.expected_version;
            let req = PublicRescheduleRequest {
                starts_at: args.starts_at,
                timezone: args.timezone,
                expected_version: version,
            };
            let request_hash = hash_of(&json!({"booking_id":id,"if_match":version,"body":req}))?;
            let scope = idem_scope(&ctx, "MCP booking_reschedule", args.idempotency_key);
            let ctx_for_usecase = ctx.clone();
            let outcome = idempotency::run(&db, &scope, &request_hash, move |mut tx| async move {
                let body = application::booking::reschedule_booking_in_tx(
                    &mut tx,
                    &ctx_for_usecase,
                    id,
                    version,
                    req,
                )
                .await?;
                Ok((tx, Stored { status: 202, body }))
            })
            .await?;
            result(outcome.into_inner().body)
        },
    );

    let cancel = tool_with_db(
        "booking_cancel",
        "Cancel a booking. Requires booking.propose, the current booking version as expected_version, and an idempotency_key.",
        input_schema::<CancelArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: CancelArgs = arguments(args)?;
            check_key(&args.idempotency_key)?;
            let id = args.booking_id;
            let version = args.expected_version;
            let request_hash = hash_of(&json!({"booking_id":id,"if_match":version}))?;
            let scope = idem_scope(&ctx, "MCP booking_cancel", args.idempotency_key);
            let ctx_for_usecase = ctx.clone();
            let outcome = idempotency::run(&db, &scope, &request_hash, move |mut tx| async move {
                let body = application::booking::cancel_booking_in_tx(
                    &mut tx,
                    &ctx_for_usecase,
                    id,
                    version,
                )
                .await?;
                Ok((tx, Stored { status: 202, body }))
            })
            .await?;
            result(outcome.into_inner().body)
        },
    );

    let attendance = tool_with_db(
        "booking_record_attendance",
        "Record whether a booking was attended or a no_show, with an optional outcome note. Requires booking.propose, the current booking version as expected_version, and an idempotency_key.",
        input_schema::<AttendanceArgs>(),
        object_schema(),
        false,
        |db, ctx, args| async move {
            let args: AttendanceArgs = arguments(args)?;
            check_key(&args.idempotency_key)?;
            let id = args.booking_id;
            let version = args.expected_version;
            let req = BookingAttendanceRequest {
                attendance_status: args.attendance_status,
                outcome_note: args.outcome_note,
            };
            let request_hash = hash_of(&json!({"booking_id":id,"if_match":version,"body":req}))?;
            let scope = idem_scope(&ctx, "MCP booking_record_attendance", args.idempotency_key);
            let ctx_for_usecase = ctx.clone();
            let outcome = idempotency::run(&db, &scope, &request_hash, move |mut tx| async move {
                let body = application::booking::record_attendance_in_tx(
                    &mut tx,
                    &ctx_for_usecase,
                    id,
                    version,
                    req,
                )
                .await?;
                Ok((tx, Stored { status: 200, body }))
            })
            .await?;
            result(outcome.into_inner().body)
        },
    );

    vec![
        slots,
        propose,
        get,
        create_meeting_type,
        update_meeting_type,
        get_availability,
        put_availability,
        create_link,
        list,
        propose_change,
        reschedule,
        cancel,
        attendance,
    ]
}
