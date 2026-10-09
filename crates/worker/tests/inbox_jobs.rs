//! inbox job handler tests (Task 8): `inbox.process_webhook` targeted
//! ingest, `inbox.sync_mailbox` overlap re-scan, and T14 — webhook/sync
//! duplicates, out-of-order delivery and replay never duplicate a message
//! or its `message.received` outbox event.
//!
//! Runs only when `DATABASE_URL` points at the dedicated test DB.

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::test_db;
use infrastructure::providers::ProviderMessage;
use infrastructure::providers::fake::{FakeDeliveryProvider, FakeMailProvider};
use infrastructure::repo::inbox as repo;
use infrastructure::testing::{TestWorld, seed_workspace};
use serde_json::json;
use uuid::Uuid;
use worker::{JobRegistry, Runner};

fn msg(
    provider_message_id: &str,
    provider_thread_id: &str,
    at: chrono::DateTime<Utc>,
    subject: &str,
    body: &str,
) -> ProviderMessage {
    ProviderMessage {
        provider_message_id: provider_message_id.into(),
        provider_thread_id: Some(provider_thread_id.into()),
        internet_message_id: Some(format!("<{provider_message_id}@fake.test>")),
        in_reply_to: None,
        references: vec![],
        from: "sender@example.com".into(),
        to: vec!["shared@test.dev".into()],
        subject: Some(subject.into()),
        body_text: Some(body.into()),
        body_html: None,
        message_at: at,
    }
}

/// Store a verified webhook event row + enqueue `inbox.process_webhook` —
/// the same two writes the ingress path performs.
async fn store_webhook_event(
    db: &infrastructure::db::Db,
    world: &TestWorld,
    event_key: &str,
    provider_message_id: &str,
) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let payload = json!({
        "event_id": event_key,
        "provider_message_id": provider_message_id,
    });
    let id = repo::insert_webhook_event(
        &mut tx,
        world.connection_id,
        event_key,
        &infrastructure::crypto::sha256_bytes(payload.to_string().as_bytes()),
        &payload,
    )
    .await
    .unwrap()
    .expect("first insert wins");
    repo::enqueue_coalesced(
        &mut tx,
        "inbox.process_webhook",
        &json!({"webhook_event_id": id}),
        &format!("webhook:{id}"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn runner(db: &infrastructure::db::Db, fake: &Arc<FakeMailProvider>) -> Runner {
    let context = worker::WorkerContext {
        db: db.clone(),
        mail: fake.clone(),
        delivery: Arc::new(FakeDeliveryProvider::new()),
        calendar: fake.clone(),
    };
    let mut reg = JobRegistry::new();
    worker::handlers::inbox::register_with_context(&mut reg, &context);
    Runner::new(db.clone(), reg)
}

#[tokio::test]
async fn t14_webhook_dup_out_of_order_and_replay_no_duplicates() {
    let Some(db) = test_db().await else { return };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let fake = Arc::new(FakeMailProvider::new());
    let runner = runner(&db, &fake).await;

    // The fake provider currently serves pm-1 + pm-2 (pm-2 older — the
    // webhook for pm-2 arrives AFTER pm-1 was already ingested).
    let now = Utc::now();
    fake.inject_inbound(msg("pm-1", "pt-x", now, "latest", "I like it"));
    fake.inject_inbound(msg(
        "pm-2",
        "pt-x",
        now - Duration::hours(2),
        "older",
        "first touch",
    ));

    // Webhook event for pm-1 → process → message row + outbox + pause job.
    let ev1 = store_webhook_event(&db, &world, "evt-1", "pm-1").await;
    let handled = runner.run_workspace_once(world.workspace_id).await.unwrap();
    assert_eq!(handled, 1);

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let m1 = repo::message_by_provider_id(&mut tx, world.mail_account_id, "pm-1")
        .await
        .unwrap()
        .expect("pm-1 ingested");
    assert_eq!(
        repo::get_webhook_event(&mut tx, ev1)
            .await
            .unwrap()
            .unwrap()
            .status,
        "processed"
    );
    assert_eq!(
        repo::count_outbox(&mut tx, "message.received")
            .await
            .unwrap(),
        1
    );
    // "I like it" → human_positive → campaigns.pause_address enqueued.
    assert!(
        repo::count_jobs(&mut tx, "campaigns.pause_address")
            .await
            .unwrap()
            >= 1
    );
    // A follow-up sync was coalesced in for drift recovery.
    assert_eq!(
        repo::count_jobs(&mut tx, "inbox.sync_mailbox")
            .await
            .unwrap(),
        1
    );
    tx.rollback().await.unwrap();

    // Replay: a second enqueue for the same event coalesces to nothing
    // (dedupe_key is durable), and a retry delivery under a fresh dedupe
    // key re-processes idempotently — pm-1 stays the only message.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let coalesced = repo::enqueue_coalesced(
        &mut tx,
        "inbox.process_webhook",
        &json!({"webhook_event_id": ev1}),
        &format!("webhook:{ev1}"),
    )
    .await
    .unwrap();
    assert!(coalesced.is_none(), "same dedupe key must not re-enqueue");
    repo::enqueue_coalesced(
        &mut tx,
        "inbox.process_webhook",
        &json!({"webhook_event_id": ev1}),
        &format!("webhook-retry:{ev1}"),
    )
    .await
    .unwrap();
    // A second event (different key) referencing the same provider message
    // — out-of-order/dup delivery from the provider.
    let ev2 = repo::insert_webhook_event(
        &mut tx,
        world.connection_id,
        "evt-2",
        &infrastructure::crypto::sha256_bytes(b"evt-2"),
        &json!({"event_id": "evt-2", "provider_message_id": "pm-1"}),
    )
    .await
    .unwrap()
    .unwrap();
    repo::enqueue_coalesced(
        &mut tx,
        "inbox.process_webhook",
        &json!({"webhook_event_id": ev2}),
        &format!("webhook:{ev2}"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    runner.run_workspace_once(world.workspace_id).await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    // pm-2 may arrive in the queued overlap re-scan; pm-1 itself still has
    // exactly one received event across webhook/sync replay.
    let pm1_received_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events
          WHERE workspace_id = $1 AND event_type = 'message.received'
            AND aggregate_id = $2",
    )
    .bind(world.workspace_id)
    .bind(m1.id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(pm1_received_events, 1);
    tx.rollback().await.unwrap();

    // The follow-up sync picks up pm-2 (out of order): last_inbound stays
    // pm-1 because pm-2 is older.
    runner.run_workspace_once(world.workspace_id).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let m2 = repo::message_by_provider_id(&mut tx, world.mail_account_id, "pm-2")
        .await
        .unwrap()
        .expect("pm-2 ingested by sync");
    assert_eq!(m1.thread_id, m2.thread_id);
    let thread = repo::get_thread(&mut tx, m1.thread_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(thread.last_inbound_message_id, Some(m1.id));
    assert_eq!(
        repo::count_outbox(&mut tx, "message.received")
            .await
            .unwrap(),
        2
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn sync_mailbox_upserts_and_self_schedules() {
    let Some(db) = test_db().await else { return };
    let seed_db = common::seed_db().await;
    let world = seed_workspace(&seed_db).await.unwrap();
    let fake = Arc::new(FakeMailProvider::new());
    let runner = runner(&db, &fake).await;

    fake.inject_inbound(msg(
        "pm-s1",
        "pt-s",
        Utc::now(),
        "unsub",
        "please unsubscribe me",
    ));

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    repo::enqueue_coalesced(
        &mut tx,
        "inbox.sync_mailbox",
        &json!({"mail_account_id": world.mail_account_id, "reason": "manual"}),
        "sync:manual-1",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    runner.run_workspace_once(world.workspace_id).await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let m = repo::message_by_provider_id(&mut tx, world.mail_account_id, "pm-s1")
        .await
        .unwrap()
        .expect("synced");
    assert_eq!(m.classification, "unsubscribe_request");
    // Pause + periodic follow-up + cursor state.
    assert!(
        repo::count_jobs(&mut tx, "campaigns.pause_address")
            .await
            .unwrap()
            >= 1
    );
    let cur = repo::get_sync_cursor(&mut tx, world.mail_account_id, "inbound")
        .await
        .unwrap()
        .expect("cursor row");
    assert!(cur.get("synced_at").is_some());
    // The periodic re-scan self-enqueued (dedupe bucket may already be used
    // by the webhook-follow-up path — at least one sync job exists beyond
    // the completed one).
    assert!(
        repo::count_jobs(&mut tx, "inbox.sync_mailbox")
            .await
            .unwrap()
            >= 1
    );
    tx.rollback().await.unwrap();

    // Re-running sync is idempotent — no second message row.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    repo::enqueue_coalesced(
        &mut tx,
        "inbox.sync_mailbox",
        &json!({"mail_account_id": world.mail_account_id, "reason": "manual2"}),
        "sync:manual-2",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runner.run_workspace_once(world.workspace_id).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    assert_eq!(
        repo::count_outbox(&mut tx, "message.received")
            .await
            .unwrap(),
        1
    );
    tx.rollback().await.unwrap();
}
