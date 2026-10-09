//! Booking worker saga coverage: a timeout is `unknown` with slot retention
//! (T21); only stale `held` rows expire (T22).

mod common;

use chrono::{Duration, Utc};
use infrastructure::db::{Db, Tx};
use infrastructure::jobs;
use infrastructure::providers::fake::{FakeMailProvider, ScriptedCalOutcome};
use infrastructure::testing::{TestWorld, seed_workspace};
use std::sync::Arc;
use uuid::Uuid;

async fn setup() -> (Db, TestWorld, Uuid, Uuid) {
    let worker_db = common::test_db().await.expect("DATABASE_URL is required");
    let api_db = common::seed_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let integration: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1,'zoho_calendar','us','calendar-test-ref','active',$2) RETURNING id",
    )
    .bind(world.workspace_id).bind(world.admin.principal_id).fetch_one(tx.conn()).await.unwrap();
    let calendar: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
         VALUES ($1,$2,$3,$4,'active') RETURNING id",
    )
    .bind(world.workspace_id).bind(world.admin.principal_id).bind(integration).bind(format!("calendar-{}", Uuid::new_v4()))
    .fetch_one(tx.conn()).await.unwrap();
    let meeting: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types (workspace_id, host_principal_id, calendar_connection_id, name)
         VALUES ($1,$2,$3,'T21 meeting') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let link: Uuid = sqlx::query_scalar(
        "INSERT INTO booking_links (workspace_id, meeting_type_id, host_principal_id, token_hash, max_uses)
         VALUES ($1,$2,$3,$4,10) RETURNING id",
    )
    .bind(world.workspace_id).bind(meeting).bind(world.admin.principal_id)
    .bind(infrastructure::crypto::sha256_bytes(Uuid::new_v4().to_string().as_bytes()).to_vec())
    .fetch_one(tx.conn()).await.unwrap();
    let email: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id,address_original,normalized_key)
         VALUES ($1,'guest@example.test','guest@example.test') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (worker_db, world, link, email)
}

async fn insert_booking(
    tx: &mut Tx<'_>,
    world: &TestWorld,
    link: Uuid,
    email: Uuid,
    status: &str,
    start: chrono::DateTime<Utc>,
    hold_expired: bool,
) -> Uuid {
    let end = start + Duration::minutes(30);
    let occupied_start = start - Duration::minutes(10);
    let occupied_end = end + Duration::minutes(10);
    let hold_expiry = if hold_expired {
        Some(Utc::now() - Duration::minutes(1))
    } else {
        None
    };
    let booking: Uuid = sqlx::query_scalar(
        "INSERT INTO bookings (workspace_id,booking_link_id,host_principal_id,guest_email_address_id,
            guest_name,guest_timezone,starts_at,ends_at,occupied_start,occupied_end,status,hold_expires_at)
         VALUES ($1,$2,$3,$4,'Guest','UTC',$5,$6,$7,$8,$9,$10) RETURNING id",
    )
    .bind(world.workspace_id).bind(link).bind(world.admin.principal_id).bind(email)
    .bind(start).bind(end).bind(occupied_start).bind(occupied_end).bind(status).bind(hold_expiry)
    .fetch_one(tx.conn()).await.unwrap();
    sqlx::query(
        "INSERT INTO booking_slot_holds (workspace_id,host_principal_id,booking_id,starts_at,ends_at,status,expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(world.workspace_id).bind(world.admin.principal_id).bind(booking).bind(occupied_start).bind(occupied_end).bind(status).bind(hold_expiry)
    .execute(tx.conn()).await.unwrap();
    booking
}

/// T21: a create-event timeout may have written a provider event. The saga
/// stores `unknown`, keeps the occupied hold, completes the job and never
/// creates a second event on a later runner pass.
#[tokio::test]
async fn t21_calendar_create_unknown_retains_hold_and_is_not_retried() {
    let (db, world, link, email) = setup().await;
    let start = Utc::now() + Duration::days(4);
    let (booking_id, job_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let id = insert_booking(&mut tx, &world, link, email, "confirming", start, false).await;
        let job = jobs::enqueue(
            &mut tx,
            "booking.confirm",
            1,
            serde_json::json!({"booking_id":id}),
            Utc::now(),
            Some(&format!("booking_confirm:{id}")),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (id, job)
    };

    let calendar = Arc::new(FakeMailProvider::new());
    calendar.script_calendar_write(ScriptedCalOutcome::Unknown);
    application::booking::confirm_booking(
        &db,
        calendar.as_ref(),
        world.workspace_id,
        booking_id,
        job_id,
    )
    .await
    .unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM bookings WHERE id=$1")
        .bind(booking_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    let hold: String =
        sqlx::query_scalar("SELECT status FROM booking_slot_holds WHERE booking_id=$1")
            .bind(booking_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(status, "unknown");
    assert_eq!(hold, "unknown");
    assert!(
        calendar.calendar_events().is_empty(),
        "unknown timeout is not assumed to have created an inspectable event"
    );
    application::booking::confirm_booking(
        &db,
        calendar.as_ref(),
        world.workspace_id,
        booking_id,
        job_id,
    )
    .await
    .unwrap();
    assert!(
        calendar.calendar_events().is_empty(),
        "unknown booking is never submitted a second time"
    );
}

/// T22: expiry only releases a `held` booking. A `confirming` hold can be
/// associated with an in-flight calendar request and must remain occupied.
#[tokio::test]
async fn t22_hold_expiry_leaves_confirming_booking_occupied() {
    let (db, world, link, email) = setup().await;
    let (held_id, confirming_id, job_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let held = insert_booking(
            &mut tx,
            &world,
            link,
            email,
            "held",
            Utc::now() + Duration::days(7),
            true,
        )
        .await;
        let confirming = insert_booking(
            &mut tx,
            &world,
            link,
            email,
            "confirming",
            Utc::now() + Duration::days(8),
            true,
        )
        .await;
        let job = jobs::enqueue(
            &mut tx,
            "booking.expire_holds",
            1,
            serde_json::json!({}),
            Utc::now(),
            Some(&format!("booking_expire_test:{}", Uuid::new_v4())),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (held, confirming, job)
    };
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    application::booking::expire_holds(&mut tx, job_id)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let held_status: String = sqlx::query_scalar("SELECT status FROM bookings WHERE id=$1")
        .bind(held_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    let held_slot: String =
        sqlx::query_scalar("SELECT status FROM booking_slot_holds WHERE booking_id=$1")
            .bind(held_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let confirming_status: String = sqlx::query_scalar("SELECT status FROM bookings WHERE id=$1")
        .bind(confirming_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    let confirming_slot: String =
        sqlx::query_scalar("SELECT status FROM booking_slot_holds WHERE booking_id=$1")
            .bind(confirming_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(held_status, "expired");
    assert_eq!(held_slot, "expired");
    assert_eq!(confirming_status, "confirming");
    assert_eq!(confirming_slot, "confirming");
}
