//! PostgreSQL-backed Office calendar provider coverage. These tests require
//! the dedicated `outreach_local_booking_backend` database only.

use chrono::{Duration, Utc};
use infrastructure::{
    db::{Db, ROLE_API, ROLE_WORKER, Tx},
    providers::{CalOutcome, CalendarProvider, OfficeCalendarProvider},
    testing::{TestWorld, seed_workspace},
};
use uuid::Uuid;

struct TestDb {
    api: Db,
    worker: Db,
}

async fn setup_db() -> TestDb {
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
    TestDb {
        api: Db::connect(&url, Some(ROLE_API))
            .await
            .expect("connect api role"),
        worker: Db::connect(&url, Some(ROLE_WORKER))
            .await
            .expect("connect worker role"),
    }
}

async fn local_calendar(db: &Db, world: &TestWorld) -> (Uuid, String) {
    let connection_id = Uuid::new_v4();
    let calendar_id = Uuid::new_v4();
    let provider_calendar_id = format!("office:{}:{}", world.workspace_id, calendar_id);
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "INSERT INTO integration_connections
            (id, workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1, $2, 'office_calendar', 'local', '', 'active', $3)",
    )
    .bind(connection_id)
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO calendar_connections
            (id, workspace_id, provider, host_principal_id, connection_id,
             provider_calendar_id, status)
         VALUES ($1, $2, 'office_calendar', $3, $4, $5, 'active')",
    )
    .bind(calendar_id)
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(connection_id)
    .bind(&provider_calendar_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (calendar_id, provider_calendar_id)
}

async fn booking_context(tx: &mut Tx<'_>, world: &TestWorld, calendar_id: Uuid) -> (Uuid, Uuid) {
    let meeting_id: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types (workspace_id, host_principal_id, calendar_connection_id, name)
         VALUES ($1, $2, $3, 'Local booking test') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let link_id: Uuid = sqlx::query_scalar(
        "INSERT INTO booking_links (workspace_id, meeting_type_id, host_principal_id, token_hash, max_uses)
         VALUES ($1, $2, $3, $4, 10) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(meeting_id)
    .bind(world.admin.principal_id)
    .bind(infrastructure::crypto::sha256_bytes(Uuid::new_v4().to_string().as_bytes()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let email_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1, $2, $2) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("guest-{}@example.test", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    (link_id, email_id)
}

async fn insert_booking(
    tx: &mut Tx<'_>,
    world: &TestWorld,
    link_id: Uuid,
    email_id: Uuid,
    start: chrono::DateTime<Utc>,
    status: &str,
    hold_status: Option<&str>,
) -> Uuid {
    let end = start + Duration::minutes(30);
    let occupied_start = start - Duration::minutes(10);
    let occupied_end = end + Duration::minutes(10);
    let booking_id: Uuid = sqlx::query_scalar(
        "INSERT INTO bookings
            (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
             guest_name, guest_timezone, starts_at, ends_at, occupied_start, occupied_end, status)
         VALUES ($1, $2, $3, $4, 'Guest', 'UTC', $5, $6, $7, $8, $9)
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(link_id)
    .bind(world.admin.principal_id)
    .bind(email_id)
    .bind(start)
    .bind(end)
    .bind(occupied_start)
    .bind(occupied_end)
    .bind(status)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    if let Some(hold_status) = hold_status {
        sqlx::query(
            "INSERT INTO booking_slot_holds
                (workspace_id, host_principal_id, booking_id, starts_at, ends_at, status)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(booking_id)
        .bind(occupied_start)
        .bind(occupied_end)
        .bind(hold_status)
        .execute(tx.conn())
        .await
        .unwrap();
    }
    booking_id
}

#[tokio::test]
async fn local_events_are_scoped_idempotent_conflict_safe_and_durable() {
    let dbs = setup_db().await;
    let world_a = seed_workspace(&dbs.api).await.unwrap();
    let world_b = seed_workspace(&dbs.api).await.unwrap();
    let (calendar_a, calendar_ref_a) = local_calendar(&dbs.api, &world_a).await;
    let (_calendar_b, calendar_ref_b) = local_calendar(&dbs.api, &world_b).await;
    let start = Utc::now() + Duration::days(4);
    let (link_a, booking_a, email_a_text) = {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        let (link, email) = booking_context(&mut tx, &world_a, calendar_a).await;
        let email_text: String =
            sqlx::query_scalar("SELECT address_original FROM email_addresses WHERE id=$1")
                .bind(email)
                .fetch_one(tx.conn())
                .await
                .unwrap();
        let booking = insert_booking(
            &mut tx,
            &world_a,
            link,
            email,
            start,
            "confirming",
            Some("confirming"),
        )
        .await;
        tx.commit().await.unwrap();
        (link, booking, email_text)
    };
    let provider = OfficeCalendarProvider::new(dbs.worker.clone());
    let range_start = start - Duration::minutes(10);
    let range_end = start + Duration::minutes(40);
    let all_busy = provider
        .free_busy_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            range_start,
            range_end,
            None,
            None,
        )
        .await;
    assert!(matches!(all_busy, CalOutcome::Ok(ref blocks) if blocks.len() == 1));
    let own_hold_excluded = provider
        .free_busy_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            range_start,
            range_end,
            Some(booking_a),
            None,
        )
        .await;
    assert!(matches!(own_hold_excluded, CalOutcome::Ok(ref blocks) if blocks.is_empty()));

    let event = provider
        .create_booking_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            "Local booking test",
            start,
            start + Duration::minutes(30),
            &email_a_text,
        )
        .await;
    let event = match event {
        CalOutcome::Ok(event) => event,
        other => panic!("expected persisted local event, got {other:?}"),
    };
    assert_eq!(event.provider_event_id, format!("office-event:{booking_a}"));
    let retry = provider
        .create_booking_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            "Local booking test",
            start,
            start + Duration::minutes(30),
            &email_a_text,
        )
        .await;
    assert!(
        matches!(retry, CalOutcome::Ok(ref retry) if retry.provider_event_id == event.provider_event_id)
    );
    let changed_retry = provider
        .create_booking_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            "Changed title",
            start,
            start + Duration::minutes(30),
            &email_a_text,
        )
        .await;
    assert!(
        matches!(changed_retry, CalOutcome::Failed { ref code, .. } if code == "IDEMPOTENCY_CONFLICT")
    );

    let (second_booking, second_attendee) = {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        let (_link, email) = booking_context(&mut tx, &world_a, calendar_a).await;
        let attendee: String = sqlx::query_scalar(
            "SELECT address_original FROM email_addresses WHERE workspace_id=$1 AND id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(email)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let booking =
            insert_booking(&mut tx, &world_a, link_a, email, start, "confirming", None).await;
        tx.commit().await.unwrap();
        (booking, attendee)
    };
    let overlap = provider
        .create_booking_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            second_booking,
            "Local booking test",
            start,
            start + Duration::minutes(30),
            &second_attendee,
        )
        .await;
    assert!(matches!(overlap, CalOutcome::Failed { ref code, .. } if code == "SLOT_UNAVAILABLE"));

    let other_start = start + Duration::days(5);
    let (other_booking, other_attendee) = {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        let attendee = format!("other-{}@example.test", Uuid::new_v4());
        let email: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
             VALUES ($1, $2, $2) RETURNING id",
        )
        .bind(world_a.workspace_id)
        .bind(&attendee)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let booking = insert_booking(
            &mut tx,
            &world_a,
            link_a,
            email,
            other_start,
            "confirming",
            Some("confirming"),
        )
        .await;
        tx.commit().await.unwrap();
        (booking, attendee)
    };
    let other_event = provider
        .create_booking_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            other_booking,
            "Local booking test",
            other_start,
            other_start + Duration::minutes(30),
            &other_attendee,
        )
        .await;
    let other_event = match other_event {
        CalOutcome::Ok(event) => event,
        other => panic!("expected second local event, got {other:?}"),
    };
    {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE bookings SET status='confirmed', provider_event_id=$3,
                    provider_revision='1' WHERE workspace_id=$1 AND id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(other_booking)
        .bind(&other_event.provider_event_id)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE booking_slot_holds SET status='confirmed'
              WHERE workspace_id=$1 AND booking_id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(other_booking)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let forged_calendar = provider
        .free_busy_scoped(
            world_a.workspace_id,
            &format!("office:{}:{}", world_a.workspace_id, Uuid::new_v4()),
            range_start,
            range_end,
            None,
            None,
        )
        .await;
    assert!(matches!(forged_calendar, CalOutcome::Failed { .. }));
    let cross_workspace = provider
        .free_busy_scoped(
            world_b.workspace_id,
            &calendar_ref_a,
            range_start,
            range_end,
            None,
            None,
        )
        .await;
    assert!(matches!(cross_workspace, CalOutcome::Failed { .. }));
    let forged_event = provider
        .cancel_event_scoped(
            world_b.workspace_id,
            &calendar_ref_b,
            booking_a,
            Uuid::new_v4(),
            &event.provider_event_id,
        )
        .await;
    assert!(matches!(forged_event, CalOutcome::Failed { .. }));

    let later_start = start + Duration::days(1);
    let later_end = later_start + Duration::minutes(30);
    let later_occupied_start = later_start - Duration::minutes(10);
    let later_occupied_end = later_end + Duration::minutes(10);
    let (other_reschedule_id, other_cancel_id) = {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        let reschedule_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_changes
                (workspace_id, booking_id, host_principal_id, kind, proposed_data,
                 requested_by, status, new_occupied_start, new_occupied_end)
             VALUES ($1, $2, $3, 'reschedule', $4, $5, 'processing', $6, $7)
             RETURNING id",
        )
        .bind(world_a.workspace_id)
        .bind(other_booking)
        .bind(world_a.admin.principal_id)
        .bind(serde_json::json!({
            "starts_at": later_start.to_rfc3339(),
            "ends_at": later_end.to_rfc3339(),
            "timezone": "UTC"
        }))
        .bind(world_a.admin.principal_id)
        .bind(later_occupied_start)
        .bind(later_occupied_end)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO booking_slot_holds
                (workspace_id, host_principal_id, booking_change_id, starts_at, ends_at, status)
             VALUES ($1, $2, $3, $4, $5, 'confirming')",
        )
        .bind(world_a.workspace_id)
        .bind(world_a.admin.principal_id)
        .bind(reschedule_id)
        .bind(later_occupied_start)
        .bind(later_occupied_end)
        .execute(tx.conn())
        .await
        .unwrap();
        let cancel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_changes
                (workspace_id, booking_id, host_principal_id, kind, proposed_data,
                 requested_by, status)
             VALUES ($1, $2, $3, 'cancel', '{}'::jsonb, $4, 'processing')
             RETURNING id",
        )
        .bind(world_a.workspace_id)
        .bind(other_booking)
        .bind(world_a.admin.principal_id)
        .bind(world_a.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (reschedule_id, cancel_id)
    };
    let event_id = event.provider_event_id.clone();
    {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE bookings SET status='confirmed', provider_event_id=$3,
                    provider_revision='1' WHERE workspace_id=$1 AND id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(booking_a)
        .bind(&event_id)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE booking_slot_holds SET status='confirmed'
              WHERE workspace_id=$1 AND booking_id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(booking_a)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let forged_reschedule = provider
        .update_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            other_reschedule_id,
            &event_id,
            "Local booking test",
            later_start,
            later_end,
            later_occupied_start,
            later_occupied_end,
        )
        .await;
    assert!(
        matches!(forged_reschedule, CalOutcome::Failed { ref code, .. } if code == "BOOKING_CHANGE_NOT_FOUND")
    );
    let forged_cancel = provider
        .cancel_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            other_cancel_id,
            &event_id,
        )
        .await;
    assert!(
        matches!(forged_cancel, CalOutcome::Failed { ref code, .. } if code == "BOOKING_CHANGE_NOT_FOUND")
    );
    let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
    let unchanged_start: chrono::DateTime<Utc> = sqlx::query_scalar(
        "SELECT starts_at FROM office_calendar_events WHERE workspace_id=$1 AND booking_id=$2",
    )
    .bind(world_a.workspace_id)
    .bind(booking_a)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(unchanged_start, start);
    {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE booking_slot_holds SET status='released'
              WHERE workspace_id=$1 AND booking_change_id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(other_reschedule_id)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query("UPDATE booking_changes SET status='failed' WHERE workspace_id=$1 AND id=$2")
            .bind(world_a.workspace_id)
            .bind(other_reschedule_id)
            .execute(tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    let reschedule_id = {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        let change_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_changes
                (workspace_id, booking_id, host_principal_id, kind, proposed_data,
                 requested_by, status, new_occupied_start, new_occupied_end)
             VALUES ($1, $2, $3, 'reschedule', $4, $5, 'processing', $6, $7)
             RETURNING id",
        )
        .bind(world_a.workspace_id)
        .bind(booking_a)
        .bind(world_a.admin.principal_id)
        .bind(serde_json::json!({
            "starts_at": later_start.to_rfc3339(),
            "ends_at": later_end.to_rfc3339(),
            "timezone": "UTC"
        }))
        .bind(world_a.admin.principal_id)
        .bind(later_occupied_start)
        .bind(later_occupied_end)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO booking_slot_holds
                (workspace_id, host_principal_id, booking_change_id, starts_at, ends_at, status)
             VALUES ($1, $2, $3, $4, $5, 'confirming')",
        )
        .bind(world_a.workspace_id)
        .bind(world_a.admin.principal_id)
        .bind(change_id)
        .bind(later_occupied_start)
        .bind(later_occupied_end)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        change_id
    };
    let moved = provider
        .update_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            reschedule_id,
            &event_id,
            "Local booking test",
            later_start,
            later_end,
            later_start - Duration::minutes(10),
            later_end + Duration::minutes(10),
        )
        .await;
    assert!(matches!(moved, CalOutcome::Ok(ref moved) if moved.start == later_start));
    let moved_retry = provider
        .update_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            reschedule_id,
            &event_id,
            "Local booking test",
            later_start,
            later_end,
            later_occupied_start,
            later_occupied_end,
        )
        .await;
    assert!(
        matches!(moved_retry, CalOutcome::Ok(ref moved) if moved.start == later_start && moved.revision == "2")
    );
    let cancel_id = {
        let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
        sqlx::query(
            "UPDATE booking_slot_holds SET status='released', expires_at=NULL
              WHERE workspace_id=$1 AND booking_id=$2 AND status='confirmed'",
        )
        .bind(world_a.workspace_id)
        .bind(booking_a)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE booking_slot_holds
                SET booking_id=$3, booking_change_id=NULL, starts_at=$4, ends_at=$5,
                    status='confirming'
              WHERE workspace_id=$1 AND booking_change_id=$2 AND status='confirming'",
        )
        .bind(world_a.workspace_id)
        .bind(reschedule_id)
        .bind(booking_a)
        .bind(later_occupied_start)
        .bind(later_occupied_end)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE bookings SET status='canceling', starts_at=$3, ends_at=$4,
                    occupied_start=$5, occupied_end=$6, provider_revision='2'
              WHERE workspace_id=$1 AND id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(booking_a)
        .bind(later_start)
        .bind(later_end)
        .bind(later_occupied_start)
        .bind(later_occupied_end)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE booking_slot_holds SET status='confirming'
              WHERE workspace_id=$1 AND booking_id=$2 AND status='confirmed'",
        )
        .bind(world_a.workspace_id)
        .bind(booking_a)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE booking_changes SET status='completed' WHERE workspace_id=$1 AND id=$2",
        )
        .bind(world_a.workspace_id)
        .bind(reschedule_id)
        .execute(tx.conn())
        .await
        .unwrap();
        let cancel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_changes
                (workspace_id, booking_id, host_principal_id, kind, proposed_data,
                 requested_by, status)
             VALUES ($1, $2, $3, 'cancel', '{}'::jsonb, $4, 'processing')
             RETURNING id",
        )
        .bind(world_a.workspace_id)
        .bind(booking_a)
        .bind(world_a.admin.principal_id)
        .bind(world_a.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        cancel_id
    };
    let canceled = provider
        .cancel_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            cancel_id,
            &event_id,
        )
        .await;
    assert!(matches!(canceled, CalOutcome::Ok(())));
    let cancel_retry = provider
        .cancel_event_scoped(
            world_a.workspace_id,
            &calendar_ref_a,
            booking_a,
            cancel_id,
            &event_id,
        )
        .await;
    assert!(matches!(cancel_retry, CalOutcome::Ok(())));
    let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
    let reactivate = sqlx::query(
        "UPDATE office_calendar_events SET status='active'
          WHERE workspace_id=$1 AND booking_id=$2",
    )
    .bind(world_a.workspace_id)
    .bind(booking_a)
    .execute(tx.conn())
    .await;
    assert!(matches!(
        reactivate,
        Err(sqlx::Error::Database(ref error)) if error.code().as_deref() == Some("23514")
    ));
    tx.rollback().await.unwrap();
    let mut tx = dbs.api.begin(world_a.workspace_id).await.unwrap();
    let event_status: String = sqlx::query_scalar(
        "SELECT status FROM office_calendar_events WHERE workspace_id=$1 AND booking_id=$2",
    )
    .bind(world_a.workspace_id)
    .bind(booking_a)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(event_status, "cancelled");
}
