//! Office-owned persistent calendar storage. No network clients or external
//! credentials are used by this adapter.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use crate::db::{Db, Tx};

use super::{BusyBlock, CalEvent, CalOutcome, CalendarProvider};

const LOCAL_PREFIX: &str = "office:";
const EVENT_PREFIX: &str = "office-event:";

#[derive(Debug, Clone, Copy)]
struct LocalCalendarRef {
    workspace_id: Uuid,
    calendar_connection_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalFailure {
    code: &'static str,
    message: &'static str,
}

impl LocalFailure {
    const INVALID_CALENDAR: Self = Self {
        code: "INVALID_CALENDAR_BINDING",
        message: "Office calendar binding is invalid",
    };
    const WORKSPACE_MISMATCH: Self = Self {
        code: "CALENDAR_SCOPE_MISMATCH",
        message: "Office calendar does not belong to the requested workspace",
    };
    const NOT_FOUND: Self = Self {
        code: "CALENDAR_NOT_FOUND",
        message: "Active Office calendar was not found",
    };
    const BOOKING_NOT_FOUND: Self = Self {
        code: "BOOKING_NOT_FOUND",
        message: "Booking is not bound to this Office calendar",
    };
    const EVENT_NOT_FOUND: Self = Self {
        code: "CALENDAR_EVENT_NOT_FOUND",
        message: "Office calendar event was not found",
    };
    const SLOT_UNAVAILABLE: Self = Self {
        code: "SLOT_UNAVAILABLE",
        message: "The requested time overlaps an active booking",
    };
    const IDEMPOTENCY_CONFLICT: Self = Self {
        code: "IDEMPOTENCY_CONFLICT",
        message: "Booking retry does not match the stored Office calendar event",
    };
    const EVENT_CANCELLED: Self = Self {
        code: "CALENDAR_EVENT_CANCELLED",
        message: "A cancelled Office calendar event cannot be changed",
    };
    const CHANGE_NOT_FOUND: Self = Self {
        code: "BOOKING_CHANGE_NOT_FOUND",
        message: "Booking change is not active for this booking",
    };
    const WORKSPACE_REQUIRED: Self = Self {
        code: "CALENDAR_WORKSPACE_REQUIRED",
        message: "Office calendar operations require an explicit workspace scope",
    };
    const DEPENDENCY: Self = Self {
        code: "DEPENDENCY_UNAVAILABLE",
        message: "Office calendar storage is temporarily unavailable",
    };

    fn outcome<T>(self) -> CalOutcome<T> {
        CalOutcome::Failed {
            code: self.code.to_string(),
            message: self.message.to_string(),
        }
    }
}

fn database_failure(error: &sqlx::Error) -> LocalFailure {
    match error.as_database_error().and_then(|e| e.code()).as_deref() {
        Some("23P01") | Some("23505") => LocalFailure::SLOT_UNAVAILABLE,
        Some("23514") | Some("23503") => LocalFailure::INVALID_CALENDAR,
        _ => LocalFailure::DEPENDENCY,
    }
}

fn parse_calendar_ref(value: &str) -> Result<LocalCalendarRef, LocalFailure> {
    let mut parts = value.split(':');
    if parts.next() != Some("office") {
        return Err(LocalFailure::INVALID_CALENDAR);
    }
    let (Some(workspace), Some(calendar), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(LocalFailure::INVALID_CALENDAR);
    };
    Ok(LocalCalendarRef {
        workspace_id: Uuid::parse_str(workspace).map_err(|_| LocalFailure::INVALID_CALENDAR)?,
        calendar_connection_id: Uuid::parse_str(calendar)
            .map_err(|_| LocalFailure::INVALID_CALENDAR)?,
    })
}

fn parse_event_id(value: &str) -> Result<Uuid, LocalFailure> {
    let booking = value
        .strip_prefix(EVENT_PREFIX)
        .ok_or(LocalFailure::INVALID_CALENDAR)?;
    Uuid::parse_str(booking).map_err(|_| LocalFailure::INVALID_CALENDAR)
}

fn validate_scope(workspace_id: Uuid, value: &str) -> Result<LocalCalendarRef, LocalFailure> {
    let calendar_ref = parse_calendar_ref(value)?;
    if calendar_ref.workspace_id != workspace_id {
        return Err(LocalFailure::WORKSPACE_MISMATCH);
    }
    Ok(calendar_ref)
}

#[derive(Debug, FromRow)]
struct CalendarBinding {
    host_principal_id: Uuid,
}

async fn binding_in_tx(
    tx: &mut Tx<'_>,
    workspace_id: Uuid,
    calendar_ref: LocalCalendarRef,
    provider_calendar_id: &str,
) -> Result<CalendarBinding, LocalFailure> {
    let binding = sqlx::query_as::<_, CalendarBinding>(
        "SELECT c.host_principal_id
           FROM calendar_connections c
           JOIN integration_connections i
             ON i.workspace_id = c.workspace_id AND i.id = c.connection_id
           JOIN principals p
             ON p.workspace_id = c.workspace_id AND p.id = c.host_principal_id
          WHERE c.workspace_id = $1
            AND c.id = $2
            AND c.provider_calendar_id = $3
            AND c.provider = 'office_calendar'
            AND c.status = 'active'
            AND i.provider = 'office_calendar'
            AND i.status = 'active'
            AND p.kind = 'human'
            AND p.status = 'active'
          FOR UPDATE OF c, i, p",
    )
    .bind(workspace_id)
    .bind(calendar_ref.calendar_connection_id)
    .bind(provider_calendar_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(|e| database_failure(&e))?
    .ok_or(LocalFailure::NOT_FOUND)?;
    Ok(binding)
}

#[derive(Debug, FromRow)]
struct BookingContext {
    host_principal_id: Uuid,
    calendar_connection_id: Uuid,
    attendee_email: String,
    title: String,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    occupied_start: DateTime<Utc>,
    occupied_end: DateTime<Utc>,
    status: String,
    provider_event_id: Option<String>,
}

#[derive(Debug, FromRow)]
struct BookingChangeContext {
    kind: String,
    status: String,
    proposed_data: serde_json::Value,
    new_occupied_start: Option<DateTime<Utc>>,
    new_occupied_end: Option<DateTime<Utc>>,
}

async fn booking_change_in_tx(
    tx: &mut Tx<'_>,
    workspace_id: Uuid,
    change_id: Uuid,
    booking_id: Uuid,
    host_id: Uuid,
) -> Result<BookingChangeContext, LocalFailure> {
    let change = sqlx::query_as::<_, BookingChangeContext>(
        "SELECT kind, status, proposed_data, new_occupied_start, new_occupied_end
           FROM booking_changes
          WHERE workspace_id = $1 AND id = $2 AND booking_id = $3
            AND host_principal_id = $4
          FOR UPDATE",
    )
    .bind(workspace_id)
    .bind(change_id)
    .bind(booking_id)
    .bind(host_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(|e| database_failure(&e))?
    .ok_or(LocalFailure::CHANGE_NOT_FOUND)?;
    Ok(change)
}

fn proposed_time(change: &BookingChangeContext, field: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(change.proposed_data.get(field)?.as_str()?)
        .ok()
        .map(|value| value.with_timezone(&Utc))
}

async fn booking_in_tx(
    tx: &mut Tx<'_>,
    workspace_id: Uuid,
    booking_id: Uuid,
) -> Result<BookingContext, LocalFailure> {
    let booking = sqlx::query_as::<_, BookingContext>(
        "SELECT b.host_principal_id, m.calendar_connection_id,
                a.address_original AS attendee_email, m.name AS title,
                b.starts_at, b.ends_at, b.occupied_start, b.occupied_end,
                b.status, b.provider_event_id
           FROM bookings b
           JOIN booking_links l
             ON l.workspace_id = b.workspace_id AND l.id = b.booking_link_id
           JOIN meeting_types m
             ON m.workspace_id = l.workspace_id AND m.id = l.meeting_type_id
           JOIN email_addresses a
             ON a.workspace_id = b.workspace_id AND a.id = b.guest_email_address_id
          WHERE b.workspace_id = $1 AND b.id = $2
          FOR UPDATE OF b",
    )
    .bind(workspace_id)
    .bind(booking_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(|e| database_failure(&e))?
    .ok_or(LocalFailure::BOOKING_NOT_FOUND)?;
    Ok(booking)
}

#[derive(Debug, FromRow)]
struct EventRow {
    provider_event_id: String,
    title: String,
    attendee_email: String,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    status: String,
    revision: i64,
}

impl EventRow {
    fn response(&self) -> CalEvent {
        CalEvent {
            provider_event_id: self.provider_event_id.clone(),
            revision: self.revision.to_string(),
            start: self.starts_at,
            end: self.ends_at,
        }
    }
}

#[derive(Debug, FromRow)]
struct BusyRow {
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
}

async fn lock_host(tx: &mut Tx<'_>, workspace_id: Uuid, host_id: Uuid) -> Result<(), LocalFailure> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("office-calendar:{workspace_id}:{host_id}"))
        .execute(tx.conn())
        .await
        .map_err(|e| database_failure(&e))?;
    Ok(())
}

async fn has_conflict(
    tx: &mut Tx<'_>,
    workspace_id: Uuid,
    host_id: Uuid,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    exclude_booking_id: Option<Uuid>,
    exclude_change_id: Option<Uuid>,
) -> Result<bool, LocalFailure> {
    let conflicting: bool = sqlx::query_scalar(
        "SELECT
             EXISTS (
                 SELECT 1 FROM booking_slot_holds h
                  WHERE h.workspace_id = $1 AND h.host_principal_id = $2
                    AND h.status IN ('held','confirming','confirmed','unknown')
                    AND (h.status <> 'held' OR h.expires_at > now())
                    AND h.starts_at < $4 AND h.ends_at > $3
                    AND ($5::uuid IS NULL OR h.booking_id IS DISTINCT FROM $5)
                    AND ($6::uuid IS NULL OR h.booking_change_id IS DISTINCT FROM $6)
             )
             OR EXISTS (
                 SELECT 1 FROM bookings b
                  WHERE b.workspace_id = $1 AND b.host_principal_id = $2
                    AND b.status IN ('confirming','confirmed','canceling','unknown')
                    AND b.occupied_start < $4 AND b.occupied_end > $3
                    AND ($5::uuid IS NULL OR b.id <> $5)
             )
             OR EXISTS (
                 SELECT 1 FROM office_calendar_events e
                  WHERE e.workspace_id = $1 AND e.host_principal_id = $2
                    AND e.status = 'active'
                    AND e.starts_at < $4 AND e.ends_at > $3
                    AND ($5::uuid IS NULL OR e.booking_id <> $5)
             )",
    )
    .bind(workspace_id)
    .bind(host_id)
    .bind(start)
    .bind(end)
    .bind(exclude_booking_id)
    .bind(exclude_change_id)
    .fetch_one(tx.conn())
    .await
    .map_err(|e| database_failure(&e))?;
    Ok(conflicting)
}

/// Durable local calendar events, tied to a single active calendar and
/// booking. Every operation takes an explicit workspace through the scoped
/// trait methods used by the application.
#[derive(Clone)]
pub struct OfficeCalendarProvider {
    db: Db,
}

impl OfficeCalendarProvider {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    fn accepts_calendar_id(value: &str) -> bool {
        value.starts_with(LOCAL_PREFIX)
    }

    async fn free_busy_scoped_inner(
        &self,
        workspace_id: Uuid,
        host: &str,
        range_start: DateTime<Utc>,
        range_end: DateTime<Utc>,
        exclude_booking_id: Option<Uuid>,
        exclude_change_id: Option<Uuid>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        if range_end <= range_start {
            return LocalFailure::INVALID_CALENDAR.outcome();
        }
        let calendar_ref = match validate_scope(workspace_id, host) {
            Ok(reference) => reference,
            Err(error) => return error.outcome(),
        };
        let mut tx = match self.db.begin(workspace_id).await {
            Ok(tx) => tx,
            Err(_) => return LocalFailure::DEPENDENCY.outcome(),
        };
        let binding = match binding_in_tx(&mut tx, workspace_id, calendar_ref, host).await {
            Ok(binding) => binding,
            Err(error) => return error.outcome(),
        };
        let rows = match sqlx::query_as::<_, BusyRow>(
            "SELECT DISTINCT starts_at, ends_at
               FROM (
                   SELECT e.starts_at, e.ends_at
                     FROM office_calendar_events e
                    WHERE e.workspace_id = $1 AND e.calendar_connection_id = $2
                      AND e.host_principal_id = $3 AND e.status = 'active'
                      AND e.starts_at < $5 AND e.ends_at > $4
                      AND ($6::uuid IS NULL OR e.booking_id <> $6)
                   UNION ALL
                   SELECT h.starts_at, h.ends_at
                     FROM booking_slot_holds h
                    WHERE h.workspace_id = $1 AND h.host_principal_id = $3
                      AND h.status IN ('held','confirming','confirmed','unknown')
                      AND (h.status <> 'held' OR h.expires_at > now())
                      AND h.starts_at < $5 AND h.ends_at > $4
                      AND ($6::uuid IS NULL OR h.booking_id IS DISTINCT FROM $6)
                      AND ($7::uuid IS NULL OR h.booking_change_id IS DISTINCT FROM $7)
                   UNION ALL
                   SELECT b.occupied_start AS starts_at, b.occupied_end AS ends_at
                     FROM bookings b
                    WHERE b.workspace_id = $1 AND b.host_principal_id = $3
                      AND (b.status IN ('confirming','confirmed','canceling','unknown')
                           OR (b.status = 'held' AND b.hold_expires_at > now()))
                      AND b.occupied_start < $5 AND b.occupied_end > $4
                      AND ($6::uuid IS NULL OR b.id <> $6)
               ) busy
              ORDER BY starts_at, ends_at",
        )
        .bind(workspace_id)
        .bind(calendar_ref.calendar_connection_id)
        .bind(binding.host_principal_id)
        .bind(range_start)
        .bind(range_end)
        .bind(exclude_booking_id)
        .bind(exclude_change_id)
        .fetch_all(tx.conn())
        .await
        {
            Ok(rows) => rows,
            Err(error) => return database_failure(&error).outcome(),
        };
        if tx.commit().await.is_err() {
            return LocalFailure::DEPENDENCY.outcome();
        }
        CalOutcome::Ok(
            rows.into_iter()
                .map(|row| BusyBlock {
                    start: row.starts_at,
                    end: row.ends_at,
                })
                .collect(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_booking_event_inner(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        if end <= start || title.trim().is_empty() || attendee_email.trim().is_empty() {
            return LocalFailure::INVALID_CALENDAR.outcome();
        }
        let calendar_ref = match validate_scope(workspace_id, host) {
            Ok(reference) => reference,
            Err(error) => return error.outcome(),
        };
        let mut tx = match self.db.begin(workspace_id).await {
            Ok(tx) => tx,
            Err(_) => return LocalFailure::DEPENDENCY.outcome(),
        };
        let binding = match binding_in_tx(&mut tx, workspace_id, calendar_ref, host).await {
            Ok(binding) => binding,
            Err(error) => return error.outcome(),
        };
        if let Err(error) = lock_host(&mut tx, workspace_id, binding.host_principal_id).await {
            return error.outcome();
        }
        let booking = match booking_in_tx(&mut tx, workspace_id, booking_id).await {
            Ok(booking) => booking,
            Err(error) => return error.outcome(),
        };
        if booking.host_principal_id != binding.host_principal_id
            || booking.calendar_connection_id != calendar_ref.calendar_connection_id
        {
            return LocalFailure::BOOKING_NOT_FOUND.outcome();
        }
        let existing = match sqlx::query_as::<_, EventRow>(
            "SELECT provider_event_id, title, attendee_email, starts_at, ends_at,
                    status, revision
               FROM office_calendar_events
              WHERE workspace_id = $1 AND booking_id = $2
              FOR UPDATE",
        )
        .bind(workspace_id)
        .bind(booking_id)
        .fetch_optional(tx.conn())
        .await
        {
            Ok(event) => event,
            Err(error) => return database_failure(&error).outcome(),
        };
        if let Some(event) = existing {
            if event.status == "active"
                && event.title == title
                && event.attendee_email == attendee_email
                && event.starts_at == start
                && event.ends_at == end
            {
                let response = event.response();
                if tx.commit().await.is_err() {
                    return LocalFailure::DEPENDENCY.outcome();
                }
                return CalOutcome::Ok(response);
            }
            return if event.status == "cancelled" {
                LocalFailure::EVENT_CANCELLED.outcome()
            } else {
                LocalFailure::IDEMPOTENCY_CONFLICT.outcome()
            };
        }
        if booking.status != "confirming" {
            return LocalFailure::BOOKING_NOT_FOUND.outcome();
        }
        if booking.title != title
            || booking.starts_at != start
            || booking.ends_at != end
            || booking.attendee_email != attendee_email
        {
            return LocalFailure::BOOKING_NOT_FOUND.outcome();
        }
        let conflict = match has_conflict(
            &mut tx,
            workspace_id,
            binding.host_principal_id,
            booking.occupied_start,
            booking.occupied_end,
            Some(booking_id),
            None,
        )
        .await
        {
            Ok(conflict) => conflict,
            Err(error) => return error.outcome(),
        };
        if conflict {
            return LocalFailure::SLOT_UNAVAILABLE.outcome();
        }
        let event_id = format!("{EVENT_PREFIX}{booking_id}");
        let inserted = sqlx::query_as::<_, EventRow>(
            "INSERT INTO office_calendar_events
                (workspace_id, host_principal_id, calendar_connection_id, booking_id,
                 provider_event_id, title, attendee_email, starts_at, ends_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             RETURNING provider_event_id, title, attendee_email, starts_at, ends_at,
                       status, revision",
        )
        .bind(workspace_id)
        .bind(binding.host_principal_id)
        .bind(calendar_ref.calendar_connection_id)
        .bind(booking_id)
        .bind(event_id)
        .bind(title)
        .bind(attendee_email)
        .bind(start)
        .bind(end)
        .fetch_one(tx.conn())
        .await;
        let event = match inserted {
            Ok(event) => event,
            Err(error) => return database_failure(&error).outcome(),
        };
        let response = event.response();
        if tx.commit().await.is_err() {
            return CalOutcome::Unknown;
        }
        CalOutcome::Ok(response)
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_booking_event_inner(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        change_id: Uuid,
        provider_event_id: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        occupied_start: DateTime<Utc>,
        occupied_end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        if end <= start || occupied_end <= occupied_start {
            return LocalFailure::INVALID_CALENDAR.outcome();
        }
        let calendar_ref = match validate_scope(workspace_id, host) {
            Ok(reference) => reference,
            Err(error) => return error.outcome(),
        };
        if parse_event_id(provider_event_id) != Ok(booking_id) {
            return LocalFailure::INVALID_CALENDAR.outcome();
        }
        let mut tx = match self.db.begin(workspace_id).await {
            Ok(tx) => tx,
            Err(_) => return LocalFailure::DEPENDENCY.outcome(),
        };
        let binding = match binding_in_tx(&mut tx, workspace_id, calendar_ref, host).await {
            Ok(binding) => binding,
            Err(error) => return error.outcome(),
        };
        if let Err(error) = lock_host(&mut tx, workspace_id, binding.host_principal_id).await {
            return error.outcome();
        }
        let booking = match booking_in_tx(&mut tx, workspace_id, booking_id).await {
            Ok(booking) => booking,
            Err(error) => return error.outcome(),
        };
        if booking.host_principal_id != binding.host_principal_id
            || booking.calendar_connection_id != calendar_ref.calendar_connection_id
            || booking.provider_event_id.as_deref() != Some(provider_event_id)
            || booking.title != title
            || !matches!(
                booking.status.as_str(),
                "confirmed" | "unknown" | "canceling"
            )
        {
            return LocalFailure::BOOKING_NOT_FOUND.outcome();
        }
        let change = match booking_change_in_tx(
            &mut tx,
            workspace_id,
            change_id,
            booking_id,
            binding.host_principal_id,
        )
        .await
        {
            Ok(change) => change,
            Err(error) => return error.outcome(),
        };
        if change.kind != "reschedule"
            || change.status != "processing"
            || change.new_occupied_start != Some(occupied_start)
            || change.new_occupied_end != Some(occupied_end)
            || proposed_time(&change, "starts_at") != Some(start)
            || proposed_time(&change, "ends_at") != Some(end)
        {
            return LocalFailure::CHANGE_NOT_FOUND.outcome();
        }
        let event = match sqlx::query_as::<_, EventRow>(
            "SELECT provider_event_id, title, attendee_email, starts_at, ends_at,
                    status, revision
               FROM office_calendar_events
              WHERE workspace_id = $1 AND calendar_connection_id = $2
                AND host_principal_id = $3 AND booking_id = $4
                AND provider_event_id = $5
              FOR UPDATE",
        )
        .bind(workspace_id)
        .bind(calendar_ref.calendar_connection_id)
        .bind(binding.host_principal_id)
        .bind(booking_id)
        .bind(provider_event_id)
        .fetch_optional(tx.conn())
        .await
        {
            Ok(Some(event)) => event,
            Ok(None) => return LocalFailure::EVENT_NOT_FOUND.outcome(),
            Err(error) => return database_failure(&error).outcome(),
        };
        if event.status != "active" {
            return LocalFailure::EVENT_CANCELLED.outcome();
        }
        if event.title == title && event.starts_at == start && event.ends_at == end {
            let response = event.response();
            if tx.commit().await.is_err() {
                return LocalFailure::DEPENDENCY.outcome();
            }
            return CalOutcome::Ok(response);
        }
        let conflict = match has_conflict(
            &mut tx,
            workspace_id,
            binding.host_principal_id,
            occupied_start,
            occupied_end,
            Some(booking_id),
            Some(change_id),
        )
        .await
        {
            Ok(conflict) => conflict,
            Err(error) => return error.outcome(),
        };
        if conflict {
            return LocalFailure::SLOT_UNAVAILABLE.outcome();
        }
        let updated = match sqlx::query_as::<_, EventRow>(
            "UPDATE office_calendar_events
                SET title = $6, starts_at = $7, ends_at = $8,
                    revision = revision + 1, updated_at = now()
              WHERE workspace_id = $1 AND calendar_connection_id = $2
                AND host_principal_id = $3 AND booking_id = $4
                AND provider_event_id = $5 AND status = 'active'
              RETURNING provider_event_id, title, attendee_email, starts_at, ends_at,
                        status, revision",
        )
        .bind(workspace_id)
        .bind(calendar_ref.calendar_connection_id)
        .bind(binding.host_principal_id)
        .bind(booking_id)
        .bind(provider_event_id)
        .bind(title)
        .bind(start)
        .bind(end)
        .fetch_one(tx.conn())
        .await
        {
            Ok(event) => event,
            Err(error) => return database_failure(&error).outcome(),
        };
        let response = updated.response();
        if tx.commit().await.is_err() {
            return CalOutcome::Unknown;
        }
        CalOutcome::Ok(response)
    }

    async fn cancel_booking_event_inner(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        change_id: Uuid,
        provider_event_id: &str,
    ) -> CalOutcome<()> {
        let calendar_ref = match validate_scope(workspace_id, host) {
            Ok(reference) => reference,
            Err(error) => return error.outcome(),
        };
        if parse_event_id(provider_event_id) != Ok(booking_id) {
            return LocalFailure::INVALID_CALENDAR.outcome();
        }
        let mut tx = match self.db.begin(workspace_id).await {
            Ok(tx) => tx,
            Err(_) => return LocalFailure::DEPENDENCY.outcome(),
        };
        let binding = match binding_in_tx(&mut tx, workspace_id, calendar_ref, host).await {
            Ok(binding) => binding,
            Err(error) => return error.outcome(),
        };
        if let Err(error) = lock_host(&mut tx, workspace_id, binding.host_principal_id).await {
            return error.outcome();
        }
        let change = match booking_change_in_tx(
            &mut tx,
            workspace_id,
            change_id,
            booking_id,
            binding.host_principal_id,
        )
        .await
        {
            Ok(change) => change,
            Err(error) => return error.outcome(),
        };
        if change.kind != "cancel"
            || change.status != "processing"
            || change.new_occupied_start.is_some()
            || change.new_occupied_end.is_some()
        {
            return LocalFailure::CHANGE_NOT_FOUND.outcome();
        }
        let booking = match booking_in_tx(&mut tx, workspace_id, booking_id).await {
            Ok(booking) => booking,
            Err(error) => return error.outcome(),
        };
        if booking.host_principal_id != binding.host_principal_id
            || booking.calendar_connection_id != calendar_ref.calendar_connection_id
            || booking.provider_event_id.as_deref() != Some(provider_event_id)
            || !matches!(
                booking.status.as_str(),
                "confirmed" | "canceling" | "canceled" | "unknown"
            )
        {
            return LocalFailure::BOOKING_NOT_FOUND.outcome();
        }
        let status: Option<String> = match sqlx::query_scalar(
            "SELECT status FROM office_calendar_events
              WHERE workspace_id = $1 AND calendar_connection_id = $2
                AND host_principal_id = $3 AND booking_id = $4
                AND provider_event_id = $5
              FOR UPDATE",
        )
        .bind(workspace_id)
        .bind(calendar_ref.calendar_connection_id)
        .bind(binding.host_principal_id)
        .bind(booking_id)
        .bind(provider_event_id)
        .fetch_optional(tx.conn())
        .await
        {
            Ok(status) => status,
            Err(error) => return database_failure(&error).outcome(),
        };
        let Some(status) = status else {
            return LocalFailure::EVENT_NOT_FOUND.outcome();
        };
        if status == "cancelled" {
            if tx.commit().await.is_err() {
                return LocalFailure::DEPENDENCY.outcome();
            }
            return CalOutcome::Ok(());
        }
        if booking.status != "canceling" {
            return LocalFailure::BOOKING_NOT_FOUND.outcome();
        }
        if let Err(error) = sqlx::query(
            "UPDATE office_calendar_events
                SET status = 'cancelled', revision = revision + 1, updated_at = now()
              WHERE workspace_id = $1 AND calendar_connection_id = $2
                AND host_principal_id = $3 AND booking_id = $4
                AND provider_event_id = $5 AND status = 'active'",
        )
        .bind(workspace_id)
        .bind(calendar_ref.calendar_connection_id)
        .bind(binding.host_principal_id)
        .bind(booking_id)
        .bind(provider_event_id)
        .execute(tx.conn())
        .await
        {
            return database_failure(&error).outcome();
        }
        if tx.commit().await.is_err() {
            return CalOutcome::Unknown;
        }
        CalOutcome::Ok(())
    }
}

#[async_trait]
impl CalendarProvider for OfficeCalendarProvider {
    async fn free_busy(
        &self,
        _host: &str,
        _range_start: DateTime<Utc>,
        _range_end: DateTime<Utc>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        LocalFailure::WORKSPACE_REQUIRED.outcome()
    }

    async fn free_busy_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        range_start: DateTime<Utc>,
        range_end: DateTime<Utc>,
        exclude_booking_id: Option<Uuid>,
        exclude_change_id: Option<Uuid>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        self.free_busy_scoped_inner(
            workspace_id,
            host,
            range_start,
            range_end,
            exclude_booking_id,
            exclude_change_id,
        )
        .await
    }

    async fn create_event(
        &self,
        _host: &str,
        _title: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
        _attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        LocalFailure::WORKSPACE_REQUIRED.outcome()
    }

    async fn create_booking_event(
        &self,
        _host: &str,
        _booking_id: Uuid,
        _title: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
        _attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        LocalFailure::WORKSPACE_REQUIRED.outcome()
    }

    async fn create_booking_event_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        self.create_booking_event_inner(
            workspace_id,
            host,
            booking_id,
            title,
            start,
            end,
            attendee_email,
        )
        .await
    }

    async fn update_event(
        &self,
        _host: &str,
        _provider_event_id: &str,
        _title: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        LocalFailure::WORKSPACE_REQUIRED.outcome()
    }

    async fn update_event_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        change_id: Uuid,
        provider_event_id: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        occupied_start: DateTime<Utc>,
        occupied_end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        self.update_booking_event_inner(
            workspace_id,
            host,
            booking_id,
            change_id,
            provider_event_id,
            title,
            start,
            end,
            occupied_start,
            occupied_end,
        )
        .await
    }

    async fn cancel_event(&self, _host: &str, _provider_event_id: &str) -> CalOutcome<()> {
        LocalFailure::WORKSPACE_REQUIRED.outcome()
    }

    async fn cancel_event_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        change_id: Uuid,
        provider_event_id: &str,
    ) -> CalOutcome<()> {
        self.cancel_booking_event_inner(
            workspace_id,
            host,
            booking_id,
            change_id,
            provider_event_id,
        )
        .await
    }
}

/// Routes Office calendar ids to the database adapter before considering the
/// configured legacy adapter. Malformed `office:` ids fail locally and never
/// fall through to a fake or external provider.
pub struct CalendarProviderRouter {
    local: OfficeCalendarProvider,
    fallback: Arc<dyn CalendarProvider>,
}

impl CalendarProviderRouter {
    pub fn new(db: Db, fallback: Arc<dyn CalendarProvider>) -> Self {
        Self {
            local: OfficeCalendarProvider::new(db),
            fallback,
        }
    }

    fn is_local(host: &str) -> bool {
        OfficeCalendarProvider::accepts_calendar_id(host)
    }
}

#[async_trait]
impl CalendarProvider for CalendarProviderRouter {
    async fn free_busy(
        &self,
        host: &str,
        range_start: DateTime<Utc>,
        range_end: DateTime<Utc>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        if Self::is_local(host) {
            self.local.free_busy(host, range_start, range_end).await
        } else {
            self.fallback.free_busy(host, range_start, range_end).await
        }
    }

    async fn free_busy_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        range_start: DateTime<Utc>,
        range_end: DateTime<Utc>,
        exclude_booking_id: Option<Uuid>,
        exclude_change_id: Option<Uuid>,
    ) -> CalOutcome<Vec<BusyBlock>> {
        if Self::is_local(host) {
            self.local
                .free_busy_scoped(
                    workspace_id,
                    host,
                    range_start,
                    range_end,
                    exclude_booking_id,
                    exclude_change_id,
                )
                .await
        } else {
            self.fallback
                .free_busy_scoped(
                    workspace_id,
                    host,
                    range_start,
                    range_end,
                    exclude_booking_id,
                    exclude_change_id,
                )
                .await
        }
    }

    async fn create_event(
        &self,
        host: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        if Self::is_local(host) {
            self.local
                .create_event(host, title, start, end, attendee_email)
                .await
        } else {
            self.fallback
                .create_event(host, title, start, end, attendee_email)
                .await
        }
    }

    async fn create_booking_event(
        &self,
        host: &str,
        booking_id: Uuid,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        if Self::is_local(host) {
            self.local
                .create_booking_event(host, booking_id, title, start, end, attendee_email)
                .await
        } else {
            self.fallback
                .create_booking_event(host, booking_id, title, start, end, attendee_email)
                .await
        }
    }

    async fn create_booking_event_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        attendee_email: &str,
    ) -> CalOutcome<CalEvent> {
        if Self::is_local(host) {
            self.local
                .create_booking_event_scoped(
                    workspace_id,
                    host,
                    booking_id,
                    title,
                    start,
                    end,
                    attendee_email,
                )
                .await
        } else {
            self.fallback
                .create_booking_event_scoped(
                    workspace_id,
                    host,
                    booking_id,
                    title,
                    start,
                    end,
                    attendee_email,
                )
                .await
        }
    }

    async fn update_event(
        &self,
        host: &str,
        provider_event_id: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        if Self::is_local(host) {
            self.local
                .update_event(host, provider_event_id, title, start, end)
                .await
        } else {
            self.fallback
                .update_event(host, provider_event_id, title, start, end)
                .await
        }
    }

    async fn update_event_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        change_id: Uuid,
        provider_event_id: &str,
        title: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
        occupied_start: DateTime<Utc>,
        occupied_end: DateTime<Utc>,
    ) -> CalOutcome<CalEvent> {
        if Self::is_local(host) {
            self.local
                .update_event_scoped(
                    workspace_id,
                    host,
                    booking_id,
                    change_id,
                    provider_event_id,
                    title,
                    start,
                    end,
                    occupied_start,
                    occupied_end,
                )
                .await
        } else {
            self.fallback
                .update_event_scoped(
                    workspace_id,
                    host,
                    booking_id,
                    change_id,
                    provider_event_id,
                    title,
                    start,
                    end,
                    occupied_start,
                    occupied_end,
                )
                .await
        }
    }

    async fn cancel_event(&self, host: &str, provider_event_id: &str) -> CalOutcome<()> {
        if Self::is_local(host) {
            self.local.cancel_event(host, provider_event_id).await
        } else {
            self.fallback.cancel_event(host, provider_event_id).await
        }
    }

    async fn cancel_event_scoped(
        &self,
        workspace_id: Uuid,
        host: &str,
        booking_id: Uuid,
        change_id: Uuid,
        provider_event_id: &str,
    ) -> CalOutcome<()> {
        if Self::is_local(host) {
            self.local
                .cancel_event_scoped(workspace_id, host, booking_id, change_id, provider_event_id)
                .await
        } else {
            self.fallback
                .cancel_event_scoped(workspace_id, host, booking_id, change_id, provider_event_id)
                .await
        }
    }
}
