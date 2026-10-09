//! Database-level booking hold guarantees (T18/T19). The exclusion constraint
//! is the concurrency authority for bookings sharing a host calendar.

mod common;

use chrono::{Duration, Utc};
use infrastructure::db::{Db, Tx};
use infrastructure::testing::{TestWorld, seed_workspace};
use uuid::Uuid;

async fn setup() -> (Db, TestWorld, Uuid, Uuid, Uuid) {
    let db = common::test_db()
        .await
        .expect("database tests require DATABASE_URL");
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let connection: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1,'zoho_calendar','us','calendar-test-ref','active',$2) RETURNING id",
    )
    .bind(world.workspace_id).bind(world.admin.principal_id).fetch_one(tx.conn()).await.unwrap();
    let calendar: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
         VALUES ($1,$2,$3,$4,'active') RETURNING id",
    )
    .bind(world.workspace_id).bind(world.admin.principal_id).bind(connection).bind(format!("calendar-{}", Uuid::new_v4()))
    .fetch_one(tx.conn()).await.unwrap();
    let meeting: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types (workspace_id, host_principal_id, calendar_connection_id, name)
         VALUES ($1,$2,$3,'Test meeting') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let link: Uuid = sqlx::query_scalar(
        "INSERT INTO booking_links (workspace_id, meeting_type_id, host_principal_id, token_hash, max_uses)
         VALUES ($1,$2,$3,$4,100) RETURNING id",
    )
    .bind(world.workspace_id).bind(meeting).bind(world.admin.principal_id)
    .bind(infrastructure::crypto::sha256_bytes(Uuid::new_v4().to_string().as_bytes()).to_vec())
    .fetch_one(tx.conn()).await.unwrap();
    let email: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1,'guest@example.test','guest@example.test') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (db, world, link, email, calendar)
}

struct HoldData {
    link: Uuid,
    email: Uuid,
    start: chrono::DateTime<Utc>,
    end: chrono::DateTime<Utc>,
    occupied_start: chrono::DateTime<Utc>,
    occupied_end: chrono::DateTime<Utc>,
    status: &'static str,
}

async fn insert_booking_hold(
    tx: &mut Tx<'_>,
    world: &TestWorld,
    hold: HoldData,
) -> Result<Uuid, sqlx::Error> {
    let booking: Uuid = sqlx::query_scalar(
        "INSERT INTO bookings (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
             guest_name, guest_timezone, starts_at, ends_at, occupied_start, occupied_end,
             status, hold_expires_at)
         VALUES ($1,$2,$3,$4,'Guest','UTC',$5,$6,$7,$8,$9,
                 CASE WHEN $9='held' THEN now() + interval '5 minutes' ELSE NULL END)
         RETURNING id",
    )
    .bind(world.workspace_id).bind(hold.link).bind(world.admin.principal_id).bind(hold.email)
    .bind(hold.start).bind(hold.end).bind(hold.occupied_start).bind(hold.occupied_end).bind(hold.status)
    .fetch_one(tx.conn()).await?;
    sqlx::query(
        "INSERT INTO booking_slot_holds (workspace_id, host_principal_id, booking_id, starts_at, ends_at, status)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(world.workspace_id).bind(world.admin.principal_id).bind(booking)
    .bind(hold.occupied_start).bind(hold.occupied_end).bind(hold.status)
    .execute(tx.conn()).await?;
    Ok(booking)
}

/// T18: fifty concurrent attempts for the same host interval have exactly
/// one winner. The test exercises PostgreSQL's EXCLUDE constraint directly.
#[tokio::test]
async fn t18_concurrent_overlapping_holds_have_one_winner() {
    let (db, world, link, email, _) = setup().await;
    let start = Utc::now() + Duration::days(5);
    let end = start + Duration::minutes(30);
    let occupied_start = start - Duration::minutes(10);
    let occupied_end = end + Duration::minutes(10);
    let mut tasks = Vec::new();
    for _ in 0..50 {
        let db = db.clone();
        let world_id = world.workspace_id;
        let principal = world.admin.principal_id;
        tasks.push(tokio::spawn(async move {
            // Concurrent EXCLUDE inserts can report 40P01 before a winner
            // commits. Like the API's retryable-conflict contract, retry the
            // entire aborted transaction before counting a rejected hold.
            for attempt in 0..5 {
            let mut tx = db.begin(world_id).await.unwrap();
            let booking: Uuid = sqlx::query_scalar(
                "INSERT INTO bookings (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
                    guest_name, guest_timezone, starts_at, ends_at, occupied_start, occupied_end, status)
                 VALUES ($1,$2,$3,$4,'Guest','UTC',$5,$6,$7,$8,'confirming') RETURNING id",
            )
            .bind(world_id).bind(link).bind(principal).bind(email).bind(start).bind(end).bind(occupied_start).bind(occupied_end)
            .fetch_one(tx.conn()).await.unwrap();
            let result = sqlx::query(
                "INSERT INTO booking_slot_holds (workspace_id,host_principal_id,booking_id,starts_at,ends_at,status)
                 VALUES ($1,$2,$3,$4,$5,'confirming')",
            )
            .bind(world_id).bind(principal).bind(booking).bind(occupied_start).bind(occupied_end)
            .execute(tx.conn()).await;
            match result {
                Ok(_) => { tx.commit().await.unwrap(); return true; }
                Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23P01") => { tx.rollback().await.unwrap(); return false; }
                Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("40P01") && attempt < 4 => { tx.rollback().await.unwrap(); }
                Err(e) => panic!("unexpected hold error: {e}"),
            }
            }
            unreachable!("retry loop returns or panics")
        }));
    }
    let mut successes = 0;
    for result in futures::future::join_all(tasks).await {
        if result.unwrap() {
            successes += 1;
        }
    }
    assert_eq!(successes, 1);
}

/// T19: separate meetings can appear not to overlap while their configured
/// buffers overlap. The authoritative occupied ranges still conflict.
#[tokio::test]
async fn t19_buffer_overlap_blocks_second_hold() {
    let (db, world, link, email, _) = setup().await;
    let base = Utc::now() + Duration::days(8);
    let first_start = base;
    let first_end = base + Duration::minutes(30);
    let second_start = base + Duration::minutes(45);
    let second_end = second_start + Duration::minutes(30);
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    insert_booking_hold(
        &mut tx,
        &world,
        HoldData {
            link,
            email,
            start: first_start,
            end: first_end,
            occupied_start: first_start - Duration::minutes(10),
            occupied_end: first_end + Duration::minutes(10),
            status: "confirming",
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let result = insert_booking_hold(
        &mut tx,
        &world,
        HoldData {
            link,
            email,
            start: second_start,
            end: second_end,
            occupied_start: second_start - Duration::minutes(10),
            occupied_end: second_end + Duration::minutes(10),
            status: "confirming",
        },
    )
    .await;
    assert!(
        matches!(result, Err(sqlx::Error::Database(ref e)) if e.code().as_deref() == Some("23P01"))
    );
    tx.rollback().await.unwrap();
}
