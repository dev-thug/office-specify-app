//! System `meeting.booking_url` issuance: email-bound capability storage,
//! configured public origin, scoped Agent access, and secret-free records.

use application::booking::{issue_bound_link_in_tx, validate_bound_link_in_tx};
use contracts::Scope;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{ctx_for, ctx_for_credential, seed_workspace};
use uuid::Uuid;

async fn test_db() -> Db {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    Db::connect(&url, Some(ROLE_API)).await.unwrap()
}

#[tokio::test]
async fn bound_booking_url_uses_configured_origin_hashes_secret_and_audits_ids() {
    let db = test_db().await;
    let world = seed_workspace(&db).await.unwrap();
    let base_url = "https://bookings.example.test";
    let (meeting_type_id, email_address_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query("UPDATE workspaces SET settings=settings || $2::jsonb WHERE id=$1")
            .bind(world.workspace_id)
            .bind(serde_json::json!({"public_base_url":base_url}))
            .execute(tx.conn())
            .await
            .unwrap();
        let integration: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_connections (workspace_id,provider,region,secret_ref,status,connected_by)
             VALUES ($1,'zoho_calendar','us','booking-test-ref','active',$2) RETURNING id",
        ).bind(world.workspace_id).bind(world.admin.principal_id).fetch_one(tx.conn()).await.unwrap();
        let calendar: Uuid = sqlx::query_scalar(
            "INSERT INTO calendar_connections (workspace_id,host_principal_id,connection_id,provider_calendar_id,status)
             VALUES ($1,$2,$3,$4,'active') RETURNING id",
        ).bind(world.workspace_id).bind(world.admin.principal_id).bind(integration)
            .bind(format!("cal-{}",Uuid::new_v4())).fetch_one(tx.conn()).await.unwrap();
        let meeting: Uuid = sqlx::query_scalar(
            "INSERT INTO meeting_types (workspace_id,host_principal_id,calendar_connection_id,name)
             VALUES ($1,$2,$3,'Booking URL test') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(calendar)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let address: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id,address_original,normalized_key)
             VALUES ($1,'bound@example.test','bound@example.test') RETURNING id",
        )
        .bind(world.workspace_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (meeting, address)
    };

    let ctx = ctx_for(&world, &world.admin, "booking-link-test");
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    validate_bound_link_in_tx(&mut tx, &ctx, meeting_type_id)
        .await
        .unwrap();
    let url = issue_bound_link_in_tx(
        &mut tx,
        &ctx,
        meeting_type_id,
        email_address_id,
        &ctx.request_id,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert!(url.starts_with(&format!("{base_url}/public/book/")));
    let raw_token = url.rsplit('/').next().unwrap();
    let expected_hash = infrastructure::crypto::sha256_bytes(raw_token.as_bytes());
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let stored: (Uuid, Vec<u8>, i32) = sqlx::query_as(
        "SELECT bound_email_address_id,token_hash,max_uses FROM booking_links WHERE token_hash=$1",
    )
    .bind(&expected_hash[..])
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(stored.0, email_address_id);
    assert_eq!(stored.1, expected_hash);
    assert_eq!(stored.2, 1);
    let audit:serde_json::Value=sqlx::query_scalar(
        "SELECT details FROM audit_logs WHERE action='booking_link.created' AND target_id=(SELECT id FROM booking_links WHERE token_hash=$1)",
    ).bind(&expected_hash[..]).fetch_one(tx.conn()).await.unwrap();
    assert!(!audit.to_string().contains(raw_token));
    let event:serde_json::Value=sqlx::query_scalar(
        "SELECT payload FROM outbox_events WHERE event_type='booking.link_created' AND aggregate_id=(SELECT id FROM booking_links WHERE token_hash=$1)",
    ).bind(&expected_hash[..]).fetch_one(tx.conn()).await.unwrap();
    assert!(!event.to_string().contains(raw_token));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn agent_requires_both_booking_scopes_to_issue_bound_link() {
    let db = test_db().await;
    let world = seed_workspace(&db).await.unwrap();
    let (meeting_type_id, email_address_id) = {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        let integration:Uuid=sqlx::query_scalar("INSERT INTO integration_connections (workspace_id,provider,region,secret_ref,status,connected_by) VALUES ($1,'zoho_calendar','us','agent-calendar-ref','active',$2) RETURNING id")
            .bind(world.workspace_id).bind(world.admin.principal_id).fetch_one(tx.conn()).await.unwrap();
        let calendar:Uuid=sqlx::query_scalar("INSERT INTO calendar_connections (workspace_id,host_principal_id,connection_id,provider_calendar_id,status) VALUES ($1,$2,$3,$4,'active') RETURNING id")
            .bind(world.workspace_id).bind(world.admin.principal_id).bind(integration).bind(format!("cal-{}",Uuid::new_v4())).fetch_one(tx.conn()).await.unwrap();
        let meeting:Uuid=sqlx::query_scalar("INSERT INTO meeting_types (workspace_id,host_principal_id,calendar_connection_id,name) VALUES ($1,$2,$3,'Agent link test') RETURNING id")
            .bind(world.workspace_id).bind(world.admin.principal_id).bind(calendar).fetch_one(tx.conn()).await.unwrap();
        let address:Uuid=sqlx::query_scalar("INSERT INTO email_addresses (workspace_id,address_original,normalized_key) VALUES ($1,'agent-bound@example.test','agent-bound@example.test') RETURNING id")
            .bind(world.workspace_id).fetch_one(tx.conn()).await.unwrap();
        tx.commit().await.unwrap();
        (meeting, address)
    };
    let weak_ctx = ctx_for_credential(
        &world,
        &world.agent,
        Uuid::new_v4(),
        contracts::ScopeSet::of([Scope::BookingPropose]),
        "agent-no-read",
    );
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    assert!(
        issue_bound_link_in_tx(
            &mut tx,
            &weak_ctx,
            meeting_type_id,
            email_address_id,
            &weak_ctx.request_id
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn revoked_live_agent_credential_cannot_issue_link_from_stale_ctx() {
    let db = test_db().await;
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE workspaces SET settings=settings || '{\"public_base_url\":\"https://bookings.example.test\"}'::jsonb WHERE id=$1")
        .bind(world.workspace_id).execute(tx.conn()).await.unwrap();
    let integration: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections (workspace_id,provider,region,secret_ref,status,connected_by)
         VALUES ($1,'zoho_calendar','us','revoked-agent-calendar','active',$2) RETURNING id",
    ).bind(world.workspace_id).bind(world.admin.principal_id).fetch_one(tx.conn()).await.unwrap();
    let calendar: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections (workspace_id,host_principal_id,connection_id,provider_calendar_id,status)
         VALUES ($1,$2,$3,$4,'active') RETURNING id",
    ).bind(world.workspace_id).bind(world.admin.principal_id).bind(integration)
        .bind(format!("cal-{}",Uuid::new_v4())).fetch_one(tx.conn()).await.unwrap();
    let meeting: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types (workspace_id,host_principal_id,calendar_connection_id,name)
         VALUES ($1,$2,$3,'Revoked credential test') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.admin.principal_id)
    .bind(calendar)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let email: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id,address_original,normalized_key)
         VALUES ($1,'revoked-agent@example.test','revoked-agent@example.test') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let required = ["booking.propose".to_string(), "booking.read".to_string()];
    let (credential_id, _token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "booking-runner",
        &required,
        chrono::Duration::days(30),
    )
    .await
    .unwrap();
    let scopes = contracts::ScopeSet::of([Scope::BookingPropose, Scope::BookingRead]);
    let stale_ctx = ctx_for_credential(
        &world,
        &world.agent,
        credential_id,
        scopes,
        "stale-agent-context",
    );
    sqlx::query("UPDATE service_credentials SET revoked_at=now() WHERE workspace_id=$1 AND id=$2")
        .bind(world.workspace_id)
        .bind(credential_id)
        .execute(tx.conn())
        .await
        .unwrap();

    let error = issue_bound_link_in_tx(&mut tx, &stale_ctx, meeting, email, &stale_ctx.request_id)
        .await
        .unwrap_err();
    assert_eq!(error.code, contracts::ErrorCode::AuthenticationRequired);
    let link_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM booking_links WHERE bound_email_address_id=$1")
            .bind(email)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(link_count, 0);
    tx.rollback().await.unwrap();
}
