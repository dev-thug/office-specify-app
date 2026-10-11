//! Booking HTTP contracts: idempotent public create/manage capabilities and
//! authorized post-meeting attendance with optimistic locking.

use std::sync::Arc;

use api::AppState;
use api::testing::TestApp;
use chrono::{Duration, Utc};
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{TEST_PASSWORD, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

async fn setup() -> (TestApp, TestWorld) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let db = db.with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await;
    (app, world)
}

struct BookingFixture {
    token: String,
    email_id: Uuid,
    calendar_connection_id: Uuid,
}

async fn create_fixture(app: &TestApp, world: &TestWorld, max_uses: i32) -> BookingFixture {
    let raw_token = infrastructure::crypto::generate_token();
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET settings = settings || '{\"booking_privacy_notice_version\":\"booking-privacy-v3\"}'::jsonb WHERE id = $1")
        .bind(world.workspace_id).execute(tx.conn()).await.unwrap();
    let cal_integration: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections (workspace_id,provider,region,secret_ref,status,connected_by)
         VALUES ($1,'zoho_calendar','us','booking-test-ref','active',$2) RETURNING id",
    ).bind(world.workspace_id).bind(world.admin.principal_id).fetch_one(tx.conn()).await.unwrap();
    let cal: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections (workspace_id,host_principal_id,connection_id,provider_calendar_id,status)
         VALUES ($1,$2,$3,$4,'active') RETURNING id",
    ).bind(world.workspace_id).bind(world.admin.principal_id).bind(cal_integration)
        .bind(format!("calendar-{}", Uuid::new_v4())).fetch_one(tx.conn()).await.unwrap();
    let meeting: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types (workspace_id,host_principal_id,calendar_connection_id,name,
            duration_minutes,buffer_before_minutes,buffer_after_minutes,minimum_notice_minutes,booking_horizon_days)
         VALUES ($1,$2,$3,'API test',30,0,0,0,30) RETURNING id",
    ).bind(world.workspace_id).bind(world.admin.principal_id).bind(cal).fetch_one(tx.conn()).await.unwrap();
    for weekday in 1..=7 {
        sqlx::query(
            "INSERT INTO availability_rules (workspace_id,host_principal_id,timezone,weekday,start_local,end_local)
             VALUES ($1,$2,'UTC',$3,'00:00','23:59')",
        ).bind(world.workspace_id).bind(world.admin.principal_id).bind(weekday).execute(tx.conn()).await.unwrap();
    }
    let email_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id,address_original,normalized_key)
         VALUES ($1,'known@example.test','known@example.test') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO booking_links (workspace_id,meeting_type_id,host_principal_id,token_hash,max_uses)
         VALUES ($1,$2,$3,$4,$5)",
    ).bind(world.workspace_id).bind(meeting).bind(world.admin.principal_id)
        .bind(infrastructure::crypto::sha256_bytes(raw_token.as_bytes()).to_vec()).bind(max_uses)
        .execute(tx.conn()).await.unwrap();
    tx.commit().await.unwrap();
    BookingFixture {
        token: raw_token,
        email_id,
        calendar_connection_id: cal,
    }
}

fn midday_in_three_days() -> chrono::DateTime<Utc> {
    let date = Utc::now().date_naive() + Duration::days(3);
    date.and_hms_opt(12, 0, 0).unwrap().and_utc()
}

#[tokio::test]
async fn public_link_without_published_privacy_notice_is_unavailable_not_internal() {
    let (app, world) = setup().await;
    let fixture = create_fixture(&app, &world, 1).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET settings = settings - 'booking_privacy_notice_version' WHERE id = $1")
        .bind(world.workspace_id).execute(tx.conn()).await.unwrap();
    tx.commit().await.unwrap();
    let response = app
        .client
        .get(app.url(&format!("/public/v1/booking-links/{}", fixture.token)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn used_up_invitation_resolves_to_its_booking_manage_page() {
    for (max_uses, key) in [
        (1, "booking-resume-single-0001"),
        (5, "booking-resume-multi-0001"),
    ] {
        let (app, world) = setup().await;
        let fixture = create_fixture(&app, &world, max_uses).await;
        let created = app
            .client
            .post(app.url(&format!(
                "/public/v1/booking-links/{}/bookings",
                fixture.token
            )))
            .header("idempotency-key", key)
            .json(&json!({
                "guest_name":"Guest Person",
                "guest_email":format!("{key}@example.test"),
                "starts_at":midday_in_three_days().to_rfc3339(),
                "timezone":"UTC",
                "privacy_notice_version":"booking-privacy-v3"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), StatusCode::ACCEPTED);
        let created: serde_json::Value = created.json().await.unwrap();
        let resume_path = format!("/public/v1/booking-links/{}/booking", fixture.token);
        let resume = app.get(&resume_path).await;
        if max_uses > 1 {
            // An open multi-use link must not reveal another guest's booking.
            assert_eq!(resume.status(), StatusCode::NOT_FOUND);
            continue;
        }
        assert_eq!(
            app.get(&format!("/public/v1/booking-links/{}", fixture.token))
                .await
                .status(),
            StatusCode::GONE
        );
        assert_eq!(resume.status(), StatusCode::OK);
        let resume: serde_json::Value = resume.json().await.unwrap();
        let manage_url = resume["data"]["manage_url"].as_str().unwrap().to_string();
        let again: serde_json::Value = app.get(&resume_path).await.json().await.unwrap();
        assert_eq!(
            again["data"]["manage_url"], manage_url,
            "reopening reuses one capability"
        );
        let manage_token = manage_url.rsplit('/').next().unwrap();
        let status = app
            .get(&format!("/public/v1/bookings/{manage_token}"))
            .await;
        assert_eq!(status.status(), StatusCode::OK);
        let status: serde_json::Value = status.json().await.unwrap();
        assert_eq!(status["data"]["id"], created["data"]["id"]);
        assert_eq!(status["data"]["duration_minutes"], 30);
        // Rescheduling reuses the invitation's availability even though the link is used up.
        let day = midday_in_three_days().date_naive();
        let slots = app
            .get(&format!(
                "/public/v1/bookings/{manage_token}/slots?from={day}&to={}&timezone=UTC",
                day + Duration::days(1)
            ))
            .await;
        assert_eq!(slots.status(), StatusCode::OK);
        let slots: serde_json::Value = slots.json().await.unwrap();
        assert!(!slots["data"]["slots"].as_array().unwrap().is_empty());

        // The host photo travels with the same capabilities.
        assert!(status["data"]["host_avatar_url"].is_null());
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        sqlx::query(
            "INSERT INTO principal_avatars (workspace_id, principal_id, content_type, bytes, updated_by)
             VALUES ($1,$2,'image/png',$3,$2)",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(b"\x89PNG\r\n\x1a\nfake".to_vec())
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let status: serde_json::Value = app
            .get(&format!("/public/v1/bookings/{manage_token}"))
            .await
            .json()
            .await
            .unwrap();
        let avatar_url = status["data"]["host_avatar_url"].as_str().unwrap().to_string();
        let image = app.get(&avatar_url).await;
        assert_eq!(image.status(), StatusCode::OK);
        assert_eq!(image.headers()["content-type"], "image/png");
        let link_avatar = app
            .get(&format!("/public/v1/booking-links/{}/host-avatar", fixture.token))
            .await;
        assert_eq!(link_avatar.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn public_booking_replay_returns_same_manage_token_without_storing_it() {
    let (app, world) = setup().await;
    let fixture = create_fixture(&app, &world, 5).await;
    let start = midday_in_three_days();
    let path = format!("/public/v1/booking-links/{}/bookings", fixture.token);
    let request = json!({
        "guest_name":"Guest Person",
        "guest_email":"fresh@example.test",
        "starts_at":start.to_rfc3339(),
        "timezone":"UTC",
        "privacy_notice_version":"booking-privacy-v3",
        "memo":"Please use the main entrance."
    });
    let first = app
        .client
        .post(app.url(&path))
        .header("idempotency-key", "booking-create-replay-key-0001")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::ACCEPTED);
    let first: serde_json::Value = first.json().await.unwrap();
    assert_eq!(first["data"]["status"], "confirming");
    let booking_id = first["data"]["id"].as_str().unwrap().to_string();
    let manage_url = first["data"]["manage_url"].as_str().unwrap().to_string();
    let status_url = first["data"]["status_url"].as_str().unwrap().to_string();
    assert!(manage_url.ends_with(status_url.rsplit('/').next().unwrap()));

    let replay = app
        .client
        .post(app.url(&path))
        .header("idempotency-key", "booking-create-replay-key-0001")
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    let replay: serde_json::Value = replay.json().await.unwrap();
    assert_eq!(replay["data"]["id"], booking_id);
    assert_eq!(replay["data"]["manage_url"], manage_url);

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let booking_id_uuid = Uuid::parse_str(&booking_id).unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM bookings WHERE id=$1")
        .bind(booking_id_uuid)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    let stored: serde_json::Value = sqlx::query_scalar(
        "SELECT response_body FROM idempotency_records WHERE endpoint='POST /public/v1/booking-links/{token}/bookings' AND idempotency_key='booking-create-replay-key-0001'",
    ).fetch_one(tx.conn()).await.unwrap();
    assert_eq!(count, 1);
    assert!(
        !stored
            .to_string()
            .contains(manage_url.rsplit('/').next().unwrap()),
        "raw manage capability must not enter the idempotency cache"
    );
    tx.rollback().await.unwrap();

    let status = app.get(&status_url).await;
    assert_eq!(status.status(), StatusCode::OK);
    let status: serde_json::Value = status.json().await.unwrap();
    assert_eq!(status["data"]["id"], booking_id);
    assert_eq!(status["data"]["status"], "confirming");
}

#[tokio::test]
async fn attendance_requires_host_scope_version_and_completed_booking() {
    let (app, world) = setup().await;
    let fixture = create_fixture(&app, &world, 5).await;
    let booking_id = {
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        let start = Utc::now() - Duration::hours(3);
        let end = start + Duration::minutes(30);
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO bookings (workspace_id,booking_link_id,host_principal_id,guest_email_address_id,
                guest_name,guest_timezone,starts_at,ends_at,occupied_start,occupied_end,status,provider_event_id)
             VALUES ($1,(SELECT id FROM booking_links WHERE token_hash=$2),$3,$4,'Guest','UTC',$5,$6,$5,$6,'confirmed','calendar-event-1') RETURNING id",
        ).bind(world.workspace_id).bind(infrastructure::crypto::sha256_bytes(fixture.token.as_bytes()).to_vec())
            .bind(world.admin.principal_id).bind(fixture.email_id).bind(start).bind(end)
            .fetch_one(tx.conn()).await.unwrap();
        sqlx::query(
            "INSERT INTO booking_slot_holds (workspace_id,host_principal_id,booking_id,starts_at,ends_at,status)
             VALUES ($1,$2,$3,$4,$5,'confirmed')",
        ).bind(world.workspace_id).bind(world.admin.principal_id).bind(id).bind(start).bind(end)
            .execute(tx.conn()).await.unwrap();
        tx.commit().await.unwrap();
        id
    };
    let login = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(login.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&login).unwrap();
    let path = format!("/api/v1/bookings/{booking_id}/attendance");
    let first = app
        .client
        .post(app.url(&path))
        .header("x-csrf-token", &csrf)
        .header("if-match", "\"1\"")
        .header("idempotency-key", "attendance-key-00001")
        .json(&json!({"attendance_status":"attended","outcome_note":"Discussed next steps."}))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let result: serde_json::Value = first.json().await.unwrap();
    assert_eq!(result["data"]["attendance_status"], "attended");
    assert_eq!(
        result["data"]["status"], "confirmed",
        "attendance never changes provider lifecycle state"
    );
    assert_eq!(result["data"]["provider_event_id"], "calendar-event-1");

    let stale = app
        .client
        .post(app.url(&path))
        .header("x-csrf-token", &csrf)
        .header("if-match", "\"1\"")
        .header("idempotency-key", "attendance-key-00002")
        .json(&json!({"attendance_status":"no_show"}))
        .send()
        .await
        .unwrap();
    let problem = TestApp::problem(stale, StatusCode::PRECONDITION_FAILED).await;
    assert_eq!(problem["code"], "VERSION_MISMATCH");
}

/// Public B.4.2 slots keep both UTC instants for a repeated wall-clock time
/// and never synthesize a local time inside the spring-forward gap.
#[tokio::test]
async fn public_slots_distinguish_repeated_time_and_skip_dst_gap() {
    use chrono::Timelike;
    use chrono_tz::America::New_York;

    let (app, world) = setup().await;
    let fixture = create_fixture(&app, &world, 5).await;
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query("DELETE FROM availability_rules WHERE workspace_id=$1 AND host_principal_id=$2")
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .execute(tx.conn())
        .await
        .unwrap();
    sqlx::query("INSERT INTO availability_rules (workspace_id,host_principal_id,timezone,weekday,start_local,end_local) VALUES ($1,$2,'America/New_York',7,'00:00','04:00')")
        .bind(world.workspace_id).bind(world.admin.principal_id).execute(tx.conn()).await.unwrap();
    sqlx::query("UPDATE meeting_types SET booking_horizon_days=365 WHERE workspace_id=$1 AND id=(SELECT meeting_type_id FROM booking_links WHERE token_hash=$2)")
        .bind(world.workspace_id).bind(infrastructure::crypto::sha256_bytes(fixture.token.as_bytes()).to_vec()).execute(tx.conn()).await.unwrap();
    tx.commit().await.unwrap();

    let fall_path = format!(
        "/public/v1/booking-links/{}/slots?from=2026-11-01&to=2026-11-01&timezone=America%2FNew_York",
        fixture.token
    );
    let fall = app.get(&fall_path).await;
    assert_eq!(fall.status(), StatusCode::OK);
    let fall: serde_json::Value = fall.json().await.unwrap();
    let repeated: Vec<_> = fall["data"]["slots"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|slot| {
            let instant = chrono::DateTime::parse_from_rfc3339(slot["starts_at"].as_str()?).ok()?;
            let local = instant.with_timezone(&New_York);
            (local.hour() == 1 && local.minute() == 30).then_some(instant)
        })
        .collect();
    assert_eq!(
        repeated.len(),
        2,
        "the fall-back wall time has two UTC choices"
    );
    assert_eq!(repeated[1] - repeated[0], Duration::hours(1));

    let spring_path = format!(
        "/public/v1/booking-links/{}/slots?from=2027-03-14&to=2027-03-14&timezone=America%2FNew_York",
        fixture.token
    );
    let spring = app.get(&spring_path).await;
    assert_eq!(spring.status(), StatusCode::OK);
    let spring: serde_json::Value = spring.json().await.unwrap();
    let nonexistent_hour_slots = spring["data"]["slots"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|slot| {
            chrono::DateTime::parse_from_rfc3339(slot["starts_at"].as_str().unwrap())
                .unwrap()
                .with_timezone(&New_York)
                .hour()
                == 2
        })
        .count();
    assert_eq!(
        nonexistent_hour_slots, 0,
        "the spring-forward 02:00 hour does not exist"
    );
}

#[tokio::test]
async fn meeting_type_and_booking_link_posts_replay_without_cached_capability() {
    let (app, world) = setup().await;
    let fixture = create_fixture(&app, &world, 5).await;
    let login = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(login.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&login).unwrap();

    let meeting_request = json!({
        "calendar_connection_id":fixture.calendar_connection_id,
        "name":"Idempotent meeting",
        "duration_minutes":45,
        "buffer_before_minutes":5,
        "buffer_after_minutes":5,
        "minimum_notice_minutes":0,
        "booking_horizon_days":30,
        "location_kind":"offline",
        "location_detail":"Seoul HQ, 5F"
    });
    let meeting_path = app.url("/api/v1/meeting-types");
    let invalid = app
        .client
        .post(&meeting_path)
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "meeting-create-bad-location-1")
        .json(&json!({
            "calendar_connection_id":fixture.calendar_connection_id,
            "name":"Bad location",
            "location_kind":"hybrid"
        }))
        .send()
        .await
        .unwrap();
    assert!(
        invalid.status().is_client_error(),
        "unknown location_kind is rejected"
    );
    let first = app
        .client
        .post(&meeting_path)
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "meeting-create-key-00001")
        .json(&meeting_request)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: serde_json::Value = first.json().await.unwrap();
    let meeting_id = first["data"]["id"].as_str().unwrap();
    assert_eq!(first["data"]["location_kind"], "offline");
    assert_eq!(first["data"]["location_detail"], "Seoul HQ, 5F");
    let replay = app
        .client
        .post(&meeting_path)
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "meeting-create-key-00001")
        .json(&meeting_request)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    let replay: serde_json::Value = replay.json().await.unwrap();
    assert_eq!(replay["data"]["id"], meeting_id);

    let link_request = json!({"meeting_type_id":meeting_id,"max_uses":3});
    let link_path = app.url("/api/v1/booking-links");
    let first = app
        .client
        .post(&link_path)
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "booking-link-create-key-0001")
        .json(&link_request)
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: serde_json::Value = first.json().await.unwrap();
    let link_url = first["data"]["token_url"].as_str().unwrap().to_string();
    let raw_token = link_url.rsplit('/').next().unwrap().to_string();
    let public: serde_json::Value = app
        .get(&format!("/public/v1/booking-links/{raw_token}"))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(public["data"]["location_kind"], "offline");
    assert_eq!(public["data"]["location_detail"], "Seoul HQ, 5F");
    let replay = app
        .client
        .post(&link_path)
        .header("x-csrf-token", &csrf)
        .header("idempotency-key", "booking-link-create-key-0001")
        .json(&link_request)
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CREATED);
    let replay: serde_json::Value = replay.json().await.unwrap();
    assert_eq!(replay["data"]["token_url"], link_url);
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let cache: serde_json::Value = sqlx::query_scalar("SELECT response_body FROM idempotency_records WHERE endpoint='POST /api/v1/booking-links' AND idempotency_key='booking-link-create-key-0001'")
        .fetch_one(tx.conn()).await.unwrap();
    assert!(
        !cache.to_string().contains(&raw_token),
        "raw booking link token must not enter the idempotency cache"
    );
    tx.rollback().await.unwrap();
    let public = app
        .get(&format!("/public/v1/booking-links/{raw_token}"))
        .await;
    assert_eq!(public.status(), StatusCode::OK);
}
