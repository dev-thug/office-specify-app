//! Booking use cases: public capabilities, internal scheduling, and the
//! calendar confirmation saga (spec §10, A.6.6, B.2/B.4).

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use chrono_tz::Tz;
use contracts::booking::*;
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Role, Scope};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::providers::{BusyBlock, CalOutcome, CalendarProvider};
use infrastructure::public_tokens;
use infrastructure::repo::approvals::{self, ApprovalTarget};
use infrastructure::repo::booking::{
    self as repo, AvailabilityExceptionRow, AvailabilityRuleRow, BookingLinkRow, BookingRow,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const HELD_TTL: Duration = Duration::minutes(5);
const RELEASE_REQUEST_TTL_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookingCreateStored {
    pub id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookingLinkStored {
    pub id: Uuid,
    pub expires_at: Option<DateTime<Utc>>,
    pub max_uses: i32,
}

#[derive(Debug, Clone)]
pub struct ResolvedLink {
    pub workspace_id: Uuid,
    pub link_id: Uuid,
    pub link: BookingLinkRow,
}

fn require_scope(ctx: &Ctx, scope: Scope) -> Result<(), AppError> {
    if ctx.scopes.contains(scope) {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::ScopeRequired,
            format!("missing scope {}", scope.as_str()),
        ))
    }
}

fn require_operator(ctx: &Ctx) -> Result<(), AppError> {
    if matches!(
        ctx.principal_kind,
        PrincipalKind::Human | PrincipalKind::Agent
    ) {
        Ok(())
    } else {
        Err(AppError::forbidden(
            "booking changes require a human or agent principal",
        ))
    }
}

fn can_manage_host(ctx: &Ctx, host_id: Uuid) -> bool {
    ctx.principal_id == host_id
        || (matches!(ctx.role, Role::Admin | Role::Manager) || crate::authz::agent_admin(ctx))
}

fn hidden_not_found() -> AppError {
    AppError::not_found("booking not found")
}

fn field_error(field: &str, detail: &str) -> AppError {
    AppError::validation(
        detail,
        vec![contracts::Violation {
            field: field.to_string(),
            code: "invalid".into(),
        }],
    )
    .with_status(422)
}

fn parse_timezone(value: &str, field: &str) -> Result<Tz, AppError> {
    value
        .parse::<Tz>()
        .map_err(|_| field_error(field, "must be a valid IANA timezone"))
}

fn validate_location(kind: Option<&str>, detail: Option<&str>) -> Result<(), AppError> {
    if kind.is_some_and(|k| !matches!(k, "online" | "offline")) {
        return Err(field_error(
            "location_kind",
            "location_kind must be online or offline",
        ));
    }
    if detail.is_some_and(|d| d.trim().chars().count() > 500) {
        return Err(field_error(
            "location_detail",
            "location_detail must be at most 500 characters",
        ));
    }
    Ok(())
}

fn validate_meeting_type(req: &CreateMeetingTypeRequest) -> Result<(), AppError> {
    validate_location(Some(&req.location_kind), req.location_detail.as_deref())?;
    if req.name.trim().is_empty() || req.name.trim().len() > 200 {
        return Err(field_error("name", "name must be 1..=200 characters"));
    }
    if !(5..=480).contains(&req.duration_minutes) {
        return Err(field_error(
            "duration_minutes",
            "duration must be 5..=480 minutes",
        ));
    }
    if req.buffer_before_minutes < 0 || req.buffer_after_minutes < 0 {
        return Err(field_error("buffer_minutes", "buffers cannot be negative"));
    }
    if req.minimum_notice_minutes < 0 || !(1..=365).contains(&req.booking_horizon_days) {
        return Err(field_error(
            "booking_window",
            "minimum notice must be nonnegative and horizon 1..=365 days",
        ));
    }
    Ok(())
}

fn validate_update_meeting_type(req: &UpdateMeetingTypeRequest) -> Result<(), AppError> {
    validate_location(
        req.location_kind.as_deref(),
        req.location_detail.as_ref().and_then(|d| d.as_deref()),
    )?;
    if req
        .name
        .as_ref()
        .is_some_and(|v| v.trim().is_empty() || v.trim().len() > 200)
    {
        return Err(field_error("name", "name must be 1..=200 characters"));
    }
    if req
        .duration_minutes
        .is_some_and(|v| !(5..=480).contains(&v))
    {
        return Err(field_error(
            "duration_minutes",
            "duration must be 5..=480 minutes",
        ));
    }
    if req.buffer_before_minutes.is_some_and(|v| v < 0)
        || req.buffer_after_minutes.is_some_and(|v| v < 0)
    {
        return Err(field_error("buffer_minutes", "buffers cannot be negative"));
    }
    if req.minimum_notice_minutes.is_some_and(|v| v < 0)
        || req
            .booking_horizon_days
            .is_some_and(|v| !(1..=365).contains(&v))
    {
        return Err(field_error(
            "booking_window",
            "minimum notice must be nonnegative and horizon 1..=365 days",
        ));
    }
    if req
        .status
        .as_deref()
        .is_some_and(|s| !matches!(s, "active" | "disabled"))
    {
        return Err(field_error("status", "status must be active or disabled"));
    }
    Ok(())
}

fn meeting_response(row: repo::MeetingTypeRow) -> MeetingTypeResponse {
    MeetingTypeResponse {
        id: row.id,
        host_principal_id: row.host_principal_id,
        calendar_connection_id: row.calendar_connection_id,
        name: row.name,
        description: row.description,
        duration_minutes: row.duration_minutes,
        buffer_before_minutes: row.buffer_before_minutes,
        buffer_after_minutes: row.buffer_after_minutes,
        minimum_notice_minutes: row.minimum_notice_minutes,
        booking_horizon_days: row.booking_horizon_days,
        location_kind: row.location_kind,
        location_detail: row.location_detail,
        status: row.status,
        version: row.version,
    }
}

fn booking_response(row: BookingRow) -> BookingResponse {
    BookingResponse {
        id: row.id,
        booking_link_id: row.booking_link_id,
        host_principal_id: row.host_principal_id,
        guest_email_address_id: row.guest_email_address_id,
        guest_email: row.guest_email,
        guest_name: row.guest_name,
        guest_memo: row.guest_memo,
        guest_timezone: row.guest_timezone,
        starts_at: row.starts_at,
        ends_at: row.ends_at,
        status: row.status,
        failure_code: row.failure_code,
        provider_event_id: row.provider_event_id,
        attendance_status: row.attendance_status,
        outcome_note: row.outcome_note,
        version: row.version,
    }
}

fn ensure_link_usable(link: &BookingLinkRow) -> Result<(), AppError> {
    if link.revoked_at.is_some()
        || link.meeting_status != "active"
        || link.calendar_status != "active"
        || link.integration_status != "active"
    {
        return Err(AppError::not_found("booking link not found"));
    }
    if link.expires_at.is_some_and(|until| until <= Utc::now())
        || link.used_count >= i64::from(link.max_uses)
    {
        return Err(AppError::new(
            ErrorCode::BookingLinkExpired,
            "booking link expired",
        ));
    }
    Ok(())
}

pub async fn create_meeting_type(
    db: &Db,
    ctx: &Ctx,
    req: CreateMeetingTypeRequest,
) -> Result<MeetingTypeResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = create_meeting_type_in_tx(&mut tx, ctx, &req).await?;
    tx.commit().await?;
    Ok(row)
}

pub async fn create_meeting_type_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateMeetingTypeRequest,
) -> Result<MeetingTypeResponse, AppError> {
    require_operator(ctx)?;
    require_scope(ctx, Scope::BookingPropose)?;
    validate_meeting_type(req)?;
    if !repo::calendar_connection_active_for_host(tx, req.calendar_connection_id, ctx.principal_id)
        .await?
    {
        return Err(AppError::not_found(
            "active calendar connection not found for this host",
        ));
    }
    let id = repo::insert_meeting_type(tx, ctx.principal_id, req).await?;
    let row = repo::get_meeting_type(tx, id)
        .await?
        .ok_or_else(|| AppError::internal("meeting type missing after insert"))?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "meeting_type.created",
            "meeting_type",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    Ok(meeting_response(row))
}

pub async fn list_meeting_types(db: &Db, ctx: &Ctx) -> Result<Vec<MeetingTypeResponse>, AppError> {
    require_scope(ctx, Scope::BookingRead)?;
    let host =
        if (matches!(ctx.role, Role::Admin | Role::Manager) || crate::authz::agent_admin(ctx)) {
            None
        } else {
            Some(ctx.principal_id)
        };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_meeting_types(&mut tx, host).await?;
    tx.rollback().await?;
    Ok(rows.into_iter().map(meeting_response).collect())
}

/// A booking.read agent may discover safe type metadata in its workspace.
/// This is deliberately separate from host management and slot/proposal ACLs:
/// discovery grants neither calendar access nor authority to reserve a slot.
pub async fn list_discoverable_meeting_types(
    db: &Db,
    ctx: &Ctx,
) -> Result<Vec<MeetingTypeDiscoveryResponse>, AppError> {
    require_scope(ctx, Scope::BookingRead)?;
    if ctx.principal_kind != PrincipalKind::Agent {
        return Ok(list_meeting_types(db, ctx)
            .await?
            .into_iter()
            .map(Into::into)
            .collect());
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_meeting_types(&mut tx, None).await?;
    tx.rollback().await?;
    Ok(rows
        .into_iter()
        .map(meeting_response)
        .map(Into::into)
        .collect())
}

pub async fn update_meeting_type(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: UpdateMeetingTypeRequest,
) -> Result<MeetingTypeResponse, AppError> {
    require_operator(ctx)?;
    require_scope(ctx, Scope::BookingPropose)?;
    validate_update_meeting_type(&req)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(current) = repo::get_meeting_type(&mut tx, id).await? else {
        tx.rollback().await?;
        return Err(AppError::not_found("meeting type not found"));
    };
    if !can_manage_host(ctx, current.host_principal_id) {
        tx.rollback().await?;
        return Err(hidden_not_found());
    }
    match repo::update_meeting_type(&mut tx, id, expected_version, &req).await? {
        Some(row) => {
            audit::record(
                &mut tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "meeting_type.updated",
                    "meeting_type",
                    ctx.request_id.clone(),
                )
                .with_target(id),
            )
            .await?;
            tx.commit().await?;
            Ok(meeting_response(row))
        }
        None if current.version != expected_version => Err(AppError::new(
            ErrorCode::VersionMismatch,
            "meeting type version changed",
        )),
        None => Err(AppError::conflict("meeting type could not be updated")),
    }
}

async fn authorize_host_access(tx: &mut Tx<'_>, ctx: &Ctx, host_id: Uuid) -> Result<(), AppError> {
    require_operator(ctx)?;
    require_scope(ctx, Scope::BookingRead)?;
    if !can_manage_host(ctx, host_id) {
        return Err(hidden_not_found());
    }
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM principals WHERE workspace_id = $1 AND id = $2 AND kind = 'human' AND status = 'active')",
    )
    .bind(ctx.workspace_id).bind(host_id).fetch_one(tx.conn()).await
    .map_err(infrastructure::error::map_sqlx)?;
    if !active {
        return Err(hidden_not_found());
    }
    Ok(())
}

fn validate_availability(req: &UpdateAvailabilityRequest) -> Result<(), AppError> {
    for (i, rule) in req.rules.iter().enumerate() {
        parse_timezone(&rule.timezone, &format!("rules[{i}].timezone"))?;
        if !(1..=7).contains(&rule.weekday) {
            return Err(field_error("weekday", "weekday must be 1..=7"));
        }
        let start = NaiveTime::parse_from_str(&rule.start_local, "%H:%M")
            .map_err(|_| field_error("start_local", "must use HH:MM"))?;
        let end = NaiveTime::parse_from_str(&rule.end_local, "%H:%M")
            .map_err(|_| field_error("end_local", "must use HH:MM"))?;
        if end <= start {
            return Err(field_error(
                "end_local",
                "end_local must be after start_local",
            ));
        }
        if rule
            .valid_from
            .zip(rule.valid_until)
            .is_some_and(|(a, b)| b < a)
        {
            return Err(field_error(
                "valid_until",
                "valid_until precedes valid_from",
            ));
        }
    }
    for exception in &req.exceptions {
        if exception.ends_at <= exception.starts_at
            || !matches!(exception.kind.as_str(), "blocked" | "available")
        {
            return Err(field_error(
                "exceptions",
                "exceptions need a positive range and kind blocked or available",
            ));
        }
    }
    Ok(())
}

fn availability_response(
    host_id: Uuid,
    rules: Vec<AvailabilityRuleRow>,
    exceptions: Vec<AvailabilityExceptionRow>,
) -> AvailabilityResponse {
    AvailabilityResponse {
        host_principal_id: host_id,
        rules: rules
            .into_iter()
            .map(|r| AvailabilityRuleRequest {
                timezone: r.timezone,
                weekday: r.weekday,
                start_local: r.start_local.format("%H:%M").to_string(),
                end_local: r.end_local.format("%H:%M").to_string(),
                valid_from: r.valid_from,
                valid_until: r.valid_until,
            })
            .collect(),
        exceptions: exceptions
            .into_iter()
            .map(|e| AvailabilityExceptionRequest {
                starts_at: e.starts_at,
                ends_at: e.ends_at,
                kind: e.kind,
                reason: e.reason,
            })
            .collect(),
    }
}

pub async fn get_availability(
    db: &Db,
    ctx: &Ctx,
    host_id: Uuid,
) -> Result<AvailabilityResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    authorize_host_access(&mut tx, ctx, host_id).await?;
    let rules = repo::availability_rules(&mut tx, host_id).await?;
    let exceptions = repo::availability_exceptions(&mut tx, host_id).await?;
    tx.rollback().await?;
    Ok(availability_response(host_id, rules, exceptions))
}

pub async fn put_availability(
    db: &Db,
    ctx: &Ctx,
    host_id: Uuid,
    req: UpdateAvailabilityRequest,
) -> Result<AvailabilityResponse, AppError> {
    require_scope(ctx, Scope::BookingPropose)?;
    validate_availability(&req)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    authorize_host_access(&mut tx, ctx, host_id).await?;
    repo::replace_availability(&mut tx, host_id, &req).await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "availability.updated",
            "principal",
            ctx.request_id.clone(),
        )
        .with_target(host_id),
    )
    .await?;
    let rules = repo::availability_rules(&mut tx, host_id).await?;
    let exceptions = repo::availability_exceptions(&mut tx, host_id).await?;
    tx.commit().await?;
    Ok(availability_response(host_id, rules, exceptions))
}

pub async fn create_booking_link(
    db: &Db,
    ctx: &Ctx,
    req: CreateBookingLinkRequest,
    raw_token: &str,
) -> Result<BookingLinkCreatedResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let stored = create_booking_link_in_tx(&mut tx, ctx, &req, raw_token).await?;
    tx.commit().await?;
    Ok(BookingLinkCreatedResponse {
        id: stored.id,
        token_url: format!("/public/book/{raw_token}"),
        expires_at: stored.expires_at,
        max_uses: stored.max_uses,
    })
}

pub async fn create_booking_link_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateBookingLinkRequest,
    raw_token: &str,
) -> Result<BookingLinkStored, AppError> {
    require_operator(ctx)?;
    require_scope(ctx, Scope::BookingPropose)?;
    if req.max_uses < 1 || req.expires_at.is_some_and(|t| t <= Utc::now()) {
        return Err(field_error(
            "expires_at",
            "max_uses must be positive and expiry must be in the future",
        ));
    }
    let Some(mt) = repo::get_meeting_type(tx, req.meeting_type_id).await? else {
        return Err(AppError::not_found("meeting type not found"));
    };
    if !can_manage_host(ctx, mt.host_principal_id) {
        return Err(hidden_not_found());
    }
    if mt.status != "active"
        || !repo::calendar_connection_active_for_host(
            tx,
            mt.calendar_connection_id,
            mt.host_principal_id,
        )
        .await?
    {
        return Err(AppError::new(
            ErrorCode::CalendarAvailabilityUnavailable,
            "meeting type calendar is unavailable",
        ));
    }
    if let Some(address_id) = req.bound_email_address_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM email_addresses WHERE workspace_id = $1 AND id = $2)",
        )
        .bind(ctx.workspace_id)
        .bind(address_id)
        .fetch_one(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        if !exists {
            return Err(AppError::not_found("email address not found"));
        }
    }
    let hash = infrastructure::crypto::sha256_bytes(raw_token.as_bytes());
    let id = repo::insert_booking_link(tx, mt.host_principal_id, req, &hash).await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "booking_link.created",
            "booking_link",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    Ok(BookingLinkStored {
        id,
        expires_at: req.expires_at,
        max_uses: req.max_uses,
    })
}

async fn bound_link_context_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    meeting_type_id: Uuid,
) -> Result<(repo::MeetingTypeRow, String), AppError> {
    require_scope(ctx, Scope::BookingPropose)?;
    let actor_kind = match ctx.principal_kind {
        PrincipalKind::Human => "human",
        PrincipalKind::Agent => "agent",
        PrincipalKind::System => "system",
    };
    if !repo::active_principal_kind(tx, ctx.principal_id, actor_kind).await? {
        return Err(AppError::new(
            ErrorCode::PrincipalDisabled,
            "booking-link requester is inactive",
        ));
    }
    if let AuthMethod::ServiceCredential { credential_id } = &ctx.auth {
        let required_scopes: &[&str] = if ctx.principal_kind == PrincipalKind::Agent {
            &[Scope::BookingPropose.as_str(), Scope::BookingRead.as_str()]
        } else {
            &[Scope::BookingPropose.as_str()]
        };
        if !infrastructure::repo::auth::credential_authorizes(
            tx,
            *credential_id,
            Some(ctx.principal_id),
            required_scopes,
        )
        .await?
        {
            return Err(AppError::new(
                ErrorCode::AuthenticationRequired,
                "service credential is revoked, expired, or lacks booking scopes",
            ));
        }
    } else if ctx.principal_kind == PrincipalKind::Agent {
        return Err(AppError::forbidden(
            "Agent booking-link issuance requires a live service credential",
        ));
    }
    let meeting = repo::get_meeting_type(tx, meeting_type_id)
        .await?
        .ok_or_else(|| AppError::not_found("meeting type not found"))?;
    match ctx.principal_kind {
        PrincipalKind::Human if can_manage_host(ctx, meeting.host_principal_id) => {}
        PrincipalKind::Human => {
            return Err(AppError::forbidden(
                "only the meeting host or a manager may issue its booking links",
            ));
        }
        PrincipalKind::Agent => {
            require_scope(ctx, Scope::BookingRead)?;
        }
        PrincipalKind::System => {
            return Err(AppError::forbidden(
                "system principal cannot issue booking links",
            ));
        }
    }
    if meeting.status != "active"
        || !repo::calendar_connection_active_for_host(
            tx,
            meeting.calendar_connection_id,
            meeting.host_principal_id,
        )
        .await?
    {
        return Err(AppError::new(
            ErrorCode::CalendarAvailabilityUnavailable,
            "meeting type calendar is unavailable",
        ));
    }
    let public_base_url = repo::configured_public_base_url(tx)
        .await?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::dependency_unavailable("public booking base URL is not configured")
        })?;
    if public_base_url.chars().any(char::is_whitespace)
        || !(public_base_url.starts_with("https://")
            || public_base_url.starts_with("http://localhost")
            || public_base_url.starts_with("http://127.0.0.1"))
    {
        return Err(AppError::dependency_unavailable(
            "public booking base URL is invalid",
        ));
    }
    Ok((meeting, public_base_url.trim_end_matches('/').to_string()))
}

/// Check the system `meeting.booking_url` binding prerequisites without
/// minting a capability. Campaign preflight can call this before freezing a
/// revision; freeze still issues one email-bound link per recipient.
pub async fn validate_bound_link_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    meeting_type_id: Uuid,
) -> Result<(), AppError> {
    let _ = bound_link_context_in_tx(tx, ctx, meeting_type_id).await?;
    Ok(())
}

/// Mint the immutable `meeting.booking_url` system value for one recipient.
/// The capability is returned only to the caller that snapshots the message;
/// persistence, audit, and outbox contain the hash or row identifiers only.
pub async fn issue_bound_link_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    meeting_type_id: Uuid,
    email_address_id: Uuid,
    request_id: &str,
) -> Result<String, AppError> {
    let (meeting, base_url) = bound_link_context_in_tx(tx, ctx, meeting_type_id).await?;
    let email_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM email_addresses WHERE workspace_id = $1 AND id = $2)",
    )
    .bind(ctx.workspace_id)
    .bind(email_address_id)
    .fetch_one(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    if !email_exists {
        return Err(AppError::not_found("email address not found"));
    }

    let token = infrastructure::crypto::generate_token();
    let hash = infrastructure::crypto::sha256_bytes(token.as_bytes());
    let request = CreateBookingLinkRequest {
        meeting_type_id,
        bound_email_address_id: Some(email_address_id),
        campaign_enrollment_id: None,
        max_uses: 1,
        expires_at: None,
    };
    let link_id = repo::insert_booking_link(tx, meeting.host_principal_id, &request, &hash).await?;
    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: Some(ctx.principal_id),
            requested_by: Some(ctx.principal_id),
            approved_by: None,
            job_id: None,
            action: "booking_link.created".into(),
            target_type: "booking_link".into(),
            target_id: Some(link_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({
                "meeting_type_id": meeting_type_id,
                "email_address_id": email_address_id,
                "max_uses": 1,
            }),
            request_id: request_id.to_string(),
            trace_id: None,
        },
    )
    .await?;
    outbox::emit(
        tx,
        "booking.link_created",
        "booking_link",
        link_id,
        1,
        serde_json::json!({
            "booking_link_id": link_id,
            "meeting_type_id": meeting_type_id,
            "email_address_id": email_address_id,
        }),
    )
    .await?;
    Ok(format!("{base_url}/public/book/{token}"))
}

pub async fn resolve_public_link(db: &Db, raw_token: &str) -> Result<ResolvedLink, AppError> {
    let location = public_tokens::booking_link_token(db, raw_token).await?;
    let mut tx = db.begin(location.workspace_id).await?;
    let Some(link) = repo::get_booking_link_by_id_from_token(&mut tx, location.token_id).await?
    else {
        tx.rollback().await?;
        return Err(AppError::not_found("booking link not found"));
    };
    if link.revoked_at.is_some() {
        tx.rollback().await?;
        return Err(AppError::not_found("booking link not found"));
    }
    if link.expires_at.is_some_and(|t| t <= Utc::now())
        || link.used_count >= i64::from(link.max_uses)
    {
        tx.rollback().await?;
        return Err(AppError::new(
            ErrorCode::BookingLinkExpired,
            "booking link expired",
        ));
    }
    let resolved = ResolvedLink {
        workspace_id: location.workspace_id,
        link_id: location.token_id,
        link,
    };
    tx.rollback().await?;
    Ok(resolved)
}

/// The booking a used-up link already produced, so reopening the invitation
/// lands on its status page instead of "link unavailable". Only links that
/// identify one guest qualify — bound to a recipient, or single-use — so an
/// open multi-use link never reveals someone else's booking.
pub struct LinkBooking {
    pub workspace_id: Uuid,
    pub link_id: Uuid,
    pub booking_id: Uuid,
    pub starts_at: DateTime<Utc>,
}

pub async fn link_booking(db: &Db, raw_token: &str) -> Result<LinkBooking, AppError> {
    let location = public_tokens::booking_link_token(db, raw_token).await?;
    let mut tx = db.begin(location.workspace_id).await?;
    let link = repo::get_booking_link_by_id_from_token(&mut tx, location.token_id).await?;
    let booking = match &link {
        Some(link)
            if link.revoked_at.is_none()
                && (link.bound_email_address_id.is_some() || link.max_uses == 1) =>
        {
            repo::latest_active_booking_for_link(&mut tx, link.id).await?
        }
        _ => None,
    };
    tx.rollback().await?;
    let (booking_id, starts_at) =
        booking.ok_or_else(|| AppError::not_found("booking not found"))?;
    Ok(LinkBooking {
        workspace_id: location.workspace_id,
        link_id: location.token_id,
        booking_id,
        starts_at,
    })
}

/// Make sure `raw_manage_token` opens `booking` (idempotent).
pub async fn ensure_link_manage_token(
    db: &Db,
    booking: &LinkBooking,
    raw_manage_token: &str,
) -> Result<(), AppError> {
    let mut tx = db.begin(booking.workspace_id).await?;
    let token_hash = infrastructure::crypto::sha256_bytes(raw_manage_token.as_bytes());
    repo::ensure_manage_token(
        &mut tx,
        booking.booking_id,
        &token_hash,
        booking.starts_at + Duration::days(90),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn public_link_details(
    db: &Db,
    raw_token: &str,
) -> Result<PublicBookingLinkResponse, AppError> {
    let resolved = resolve_public_link(db, raw_token).await?;
    if resolved.link.meeting_status != "active"
        || resolved.link.calendar_status != "active"
        || resolved.link.integration_status != "active"
    {
        return Err(AppError::new(
            ErrorCode::CalendarAvailabilityUnavailable,
            "calendar is unavailable",
        ));
    }
    let (timezone, privacy_notice_version, invitee) = {
        let mut tx = db.begin(resolved.workspace_id).await?;
        let rows = repo::availability_rules(&mut tx, resolved.link.host_principal_id).await?;
        let version = repo::published_privacy_notice_version(&mut tx)
            .await?
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                AppError::dependency_unavailable(
                    "published booking privacy notice is not configured",
                )
            })?;
        let invitee = match resolved.link.bound_email_address_id {
            Some(address_id) => bound_invitee(&mut tx, address_id).await?,
            None => None,
        };
        tx.rollback().await?;
        (
            rows.first()
                .map(|r| r.timezone.clone())
                .unwrap_or_else(|| "UTC".into()),
            version,
            invitee,
        )
    };
    Ok(PublicBookingLinkResponse {
        meeting_type_id: resolved.link.meeting_type_id,
        name: resolved.link.meeting_name,
        description: resolved.link.meeting_description,
        host_name: resolved.link.host_name,
        duration_minutes: resolved.link.duration_minutes,
        timezone,
        minimum_notice_minutes: resolved.link.minimum_notice_minutes,
        booking_horizon_days: resolved.link.booking_horizon_days,
        privacy_notice_version,
        invitee,
        location_kind: resolved.link.location_kind,
        location_detail: resolved.link.location_detail,
    })
}

/// Prefill facts for a link bound to one recipient: the invited address and,
/// when the CRM knows it, the contact and company behind that address.
async fn bound_invitee(
    tx: &mut Tx<'_>,
    address_id: Uuid,
) -> Result<Option<PublicBookingInvitee>, AppError> {
    use infrastructure::repo::campaigns as crm;
    let Some((email, _)) = crm::get_email_address(tx, address_id).await? else {
        return Ok(None);
    };
    let (mut name, mut company_name) = (None, None);
    if let Some(contact_id) = crm::first_contact_for_address(tx, address_id).await?
        && let Some((contact_name, company)) =
            crm::contact_and_company_names(tx, contact_id).await?
    {
        name = Some(contact_name).filter(|value| !value.trim().is_empty());
        company_name = company.filter(|value| !value.trim().is_empty());
    }
    Ok(Some(PublicBookingInvitee {
        email,
        name,
        company_name,
    }))
}

fn day_bounds(date: NaiveDate, timezone: Tz) -> Result<(DateTime<Utc>, DateTime<Utc>), AppError> {
    let next = date
        .succ_opt()
        .ok_or_else(|| field_error("to", "date is out of range"))?;
    let start_local = date.and_hms_opt(0, 0, 0).expect("valid midnight");
    let end_local = next.and_hms_opt(0, 0, 0).expect("valid midnight");
    let start = domain::booking::local_instants(start_local, timezone)
        .into_iter()
        .next()
        .ok_or_else(|| field_error("from", "midnight does not exist in timezone"))?;
    let end = domain::booking::local_instants(end_local, timezone)
        .into_iter()
        .next()
        .ok_or_else(|| field_error("to", "midnight does not exist in timezone"))?;
    Ok((start, end))
}

fn intersects(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    other_start: DateTime<Utc>,
    other_end: DateTime<Utc>,
) -> bool {
    start < other_end && other_start < end
}

fn in_query_range(start: DateTime<Utc>, range: (DateTime<Utc>, DateTime<Utc>)) -> bool {
    start >= range.0 && start < range.1
}

fn rule_generates(
    rule: &AvailabilityRuleRow,
    date: NaiveDate,
    duration: i32,
    range: (DateTime<Utc>, DateTime<Utc>),
    earliest: DateTime<Utc>,
    latest: DateTime<Utc>,
) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    if i16::try_from(date.weekday().number_from_monday()).ok() != Some(rule.weekday)
        || rule.valid_from.is_some_and(|d| date < d)
        || rule.valid_until.is_some_and(|d| date > d)
    {
        return Vec::new();
    }
    let Ok(timezone) = rule.timezone.parse::<Tz>() else {
        return Vec::new();
    };
    let mut local_start_seconds = rule.start_local.num_seconds_from_midnight();
    let end_local_seconds = rule.end_local.num_seconds_from_midnight();
    let slot_duration_seconds = u32::try_from(duration).unwrap_or_default() * 60;
    let slot_duration = Duration::seconds(i64::from(slot_duration_seconds));
    let mut slots = Vec::new();
    while local_start_seconds.saturating_add(slot_duration_seconds) <= end_local_seconds {
        let Some(local_start) =
            NaiveTime::from_num_seconds_from_midnight_opt(local_start_seconds, 0)
        else {
            break;
        };
        let local = NaiveDateTime::new(date, local_start);
        for start in domain::booking::local_instants(local, timezone) {
            let end = start + slot_duration;
            if in_query_range(start, range) && start >= earliest && start <= latest {
                slots.push((start, end));
            }
        }
        local_start_seconds = local_start_seconds.saturating_add(slot_duration_seconds);
    }
    slots
}

fn available_exception_slots(
    exception: &AvailabilityExceptionRow,
    duration: i32,
    range: (DateTime<Utc>, DateTime<Utc>),
    earliest: DateTime<Utc>,
    latest: DateTime<Utc>,
) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let slot_duration = Duration::minutes(i64::from(duration));
    let mut start = exception.starts_at;
    let mut slots = Vec::new();
    while start + slot_duration <= exception.ends_at {
        let end = start + slot_duration;
        if in_query_range(start, range) && start >= earliest && start <= latest {
            slots.push((start, end));
        }
        start += slot_duration;
    }
    slots
}

async fn compute_slots(
    db: &Db,
    calendar: &dyn CalendarProvider,
    resolved: ResolvedLink,
    from: NaiveDate,
    to: NaiveDate,
    display_timezone: &str,
) -> Result<SlotsResponse, AppError> {
    if to < from || to.signed_duration_since(from).num_days() > 30 {
        return Err(field_error(
            "to",
            "date range must be ordered and at most 31 days",
        ));
    }
    let display_tz = parse_timezone(display_timezone, "timezone")?;
    if resolved.link.calendar_status != "active"
        || resolved.link.integration_status != "active"
        || resolved.link.meeting_status != "active"
    {
        return Err(AppError::new(
            ErrorCode::CalendarAvailabilityUnavailable,
            "calendar availability is unavailable",
        ));
    }
    let range = (
        day_bounds(from, display_tz)?.0,
        day_bounds(to, display_tz)?.1,
    );
    let before = Duration::minutes(i64::from(resolved.link.buffer_before_minutes));
    let after = Duration::minutes(i64::from(resolved.link.buffer_after_minutes));
    let availability_range = (range.0 - before, range.1 + after);
    let (rules, exceptions, holds) = {
        let mut tx = db.begin(resolved.workspace_id).await?;
        let rules = repo::availability_rules(&mut tx, resolved.link.host_principal_id).await?;
        let exceptions =
            repo::availability_exceptions(&mut tx, resolved.link.host_principal_id).await?;
        let holds = repo::active_holds_between(
            &mut tx,
            resolved.link.host_principal_id,
            availability_range.0,
            availability_range.1,
        )
        .await?;
        tx.rollback().await?;
        (rules, exceptions, holds)
    };
    let busy = match calendar
        .free_busy_scoped(
            resolved.workspace_id,
            &resolved.link.provider_calendar_id,
            availability_range.0,
            availability_range.1,
            None,
            None,
        )
        .await
    {
        CalOutcome::Ok(blocks) => blocks,
        CalOutcome::Failed { .. } | CalOutcome::Unknown => {
            return Err(AppError::new(
                ErrorCode::CalendarAvailabilityUnavailable,
                "calendar availability could not be checked",
            ));
        }
    };
    let now = Utc::now();
    let earliest = now + Duration::minutes(i64::from(resolved.link.minimum_notice_minutes));
    let latest = now + Duration::days(i64::from(resolved.link.booking_horizon_days));
    let mut slots = Vec::new();
    let range_start_local = range
        .0
        .with_timezone(&display_tz)
        .date_naive()
        .pred_opt()
        .unwrap_or(from);
    let range_end_local = range
        .1
        .with_timezone(&display_tz)
        .date_naive()
        .succ_opt()
        .unwrap_or(to);
    for rule in &rules {
        let Ok(rule_tz) = rule.timezone.parse::<Tz>() else {
            return Err(AppError::new(
                ErrorCode::CalendarAvailabilityUnavailable,
                "availability timezone is invalid",
            ));
        };
        let start_date = range
            .0
            .with_timezone(&rule_tz)
            .date_naive()
            .pred_opt()
            .unwrap_or(range_start_local);
        let end_date = range
            .1
            .with_timezone(&rule_tz)
            .date_naive()
            .succ_opt()
            .unwrap_or(range_end_local);
        let mut date = start_date;
        while date <= end_date {
            slots.extend(rule_generates(
                rule,
                date,
                resolved.link.duration_minutes,
                range,
                earliest,
                latest,
            ));
            let Some(next) = date.succ_opt() else {
                break;
            };
            date = next;
        }
    }
    for exception in exceptions.iter().filter(|e| e.kind == "available") {
        slots.extend(available_exception_slots(
            exception,
            resolved.link.duration_minutes,
            range,
            earliest,
            latest,
        ));
    }
    slots.sort_by_key(|(start, _)| *start);
    slots.dedup_by_key(|(start, _)| *start);
    slots.retain(|(start, end)| {
        let occupied_start = *start - before;
        let occupied_end = *end + after;
        !exceptions.iter().any(|e| {
            e.kind == "blocked" && intersects(occupied_start, occupied_end, e.starts_at, e.ends_at)
        }) && !holds
            .iter()
            .any(|h| intersects(occupied_start, occupied_end, h.starts_at, h.ends_at))
            && !busy
                .iter()
                .any(|b: &BusyBlock| intersects(occupied_start, occupied_end, b.start, b.end))
    });
    Ok(SlotsResponse {
        timezone: display_timezone.to_string(),
        availability_checked_at: now,
        slots: slots
            .into_iter()
            .map(|(starts_at, ends_at)| SlotResponse { starts_at, ends_at })
            .collect(),
        reservation_guaranteed: false,
    })
}

pub async fn list_public_slots(
    db: &Db,
    calendar: &dyn CalendarProvider,
    raw_token: &str,
    from: NaiveDate,
    to: NaiveDate,
    timezone: &str,
) -> Result<SlotsResponse, AppError> {
    let resolved = resolve_public_link(db, raw_token).await?;
    compute_slots(db, calendar, resolved, from, to, timezone).await
}

pub async fn list_meeting_slots(
    db: &Db,
    calendar: &dyn CalendarProvider,
    ctx: &Ctx,
    meeting_type_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    timezone: &str,
) -> Result<SlotsResponse, AppError> {
    require_scope(ctx, Scope::BookingRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(meeting) = repo::get_meeting_type(&mut tx, meeting_type_id).await? else {
        tx.rollback().await?;
        return Err(AppError::not_found("meeting type not found"));
    };
    if !can_manage_host(ctx, meeting.host_principal_id) {
        tx.rollback().await?;
        return Err(hidden_not_found());
    }
    let link: Option<BookingLinkRow> = sqlx::query_as(
        "SELECT m.id AS id, m.id AS meeting_type_id, m.host_principal_id,
                NULL::uuid AS bound_email_address_id, NULL::uuid AS campaign_enrollment_id,
                1 AS max_uses, NULL::timestamptz AS expires_at, NULL::timestamptz AS revoked_at,
                0::bigint AS used_count, m.name AS meeting_name, m.description AS meeting_description,
                m.duration_minutes, m.buffer_before_minutes, m.buffer_after_minutes,
                m.minimum_notice_minutes, m.booking_horizon_days,
                m.location_kind, m.location_detail, m.status AS meeting_status,
                c.id AS calendar_connection_id, c.provider_calendar_id, c.status AS calendar_status,
                i.status AS integration_status,
                p.name AS host_name
           FROM meeting_types m
           JOIN calendar_connections c ON c.workspace_id = m.workspace_id AND c.id = m.calendar_connection_id
           JOIN integration_connections i ON i.workspace_id = c.workspace_id AND i.id = c.connection_id
           JOIN principals p ON p.workspace_id = m.workspace_id AND p.id = m.host_principal_id
          WHERE m.workspace_id = $1 AND m.id = $2",
    )
    .bind(ctx.workspace_id).bind(meeting_type_id).fetch_optional(tx.conn()).await
    .map_err(infrastructure::error::map_sqlx)?;
    let (rules, exceptions) = (
        repo::availability_rules(&mut tx, meeting.host_principal_id).await?,
        repo::availability_exceptions(&mut tx, meeting.host_principal_id).await?,
    );
    tx.rollback().await?;
    let link = link.ok_or_else(|| AppError::not_found("meeting type not found"))?;
    if link.calendar_status != "active"
        || link.integration_status != "active"
        || link.meeting_status != "active"
    {
        return Err(AppError::new(
            ErrorCode::CalendarAvailabilityUnavailable,
            "calendar availability is unavailable",
        ));
    }
    // Internal MCP/REST meeting-type lookups share the same provider-aware
    // filtering path; use an ephemeral zero-use link DTO carrying the saved
    // meeting settings, without minting or exposing a public capability.
    compute_meeting_slots(
        db,
        calendar,
        MeetingSlotInput {
            workspace_id: ctx.workspace_id,
            link,
            rules,
            exceptions,
            from,
            to,
            timezone: timezone.to_string(),
        },
    )
    .await
}

pub async fn list_booking_link_slots(
    db: &Db,
    calendar: &dyn CalendarProvider,
    ctx: &Ctx,
    booking_link_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    timezone: &str,
) -> Result<SlotsResponse, AppError> {
    require_scope(ctx, Scope::BookingRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(link) = repo::get_booking_link(&mut tx, booking_link_id, false).await? else {
        tx.rollback().await?;
        return Err(hidden_not_found());
    };
    if !can_manage_host(ctx, link.host_principal_id) {
        tx.rollback().await?;
        return Err(hidden_not_found());
    }
    let resolved = ResolvedLink {
        workspace_id: ctx.workspace_id,
        link_id: link.id,
        link,
    };
    tx.rollback().await?;
    compute_slots(db, calendar, resolved, from, to, timezone).await
}

struct MeetingSlotInput {
    workspace_id: Uuid,
    link: BookingLinkRow,
    rules: Vec<AvailabilityRuleRow>,
    exceptions: Vec<AvailabilityExceptionRow>,
    from: NaiveDate,
    to: NaiveDate,
    timezone: String,
}

async fn compute_meeting_slots(
    db: &Db,
    calendar: &dyn CalendarProvider,
    input: MeetingSlotInput,
) -> Result<SlotsResponse, AppError> {
    let MeetingSlotInput {
        workspace_id,
        link,
        rules,
        exceptions,
        from,
        to,
        timezone,
    } = input;
    if to < from || to.signed_duration_since(from).num_days() > 30 {
        return Err(field_error(
            "to",
            "date range must be ordered and at most 31 days",
        ));
    }
    let display_tz = parse_timezone(&timezone, "timezone")?;
    let range = (
        day_bounds(from, display_tz)?.0,
        day_bounds(to, display_tz)?.1,
    );
    let before = Duration::minutes(i64::from(link.buffer_before_minutes));
    let after = Duration::minutes(i64::from(link.buffer_after_minutes));
    let availability_range = (range.0 - before, range.1 + after);
    let holds = {
        let mut tx = db.begin(workspace_id).await?;
        let rows = repo::active_holds_between(
            &mut tx,
            link.host_principal_id,
            availability_range.0,
            availability_range.1,
        )
        .await?;
        tx.rollback().await?;
        rows
    };
    let busy = match calendar
        .free_busy_scoped(
            workspace_id,
            &link.provider_calendar_id,
            availability_range.0,
            availability_range.1,
            None,
            None,
        )
        .await
    {
        CalOutcome::Ok(v) => v,
        CalOutcome::Failed { .. } | CalOutcome::Unknown => {
            return Err(AppError::new(
                ErrorCode::CalendarAvailabilityUnavailable,
                "calendar availability could not be checked",
            ));
        }
    };
    let now = Utc::now();
    let earliest = now + Duration::minutes(i64::from(link.minimum_notice_minutes));
    let latest = now + Duration::days(i64::from(link.booking_horizon_days));
    let mut slots = Vec::new();
    for rule in &rules {
        let tz: Tz = rule.timezone.parse().map_err(|_| {
            AppError::new(
                ErrorCode::CalendarAvailabilityUnavailable,
                "availability timezone is invalid",
            )
        })?;
        let mut day = range
            .0
            .with_timezone(&tz)
            .date_naive()
            .pred_opt()
            .unwrap_or(from);
        let end = range
            .1
            .with_timezone(&tz)
            .date_naive()
            .succ_opt()
            .unwrap_or(to);
        while day <= end {
            slots.extend(rule_generates(
                rule,
                day,
                link.duration_minutes,
                range,
                earliest,
                latest,
            ));
            day = day.succ_opt().unwrap_or(end + Duration::days(1));
        }
    }
    for exception in exceptions.iter().filter(|e| e.kind == "available") {
        slots.extend(available_exception_slots(
            exception,
            link.duration_minutes,
            range,
            earliest,
            latest,
        ));
    }
    slots.sort_by_key(|x| x.0);
    slots.dedup_by_key(|x| x.0);
    slots.retain(|(s, e)| {
        let os = *s - before;
        let oe = *e + after;
        !exceptions
            .iter()
            .any(|x| x.kind == "blocked" && intersects(os, oe, x.starts_at, x.ends_at))
            && !holds
                .iter()
                .any(|x| intersects(os, oe, x.starts_at, x.ends_at))
            && !busy.iter().any(|x| intersects(os, oe, x.start, x.end))
    });
    Ok(SlotsResponse {
        timezone,
        availability_checked_at: now,
        slots: slots
            .into_iter()
            .map(|(starts_at, ends_at)| SlotResponse { starts_at, ends_at })
            .collect(),
        reservation_guaranteed: false,
    })
}

pub async fn create_public_booking_in_tx(
    tx: &mut Tx<'_>,
    link_id: Uuid,
    request: &CreatePublicBookingRequest,
    manage_token: &str,
    request_id: &str,
) -> Result<BookingCreateStored, AppError> {
    validate_guest(request)?;
    let location_id = link_id;
    let Some(link) = repo::get_booking_link(tx, location_id, true).await? else {
        return Err(AppError::not_found("booking link not found"));
    };
    ensure_link_usable(&link)?;
    if repo::booking_link_use_count(tx, link.id).await? >= i64::from(link.max_uses) {
        return Err(AppError::new(
            ErrorCode::BookingLinkExpired,
            "booking link expired",
        ));
    }
    let notice_version = repo::published_privacy_notice_version(tx)
        .await?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::dependency_unavailable("published booking privacy notice is not configured")
        })?;
    if request.privacy_notice_version != notice_version {
        return Err(field_error(
            "privacy_notice_version",
            "privacy notice version is not current",
        ));
    }
    let normalized = domain::email_address::normalize(
        &request.guest_email,
        domain::email_address::LocalPartPolicy::Lowercase,
    )
    .map_err(|_| field_error("guest_email", "guest_email must be a valid email address"))?;
    let invitation_bound = if let Some(bound_id) = link.bound_email_address_id {
        let bound: Option<String> = sqlx::query_scalar(
            "SELECT normalized_key FROM email_addresses WHERE workspace_id = $1 AND id = $2",
        )
        .bind(tx.workspace_id())
        .bind(bound_id)
        .fetch_optional(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        if bound.as_deref() != Some(normalized.normalized_key.as_str()) {
            return Err(AppError::new(
                ErrorCode::BookingRecipientMismatch,
                "booking recipient does not match this invitation",
            ));
        }
        true
    } else {
        false
    };
    let end = request.starts_at + Duration::minutes(i64::from(link.duration_minutes));
    let occupied_start =
        request.starts_at - Duration::minutes(i64::from(link.buffer_before_minutes));
    let occupied_end = end + Duration::minutes(i64::from(link.buffer_after_minutes));
    let email_id = repo::find_or_create_email_address(
        tx,
        normalized.original.as_str(),
        &normalized.normalized_key,
    )
    .await?;
    let booking_id = repo::insert_booking_and_hold(
        tx,
        &link,
        email_id,
        if invitation_bound {
            "invitation_bound"
        } else {
            "unverified"
        },
        request,
        request.starts_at,
        end,
        occupied_start,
        occupied_end,
    )
    .await?;
    let token_hash = infrastructure::crypto::sha256_bytes(manage_token.as_bytes());
    let expires_at = request.starts_at + Duration::days(90);
    repo::issue_manage_token(tx, booking_id, &token_hash, expires_at).await?;
    let _ = infrastructure::jobs::enqueue(
        tx,
        "booking.confirm",
        1,
        serde_json::json!({"booking_id": booking_id}),
        Utc::now(),
        Some(&format!("booking_confirm:{booking_id}")),
    )
    .await?;
    let expire_at = Utc::now() + HELD_TTL;
    let tick_key = format!("booking_expire_holds:{}", expire_at.format("%Y%m%d%H%M"));
    let _ = infrastructure::jobs::enqueue(
        tx,
        "booking.expire_holds",
        1,
        serde_json::json!({}),
        expire_at,
        Some(&tick_key),
    )
    .await?;
    outbox::emit(
        tx,
        "booking.requested",
        "booking",
        booking_id,
        1,
        serde_json::json!({"booking_id": booking_id, "email_address_id": email_id}),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: None,
            requested_by: None,
            approved_by: None,
            job_id: None,
            action: "booking.requested".into(),
            target_type: "booking".into(),
            target_id: Some(booking_id),
            outcome: "ok".into(),
            metadata: serde_json::json!({"booking_link_id": link.id, "email_address_id": email_id}),
            request_id: request_id.to_string(),
            trace_id: None,
        },
    )
    .await?;
    Ok(BookingCreateStored {
        id: booking_id,
        starts_at: request.starts_at,
        ends_at: end,
        timezone: request.timezone.clone(),
    })
}

fn validate_guest(request: &CreatePublicBookingRequest) -> Result<(), AppError> {
    if request.guest_name.trim().is_empty() || request.guest_name.trim().len() > 300 {
        return Err(field_error(
            "guest_name",
            "guest_name must be 1..=300 characters",
        ));
    }
    if request.guest_email.trim().is_empty() || request.guest_email.len() > 320 {
        return Err(field_error(
            "guest_email",
            "guest_email must be 1..=320 characters",
        ));
    }
    if request.privacy_notice_version.trim().is_empty()
        || request.privacy_notice_version.len() > 100
    {
        return Err(field_error(
            "privacy_notice_version",
            "privacy_notice_version is required",
        ));
    }
    if request
        .memo
        .as_ref()
        .is_some_and(|memo| memo.chars().count() > 2000)
    {
        return Err(field_error("memo", "memo must be at most 2,000 characters"));
    }
    parse_timezone(&request.timezone, "timezone")?;
    Ok(())
}

/// Candidate times for rescheduling: the same availability the original
/// invitation offered, computed for the booking's own link (even when the
/// link itself is used up).
pub async fn manage_slots(
    db: &Db,
    calendar: &dyn CalendarProvider,
    raw_token: &str,
    from: NaiveDate,
    to: NaiveDate,
    timezone: &str,
) -> Result<SlotsResponse, AppError> {
    let location = public_tokens::booking_manage_token(db, raw_token).await?;
    let mut tx = db.begin(location.workspace_id).await?;
    let token = repo::get_manage_token(&mut tx, location.token_id)
        .await?
        .filter(|token| token.revoked_at.is_none() && token.expires_at > Utc::now());
    let booking = match token {
        Some(token) => repo::get_booking(&mut tx, token.booking_id).await?,
        None => None,
    };
    let link = match &booking {
        Some(booking) => repo::get_booking_link(&mut tx, booking.booking_link_id, false).await?,
        None => None,
    };
    tx.rollback().await?;
    let link = link.ok_or_else(hidden_not_found)?;
    let resolved = ResolvedLink {
        workspace_id: location.workspace_id,
        link_id: link.id,
        link,
    };
    compute_slots(db, calendar, resolved, from, to, timezone).await
}

pub async fn manage_booking(db: &Db, raw_token: &str) -> Result<ManageBookingResponse, AppError> {
    let location = public_tokens::booking_manage_token(db, raw_token).await?;
    let mut tx = db.begin(location.workspace_id).await?;
    let Some(token) = repo::get_manage_token(&mut tx, location.token_id).await? else {
        tx.rollback().await?;
        return Err(hidden_not_found());
    };
    if token.revoked_at.is_some() || token.expires_at <= Utc::now() {
        tx.rollback().await?;
        return Err(hidden_not_found());
    }
    let Some(booking) = repo::get_booking(&mut tx, token.booking_id).await? else {
        tx.rollback().await?;
        return Err(hidden_not_found());
    };
    #[allow(clippy::type_complexity)]
    let public_details: Option<(
        String,
        String,
        Option<String>,
        i32,
        i32,
        String,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT m.name, p.name, m.description, m.duration_minutes, m.booking_horizon_days,
                m.location_kind, m.location_detail
           FROM bookings b
           JOIN booking_links l
             ON l.workspace_id = b.workspace_id AND l.id = b.booking_link_id
           JOIN meeting_types m
             ON m.workspace_id = l.workspace_id AND m.id = l.meeting_type_id
            AND m.host_principal_id = b.host_principal_id
           JOIN principals p
             ON p.workspace_id = b.workspace_id AND p.id = b.host_principal_id
          WHERE b.workspace_id = $1 AND b.id = $2 AND p.kind = 'human'",
    )
    .bind(location.workspace_id)
    .bind(booking.id)
    .fetch_optional(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    let Some((
        meeting_name,
        host_display_name,
        meeting_description,
        duration_minutes,
        booking_horizon_days,
        location_kind,
        location_detail,
    )) = public_details
    else {
        tx.rollback().await?;
        return Err(hidden_not_found());
    };
    tx.rollback().await?;
    Ok(ManageBookingResponse {
        id: booking.id,
        guest_name: booking.guest_name,
        meeting_name,
        meeting_description,
        duration_minutes,
        booking_horizon_days,
        location_kind,
        location_detail,
        host_display_name,
        guest_memo: booking.guest_memo,
        starts_at: booking.starts_at,
        ends_at: booking.ends_at,
        guest_timezone: booking.guest_timezone.clone(),
        status: booking.status.clone(),
        version: booking.version,
        message: booking_message(&booking.status),
    })
}

fn booking_message(status: &str) -> String {
    match status {
        "confirmed" => "예약이 확정됐어요. 미팅 전에 자세한 안내를 연락드릴게요.".into(),
        "confirming" | "held" => "예약 요청을 받았어요. 일정이 확정되면 연락드릴게요.".into(),
        "unknown" | "canceling" => {
            "캘린더 처리 결과를 확인하고 있어요. 같은 요청을 다시 보내지 않아도 돼요.".into()
        }
        "canceled" => "예약이 취소됐어요.".into(),
        "failed" | "expired" => "예약을 완료하지 못했어요. 다른 시간을 골라 주세요.".into(),
        _ => "예약 상태를 확인해 주세요.".into(),
    }
}

pub async fn list_bookings(
    db: &Db,
    ctx: &Ctx,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<Vec<BookingResponse>, AppError> {
    require_scope(ctx, Scope::BookingRead)?;
    if from.zip(to).is_some_and(|(a, b)| b <= a) {
        return Err(field_error("to", "to must be after from"));
    }
    let host_ids =
        if (matches!(ctx.role, Role::Admin | Role::Manager) || crate::authz::agent_admin(ctx)) {
            vec![]
        } else {
            vec![ctx.principal_id]
        };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_bookings(&mut tx, &host_ids, from, to).await?;
    tx.rollback().await?;
    Ok(rows.into_iter().map(booking_response).collect())
}

pub async fn get_booking(db: &Db, ctx: &Ctx, id: Uuid) -> Result<BookingResponse, AppError> {
    require_scope(ctx, Scope::BookingRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(row) = repo::get_booking(&mut tx, id).await? else {
        tx.rollback().await?;
        return Err(hidden_not_found());
    };
    if !can_manage_host(ctx, row.host_principal_id) {
        tx.rollback().await?;
        return Err(hidden_not_found());
    }
    tx.rollback().await?;
    Ok(booking_response(row))
}

pub async fn record_attendance(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    request: BookingAttendanceRequest,
) -> Result<BookingResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let result = record_attendance_in_tx(&mut tx, ctx, id, expected_version, request).await?;
    tx.commit().await?;
    Ok(result)
}

pub async fn record_attendance_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    request: BookingAttendanceRequest,
) -> Result<BookingResponse, AppError> {
    require_operator(ctx)?;
    require_scope(ctx, Scope::BookingPropose)?;
    if !matches!(request.attendance_status.as_str(), "attended" | "no_show") {
        return Err(field_error(
            "attendance_status",
            "attendance_status must be attended or no_show",
        ));
    }
    if request
        .outcome_note
        .as_ref()
        .is_some_and(|note| note.chars().count() > 2000)
    {
        return Err(field_error(
            "outcome_note",
            "outcome_note must be at most 2,000 characters",
        ));
    }
    let Some(current) = repo::get_booking(tx, id).await? else {
        return Err(hidden_not_found());
    };
    if !can_manage_host(ctx, current.host_principal_id) {
        return Err(hidden_not_found());
    }
    if current.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "booking version changed",
        ));
    }
    if current.status != "confirmed" || current.ends_at > Utc::now() {
        return Err(AppError::conflict(
            "attendance can only be recorded for a completed confirmed booking",
        ));
    }
    match repo::set_attendance(
        tx,
        id,
        expected_version,
        &request.attendance_status,
        request.outcome_note.as_deref().map(str::trim),
    )
    .await?
    {
        Some(row) => {
            audit::record(
                tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "booking.attendance_recorded",
                    "booking",
                    ctx.request_id.clone(),
                )
                .with_target(id),
            )
            .await?;
            Ok(booking_response(row))
        }
        None => Err(AppError::new(
            ErrorCode::VersionMismatch,
            "booking version changed",
        )),
    }
}

pub async fn validate_public_slot(
    db: &Db,
    calendar: &dyn CalendarProvider,
    raw_token: &str,
    request: &CreatePublicBookingRequest,
) -> Result<(), AppError> {
    validate_guest(request)?;
    let resolved = resolve_public_link(db, raw_token).await?;
    let tz = parse_timezone(&request.timezone, "timezone")?;
    let date = request.starts_at.with_timezone(&tz).date_naive();
    let to = date
        .succ_opt()
        .ok_or_else(|| field_error("starts_at", "time is out of range"))?;
    let slots = compute_slots(db, calendar, resolved.clone(), date, to, &request.timezone).await?;
    if slots.slots.iter().any(|s| s.starts_at == request.starts_at) {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::SlotUnavailable,
            "selected slot is no longer available",
        ))
    }
}

fn check_booking_version(row: &BookingRow, expected: i64) -> Result<(), AppError> {
    if row.version != expected {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "booking version changed",
        ));
    }
    Ok(())
}

pub async fn propose_public_reschedule_in_tx(
    db: &Db,
    tx: &mut Tx<'_>,
    calendar: &dyn CalendarProvider,
    manage_token_id: Uuid,
    request: PublicRescheduleRequest,
    request_id: &str,
) -> Result<BookingChangeResponse, AppError> {
    let token = repo::get_manage_token(tx, manage_token_id)
        .await?
        .ok_or_else(hidden_not_found)?;
    if token.revoked_at.is_some() || token.expires_at <= Utc::now() {
        return Err(hidden_not_found());
    }
    let booking = repo::get_booking(tx, token.booking_id)
        .await?
        .ok_or_else(hidden_not_found)?;
    check_booking_version(&booking, request.expected_version)?;
    let link = repo::get_booking_link(tx, booking.booking_link_id, false)
        .await?
        .ok_or_else(hidden_not_found)?;
    let meeting = repo::get_meeting_type(tx, link.meeting_type_id)
        .await?
        .ok_or_else(hidden_not_found)?;
    validate_reschedule_slot(
        db,
        calendar,
        tx.workspace_id()
            .ok_or_else(|| AppError::internal("workspace tx required"))?,
        booking.id,
        meeting,
        request.starts_at,
        &request.timezone,
    )
    .await?;
    start_reschedule_change_in_tx(
        tx,
        None,
        booking.id,
        request.expected_version,
        request.starts_at,
        &request.timezone,
        request_id,
    )
    .await
}

async fn validate_reschedule_slot(
    db: &Db,
    calendar: &dyn CalendarProvider,
    workspace_id: Uuid,
    booking_id: Uuid,
    meeting: repo::MeetingTypeRow,
    starts_at: DateTime<Utc>,
    timezone: &str,
) -> Result<(), AppError> {
    let tz = parse_timezone(timezone, "timezone")?;
    let date = starts_at.with_timezone(&tz).date_naive();
    let now = Utc::now();
    if starts_at < now + Duration::minutes(i64::from(meeting.minimum_notice_minutes))
        || starts_at > now + Duration::days(i64::from(meeting.booking_horizon_days))
    {
        return Err(AppError::new(
            ErrorCode::SlotUnavailable,
            "selected slot is outside the booking window",
        ));
    }
    let before = Duration::minutes(i64::from(meeting.buffer_before_minutes));
    let after = Duration::minutes(i64::from(meeting.buffer_after_minutes));
    let (range, rules, exceptions, holds) = {
        let range = (
            day_bounds(date, tz)?.0,
            day_bounds(
                date.succ_opt()
                    .ok_or_else(|| field_error("starts_at", "out of range"))?,
                tz,
            )?
            .1,
        );
        let hold_range = (range.0 - before, range.1 + after);
        let mut tx = db.begin(workspace_id).await?;
        let rules = repo::availability_rules(&mut tx, meeting.host_principal_id).await?;
        let exceptions = repo::availability_exceptions(&mut tx, meeting.host_principal_id).await?;
        let holds = repo::active_holds_between_except_booking(
            &mut tx,
            meeting.host_principal_id,
            hold_range.0,
            hold_range.1,
            booking_id,
        )
        .await?;
        tx.rollback().await?;
        (range, rules, exceptions, holds)
    };
    let cal_id = {
        let mut tx = db.begin(workspace_id).await?;
        let id: String=sqlx::query_scalar("SELECT c.provider_calendar_id FROM calendar_connections c WHERE c.workspace_id=$1 AND c.id=$2 AND c.status='active'")
            .bind(workspace_id).bind(meeting.calendar_connection_id).fetch_optional(tx.conn()).await.map_err(infrastructure::error::map_sqlx)?
            .ok_or_else(|| AppError::new(ErrorCode::CalendarAvailabilityUnavailable,"calendar is unavailable"))?;
        tx.rollback().await?;
        id
    };
    let busy = match calendar
        .free_busy_scoped(
            workspace_id,
            &cal_id,
            range.0 - before,
            range.1 + after,
            Some(booking_id),
            None,
        )
        .await
    {
        CalOutcome::Ok(v) => v,
        _ => {
            return Err(AppError::new(
                ErrorCode::CalendarAvailabilityUnavailable,
                "calendar availability could not be checked",
            ));
        }
    };
    let ends = starts_at + Duration::minutes(i64::from(meeting.duration_minutes));
    let occupied_start = starts_at - Duration::minutes(i64::from(meeting.buffer_before_minutes));
    let occupied_end = ends + Duration::minutes(i64::from(meeting.buffer_after_minutes));
    let scheduled = rules.iter().any(|r| {
        let Ok(tz) = r.timezone.parse::<Tz>() else {
            return false;
        };
        let local = starts_at.with_timezone(&tz).naive_local();
        r.weekday == i16::try_from(local.date().weekday().number_from_monday()).unwrap_or_default()
            && r.valid_from.is_none_or(|d| local.date() >= d)
            && r.valid_until.is_none_or(|d| local.date() <= d)
            && local.time() >= r.start_local
            && local.time() + Duration::minutes(i64::from(meeting.duration_minutes)) <= r.end_local
    }) || exceptions
        .iter()
        .any(|e| e.kind == "available" && starts_at >= e.starts_at && ends <= e.ends_at);
    if !scheduled
        || exceptions.iter().any(|e| {
            e.kind == "blocked" && intersects(occupied_start, occupied_end, e.starts_at, e.ends_at)
        })
        || holds
            .iter()
            .any(|h| intersects(occupied_start, occupied_end, h.starts_at, h.ends_at))
        || busy
            .iter()
            .any(|b| intersects(occupied_start, occupied_end, b.start, b.end))
    {
        return Err(AppError::new(
            ErrorCode::SlotUnavailable,
            "selected slot is no longer available",
        ));
    }
    Ok(())
}

async fn start_reschedule_change_in_tx(
    tx: &mut Tx<'_>,
    actor: Option<Uuid>,
    booking_id: Uuid,
    expected_version: i64,
    starts_at: DateTime<Utc>,
    timezone: &str,
    request_id: &str,
) -> Result<BookingChangeResponse, AppError> {
    let b = repo::booking_saga(tx, booking_id)
        .await?
        .ok_or_else(hidden_not_found)?;
    let duration = (b.ends_at - b.starts_at).num_minutes();
    let new_end = starts_at + Duration::minutes(duration);
    let old_buffer_before = Duration::minutes(i64::from(b.buffer_before_minutes));
    let old_buffer_after = Duration::minutes(i64::from(b.buffer_after_minutes));
    let new_occ_start = starts_at - old_buffer_before;
    let new_occ_end = new_end + old_buffer_after;
    let change_id = repo::begin_reschedule_change(
        tx,
        booking_id,
        expected_version,
        starts_at,
        new_end,
        new_occ_start,
        new_occ_end,
        timezone,
        actor,
    )
    .await?;
    let _ = infrastructure::jobs::enqueue(
        tx,
        "booking.apply_change",
        1,
        serde_json::json!({"booking_change_id":change_id}),
        Utc::now(),
        Some(&format!("booking_apply_change:{change_id}")),
    )
    .await?;
    if let Some(actor) = actor {
        audit::record(
            tx,
            AuditEntry::new(actor, "booking.reschedule_requested", "booking", request_id)
                .with_target(booking_id),
        )
        .await?;
    }
    Ok(BookingChangeResponse {
        id: change_id,
        booking_id: Some(booking_id),
        kind: "reschedule".into(),
        status: "processing".into(),
        approval_request_id: None,
    })
}

pub async fn propose_public_cancel_in_tx(
    tx: &mut Tx<'_>,
    manage_token_id: Uuid,
    request: PublicCancelRequest,
    request_id: &str,
) -> Result<BookingChangeResponse, AppError> {
    let token = repo::get_manage_token(tx, manage_token_id)
        .await?
        .ok_or_else(hidden_not_found)?;
    if token.revoked_at.is_some() || token.expires_at <= Utc::now() {
        return Err(hidden_not_found());
    }
    let booking = repo::get_booking(tx, token.booking_id)
        .await?
        .ok_or_else(hidden_not_found)?;
    check_booking_version(&booking, request.expected_version)?;
    let change_id =
        repo::begin_cancel_change(tx, booking.id, request.expected_version, None).await?;
    let _ = infrastructure::jobs::enqueue(
        tx,
        "booking.apply_change",
        1,
        serde_json::json!({"booking_change_id": change_id}),
        Utc::now(),
        Some(&format!("booking_apply_change:{change_id}")),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: None,
            requested_by: None,
            approved_by: None,
            job_id: None,
            action: "booking.cancel_requested".into(),
            target_type: "booking".into(),
            target_id: Some(booking.id),
            outcome: "ok".into(),
            metadata: serde_json::json!({"change_id": change_id}),
            request_id: request_id.into(),
            trace_id: None,
        },
    )
    .await?;
    Ok(BookingChangeResponse {
        id: change_id,
        booking_id: Some(booking.id),
        kind: "cancel".into(),
        status: "processing".into(),
        approval_request_id: None,
    })
}

pub async fn propose_change(
    db: &Db,
    ctx: &Ctx,
    request: BookingChangeProposalRequest,
) -> Result<BookingChangeResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = propose_change_in_tx(&mut tx, ctx, request).await?;
    tx.commit().await?;
    Ok(response)
}

pub async fn propose_change_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    request: BookingChangeProposalRequest,
) -> Result<BookingChangeResponse, AppError> {
    require_scope(ctx, Scope::BookingPropose)?;
    if !matches!(request.kind.as_str(), "reschedule" | "cancel" | "create") {
        return Err(field_error(
            "kind",
            "kind must be create, reschedule or cancel",
        ));
    }
    if request.kind != "create" && request.booking_id.is_none() {
        return Err(field_error(
            "booking_id",
            "booking_id is required for reschedule/cancel",
        ));
    }
    if request.kind == "reschedule" && (request.starts_at.is_none() || request.timezone.is_none()) {
        return Err(field_error(
            "starts_at",
            "reschedule requires starts_at and timezone",
        ));
    }
    if request
        .timezone
        .as_deref()
        .is_some_and(|z| z.parse::<Tz>().is_err())
    {
        return Err(field_error("timezone", "must be a valid IANA timezone"));
    }
    let host = if let Some(id) = request.booking_id {
        repo::get_booking(tx, id)
            .await?
            .ok_or_else(hidden_not_found)?
            .host_principal_id
    } else {
        ctx.principal_id
    };
    if !can_manage_host(ctx, host) {
        return Err(hidden_not_found());
    }
    let data = serde_json::json!({"starts_at":request.starts_at,"timezone":request.timezone,"reason":request.reason});
    let id = repo::insert_change(
        tx,
        request.booking_id,
        host,
        &request.kind,
        &data,
        Some(ctx.principal_id),
    )
    .await?;
    let snapshot = infrastructure::crypto::sha256_bytes(data.to_string().as_bytes());
    let approval_id = approvals::insert_request(
        tx,
        ctx.principal_id,
        ApprovalTarget::BookingChange(id),
        &snapshot,
        Utc::now() + Duration::days(RELEASE_REQUEST_TTL_DAYS),
    )
    .await?;
    let mut audit_entry = AuditEntry::new(
        ctx.principal_id,
        "booking.change_proposed",
        "booking_change",
        ctx.request_id.clone(),
    )
    .with_target(id);
    audit_entry.metadata =
        serde_json::json!({"approval_request_id":approval_id,"reason":request.reason});
    audit::record(tx, audit_entry).await?;
    Ok(BookingChangeResponse {
        id,
        booking_id: request.booking_id,
        kind: request.kind,
        status: "proposed".into(),
        approval_request_id: Some(approval_id),
    })
}

pub async fn create_mcp_booking_proposal_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    request: &McpBookingProposalRequest,
) -> Result<BookingChangeResponse, AppError> {
    require_scope(ctx, Scope::BookingPropose)?;
    if request.idempotency_key.is_empty() || request.idempotency_key.len() > 128 {
        return Err(field_error(
            "idempotency_key",
            "idempotency_key must be 1..=128 characters",
        ));
    }
    if request.guest_name.trim().is_empty() || request.guest_name.trim().len() > 300 {
        return Err(field_error(
            "guest_name",
            "guest_name must be 1..=300 characters",
        ));
    }
    if domain::email_address::normalize(
        &request.guest_email,
        domain::email_address::LocalPartPolicy::Lowercase,
    )
    .is_err()
    {
        return Err(field_error(
            "guest_email",
            "guest_email must be a valid email address",
        ));
    }
    if request.purpose.trim().is_empty() || request.purpose.trim().len() > 100 {
        return Err(field_error("purpose", "purpose must be 1..=100 characters"));
    }
    parse_timezone(&request.timezone, "timezone")?;
    let mt = repo::get_meeting_type(tx, request.meeting_type_id)
        .await?
        .ok_or_else(|| AppError::not_found("meeting type not found"))?;
    if !can_manage_host(ctx, mt.host_principal_id) {
        return Err(hidden_not_found());
    }
    let proposed_data = serde_json::json!({
        "meeting_type_id": request.meeting_type_id,
        "guest_name": request.guest_name.trim(),
        "guest_email": request.guest_email.trim(),
        "starts_at": request.desired_starts_at,
        "timezone": request.timezone,
        "purpose": request.purpose.trim(),
        "reason": request.reason,
    });
    let change_id = repo::insert_change(
        tx,
        None,
        mt.host_principal_id,
        "create",
        &proposed_data,
        Some(ctx.principal_id),
    )
    .await?;
    let snapshot = infrastructure::crypto::sha256_bytes(proposed_data.to_string().as_bytes());
    let approval_request_id = approvals::insert_request(
        tx,
        ctx.principal_id,
        ApprovalTarget::BookingChange(change_id),
        &snapshot,
        Utc::now() + Duration::days(RELEASE_REQUEST_TTL_DAYS),
    )
    .await?;
    audit::record(tx, AuditEntry {
        actor_principal_id: Some(ctx.principal_id), requested_by: Some(ctx.principal_id),
        approved_by: None, job_id: None, action: "booking.change_proposed".into(),
        target_type: "booking_change".into(), target_id: Some(change_id), outcome: "ok".into(),
        metadata: serde_json::json!({"approval_request_id": approval_request_id, "kind": "create", "reason": request.reason}),
        request_id: ctx.request_id.clone(), trace_id: None,
    }).await?;
    Ok(BookingChangeResponse {
        id: change_id,
        booking_id: None,
        kind: "create".into(),
        status: "proposed".into(),
        approval_request_id: Some(approval_request_id),
    })
}

pub async fn reschedule_booking_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    request: PublicRescheduleRequest,
) -> Result<BookingChangeResponse, AppError> {
    require_operator(ctx)?;
    require_scope(ctx, Scope::BookingPropose)?;
    let booking = repo::get_booking(tx, id)
        .await?
        .ok_or_else(hidden_not_found)?;
    if !can_manage_host(ctx, booking.host_principal_id) {
        return Err(hidden_not_found());
    }
    start_reschedule_change_in_tx(
        tx,
        Some(ctx.principal_id),
        id,
        expected_version,
        request.starts_at,
        &request.timezone,
        &ctx.request_id,
    )
    .await
}

pub async fn reschedule_booking(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    request: PublicRescheduleRequest,
) -> Result<BookingChangeResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = reschedule_booking_in_tx(&mut tx, ctx, id, expected_version, request).await?;
    tx.commit().await?;
    Ok(response)
}

pub async fn cancel_booking_in_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
) -> Result<BookingChangeResponse, AppError> {
    require_operator(ctx)?;
    require_scope(ctx, Scope::BookingPropose)?;
    let booking = repo::get_booking(tx, id)
        .await?
        .ok_or_else(hidden_not_found)?;
    if !can_manage_host(ctx, booking.host_principal_id) {
        return Err(hidden_not_found());
    }
    let change_id =
        repo::begin_cancel_change(tx, id, expected_version, Some(ctx.principal_id)).await?;
    let _ = infrastructure::jobs::enqueue(
        tx,
        "booking.apply_change",
        1,
        serde_json::json!({"booking_change_id":change_id}),
        Utc::now(),
        Some(&format!("booking_apply_change:{change_id}")),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "booking.cancel_requested",
            "booking",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    Ok(BookingChangeResponse {
        id: change_id,
        booking_id: Some(id),
        kind: "cancel".into(),
        status: "processing".into(),
        approval_request_id: None,
    })
}

pub async fn cancel_booking(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
) -> Result<BookingChangeResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let response = cancel_booking_in_tx(&mut tx, ctx, id, expected_version).await?;
    tx.commit().await?;
    Ok(response)
}

pub async fn confirm_booking(
    db: &Db,
    calendar: &dyn CalendarProvider,
    workspace_id: Uuid,
    booking_id: Uuid,
    job_id: Uuid,
) -> Result<(), AppError> {
    let saga = {
        let mut tx = db.begin(workspace_id).await?;
        let v = repo::booking_saga(&mut tx, booking_id).await?;
        tx.commit().await?;
        v
    };
    let Some(saga) = saga.filter(|b| b.status == "confirming") else {
        return Ok(());
    };
    match calendar
        .free_busy_scoped(
            workspace_id,
            &saga.provider_calendar_id,
            saga.occupied_start,
            saga.occupied_end,
            Some(booking_id),
            None,
        )
        .await
    {
        CalOutcome::Ok(blocks)
            if blocks
                .iter()
                .any(|b| intersects(saga.occupied_start, saga.occupied_end, b.start, b.end)) =>
        {
            let mut tx = db.begin(workspace_id).await?;
            repo::mark_booking_failed(&mut tx, booking_id, "SLOT_UNAVAILABLE").await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.confirm_failed",
                serde_json::json!({"code":"SLOT_UNAVAILABLE"}),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
        CalOutcome::Ok(_) => {}
        CalOutcome::Failed { code, .. } => {
            let mut tx = db.begin(workspace_id).await?;
            repo::mark_booking_failed(&mut tx, booking_id, &code).await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.confirm_failed",
                serde_json::json!({"code":code}),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
        CalOutcome::Unknown => {
            let mut tx = db.begin(workspace_id).await?;
            repo::mark_booking_unknown(&mut tx, booking_id, "CALENDAR_AVAILABILITY_UNKNOWN")
                .await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.confirm_unknown",
                serde_json::json!({}),
            )
            .await?;
            tx.commit().await?;
            return Ok(());
        }
    }
    let outcome = calendar
        .create_booking_event_scoped(
            workspace_id,
            &saga.provider_calendar_id,
            booking_id,
            &saga.meeting_name,
            saga.starts_at,
            saga.ends_at,
            &saga.guest_email,
        )
        .await;
    let mut tx = db.begin(workspace_id).await?;
    match outcome {
        CalOutcome::Ok(event) => {
            if repo::mark_booking_confirmed(
                &mut tx,
                booking_id,
                &event.provider_event_id,
                &event.revision,
            )
            .await?
            {
                outbox::emit(
                    &mut tx,
                    outbox::event_types::BOOKING_CONFIRMED,
                    "booking",
                    booking_id,
                    saga.version + 1,
                    serde_json::json!({"booking_id":booking_id}),
                )
                .await?;
                record_worker_booking(
                    &mut tx,
                    booking_id,
                    job_id,
                    "booking.confirmed",
                    serde_json::json!({}),
                )
                .await?;
            }
        }
        CalOutcome::Failed { code, .. } => {
            repo::mark_booking_failed(&mut tx, booking_id, &code).await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.confirm_failed",
                serde_json::json!({"code":code}),
            )
            .await?;
        }
        CalOutcome::Unknown => {
            repo::mark_booking_unknown(&mut tx, booking_id, "PROVIDER_SUBMISSION_OUTCOME_UNKNOWN")
                .await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.confirm_unknown",
                serde_json::json!({}),
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

async fn record_worker_booking(
    tx: &mut Tx<'_>,
    id: Uuid,
    job_id: Uuid,
    action: &str,
    metadata: serde_json::Value,
) -> Result<(), AppError> {
    let actor:Option<Uuid>=sqlx::query_scalar("SELECT id FROM principals WHERE workspace_id=$1 AND kind='system' AND status='active' ORDER BY created_at LIMIT 1")
        .bind(tx.workspace_id()).fetch_optional(tx.conn()).await.map_err(infrastructure::error::map_sqlx)?;
    audit::record(
        tx,
        AuditEntry {
            actor_principal_id: actor,
            requested_by: None,
            approved_by: None,
            job_id: Some(job_id),
            action: action.into(),
            target_type: "booking".into(),
            target_id: Some(id),
            outcome: "ok".into(),
            metadata,
            request_id: format!("job:{job_id}"),
            trace_id: None,
        },
    )
    .await
}

pub async fn apply_booking_change(
    db: &Db,
    calendar: &dyn CalendarProvider,
    workspace_id: Uuid,
    change_id: Uuid,
    job_id: Uuid,
) -> Result<(), AppError> {
    let change = {
        let mut tx = db.begin(workspace_id).await?;
        let c = repo::booking_change_saga(&mut tx, change_id).await?;
        tx.commit().await?;
        c
    };
    let Some(change) = change.filter(|c| c.status == "processing") else {
        return Ok(());
    };
    let Some(booking_id) = change.booking_id else {
        return Ok(());
    };
    let (Some(provider_calendar_id), Some(event_id)) = (
        change.provider_calendar_id.as_deref(),
        change.provider_event_id.as_deref(),
    ) else {
        return Ok(());
    };
    if change.kind == "cancel" {
        let result = calendar
            .cancel_event_scoped(
                workspace_id,
                provider_calendar_id,
                booking_id,
                change_id,
                event_id,
            )
            .await;
        let mut tx = db.begin(workspace_id).await?;
        match result {
            CalOutcome::Ok(()) => {
                repo::complete_cancel(&mut tx, change_id, booking_id).await?;
                record_worker_booking(
                    &mut tx,
                    booking_id,
                    job_id,
                    "booking.canceled",
                    serde_json::json!({"change_id":change_id}),
                )
                .await?;
            }
            CalOutcome::Failed { code, .. } => {
                repo::fail_change(&mut tx, &change, booking_id, &code).await?;
                record_worker_booking(
                    &mut tx,
                    booking_id,
                    job_id,
                    "booking.cancel_failed",
                    serde_json::json!({"code":code}),
                )
                .await?;
            }
            CalOutcome::Unknown => {
                repo::mark_change_unknown(&mut tx, &change, booking_id).await?;
                record_worker_booking(
                    &mut tx,
                    booking_id,
                    job_id,
                    "booking.cancel_unknown",
                    serde_json::json!({}),
                )
                .await?;
            }
        }
        tx.commit().await?;
        return Ok(());
    }
    let starts_at = change
        .proposed_data
        .get("starts_at")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|s| s.with_timezone(&Utc))
        .ok_or_else(|| AppError::validation("reschedule starts_at missing", vec![]))?;
    let ends_at = change
        .proposed_data
        .get("ends_at")
        .and_then(serde_json::Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|s| s.with_timezone(&Utc))
        .ok_or_else(|| AppError::validation("reschedule ends_at missing", vec![]))?;
    let occupied_start = change
        .new_occupied_start
        .ok_or_else(|| AppError::validation("reschedule hold missing", vec![]))?;
    let occupied_end = change
        .new_occupied_end
        .ok_or_else(|| AppError::validation("reschedule hold missing", vec![]))?;
    let free = calendar
        .free_busy_scoped(
            workspace_id,
            provider_calendar_id,
            occupied_start,
            occupied_end,
            Some(booking_id),
            Some(change_id),
        )
        .await;
    let failure = match free {
        CalOutcome::Ok(blocks)
            if blocks
                .iter()
                .any(|b| intersects(occupied_start, occupied_end, b.start, b.end)) =>
        {
            Some("SLOT_UNAVAILABLE")
        }
        CalOutcome::Ok(_) => None,
        CalOutcome::Failed { .. } | CalOutcome::Unknown => {
            Some("CALENDAR_AVAILABILITY_UNAVAILABLE")
        }
    };
    if let Some(code) = failure {
        let mut tx = db.begin(workspace_id).await?;
        repo::fail_change(&mut tx, &change, booking_id, code).await?;
        record_worker_booking(
            &mut tx,
            booking_id,
            job_id,
            "booking.reschedule_failed",
            serde_json::json!({"code":code}),
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    }
    let timezone = change
        .proposed_data
        .get("timezone")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("UTC");
    let event = calendar
        .update_event_scoped(
            workspace_id,
            provider_calendar_id,
            booking_id,
            change_id,
            event_id,
            change.meeting_name.as_deref().unwrap_or("Meeting"),
            starts_at,
            ends_at,
            occupied_start,
            occupied_end,
        )
        .await;
    let mut tx = db.begin(workspace_id).await?;
    match event {
        CalOutcome::Ok(event) => {
            repo::complete_reschedule(
                &mut tx,
                &change,
                repo::RescheduleCompletion {
                    booking_id,
                    starts_at,
                    ends_at,
                    occupied_start,
                    occupied_end,
                    event_id: &event.provider_event_id,
                    revision: &event.revision,
                },
            )
            .await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.rescheduled",
                serde_json::json!({"change_id":change_id,"timezone":timezone}),
            )
            .await?;
        }
        CalOutcome::Failed { code, .. } => {
            repo::fail_change(&mut tx, &change, booking_id, &code).await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.reschedule_failed",
                serde_json::json!({"code":code}),
            )
            .await?;
        }
        CalOutcome::Unknown => {
            repo::mark_change_unknown(&mut tx, &change, booking_id).await?;
            record_worker_booking(
                &mut tx,
                booking_id,
                job_id,
                "booking.reschedule_unknown",
                serde_json::json!({}),
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn expire_holds(tx: &mut Tx<'_>, job_id: Uuid) -> Result<(), AppError> {
    let ids = repo::expire_held_bookings(tx).await?;
    if !ids.is_empty() {
        let actor:Option<Uuid>=sqlx::query_scalar("SELECT id FROM principals WHERE workspace_id=$1 AND kind='system' AND status='active' ORDER BY created_at LIMIT 1").bind(tx.workspace_id()).fetch_optional(tx.conn()).await.map_err(infrastructure::error::map_sqlx)?;
        for id in ids {
            audit::record(
                tx,
                AuditEntry {
                    actor_principal_id: actor,
                    requested_by: None,
                    approved_by: None,
                    job_id: Some(job_id),
                    action: "booking.hold_expired".into(),
                    target_type: "booking".into(),
                    target_id: Some(id),
                    outcome: "ok".into(),
                    metadata: serde_json::json!({}),
                    request_id: format!("job:{job_id}"),
                    trace_id: None,
                },
            )
            .await?;
        }
    }
    Ok(())
}
