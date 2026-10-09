//! Booking module HTTP routes (spec §10, B.2, B.4).

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::IntoResponse;
use chrono::{DateTime, NaiveDate, Utc};
use contracts::booking::*;
use contracts::{AppError, Envelope, Meta, Problem};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::time::Duration;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

use crate::extractors::{AuthCtx, IdempotencyKey, IfMatch, Json, RequestIdHeader};
use crate::problem::ProblemError;
use crate::state::AppState;

fn envelope<T: serde::Serialize>(data: T, request_id: &str) -> Envelope<T> {
    Envelope {
        data,
        meta: Meta {
            request_id: request_id.to_string(),
        },
        page: None,
    }
}

fn etag(version: i64) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&format!("\"{version}\"")) {
        headers.insert(header::ETAG, value);
    }
    headers
}

#[derive(Debug, Deserialize)]
struct BookingListQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct SlotQuery {
    from: String,
    to: String,
    timezone: String,
}

fn parse_date(field: &str, value: &str) -> Result<NaiveDate, AppError> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| AppError::validation(format!("{field} must be an ISO date"), vec![]))
}

pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_meeting_types))
        .routes(routes!(create_meeting_type))
        .routes(routes!(update_meeting_type))
        .routes(routes!(get_availability))
        .routes(routes!(put_availability))
        .routes(routes!(create_booking_link))
        .routes(routes!(list_bookings))
        .routes(routes!(get_booking))
        .routes(routes!(propose_booking_change))
        .routes(routes!(reschedule_booking))
        .routes(routes!(cancel_booking))
        .routes(routes!(record_booking_attendance))
}

pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_public_link))
        .routes(routes!(list_public_slots))
        .routes(routes!(create_public_booking))
        .routes(routes!(get_manage_booking))
        .routes(routes!(reschedule_public_booking))
        .routes(routes!(cancel_public_booking))
}

#[utoipa::path(get, path = "/meeting-types", responses((status = 200, body = [MeetingTypeResponse]), (status = 401, body = Problem)))]
async fn list_meeting_types(
    State(state): State<AppState>,
    auth: AuthCtx,
) -> Result<axum::Json<Envelope<Vec<MeetingTypeResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let rows = application::booking::list_meeting_types(&state.db, &ctx).await?;
    Ok(axum::Json(envelope(rows, &ctx.request_id)))
}

#[utoipa::path(post, path = "/meeting-types", request_body = CreateMeetingTypeRequest,
    responses((status = 201, body = MeetingTypeResponse), (status = 422, body = Problem)))]
async fn create_meeting_type(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<CreateMeetingTypeRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&req)
            .map_err(|e| AppError::internal(format!("meeting type encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/meeting-types".into(),
        key,
    };
    let ctx_for_usecase = ctx.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let row =
                application::booking::create_meeting_type_in_tx(&mut tx, &ctx_for_usecase, &req)
                    .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored {
                    status: 201,
                    body: row,
                },
            ))
        },
    )
    .await?;
    let row = outcome.into_inner().body;
    let headers = etag(row.version);
    Ok((
        StatusCode::CREATED,
        headers,
        axum::Json(envelope(row, &ctx.request_id)),
    ))
}

#[utoipa::path(patch, path = "/meeting-types/{id}", params(("id" = Uuid, Path), ("if-match" = String, Header)),
    request_body = UpdateMeetingTypeRequest, responses((status = 200, body = MeetingTypeResponse), (status = 412, body = Problem), (status = 428, body = Problem)))]
async fn update_meeting_type(
    State(state): State<AppState>,
    auth: AuthCtx,
    IfMatch(version): IfMatch,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateMeetingTypeRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let row = application::booking::update_meeting_type(&state.db, &ctx, id, version, req).await?;
    let headers = etag(row.version);
    Ok((headers, axum::Json(envelope(row, &ctx.request_id))))
}

#[utoipa::path(get, path = "/hosts/{id}/availability", params(("id" = Uuid, Path)),
    responses((status = 200, body = AvailabilityResponse), (status = 404, body = Problem)))]
async fn get_availability(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<axum::Json<Envelope<AvailabilityResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let row = application::booking::get_availability(&state.db, &ctx, id).await?;
    Ok(axum::Json(envelope(row, &ctx.request_id)))
}

#[utoipa::path(put, path = "/hosts/{id}/availability", params(("id" = Uuid, Path)),
    request_body = UpdateAvailabilityRequest, responses((status = 200, body = AvailabilityResponse), (status = 422, body = Problem)))]
async fn put_availability(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateAvailabilityRequest>,
) -> Result<axum::Json<Envelope<AvailabilityResponse>>, ProblemError> {
    let ctx = auth.into_inner();
    let row = application::booking::put_availability(&state.db, &ctx, id, req).await?;
    Ok(axum::Json(envelope(row, &ctx.request_id)))
}

#[utoipa::path(post, path = "/booking-links", request_body = CreateBookingLinkRequest,
    responses((status = 201, body = BookingLinkCreatedResponse), (status = 404, body = Problem)))]
async fn create_booking_link(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<CreateBookingLinkRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&req)
            .map_err(|e| AppError::internal(format!("booking link encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/booking-links".into(),
        key: key.clone(),
    };
    // Raw capability is reproducible for an idempotent replay, but only its
    // hash is stored with the link and the cached response contains no token.
    let seed = format!(
        "booking-link:{}:{}:{}",
        ctx.workspace_id, ctx.principal_id, key
    );
    let raw = infrastructure::crypto::b64_encode(&infrastructure::crypto::hmac_sha256(
        &state.config.hmac_key,
        seed.as_bytes(),
    ));
    let raw_for_closure = raw.clone();
    let ctx_for_usecase = ctx.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let row = application::booking::create_booking_link_in_tx(
                &mut tx,
                &ctx_for_usecase,
                &req,
                &raw_for_closure,
            )
            .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored {
                    status: 201,
                    body: row,
                },
            ))
        },
    )
    .await?;
    let stored = outcome.into_inner();
    let row = BookingLinkCreatedResponse {
        id: stored.body.id,
        token_url: format!("/public/book/{raw}"),
        expires_at: stored.body.expires_at,
        max_uses: stored.body.max_uses,
    };
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(row, &ctx.request_id)),
    ))
}

#[utoipa::path(get, path = "/bookings", params(("from" = Option<DateTime<Utc>>, Query), ("to" = Option<DateTime<Utc>>, Query)),
    responses((status = 200, body = [BookingResponse]), (status = 401, body = Problem)))]
async fn list_bookings(
    State(state): State<AppState>,
    auth: AuthCtx,
    Query(q): Query<BookingListQuery>,
) -> Result<axum::Json<Envelope<Vec<BookingResponse>>>, ProblemError> {
    let ctx = auth.into_inner();
    let rows = application::booking::list_bookings(&state.db, &ctx, q.from, q.to).await?;
    Ok(axum::Json(envelope(rows, &ctx.request_id)))
}

#[utoipa::path(get, path = "/bookings/{id}", params(("id" = Uuid, Path)),
    responses((status = 200, body = BookingResponse), (status = 404, body = Problem)))]
async fn get_booking(
    State(state): State<AppState>,
    auth: AuthCtx,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let row = application::booking::get_booking(&state.db, &ctx, id).await?;
    Ok((
        etag(row.version),
        axum::Json(envelope(row, &ctx.request_id)),
    ))
}

#[utoipa::path(post, path = "/booking-change-proposals", request_body = BookingChangeProposalRequest,
    responses((status = 201, body = BookingChangeResponse), (status = 403, body = Problem)))]
async fn propose_booking_change(
    State(state): State<AppState>,
    auth: AuthCtx,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<BookingChangeProposalRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&req)
            .map_err(|e| AppError::internal(format!("booking proposal encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/booking-change-proposals".into(),
        key,
    };
    let ctx_for_usecase = ctx.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let row =
                application::booking::propose_change_in_tx(&mut tx, &ctx_for_usecase, req).await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored {
                    status: 201,
                    body: row,
                },
            ))
        },
    )
    .await?;
    let row = outcome.into_inner().body;
    Ok((
        StatusCode::CREATED,
        axum::Json(envelope(row, &ctx.request_id)),
    ))
}

#[utoipa::path(post, path = "/bookings/{id}/reschedule", params(("id" = Uuid, Path), ("if-match" = String, Header)),
    request_body = PublicRescheduleRequest, responses((status = 202, body = BookingChangeResponse), (status = 412, body = Problem)))]
async fn reschedule_booking(
    State(state): State<AppState>,
    auth: AuthCtx,
    IfMatch(version): IfMatch,
    IdempotencyKey(key): IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(mut req): Json<PublicRescheduleRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    req.expected_version = version;
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&json!({"booking_id":id,"if_match":version,"body":req}))
            .map_err(|e| AppError::internal(format!("booking reschedule encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/bookings/{id}/reschedule".into(),
        key,
    };
    let ctx_for_usecase = ctx.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let body = application::booking::reschedule_booking_in_tx(
                &mut tx,
                &ctx_for_usecase,
                id,
                version,
                req,
            )
            .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 202, body },
            ))
        },
    )
    .await?;
    let row = outcome.into_inner().body;
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(envelope(row, &ctx.request_id)),
    ))
}

#[utoipa::path(post, path = "/bookings/{id}/cancel", params(("id" = Uuid, Path), ("if-match" = String, Header)),
    responses((status = 202, body = BookingChangeResponse), (status = 412, body = Problem)))]
async fn cancel_booking(
    State(state): State<AppState>,
    auth: AuthCtx,
    IfMatch(version): IfMatch,
    IdempotencyKey(key): IdempotencyKey,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&json!({"booking_id":id,"if_match":version}))
            .map_err(|e| AppError::internal(format!("booking cancel encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/bookings/{id}/cancel".into(),
        key,
    };
    let ctx_for_usecase = ctx.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let body =
                application::booking::cancel_booking_in_tx(&mut tx, &ctx_for_usecase, id, version)
                    .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 202, body },
            ))
        },
    )
    .await?;
    let row = outcome.into_inner().body;
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(envelope(row, &ctx.request_id)),
    ))
}

#[utoipa::path(post, path = "/bookings/{id}/attendance", params(("id" = Uuid, Path), ("if-match" = String, Header)),
    request_body = BookingAttendanceRequest,
    responses((status = 200, body = BookingResponse), (status = 404, body = Problem), (status = 409, body = Problem), (status = 412, body = Problem), (status = 428, body = Problem)))]
async fn record_booking_attendance(
    State(state): State<AppState>,
    auth: AuthCtx,
    IfMatch(version): IfMatch,
    IdempotencyKey(key): IdempotencyKey,
    Path(id): Path<Uuid>,
    Json(req): Json<BookingAttendanceRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    let ctx = auth.into_inner();
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&json!({"booking_id":id,"if_match":version,"body":req}))
            .map_err(|e| AppError::internal(format!("attendance encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: "POST /api/v1/bookings/{id}/attendance".into(),
        key,
    };
    let ctx_for_usecase = ctx.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let row = application::booking::record_attendance_in_tx(
                &mut tx,
                &ctx_for_usecase,
                id,
                version,
                req,
            )
            .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored {
                    status: 200,
                    body: row,
                },
            ))
        },
    )
    .await?;
    let row = outcome.into_inner().body;
    Ok((
        etag(row.version),
        axum::Json(envelope(row, &ctx.request_id)),
    ))
}

#[utoipa::path(get, path = "/booking-links/{token}", params(("token" = String, Path)),
    responses((status = 200, body = PublicBookingLinkResponse), (status = 404, body = Problem), (status = 410, body = Problem)))]
async fn get_public_link(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    request_id: RequestIdHeader,
) -> Result<axum::Json<Envelope<PublicBookingLinkResponse>>, ProblemError> {
    public_booking_read_limit(&state, &headers, &addr).await?;
    let row = application::booking::public_link_details(&state.db, &token).await?;
    Ok(axum::Json(envelope(row, &request_id.0)))
}

#[utoipa::path(get, path = "/booking-links/{token}/slots", params(("token" = String, Path), ("from" = String, Query), ("to" = String, Query), ("timezone" = String, Query)),
    responses((status = 200, body = SlotsResponse), (status = 503, body = Problem)))]
async fn list_public_slots(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Query(q): Query<SlotQuery>,
    request_id: RequestIdHeader,
) -> Result<axum::Json<Envelope<SlotsResponse>>, ProblemError> {
    public_booking_read_limit(&state, &headers, &addr).await?;
    let from = parse_date("from", &q.from)?;
    let to = parse_date("to", &q.to)?;
    let row = application::booking::list_public_slots(
        &state.db,
        state.calendar.as_ref(),
        &token,
        from,
        to,
        &q.timezone,
    )
    .await?;
    Ok(axum::Json(envelope(row, &request_id.0)))
}

#[utoipa::path(post, path = "/booking-links/{token}/bookings", params(("token" = String, Path)),
    request_body = CreatePublicBookingRequest, responses((status = 202, body = BookingAcceptedResponse), (status = 409, body = Problem), (status = 410, body = Problem)))]
async fn create_public_booking(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    request_id: RequestIdHeader,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<CreatePublicBookingRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    public_booking_limit(&state, &headers, &addr).await?;
    let location = infrastructure::public_tokens::booking_link_token(&state.db, &token).await?;
    let capability = infrastructure::crypto::sha256_hex(token.as_bytes());
    let canonical = serde_json::to_vec(&req)
        .map_err(|e| AppError::internal(format!("booking request encode: {e}")))?;
    let request_hash = infrastructure::crypto::sha256_bytes(&canonical);
    let scope = infrastructure::idempotency::Scope {
        workspace_id: location.workspace_id,
        principal_or_capability: capability.clone(),
        endpoint: "POST /public/v1/booking-links/{token}/bookings".into(),
        key: key.clone(),
    };
    // The idempotency utility returns its stored result before entering this
    // closure on replay, so slot revalidation applies only to the leader.
    let idem_key = key.clone();
    let raw_token = token.clone();
    let config_key = state.config.hmac_key.clone();
    let capability_for_closure = capability.clone();
    let request_id_value = request_id.0.clone();
    let db = &state.db;
    let calendar = state.calendar.clone();
    let link_id = location.token_id;
    let outcome =
        infrastructure::idempotency::run(db, &scope, &request_hash, move |mut tx| async move {
            application::booking::validate_public_slot(db, calendar.as_ref(), &raw_token, &req)
                .await?;
            let manage_seed =
                format!("booking-manage:{capability_for_closure}:{idem_key}:{link_id}");
            let manage_token = infrastructure::crypto::b64_encode(
                &infrastructure::crypto::hmac_sha256(&config_key, manage_seed.as_bytes()),
            );
            let created = application::booking::create_public_booking_in_tx(
                &mut tx,
                link_id,
                &req,
                &manage_token,
                &request_id_value,
            )
            .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored {
                    status: 202,
                    body: created,
                },
            ))
        })
        .await?;
    let stored = outcome.into_inner();
    let token_seed = format!("booking-manage:{capability}:{key}:{link_id}");
    let manage_token = infrastructure::crypto::b64_encode(&infrastructure::crypto::hmac_sha256(
        &state.config.hmac_key,
        token_seed.as_bytes(),
    ));
    let body = BookingAcceptedResponse {
        id: stored.body.id,
        status: "confirming".into(),
        starts_at: stored.body.starts_at,
        ends_at: stored.body.ends_at,
        timezone: stored.body.timezone,
        manage_url: format!("/public/booking/{manage_token}"),
        status_url: format!("/public/v1/bookings/{manage_token}"),
        message: "예약 요청을 접수했습니다. 캘린더 확정 결과를 확인해 주세요.".into(),
    };
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(envelope(body, &request_id.0)),
    ))
}

async fn public_booking_limit(
    state: &AppState,
    headers: &HeaderMap,
    addr: &SocketAddr,
) -> Result<(), AppError> {
    if let Some(limiter) = &state.rate_limiter {
        let ip = crate::extractors::client_ip(&state.config, headers, addr.ip());
        limiter
            .require(
                &format!("booking:create:{ip}"),
                20,
                Duration::from_secs(3600),
                infrastructure::ratelimit::FailMode::Closed,
            )
            .await?;
    }
    Ok(())
}

async fn public_booking_read_limit(
    state: &AppState,
    headers: &HeaderMap,
    addr: &SocketAddr,
) -> Result<(), AppError> {
    if let Some(limiter) = &state.rate_limiter {
        let ip = crate::extractors::client_ip(&state.config, headers, addr.ip());
        limiter
            .require(
                &format!("booking:read:{ip}"),
                120,
                Duration::from_secs(60),
                infrastructure::ratelimit::FailMode::Closed,
            )
            .await?;
    }
    Ok(())
}

async fn public_booking_manage_limit(
    state: &AppState,
    headers: &HeaderMap,
    addr: &SocketAddr,
) -> Result<(), AppError> {
    if let Some(limiter) = &state.rate_limiter {
        let ip = crate::extractors::client_ip(&state.config, headers, addr.ip());
        limiter
            .require(
                &format!("booking:manage:{ip}"),
                30,
                Duration::from_secs(60),
                infrastructure::ratelimit::FailMode::Closed,
            )
            .await?;
    }
    Ok(())
}

#[utoipa::path(get, path = "/bookings/{manage_token}", params(("manage_token" = String, Path)),
    responses((status = 200, body = ManageBookingResponse), (status = 404, body = Problem)))]
async fn get_manage_booking(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    request_id: RequestIdHeader,
) -> Result<axum::Json<Envelope<ManageBookingResponse>>, ProblemError> {
    public_booking_read_limit(&state, &headers, &addr).await?;
    let row = application::booking::manage_booking(&state.db, &token).await?;
    Ok(axum::Json(envelope(row, &request_id.0)))
}

#[utoipa::path(post, path = "/bookings/{manage_token}/reschedule", params(("manage_token" = String, Path)),
    request_body = PublicRescheduleRequest, responses((status = 202, body = BookingChangeResponse), (status = 412, body = Problem)))]
async fn reschedule_public_booking(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    request_id: RequestIdHeader,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<PublicRescheduleRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    public_booking_manage_limit(&state, &headers, &addr).await?;
    let location = infrastructure::public_tokens::booking_manage_token(&state.db, &token).await?;
    let capability = infrastructure::crypto::sha256_hex(token.as_bytes());
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&req)
            .map_err(|e| AppError::internal(format!("public reschedule encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: location.workspace_id,
        principal_or_capability: capability,
        endpoint: "POST /public/v1/bookings/{manage_token}/reschedule".into(),
        key,
    };
    let request_id_value = request_id.0.clone();
    let calendar = state.calendar.clone();
    let db = &state.db;
    let manage_token_id = location.token_id;
    let outcome =
        infrastructure::idempotency::run(db, &scope, &request_hash, move |mut tx| async move {
            let body = application::booking::propose_public_reschedule_in_tx(
                db,
                &mut tx,
                calendar.as_ref(),
                manage_token_id,
                req,
                &request_id_value,
            )
            .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 202, body },
            ))
        })
        .await?;
    let row = outcome.into_inner().body;
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(envelope(row, &request_id.0)),
    ))
}

#[utoipa::path(post, path = "/bookings/{manage_token}/cancel", params(("manage_token" = String, Path)),
    request_body = PublicCancelRequest, responses((status = 202, body = BookingChangeResponse), (status = 412, body = Problem)))]
async fn cancel_public_booking(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    request_id: RequestIdHeader,
    IdempotencyKey(key): IdempotencyKey,
    Json(req): Json<PublicCancelRequest>,
) -> Result<impl IntoResponse, ProblemError> {
    public_booking_manage_limit(&state, &headers, &addr).await?;
    let location = infrastructure::public_tokens::booking_manage_token(&state.db, &token).await?;
    let capability = infrastructure::crypto::sha256_hex(token.as_bytes());
    let request_hash = infrastructure::crypto::sha256_bytes(
        &serde_json::to_vec(&req)
            .map_err(|e| AppError::internal(format!("public cancel encode: {e}")))?,
    );
    let scope = infrastructure::idempotency::Scope {
        workspace_id: location.workspace_id,
        principal_or_capability: capability,
        endpoint: "POST /public/v1/bookings/{manage_token}/cancel".into(),
        key,
    };
    let request_id_value = request_id.0.clone();
    let outcome = infrastructure::idempotency::run(
        &state.db,
        &scope,
        &request_hash,
        move |mut tx| async move {
            let body = application::booking::propose_public_cancel_in_tx(
                &mut tx,
                location.token_id,
                req,
                &request_id_value,
            )
            .await?;
            Ok((
                tx,
                infrastructure::idempotency::Stored { status: 202, body },
            ))
        },
    )
    .await?;
    let row = outcome.into_inner().body;
    Ok((
        StatusCode::ACCEPTED,
        axum::Json(envelope(row, &request_id.0)),
    ))
}
