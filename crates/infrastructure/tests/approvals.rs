//! `repo::approvals::insert_request` — one test per `ApprovalTarget`
//! variant: correct action_type + single FK column (DDL CHECK), status
//! 'pending', and the `approval.requested` audit row in the same tx.

mod common;

use chrono::Utc;
use infrastructure::db::{Db, Tx};
use infrastructure::repo::approvals::{ApprovalTarget, insert_request};
use infrastructure::testing::{TestWorld, seed_workspace};
use uuid::Uuid;

async fn insert_email_address(tx: &mut Tx<'_>, local: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(format!("{local}@example.com"))
    .bind(format!("{local}@example.com"))
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

/// sender_identity + delivery_channel — shared by launch & outbound paths.
async fn insert_channel(tx: &mut Tx<'_>, world: &TestWorld) -> (Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    let ident: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities (workspace_id, mail_account_id, address, display_name)
         VALUES ($1, $2, 'shared@test.dev', 'Shared') RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    // campaign_revisions.purpose is CHECK'd 'marketing' and fk_153 is a
    // composite FK on (delivery_channel_id, purpose) — the channel must be
    // purpose='marketing' for the launch path.
    let channel: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id, provider, purpose, name)
         VALUES ($1, $2, $3, $4, 'zoho_mail', 'marketing', 'Marketing') RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(ident)
    .bind(world.connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    (ident, channel)
}

async fn insert_launch(tx: &mut Tx<'_>, world: &TestWorld) -> Uuid {
    let ws = tx.workspace_id().unwrap();
    let (_ident, channel) = insert_channel(tx, world).await;
    let campaign: Uuid = sqlx::query_scalar(
        "INSERT INTO campaigns (workspace_id, mail_account_id, name, owner_principal_id)
         VALUES ($1, $2, 'Launch test', $3) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let revision: Uuid = sqlx::query_scalar(
        "INSERT INTO campaign_revisions
            (workspace_id, campaign_id, mail_account_id, delivery_channel_id,
             revision_no, timezone, schedule_rules, created_by)
         VALUES ($1, $2, $3, $4, 1, 'UTC', '{}', $5) RETURNING id",
    )
    .bind(ws)
    .bind(campaign)
    .bind(world.mail_account_id)
    .bind(channel)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO campaign_launches
            (workspace_id, revision_id, delivery_channel_id, requested_by, expires_at)
         VALUES ($1, $2, $3, $4, now() + interval '1 day') RETURNING id",
    )
    .bind(ws)
    .bind(revision)
    .bind(channel)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_outbound(tx: &mut Tx<'_>, world: &TestWorld) -> Uuid {
    let ws = tx.workspace_id().unwrap();
    let (ident, channel) = insert_channel(tx, world).await;
    let to = insert_email_address(tx, "rcpt").await;
    sqlx::query_scalar(
        "INSERT INTO outbound_messages
            (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
             to_email_address_id, requested_by, purpose,
             from_address_snapshot, to_address_snapshot, subject, body_text, payload_hash)
         VALUES ($1, $2, $3, $4, $5, $6, 'marketing',
                 'shared@test.dev', 'rcpt@example.com', 'hi', 'body',
                 decode(md5(random()::text), 'hex'))
         RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(channel)
    .bind(ident)
    .bind(to)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_booking_change(tx: &mut Tx<'_>, world: &TestWorld) -> Uuid {
    // kind='create' legally has booking_id NULL — skips the bookings chain.
    sqlx::query_scalar(
        "INSERT INTO booking_changes
            (workspace_id, host_principal_id, kind, proposed_data, requested_by)
         VALUES ($1, $2, 'create', '{}', $2) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn insert_suppression(tx: &mut Tx<'_>, world: &TestWorld) -> Uuid {
    let addr = insert_email_address(tx, "unsub").await;
    sqlx::query_scalar(
        "INSERT INTO suppressions
            (workspace_id, email_address_id, purpose_scope, reason, created_by)
         VALUES ($1, $2, 'marketing', 'manual', $3) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(addr)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

/// Assert the row exists, is pending, and carries exactly the right FK.
async fn assert_request(db: &Db, ws: Uuid, request_id: Uuid, action_type: &str, target_id: Uuid) {
    let mut tx = db.begin(ws).await.unwrap();
    let row: (String, String, i32) = sqlx::query_as(
        "SELECT action_type, status,
                num_nonnulls(launch_id, outbound_message_id, booking_change_id, suppression_id)::int
           FROM approval_requests WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(request_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(row.0, action_type);
    assert_eq!(row.1, "pending");
    assert_eq!(row.2, 1, "exactly one target FK must be set");

    // The right column points at the right target.
    let col = match action_type {
        "campaign_launch" => "launch_id",
        "message_submit" => "outbound_message_id",
        "booking_change" => "booking_change_id",
        "suppression_release" => "suppression_id",
        _ => unreachable!(),
    };
    let hit: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM approval_requests WHERE id = $1 AND {col} = $2"
    ))
    .bind(request_id)
    .bind(target_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(hit, 1);

    // Same-tx audit entry.
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs
          WHERE workspace_id = $1 AND action = 'approval.requested' AND target_id = $2",
    )
    .bind(ws)
    .bind(request_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(audited, 1, "approval.requested audit missing");
    tx.rollback().await.unwrap();
}

const HASH: [u8; 32] = [9u8; 32];

macro_rules! variant_test {
    ($name:ident, $builder:ident, $variant:ident, $action:literal) => {
        #[tokio::test]
        async fn $name() {
            let Some(db) = common::test_db().await else {
                return;
            };
            let world = seed_workspace(&db).await.unwrap();
            let ws = world.workspace_id;

            let mut tx = db.begin(ws).await.unwrap();
            let target_id = $builder(&mut tx, &world).await;
            let request_id = insert_request(
                &mut tx,
                world.admin.principal_id,
                ApprovalTarget::$variant(target_id),
                &HASH,
                Utc::now() + chrono::Duration::days(1),
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();

            assert_request(&db, ws, request_id, $action, target_id).await;
        }
    };
}

variant_test!(
    insert_request_campaign_launch,
    insert_launch,
    CampaignLaunch,
    "campaign_launch"
);
variant_test!(
    insert_request_outbound_message,
    insert_outbound,
    OutboundMessage,
    "message_submit"
);
variant_test!(
    insert_request_booking_change,
    insert_booking_change,
    BookingChange,
    "booking_change"
);
variant_test!(
    insert_request_suppression_release,
    insert_suppression,
    SuppressionRelease,
    "suppression_release"
);
