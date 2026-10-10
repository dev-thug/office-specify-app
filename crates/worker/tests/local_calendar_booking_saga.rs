//! Exercise the real WorkerContext calendar adapter through booking
//! confirmation. Only `outreach_local_booking_backend` is accepted.

use chrono::{Duration, Utc};
use contracts::AuthMethod;
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API, ROLE_WORKER},
    jobs,
    providers::CalOutcome,
    testing::{ctx_for, seed_workspace},
};
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn worker_confirms_a_booking_into_the_persistent_office_calendar() {
    let url = std::env::var("LOCAL_CALENDAR_TEST_DATABASE_URL")
        .expect("LOCAL_CALENDAR_TEST_DATABASE_URL must point at the isolated calendar test DB");
    let database = url
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default();
    assert_eq!(database, "outreach_local_booking_backend");
    let owner = Db::connect(&url, None).await.expect("connect owner");
    owner.migrate().await.expect("apply migrations");
    let api_db = Db::connect(&url, Some(ROLE_API))
        .await
        .expect("connect api role");
    let worker_db = Db::connect(&url, Some(ROLE_WORKER))
        .await
        .expect("connect worker role");
    let world = seed_workspace(&api_db).await.unwrap();
    let ctx = ctx_for(&world, &world.admin, "local-calendar-saga-test");

    let (calendar_connection, provider_calendar_id) = {
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let (calendar, created) =
            application::mailboxes::calendar::connect_local_calendar_in_tx(&mut tx, &ctx)
                .await
                .unwrap();
        assert!(created);
        tx.commit().await.unwrap();
        assert!(matches!(ctx.auth, AuthMethod::Session { .. }));
        (calendar.id, calendar.provider_calendar_id)
    };
    let (booking_id, job_id, starts_at, ends_at, guest_email) = {
        let starts_at = Utc::now() + Duration::days(4);
        let ends_at = starts_at + Duration::minutes(30);
        let occupied_start = starts_at - Duration::minutes(10);
        let occupied_end = ends_at + Duration::minutes(10);
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let meeting_id: Uuid = sqlx::query_scalar(
            "INSERT INTO meeting_types
                (workspace_id, host_principal_id, calendar_connection_id, name)
             VALUES ($1, $2, $3, 'Worker local calendar test') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(calendar_connection)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let link_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_links
                (workspace_id, meeting_type_id, host_principal_id, token_hash, max_uses)
             VALUES ($1, $2, $3, $4, 10) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(meeting_id)
        .bind(world.admin.principal_id)
        .bind(infrastructure::crypto::sha256_bytes(
            Uuid::new_v4().to_string().as_bytes(),
        ))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let guest_email = format!("guest-{}@example.test", Uuid::new_v4());
        let guest_email_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
             VALUES ($1, $2, $2) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(&guest_email)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let booking_id: Uuid = sqlx::query_scalar(
            "INSERT INTO bookings
                (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
                 guest_name, guest_timezone, starts_at, ends_at, occupied_start,
                 occupied_end, status)
             VALUES ($1, $2, $3, $4, 'Guest', 'UTC', $5, $6, $7, $8, 'confirming')
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(link_id)
        .bind(world.admin.principal_id)
        .bind(guest_email_id)
        .bind(starts_at)
        .bind(ends_at)
        .bind(occupied_start)
        .bind(occupied_end)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO booking_slot_holds
                (workspace_id, host_principal_id, booking_id, starts_at, ends_at, status)
             VALUES ($1, $2, $3, $4, $5, 'confirming')",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(booking_id)
        .bind(occupied_start)
        .bind(occupied_end)
        .execute(tx.conn())
        .await
        .unwrap();
        let job_id = jobs::enqueue(
            &mut tx,
            "booking.confirm",
            1,
            json!({"booking_id": booking_id}),
            Utc::now(),
            Some(&format!("booking_confirm:{booking_id}")),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (booking_id, job_id, starts_at, ends_at, guest_email)
    };

    let context = worker::WorkerContext::from_config(worker_db.clone(), &Config::test());
    application::booking::confirm_booking(
        &worker_db,
        context.calendar.as_ref(),
        world.workspace_id,
        booking_id,
        job_id,
    )
    .await
    .unwrap();

    let mut tx = worker_db.begin(world.workspace_id).await.unwrap();
    let (status, event_id): (String, String) = sqlx::query_as(
        "SELECT status, provider_event_id FROM bookings WHERE workspace_id=$1 AND id=$2",
    )
    .bind(world.workspace_id)
    .bind(booking_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(status, "confirmed");
    assert_eq!(event_id, format!("office-event:{booking_id}"));
    let event_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM office_calendar_events
          WHERE workspace_id=$1 AND booking_id=$2 AND status='active'",
    )
    .bind(world.workspace_id)
    .bind(booking_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(event_count, 1);
    tx.commit().await.unwrap();

    let busy = context
        .calendar
        .free_busy_scoped(
            world.workspace_id,
            &provider_calendar_id,
            starts_at - Duration::minutes(10),
            ends_at + Duration::minutes(10),
            None,
            None,
        )
        .await;
    assert!(matches!(busy, CalOutcome::Ok(ref blocks) if !blocks.is_empty()));

    let reschedule_start = starts_at + Duration::days(2);
    let reschedule_end = reschedule_start + Duration::minutes(30);
    let reschedule_occupied_start = reschedule_start - Duration::minutes(10);
    let reschedule_occupied_end = reschedule_end + Duration::minutes(10);
    let (reschedule_id, reschedule_job_id) = {
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let change_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_changes
                (workspace_id, booking_id, host_principal_id, kind, proposed_data,
                 requested_by, status, new_occupied_start, new_occupied_end)
             VALUES ($1, $2, $3, 'reschedule', $4, $5, 'processing', $6, $7)
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(booking_id)
        .bind(world.admin.principal_id)
        .bind(json!({
            "starts_at": reschedule_start.to_rfc3339(),
            "ends_at": reschedule_end.to_rfc3339(),
            "timezone": "UTC",
            "expanded_existing_hold": false
        }))
        .bind(world.admin.principal_id)
        .bind(reschedule_occupied_start)
        .bind(reschedule_occupied_end)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO booking_slot_holds
                (workspace_id, host_principal_id, booking_change_id, starts_at, ends_at, status)
             VALUES ($1, $2, $3, $4, $5, 'confirming')",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(change_id)
        .bind(reschedule_occupied_start)
        .bind(reschedule_occupied_end)
        .execute(tx.conn())
        .await
        .unwrap();
        let job_id = jobs::enqueue(
            &mut tx,
            "booking.apply_change",
            1,
            json!({"booking_change_id": change_id}),
            Utc::now(),
            Some(&format!("booking_change:{change_id}")),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (change_id, job_id)
    };
    application::booking::apply_booking_change(
        &worker_db,
        context.calendar.as_ref(),
        world.workspace_id,
        reschedule_id,
        reschedule_job_id,
    )
    .await
    .unwrap();
    let mut tx = worker_db.begin(world.workspace_id).await.unwrap();
    let (rescheduled_status, saved_start): (String, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT status, starts_at FROM bookings WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(booking_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let event_start: chrono::DateTime<Utc> = sqlx::query_scalar(
        "SELECT starts_at FROM office_calendar_events WHERE workspace_id=$1 AND booking_id=$2",
    )
    .bind(world.workspace_id)
    .bind(booking_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(rescheduled_status, "confirmed");
    assert_eq!(saved_start, reschedule_start);
    assert_eq!(event_start, reschedule_start);
    tx.commit().await.unwrap();

    let (cancel_id, cancel_job_id) = {
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let change_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_changes
                (workspace_id, booking_id, host_principal_id, kind, proposed_data,
                 requested_by, status)
             VALUES ($1, $2, $3, 'cancel', '{}'::jsonb, $4, 'processing')
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(booking_id)
        .bind(world.admin.principal_id)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query("UPDATE bookings SET status='canceling' WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(booking_id)
            .execute(tx.conn())
            .await
            .unwrap();
        sqlx::query(
            "UPDATE booking_slot_holds SET status='confirming'
              WHERE workspace_id=$1 AND booking_id=$2 AND status='confirmed'",
        )
        .bind(world.workspace_id)
        .bind(booking_id)
        .execute(tx.conn())
        .await
        .unwrap();
        let job_id = jobs::enqueue(
            &mut tx,
            "booking.apply_change",
            1,
            json!({"booking_change_id": change_id}),
            Utc::now(),
            Some(&format!("booking_change:{change_id}")),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (change_id, job_id)
    };
    application::booking::apply_booking_change(
        &worker_db,
        context.calendar.as_ref(),
        world.workspace_id,
        cancel_id,
        cancel_job_id,
    )
    .await
    .unwrap();
    let mut tx = worker_db.begin(world.workspace_id).await.unwrap();
    let booking_status: String =
        sqlx::query_scalar("SELECT status FROM bookings WHERE workspace_id=$1 AND id=$2")
            .bind(world.workspace_id)
            .bind(booking_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    let event_status: String = sqlx::query_scalar(
        "SELECT status FROM office_calendar_events WHERE workspace_id=$1 AND booking_id=$2",
    )
    .bind(world.workspace_id)
    .bind(booking_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(booking_status, "canceled");
    assert_eq!(event_status, "cancelled");
    tx.commit().await.unwrap();
    let _ = guest_email;
}
