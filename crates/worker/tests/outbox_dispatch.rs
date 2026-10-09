//! Outbox dispatcher tests (spec §4.7): subscription delivery,
//! processed_events dedupe, per-consumer failure isolation + retry,
//! concurrent dispatchers applying a handler exactly once, and publish of
//! events with no subscribers.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::outbox::{self, OutboxRow};
use infrastructure::testing::seed_workspace;
use serde_json::json;
use uuid::Uuid;
use worker::consumers::{EventConsumer, EventCtx};
use worker::{JobRegistry, Runner};

/// Test consumer: counts invocations, optionally fails, and writes an audit
/// row as its transactional side effect.
struct Recorder {
    name: &'static str,
    /// Real principal id — audit_logs.actor_principal_id is an FK.
    actor: Uuid,
    fail: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl EventConsumer for Recorder {
    fn name(&self) -> &'static str {
        self.name
    }
    fn event_types(&self) -> &'static [&'static str] {
        &["test.event"]
    }
    async fn handle(
        &self,
        ctx: &EventCtx,
        tx: &mut Tx<'_>,
        _event: &OutboxRow,
    ) -> Result<(), AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(AppError::internal("scripted failure"));
        }
        audit::record(
            tx,
            AuditEntry::new(
                self.actor,
                "test.consumed",
                "outbox_event",
                ctx.event_id.to_string(),
            )
            .with_target(ctx.event_id),
        )
        .await
    }
}

fn recorder(
    name: &'static str,
    actor: Uuid,
    fail: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
) -> Recorder {
    Recorder {
        name,
        actor,
        fail,
        calls,
    }
}

fn runner(db: &Db) -> Runner {
    let mut r = Runner::new(db.clone(), JobRegistry::new());
    r.backoff_base = Duration::from_millis(5);
    r
}

async fn emit(db: &Db, ws: Uuid, event_type: &str) -> Uuid {
    let mut tx = db.begin(ws).await.unwrap();
    let id = outbox::emit(&mut tx, event_type, "test", Uuid::new_v4(), 1, json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    id
}

async fn processed_count(db: &Db, ws: Uuid, consumer: &str, event_id: Uuid) -> i64 {
    let mut tx = db.begin(ws).await.unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM processed_events WHERE consumer = $1 AND event_id = $2",
    )
    .bind(consumer)
    .bind(event_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    n
}

async fn is_published(db: &Db, ws: Uuid, event_id: Uuid) -> bool {
    let mut tx = db.begin(ws).await.unwrap();
    let published: bool =
        sqlx::query_scalar("SELECT published_at IS NOT NULL FROM outbox_events WHERE id = $1")
            .bind(event_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    tx.rollback().await.unwrap();
    published
}

async fn audit_count(db: &Db, ws: Uuid, event_id: Uuid) -> i64 {
    let mut tx = db.begin(ws).await.unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs WHERE action = 'test.consumed' AND target_id = $1",
    )
    .bind(event_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    n
}

/// (a) emitted event → subscribed consumer invoked once, processed_events
/// row, event published.
#[tokio::test]
async fn dispatch_delivers_event_once() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let calls = Arc::new(AtomicUsize::new(0));
    let mut reg = JobRegistry::new();
    reg.register_consumer(recorder(
        "test.consumer",
        world.system.principal_id,
        Arc::new(AtomicBool::new(false)),
        calls.clone(),
    ));
    let mut r = Runner::new(db.clone(), reg);
    r.backoff_base = Duration::from_millis(5);

    let event_id = emit(&db, ws, "test.event").await;
    let ran = r.dispatch_workspace(ws).await.unwrap();

    assert_eq!(ran, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(processed_count(&db, ws, "test.consumer", event_id).await, 1);
    assert!(is_published(&db, ws, event_id).await);

    // Second tick: nothing left to do.
    assert_eq!(r.dispatch_workspace(ws).await.unwrap(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// (b) consumer A fails, B succeeds → B processed, event unpublished; after
/// A recovers, next tick invokes only A, then publishes.
#[tokio::test]
async fn consumer_failure_isolated_and_retried() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let fail_a = Arc::new(AtomicBool::new(true));
    let calls_a = Arc::new(AtomicUsize::new(0));
    let calls_b = Arc::new(AtomicUsize::new(0));
    let mut reg = JobRegistry::new();
    reg.register_consumer(recorder(
        "cons.a",
        world.system.principal_id,
        fail_a.clone(),
        calls_a.clone(),
    ));
    reg.register_consumer(recorder(
        "cons.b",
        world.system.principal_id,
        Arc::new(AtomicBool::new(false)),
        calls_b.clone(),
    ));
    let mut r = Runner::new(db.clone(), reg);
    r.backoff_base = Duration::from_millis(5);

    let event_id = emit(&db, ws, "test.event").await;
    r.dispatch_workspace(ws).await.unwrap();

    assert_eq!(calls_a.load(Ordering::SeqCst), 1, "A attempted");
    assert_eq!(calls_b.load(Ordering::SeqCst), 1, "B ran");
    assert_eq!(processed_count(&db, ws, "cons.b", event_id).await, 1);
    assert_eq!(processed_count(&db, ws, "cons.a", event_id).await, 0);
    assert!(!is_published(&db, ws, event_id).await);

    // A recovers; after its backoff expires only A is invoked, then publish.
    fail_a.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(20)).await;
    r.dispatch_workspace(ws).await.unwrap();

    assert_eq!(calls_a.load(Ordering::SeqCst), 2, "A retried");
    assert_eq!(calls_b.load(Ordering::SeqCst), 1, "B not re-run");
    assert_eq!(processed_count(&db, ws, "cons.a", event_id).await, 1);
    assert!(is_published(&db, ws, event_id).await);
}

/// (c) two dispatchers concurrently on the same event → handler applied
/// exactly once (processed_events unique constraint decides).
#[tokio::test]
async fn concurrent_dispatchers_apply_once() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let calls = Arc::new(AtomicUsize::new(0));
    let actor = world.system.principal_id;
    let mk = |db: &Db| {
        let mut reg = JobRegistry::new();
        reg.register_consumer(recorder(
            "dup.consumer",
            actor,
            Arc::new(AtomicBool::new(false)),
            calls.clone(),
        ));
        let mut r = Runner::new(db.clone(), reg);
        r.backoff_base = Duration::from_millis(5);
        r
    };
    let r1 = mk(&db);
    let r2 = mk(&db);

    let event_id = emit(&db, ws, "test.event").await;
    let (a, b) = tokio::join!(r1.dispatch_workspace(ws), r2.dispatch_workspace(ws));
    a.unwrap();
    b.unwrap();

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "handler ran more than once"
    );
    assert_eq!(processed_count(&db, ws, "dup.consumer", event_id).await, 1);
    assert_eq!(
        audit_count(&db, ws, event_id).await,
        1,
        "side effect duplicated"
    );
    assert!(is_published(&db, ws, event_id).await);
}

/// (d) event with no subscribers → published, no processed rows.
#[tokio::test]
async fn unsubscribed_event_is_published() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let r = runner(&db); // empty registry
    let event_id = emit(&db, ws, "test.unsubscribed").await;
    r.dispatch_workspace(ws).await.unwrap();

    assert!(is_published(&db, ws, event_id).await);
}
