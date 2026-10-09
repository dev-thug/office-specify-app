//! Booking module repositories (spec §10, A.6.6).

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::{db_constraint, map_sqlx};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MeetingTypeRow {
    pub id: Uuid,
    pub host_principal_id: Uuid,
    pub calendar_connection_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub duration_minutes: i32,
    pub buffer_before_minutes: i32,
    pub buffer_after_minutes: i32,
    pub minimum_notice_minutes: i32,
    pub booking_horizon_days: i32,
    pub status: String,
    pub version: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookingLinkRow {
    pub id: Uuid,
    pub meeting_type_id: Uuid,
    pub host_principal_id: Uuid,
    pub bound_email_address_id: Option<Uuid>,
    pub campaign_enrollment_id: Option<Uuid>,
    pub max_uses: i32,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub used_count: i64,
    pub meeting_name: String,
    pub meeting_description: Option<String>,
    pub duration_minutes: i32,
    pub buffer_before_minutes: i32,
    pub buffer_after_minutes: i32,
    pub minimum_notice_minutes: i32,
    pub booking_horizon_days: i32,
    pub meeting_status: String,
    pub calendar_connection_id: Uuid,
    pub provider_calendar_id: String,
    pub calendar_status: String,
    pub integration_status: String,
    pub host_name: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AvailabilityRuleRow {
    pub id: Uuid,
    pub timezone: String,
    pub weekday: i16,
    pub start_local: NaiveTime,
    pub end_local: NaiveTime,
    pub valid_from: Option<NaiveDate>,
    pub valid_until: Option<NaiveDate>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AvailabilityExceptionRow {
    pub id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub kind: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookingRow {
    pub id: Uuid,
    pub booking_link_id: Uuid,
    pub host_principal_id: Uuid,
    pub guest_email_address_id: Uuid,
    pub guest_email: Option<String>,
    pub guest_name: String,
    pub guest_memo: Option<String>,
    pub guest_timezone: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub failure_code: Option<String>,
    pub provider_event_id: Option<String>,
    pub attendance_status: Option<String>,
    pub outcome_note: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookingSagaRow {
    pub id: Uuid,
    pub booking_link_id: Uuid,
    pub host_principal_id: Uuid,
    pub guest_email: String,
    pub guest_name: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub occupied_start: DateTime<Utc>,
    pub occupied_end: DateTime<Utc>,
    pub status: String,
    pub provider_event_id: Option<String>,
    pub version: i64,
    pub provider_calendar_id: String,
    pub meeting_name: String,
    pub buffer_before_minutes: i32,
    pub buffer_after_minutes: i32,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookingChangeRow {
    pub id: Uuid,
    pub booking_id: Option<Uuid>,
    pub host_principal_id: Uuid,
    pub kind: String,
    pub proposed_data: serde_json::Value,
    pub status: String,
    pub new_occupied_start: Option<DateTime<Utc>>,
    pub new_occupied_end: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ManageTokenRow {
    pub booking_id: Uuid,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HoldRange {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BookingChangeSagaRow {
    pub id: Uuid,
    pub booking_id: Option<Uuid>,
    pub host_principal_id: Uuid,
    pub kind: String,
    pub proposed_data: serde_json::Value,
    pub status: String,
    pub new_occupied_start: Option<DateTime<Utc>>,
    pub new_occupied_end: Option<DateTime<Utc>>,
    pub provider_calendar_id: Option<String>,
    pub provider_event_id: Option<String>,
    pub guest_email: Option<String>,
    pub guest_name: Option<String>,
    pub meeting_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RescheduleBookingLockRow {
    host_principal_id: Uuid,
    version: i64,
    status: String,
    occupied_start: DateTime<Utc>,
    occupied_end: DateTime<Utc>,
}

pub struct RescheduleCompletion<'a> {
    pub booking_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub occupied_start: DateTime<Utc>,
    pub occupied_end: DateTime<Utc>,
    pub event_id: &'a str,
    pub revision: &'a str,
}

const MT_COLS: &str = "id, host_principal_id, calendar_connection_id, name, description,
    duration_minutes, buffer_before_minutes, buffer_after_minutes,
    minimum_notice_minutes, booking_horizon_days, status, version";

pub async fn get_meeting_type(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<MeetingTypeRow>, AppError> {
    sqlx::query_as::<_, MeetingTypeRow>(&format!(
        "SELECT {MT_COLS} FROM meeting_types WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn list_meeting_types(
    tx: &mut Tx<'_>,
    host_id: Option<Uuid>,
) -> Result<Vec<MeetingTypeRow>, AppError> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(&format!(
        "SELECT {MT_COLS} FROM meeting_types WHERE workspace_id = "
    ));
    qb.push_bind(tx.workspace_id());
    if let Some(host) = host_id {
        qb.push(" AND host_principal_id = ").push_bind(host);
    }
    qb.push(" ORDER BY created_at DESC, id DESC");
    qb.build_query_as::<MeetingTypeRow>()
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

pub async fn calendar_connection_active_for_host(
    tx: &mut Tx<'_>,
    connection_id: Uuid,
    host_id: Uuid,
) -> Result<bool, AppError> {
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM calendar_connections c
             JOIN principals p ON p.workspace_id = c.workspace_id AND p.id = c.host_principal_id
             JOIN integration_connections i ON i.workspace_id = c.workspace_id AND i.id = c.connection_id
            WHERE c.workspace_id = $1 AND c.id = $2 AND c.host_principal_id = $3
              AND c.status = 'active' AND i.status = 'active'
              AND p.kind = 'human' AND p.status = 'active'
         )",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .bind(host_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(active)
}

pub async fn active_principal_kind(
    tx: &mut Tx<'_>,
    principal_id: Uuid,
    kind: &str,
) -> Result<bool, AppError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM principals WHERE workspace_id=$1 AND id=$2 AND kind=$3 AND status='active')",
    )
    .bind(tx.workspace_id()).bind(principal_id).bind(kind).fetch_one(tx.conn()).await.map_err(map_sqlx)
}

pub async fn published_privacy_notice_version(tx: &mut Tx<'_>) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT settings->>'booking_privacy_notice_version' FROM workspaces WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Configured public origin for system-generated booking links. No local
/// development fallback is safe for outbound campaign snapshots.
pub async fn configured_public_base_url(tx: &mut Tx<'_>) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT NULLIF(btrim(settings->>'public_base_url'), '') FROM workspaces WHERE id = $1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
    .and_then(|value| crate::config::public_base_url(value.flatten()).map(Some))
}

pub async fn booking_link_use_count(tx: &mut Tx<'_>, link_id: Uuid) -> Result<i64, AppError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM bookings WHERE workspace_id = $1 AND booking_link_id = $2
           AND status NOT IN ('canceled','failed','expired')",
    )
    .bind(tx.workspace_id())
    .bind(link_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn insert_meeting_type(
    tx: &mut Tx<'_>,
    host_id: Uuid,
    request: &contracts::booking::CreateMeetingTypeRequest,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO meeting_types
            (workspace_id, host_principal_id, calendar_connection_id, name, description,
             duration_minutes, buffer_before_minutes, buffer_after_minutes,
             minimum_notice_minutes, booking_horizon_days)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(host_id)
    .bind(request.calendar_connection_id)
    .bind(request.name.trim())
    .bind(request.description.as_deref().map(str::trim))
    .bind(request.duration_minutes)
    .bind(request.buffer_before_minutes)
    .bind(request.buffer_after_minutes)
    .bind(request.minimum_notice_minutes)
    .bind(request.booking_horizon_days)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn update_meeting_type(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    request: &contracts::booking::UpdateMeetingTypeRequest,
) -> Result<Option<MeetingTypeRow>, AppError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE meeting_types SET
             name = COALESCE($4, name),
             description = CASE WHEN $5::boolean THEN $6 ELSE description END,
             duration_minutes = COALESCE($7, duration_minutes),
             buffer_before_minutes = COALESCE($8, buffer_before_minutes),
             buffer_after_minutes = COALESCE($9, buffer_after_minutes),
             minimum_notice_minutes = COALESCE($10, minimum_notice_minutes),
             booking_horizon_days = COALESCE($11, booking_horizon_days),
             status = COALESCE($12, status), version = version + 1
         WHERE workspace_id = $1 AND id = $2 AND version = $3
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .bind(request.name.as_deref().map(str::trim))
    .bind(request.description.is_some())
    .bind(
        request
            .description
            .as_ref()
            .and_then(|v| v.as_deref())
            .map(str::trim),
    )
    .bind(request.duration_minutes)
    .bind(request.buffer_before_minutes)
    .bind(request.buffer_after_minutes)
    .bind(request.minimum_notice_minutes)
    .bind(request.booking_horizon_days)
    .bind(&request.status)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    match updated {
        Some(_) => get_meeting_type(tx, id).await,
        None => Ok(None),
    }
}

pub async fn availability_rules(
    tx: &mut Tx<'_>,
    host_id: Uuid,
) -> Result<Vec<AvailabilityRuleRow>, AppError> {
    sqlx::query_as::<_, AvailabilityRuleRow>(
        "SELECT id, timezone, weekday, start_local, end_local, valid_from, valid_until
           FROM availability_rules WHERE workspace_id = $1 AND host_principal_id = $2
          ORDER BY weekday, start_local, id",
    )
    .bind(tx.workspace_id())
    .bind(host_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn availability_exceptions(
    tx: &mut Tx<'_>,
    host_id: Uuid,
) -> Result<Vec<AvailabilityExceptionRow>, AppError> {
    sqlx::query_as::<_, AvailabilityExceptionRow>(
        "SELECT id, starts_at, ends_at, kind, reason
           FROM availability_exceptions WHERE workspace_id = $1 AND host_principal_id = $2
          ORDER BY starts_at, id",
    )
    .bind(tx.workspace_id())
    .bind(host_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn replace_availability(
    tx: &mut Tx<'_>,
    host_id: Uuid,
    req: &contracts::booking::UpdateAvailabilityRequest,
) -> Result<(), AppError> {
    sqlx::query(
        "DELETE FROM availability_rules WHERE workspace_id = $1 AND host_principal_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(host_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    sqlx::query(
        "DELETE FROM availability_exceptions WHERE workspace_id = $1 AND host_principal_id = $2",
    )
    .bind(tx.workspace_id())
    .bind(host_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    for rule in &req.rules {
        let start = NaiveTime::parse_from_str(&rule.start_local, "%H:%M")
            .map_err(|_| AppError::validation("start_local must use HH:MM", vec![]))?;
        let end = NaiveTime::parse_from_str(&rule.end_local, "%H:%M")
            .map_err(|_| AppError::validation("end_local must use HH:MM", vec![]))?;
        sqlx::query(
            "INSERT INTO availability_rules
                (workspace_id, host_principal_id, timezone, weekday, start_local, end_local, valid_from, valid_until)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(tx.workspace_id()).bind(host_id).bind(&rule.timezone).bind(rule.weekday)
        .bind(start).bind(end).bind(rule.valid_from).bind(rule.valid_until)
        .execute(tx.conn()).await.map_err(map_sqlx)?;
    }
    for exception in &req.exceptions {
        sqlx::query(
            "INSERT INTO availability_exceptions
                (workspace_id, host_principal_id, starts_at, ends_at, kind, reason)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(tx.workspace_id())
        .bind(host_id)
        .bind(exception.starts_at)
        .bind(exception.ends_at)
        .bind(&exception.kind)
        .bind(exception.reason.as_deref().map(str::trim))
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }
    Ok(())
}

pub async fn insert_booking_link(
    tx: &mut Tx<'_>,
    host_id: Uuid,
    request: &contracts::booking::CreateBookingLinkRequest,
    token_hash: &[u8; 32],
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO booking_links
            (workspace_id, meeting_type_id, host_principal_id, token_hash,
             bound_email_address_id, campaign_enrollment_id, max_uses, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(request.meeting_type_id)
    .bind(host_id)
    .bind(&token_hash[..])
    .bind(request.bound_email_address_id)
    .bind(request.campaign_enrollment_id)
    .bind(request.max_uses)
    .bind(request.expires_at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn issue_manage_token(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
    token_hash: &[u8; 32],
    expires_at: DateTime<Utc>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO booking_manage_tokens (workspace_id, booking_id, token_hash, expires_at)
         VALUES ($1,$2,$3,$4) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .bind(&token_hash[..])
    .bind(expires_at)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_manage_token(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<ManageTokenRow>, AppError> {
    sqlx::query_as::<_, ManageTokenRow>(
        "SELECT booking_id, expires_at, revoked_at FROM booking_manage_tokens
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn active_holds_between(
    tx: &mut Tx<'_>,
    host_id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<Vec<HoldRange>, AppError> {
    sqlx::query_as::<_, HoldRange>(
        "SELECT starts_at, ends_at FROM booking_slot_holds
          WHERE workspace_id = $1 AND host_principal_id = $2
            AND status IN ('held','confirming','confirmed','unknown')
            AND tstzrange(starts_at, ends_at, '[)') && tstzrange($3, $4, '[)')
          ORDER BY starts_at",
    )
    .bind(tx.workspace_id())
    .bind(host_id)
    .bind(start)
    .bind(end)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn active_holds_between_except_booking(
    tx: &mut Tx<'_>,
    host_id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    exclude_booking_id: Uuid,
) -> Result<Vec<HoldRange>, AppError> {
    sqlx::query_as::<_, HoldRange>(
        "SELECT starts_at, ends_at FROM booking_slot_holds
          WHERE workspace_id = $1 AND host_principal_id = $2
            AND status IN ('held','confirming','confirmed','unknown')
            AND booking_id IS DISTINCT FROM $5
            AND tstzrange(starts_at, ends_at, '[)') && tstzrange($3, $4, '[)')
          ORDER BY starts_at",
    )
    .bind(tx.workspace_id())
    .bind(host_id)
    .bind(start)
    .bind(end)
    .bind(exclude_booking_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

const LINK_COLS: &str = "l.id, l.meeting_type_id, l.host_principal_id,
    l.bound_email_address_id, l.campaign_enrollment_id, l.max_uses, l.expires_at,
    l.revoked_at,
    (SELECT count(*) FROM bookings b WHERE b.workspace_id = l.workspace_id AND b.booking_link_id = l.id
       AND b.status NOT IN ('canceled','failed','expired')) AS used_count,
    m.name AS meeting_name, m.description AS meeting_description, m.duration_minutes,
    m.buffer_before_minutes, m.buffer_after_minutes, m.minimum_notice_minutes,
    m.booking_horizon_days, m.status AS meeting_status,
    c.id AS calendar_connection_id, c.provider_calendar_id, c.status AS calendar_status,
    i.status AS integration_status,
    p.name AS host_name";

pub async fn get_booking_link(
    tx: &mut Tx<'_>,
    id: Uuid,
    lock: bool,
) -> Result<Option<BookingLinkRow>, AppError> {
    let lock = if lock { " FOR UPDATE OF l" } else { "" };
    sqlx::query_as::<_, BookingLinkRow>(&format!(
        "SELECT {LINK_COLS} FROM booking_links l
           JOIN meeting_types m ON m.workspace_id = l.workspace_id AND m.id = l.meeting_type_id
           JOIN calendar_connections c ON c.workspace_id = m.workspace_id AND c.id = m.calendar_connection_id
           JOIN integration_connections i ON i.workspace_id = c.workspace_id AND i.id = c.connection_id
           JOIN principals p ON p.workspace_id = l.workspace_id AND p.id = l.host_principal_id
          WHERE l.workspace_id = $1 AND l.id = $2{lock}"
    ))
    .bind(tx.workspace_id()).bind(id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}

pub async fn get_booking_link_by_id_from_token(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<BookingLinkRow>, AppError> {
    get_booking_link(tx, id, false).await
}

pub async fn get_booking(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<BookingRow>, AppError> {
    sqlx::query_as::<_, BookingRow>(
        "SELECT b.id, b.booking_link_id, b.host_principal_id, b.guest_email_address_id,
                a.address_original AS guest_email, b.guest_name, b.guest_memo, b.guest_timezone,
                b.starts_at, b.ends_at, b.status, b.failure_code, b.provider_event_id,
                b.attendance_status, b.outcome_note, b.version
           FROM bookings b JOIN email_addresses a
             ON a.workspace_id = b.workspace_id AND a.id = b.guest_email_address_id
          WHERE b.workspace_id = $1 AND b.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn list_bookings(
    tx: &mut Tx<'_>,
    host_ids: &[Uuid],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<Vec<BookingRow>, AppError> {
    sqlx::query_as::<_, BookingRow>(
        "SELECT b.id, b.booking_link_id, b.host_principal_id, b.guest_email_address_id,
                a.address_original AS guest_email, b.guest_name, b.guest_memo, b.guest_timezone,
                b.starts_at, b.ends_at, b.status, b.failure_code, b.provider_event_id,
                b.attendance_status, b.outcome_note, b.version
           FROM bookings b JOIN email_addresses a
             ON a.workspace_id = b.workspace_id AND a.id = b.guest_email_address_id
          WHERE b.workspace_id = $1 AND (cardinality($2::uuid[]) = 0 OR b.host_principal_id = ANY($2))
            AND ($3::timestamptz IS NULL OR b.starts_at >= $3)
            AND ($4::timestamptz IS NULL OR b.starts_at < $4)
          ORDER BY b.starts_at DESC, b.id DESC LIMIT 200",
    )
    .bind(tx.workspace_id()).bind(host_ids).bind(from).bind(to)
    .fetch_all(tx.conn()).await.map_err(map_sqlx)
}

pub async fn set_attendance(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    attendance_status: &str,
    outcome_note: Option<&str>,
) -> Result<Option<BookingRow>, AppError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE bookings SET attendance_status = $4, outcome_note = $5, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $3
            AND status = 'confirmed' AND ends_at <= now()
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .bind(attendance_status)
    .bind(outcome_note)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    match updated {
        Some(_) => get_booking(tx, id).await,
        None => Ok(None),
    }
}

pub async fn find_or_create_email_address(
    tx: &mut Tx<'_>,
    original: &str,
    normalized_key: &str,
) -> Result<Uuid, AppError> {
    if let Some(id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM email_addresses WHERE workspace_id = $1 AND normalized_key = $2",
    )
    .bind(tx.workspace_id())
    .bind(normalized_key)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?
    {
        return Ok(id);
    }
    sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1,$2,$3) ON CONFLICT (workspace_id, normalized_key)
         DO UPDATE SET normalized_key = EXCLUDED.normalized_key RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(original)
    .bind(normalized_key)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Create a booking and its authoritative buffered hold atomically. Constraint
/// conflicts become the public `SLOT_UNAVAILABLE` code.
#[allow(clippy::too_many_arguments)]
pub async fn insert_booking_and_hold(
    tx: &mut Tx<'_>,
    link: &BookingLinkRow,
    guest_email_address_id: Uuid,
    guest_email_verification_status: &str,
    req: &contracts::booking::CreatePublicBookingRequest,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    occupied_start: DateTime<Utc>,
    occupied_end: DateTime<Utc>,
) -> Result<Uuid, AppError> {
    // Serialize max_uses and booking creation per link. The caller already
    // locks the link row, then this count is stable until commit.
    if link.used_count >= i64::from(link.max_uses) {
        return Err(AppError::new(
            contracts::ErrorCode::BookingLinkExpired,
            "booking link has no remaining uses",
        ));
    }
    let booking_id: Uuid = sqlx::query_scalar(
        "INSERT INTO bookings
            (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
             guest_name, guest_memo, guest_timezone, guest_email_verification_status,
             privacy_notice_version, privacy_acknowledged_at,
             starts_at, ends_at, occupied_start, occupied_end, status)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,now(),$10,$11,$12,$13,'confirming') RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(link.id)
    .bind(link.host_principal_id)
    .bind(guest_email_address_id)
    .bind(req.guest_name.trim())
    .bind(req.memo.as_deref().map(str::trim).filter(|m| !m.is_empty()))
    .bind(&req.timezone)
    .bind(guest_email_verification_status)
    .bind(req.privacy_notice_version.trim())
    .bind(starts_at)
    .bind(ends_at)
    .bind(occupied_start)
    .bind(occupied_end)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;

    let hold_result = sqlx::query(
        "INSERT INTO booking_slot_holds
            (workspace_id, host_principal_id, booking_id, starts_at, ends_at, status)
         VALUES ($1,$2,$3,$4,$5,'confirming')",
    )
    .bind(tx.workspace_id())
    .bind(link.host_principal_id)
    .bind(booking_id)
    .bind(occupied_start)
    .bind(occupied_end)
    .execute(tx.conn())
    .await;
    if let Err(error) = hold_result {
        return Err(map_hold_error(error));
    }
    Ok(booking_id)
}

fn map_hold_error(error: sqlx::Error) -> AppError {
    if db_constraint(&error).as_deref() == Some("booking_slots_no_overlap") {
        AppError::new(
            contracts::ErrorCode::SlotUnavailable,
            "selected slot is no longer available",
        )
    } else {
        map_sqlx(error)
    }
}

pub async fn booking_saga(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
) -> Result<Option<BookingSagaRow>, AppError> {
    sqlx::query_as::<_, BookingSagaRow>(
        "SELECT b.id, b.booking_link_id, b.host_principal_id, a.address_original AS guest_email,
                b.guest_name, b.starts_at, b.ends_at, b.occupied_start, b.occupied_end,
                b.status, b.provider_event_id, b.version, c.provider_calendar_id,
                m.name AS meeting_name, m.buffer_before_minutes, m.buffer_after_minutes
           FROM bookings b JOIN email_addresses a
             ON a.workspace_id = b.workspace_id AND a.id = b.guest_email_address_id
           JOIN booking_links l ON l.workspace_id = b.workspace_id AND l.id = b.booking_link_id
           JOIN meeting_types m ON m.workspace_id = l.workspace_id AND m.id = l.meeting_type_id
           JOIN calendar_connections c ON c.workspace_id = m.workspace_id AND c.id = m.calendar_connection_id
          WHERE b.workspace_id = $1 AND b.id = $2 FOR UPDATE OF b",
    )
    .bind(tx.workspace_id()).bind(booking_id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}

pub async fn mark_booking_confirmed(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
    event_id: &str,
    revision: &str,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE bookings SET status = 'confirmed', provider_event_id = $3,
                provider_revision = $4, confirmation_verified_at = now(),
                hold_expires_at = NULL, failure_code = NULL, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND status = 'confirming'",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .bind(event_id)
    .bind(revision)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    if n == 1 {
        sqlx::query("UPDATE booking_slot_holds SET status = 'confirmed', expires_at = NULL WHERE workspace_id = $1 AND booking_id = $2 AND status = 'confirming'")
            .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    }
    Ok(n == 1)
}

pub async fn mark_booking_unknown(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
    failure_code: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE bookings SET status = 'unknown', failure_code = $3, version = version + 1 WHERE workspace_id = $1 AND id = $2 AND status IN ('confirming','canceling')")
        .bind(tx.workspace_id()).bind(booking_id).bind(failure_code).execute(tx.conn()).await.map_err(map_sqlx)?;
    sqlx::query("UPDATE booking_slot_holds SET status = 'unknown', expires_at = NULL WHERE workspace_id = $1 AND booking_id = $2 AND status IN ('held','confirming','confirmed')")
        .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}

pub async fn mark_booking_failed(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
    failure_code: &str,
) -> Result<(), AppError> {
    sqlx::query("UPDATE bookings SET status = 'failed', failure_code = $3, version = version + 1 WHERE workspace_id = $1 AND id = $2 AND status IN ('held','confirming')")
        .bind(tx.workspace_id()).bind(booking_id).bind(failure_code).execute(tx.conn()).await.map_err(map_sqlx)?;
    sqlx::query("UPDATE booking_slot_holds SET status = 'released', expires_at = NULL WHERE workspace_id = $1 AND booking_id = $2 AND status IN ('held','confirming')")
        .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}

pub async fn expire_held_bookings(tx: &mut Tx<'_>) -> Result<Vec<Uuid>, AppError> {
    let ids = sqlx::query_scalar::<_, Uuid>(
        "UPDATE bookings SET status = 'expired', version = version + 1
          WHERE workspace_id = $1 AND status = 'held' AND hold_expires_at <= now()
          RETURNING id",
    )
    .bind(tx.workspace_id())
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if !ids.is_empty() {
        sqlx::query("UPDATE booking_slot_holds SET status = 'expired', expires_at = NULL WHERE workspace_id = $1 AND booking_id = ANY($2) AND status = 'held'")
            .bind(tx.workspace_id()).bind(&ids).execute(tx.conn()).await.map_err(map_sqlx)?;
    }
    Ok(ids)
}

pub async fn insert_change(
    tx: &mut Tx<'_>,
    booking_id: Option<Uuid>,
    host_id: Uuid,
    kind: &str,
    proposed_data: &serde_json::Value,
    requested_by: Option<Uuid>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO booking_changes (workspace_id, booking_id, host_principal_id, kind, proposed_data, requested_by)
         VALUES ($1,$2,$3,$4,$5,$6) RETURNING id",
    )
    .bind(tx.workspace_id()).bind(booking_id).bind(host_id).bind(kind).bind(proposed_data).bind(requested_by)
    .fetch_one(tx.conn()).await.map_err(map_sqlx)
}

pub async fn get_change(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<BookingChangeRow>, AppError> {
    sqlx::query_as::<_, BookingChangeRow>(
        "SELECT id, booking_id, host_principal_id, kind, proposed_data, status, new_occupied_start, new_occupied_end
           FROM booking_changes WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id()).bind(id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}

pub async fn update_change_status(tx: &mut Tx<'_>, id: Uuid, status: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE booking_changes SET status = $3, version = version + 1 WHERE workspace_id = $1 AND id = $2")
        .bind(tx.workspace_id()).bind(id).bind(status).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(())
}

/// Lock and reserve a reschedule change. The booking row is serialized by
/// optimistic version first. If the new occupied range overlaps its current
/// hold, expand that one hold to the union; otherwise insert a separate
/// change hold. This avoids the self-conflict forbidden by A.6.6.
#[allow(clippy::too_many_arguments)]
pub async fn begin_reschedule_change(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
    expected_version: i64,
    new_starts_at: DateTime<Utc>,
    new_ends_at: DateTime<Utc>,
    new_occupied_start: DateTime<Utc>,
    new_occupied_end: DateTime<Utc>,
    timezone: &str,
    requested_by: Option<Uuid>,
) -> Result<Uuid, AppError> {
    let row: Option<RescheduleBookingLockRow> = sqlx::query_as(
        "SELECT host_principal_id, version, status, occupied_start, occupied_end
           FROM bookings WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some(row) = row else {
        return Err(AppError::not_found("booking not found"));
    };
    if row.version != expected_version {
        return Err(AppError::new(
            contracts::ErrorCode::VersionMismatch,
            "booking version changed",
        ));
    }
    if row.status != "confirmed" {
        return Err(AppError::conflict(
            "booking is not in a reschedulable state",
        ));
    }
    let hold: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM booking_slot_holds WHERE workspace_id = $1 AND booking_id = $2
            AND status = 'confirmed' FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let Some(hold_id) = hold else {
        return Err(AppError::conflict("booking has no active slot hold"));
    };
    let overlaps_self =
        new_occupied_start < row.occupied_end && row.occupied_start < new_occupied_end;
    let proposed = serde_json::json!({
        "starts_at": new_starts_at,
        "ends_at": new_ends_at,
        "timezone": timezone,
        "new_occupied_start": new_occupied_start,
        "new_occupied_end": new_occupied_end,
        "old_occupied_start": row.occupied_start,
        "old_occupied_end": row.occupied_end,
        "expanded_existing_hold": overlaps_self,
    });
    let change_id: Uuid = sqlx::query_scalar(
        "INSERT INTO booking_changes
            (workspace_id, booking_id, host_principal_id, kind, proposed_data,
             requested_by, status, new_occupied_start, new_occupied_end)
         VALUES ($1,$2,$3,'reschedule',$4,$5,'processing',$6,$7) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .bind(row.host_principal_id)
    .bind(&proposed)
    .bind(requested_by)
    .bind(new_occupied_start)
    .bind(new_occupied_end)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if overlaps_self {
        sqlx::query(
            "UPDATE booking_slot_holds SET starts_at = LEAST(starts_at,$3), ends_at = GREATEST(ends_at,$4), status = 'confirming'
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(tx.workspace_id()).bind(hold_id).bind(new_occupied_start).bind(new_occupied_end)
        .execute(tx.conn()).await.map_err(map_hold_error)?;
    } else {
        sqlx::query(
            "INSERT INTO booking_slot_holds
                (workspace_id, host_principal_id, booking_change_id, starts_at, ends_at, status)
             VALUES ($1,$2,$3,$4,$5,'confirming')",
        )
        .bind(tx.workspace_id())
        .bind(row.host_principal_id)
        .bind(change_id)
        .bind(new_occupied_start)
        .bind(new_occupied_end)
        .execute(tx.conn())
        .await
        .map_err(map_hold_error)?;
    }
    Ok(change_id)
}

pub async fn begin_cancel_change(
    tx: &mut Tx<'_>,
    booking_id: Uuid,
    expected_version: i64,
    requested_by: Option<Uuid>,
) -> Result<Uuid, AppError> {
    let row: Option<(Uuid, i64, String)> = sqlx::query_as(
        "SELECT host_principal_id, version, status FROM bookings WHERE workspace_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(tx.workspace_id()).bind(booking_id).fetch_optional(tx.conn()).await.map_err(map_sqlx)?;
    let Some((host_id, version, status)) = row else {
        return Err(AppError::not_found("booking not found"));
    };
    if version != expected_version {
        return Err(AppError::new(
            contracts::ErrorCode::VersionMismatch,
            "booking version changed",
        ));
    }
    if status != "confirmed" {
        return Err(AppError::conflict("booking is not in a cancelable state"));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO booking_changes
            (workspace_id, booking_id, host_principal_id, kind, proposed_data, requested_by, status)
         VALUES ($1,$2,$3,'cancel','{}',$4,'processing') RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(booking_id)
    .bind(host_id)
    .bind(requested_by)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    sqlx::query("UPDATE bookings SET status = 'canceling', version = version + 1 WHERE workspace_id = $1 AND id = $2")
        .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    sqlx::query("UPDATE booking_slot_holds SET status = 'confirming' WHERE workspace_id = $1 AND booking_id = $2 AND status = 'confirmed'")
        .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    Ok(id)
}

pub async fn booking_change_saga(
    tx: &mut Tx<'_>,
    change_id: Uuid,
) -> Result<Option<BookingChangeSagaRow>, AppError> {
    sqlx::query_as::<_, BookingChangeSagaRow>(
        "SELECT ch.id, ch.booking_id, ch.host_principal_id, ch.kind, ch.proposed_data,
                ch.status, ch.new_occupied_start, ch.new_occupied_end,
                c.provider_calendar_id, b.provider_event_id, a.address_original AS guest_email,
                b.guest_name, m.name AS meeting_name
           FROM booking_changes ch
           LEFT JOIN bookings b ON b.workspace_id = ch.workspace_id AND b.id = ch.booking_id
           LEFT JOIN booking_links l ON l.workspace_id = b.workspace_id AND l.id = b.booking_link_id
           LEFT JOIN meeting_types m ON m.workspace_id = l.workspace_id AND m.id = l.meeting_type_id
           LEFT JOIN calendar_connections c ON c.workspace_id = m.workspace_id AND c.id = m.calendar_connection_id
           LEFT JOIN email_addresses a ON a.workspace_id = b.workspace_id AND a.id = b.guest_email_address_id
          WHERE ch.workspace_id = $1 AND ch.id = $2 FOR UPDATE OF ch",
    )
    .bind(tx.workspace_id()).bind(change_id).fetch_optional(tx.conn()).await.map_err(map_sqlx)
}

pub async fn complete_reschedule(
    tx: &mut Tx<'_>,
    change: &BookingChangeSagaRow,
    completion: RescheduleCompletion<'_>,
) -> Result<(), AppError> {
    let RescheduleCompletion {
        booking_id,
        starts_at,
        ends_at,
        occupied_start,
        occupied_end,
        event_id,
        revision,
    } = completion;
    let expanded = change
        .proposed_data
        .get("expanded_existing_hold")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if expanded {
        sqlx::query("UPDATE booking_slot_holds SET starts_at = $3, ends_at = $4, status = 'confirmed' WHERE workspace_id = $1 AND booking_id = $2 AND status = 'confirming'")
            .bind(tx.workspace_id()).bind(booking_id).bind(occupied_start).bind(occupied_end)
            .execute(tx.conn()).await.map_err(map_sqlx)?;
    } else {
        sqlx::query("UPDATE booking_slot_holds SET status = 'released', expires_at = NULL WHERE workspace_id = $1 AND booking_id = $2 AND status = 'confirmed'")
            .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE booking_slot_holds SET booking_id = $3, booking_change_id = NULL, starts_at = $4, ends_at = $5, status = 'confirmed' WHERE workspace_id = $1 AND booking_change_id = $2 AND status = 'confirming'")
            .bind(tx.workspace_id()).bind(change.id).bind(booking_id).bind(occupied_start).bind(occupied_end)
            .execute(tx.conn()).await.map_err(map_sqlx)?;
    }
    sqlx::query("UPDATE bookings SET starts_at = $3, ends_at = $4, occupied_start = $5, occupied_end = $6, status = 'confirmed', provider_event_id = $7, provider_revision = $8, confirmation_verified_at = now(), failure_code = NULL, version = version + 1 WHERE workspace_id = $1 AND id = $2")
        .bind(tx.workspace_id()).bind(booking_id).bind(starts_at).bind(ends_at).bind(occupied_start).bind(occupied_end).bind(event_id).bind(revision)
        .execute(tx.conn()).await.map_err(map_sqlx)?;
    update_change_status(tx, change.id, "completed").await
}

pub async fn complete_cancel(
    tx: &mut Tx<'_>,
    change_id: Uuid,
    booking_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query("UPDATE bookings SET status = 'canceled', version = version + 1 WHERE workspace_id = $1 AND id = $2 AND status = 'canceling'")
        .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    sqlx::query("UPDATE booking_slot_holds SET status = 'released', expires_at = NULL WHERE workspace_id = $1 AND booking_id = $2 AND status IN ('confirmed','confirming','unknown')")
        .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    update_change_status(tx, change_id, "completed").await
}

pub async fn fail_change(
    tx: &mut Tx<'_>,
    change: &BookingChangeSagaRow,
    booking_id: Uuid,
    failure_code: &str,
) -> Result<(), AppError> {
    if change.kind == "reschedule" {
        let expanded = change
            .proposed_data
            .get("expanded_existing_hold")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if expanded {
            let old_start = change
                .proposed_data
                .get("old_occupied_start")
                .and_then(serde_json::Value::as_str)
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.with_timezone(&Utc));
            let old_end = change
                .proposed_data
                .get("old_occupied_end")
                .and_then(serde_json::Value::as_str)
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.with_timezone(&Utc));
            if let (Some(start), Some(end)) = (old_start, old_end) {
                sqlx::query("UPDATE booking_slot_holds SET starts_at = $3, ends_at = $4, status = 'confirmed' WHERE workspace_id = $1 AND booking_id = $2 AND status = 'confirming'")
                    .bind(tx.workspace_id()).bind(booking_id).bind(start).bind(end).execute(tx.conn()).await.map_err(map_sqlx)?;
            }
        } else {
            sqlx::query("UPDATE booking_slot_holds SET status = 'released', expires_at = NULL WHERE workspace_id = $1 AND booking_change_id = $2 AND status = 'confirming'")
                .bind(tx.workspace_id()).bind(change.id).execute(tx.conn()).await.map_err(map_sqlx)?;
        }
        sqlx::query("UPDATE bookings SET failure_code = $3, version = version + 1 WHERE workspace_id = $1 AND id = $2")
            .bind(tx.workspace_id()).bind(booking_id).bind(failure_code).execute(tx.conn()).await.map_err(map_sqlx)?;
    } else if change.kind == "cancel" {
        sqlx::query("UPDATE bookings SET status = 'confirmed', failure_code = $3, version = version + 1 WHERE workspace_id = $1 AND id = $2 AND status = 'canceling'")
            .bind(tx.workspace_id()).bind(booking_id).bind(failure_code).execute(tx.conn()).await.map_err(map_sqlx)?;
        sqlx::query("UPDATE booking_slot_holds SET status = 'confirmed' WHERE workspace_id = $1 AND booking_id = $2 AND status = 'confirming'")
            .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    }
    update_change_status(tx, change.id, "failed").await
}

pub async fn mark_change_unknown(
    tx: &mut Tx<'_>,
    change: &BookingChangeSagaRow,
    booking_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query("UPDATE bookings SET status = 'unknown', failure_code = 'PROVIDER_SUBMISSION_OUTCOME_UNKNOWN', version = version + 1 WHERE workspace_id = $1 AND id = $2")
        .bind(tx.workspace_id()).bind(booking_id).execute(tx.conn()).await.map_err(map_sqlx)?;
    sqlx::query("UPDATE booking_slot_holds SET status = 'unknown', expires_at = NULL WHERE workspace_id = $1 AND (booking_id = $2 OR booking_change_id = $3) AND status IN ('confirming','confirmed')")
        .bind(tx.workspace_id()).bind(booking_id).bind(change.id).execute(tx.conn()).await.map_err(map_sqlx)?;
    update_change_status(tx, change.id, "unknown").await
}
