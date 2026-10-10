//! Public booking-manage response fields are intentionally limited to data
//! already shown on the public booking link. Uses the isolated local test DB.

use api::{AppState, testing::TestApp};
use chrono::{Duration, Utc};
use infrastructure::{
    config::Config,
    db::{Db, ROLE_API},
    testing::seed_workspace,
};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

#[tokio::test]
async fn public_manage_booking_includes_only_public_meeting_and_host_names() {
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
    let db = Db::connect(&url, Some(ROLE_API))
        .await
        .expect("connect api role");
    let world = seed_workspace(&db).await.unwrap();
    let db = db.with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db.clone(),
        Arc::new(Config::test()),
        None,
    ))
    .await;

    let (booking_id, manage_token) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let calendar_integration: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id, provider, region, secret_ref, status, connected_by)
             VALUES ($1, 'zoho_calendar', 'us', 'test-calendar-ref', 'active', $2)
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let calendar: Uuid = sqlx::query_scalar(
            "INSERT INTO calendar_connections
                (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
             VALUES ($1, $2, $3, 'public-manage-test-calendar', 'active') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(calendar_integration)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let meeting_id: Uuid = sqlx::query_scalar(
            "INSERT INTO meeting_types
                (workspace_id, host_principal_id, calendar_connection_id, name)
             VALUES ($1, $2, $3, 'Quarterly planning call') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(calendar)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let booking_link_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_links
                (workspace_id, meeting_type_id, host_principal_id, token_hash, max_uses)
             VALUES ($1, $2, $3, $4, 5) RETURNING id",
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
        let guest_email_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
             VALUES ($1, 'private-guest@example.test', 'private-guest@example.test')
             RETURNING id",
        )
        .bind(world.workspace_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let start = Utc::now() + Duration::days(3);
        let booking_id: Uuid = sqlx::query_scalar(
            "INSERT INTO bookings
                (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
                 guest_name, guest_timezone, starts_at, ends_at, occupied_start,
                 occupied_end, status, provider_event_id)
             VALUES ($1, $2, $3, $4, 'Guest Name', 'UTC', $5, $6, $5, $6,
                     'confirmed', 'private-provider-event')
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(booking_link_id)
        .bind(world.admin.principal_id)
        .bind(guest_email_id)
        .bind(start)
        .bind(start + Duration::minutes(30))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let manage_token = infrastructure::crypto::generate_token();
        sqlx::query(
            "INSERT INTO booking_manage_tokens (workspace_id, booking_id, token_hash, expires_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(world.workspace_id)
        .bind(booking_id)
        .bind(infrastructure::crypto::sha256_bytes(manage_token.as_bytes()).to_vec())
        .bind(Utc::now() + Duration::days(7))
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (booking_id, manage_token)
    };

    let response = app
        .get(&format!("/public/v1/bookings/{manage_token}"))
        .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    let data = body.get("data").expect("Envelope data");
    assert_eq!(data["id"], booking_id.to_string());
    assert_eq!(data["meeting_name"], "Quarterly planning call");
    assert_eq!(data["host_display_name"], "Admin");
    assert!(data.get("guest_email").is_none());
    assert!(data.get("provider_event_id").is_none());
    assert!(data.get("calendar_connection_id").is_none());
}
