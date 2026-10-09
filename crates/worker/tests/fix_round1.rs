//! Fix-round-1 worker coverage: R13 lease heartbeat keeps a long-running
//! job fenced (no double-run under a competing reclaimer), R14 handler
//! panics become failed attempts, R17 poison events cannot starve
//! deliverable ones.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{self, JobCtx};
use infrastructure::outbox::{self, OutboxRow};
use infrastructure::testing::seed_workspace;
use serde_json::{Value, json};
use uuid::Uuid;
use worker::consumers::{EventConsumer, EventCtx};
use worker::{JobRegistry, Runner};

// ---------------------------------------------------------------- R17

struct TypedConsumer {
    name: &'static str,
    types: &'static [&'static str],
    fail: bool,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl EventConsumer for TypedConsumer {
    fn name(&self) -> &'static str {
        self.name
    }
    fn event_types(&self) -> &'static [&'static str] {
        self.types
    }
    async fn handle(
        &self,
        _ctx: &EventCtx,
        _tx: &mut Tx<'_>,
        _event: &OutboxRow,
    ) -> Result<(), AppError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(AppError::internal("poison"));
        }
        Ok(())
    }
}

async fn emit(db: &infrastructure::db::Db, ws: Uuid, event_type: &str) {
    let mut tx = db.begin(ws).await.unwrap();
    outbox::emit(&mut tx, event_type, "test", Uuid::new_v4(), 1, json!({}))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// R17: 100 poison events at the head must not starve a deliverable event.
/// After the poison consumer's failures are observed once (backoff
/// established), a single tick delivers the good event even though 100
/// unpublished events are older.
#[tokio::test]
async fn poison_events_do_not_starve_good_event() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    let good_calls = Arc::new(AtomicUsize::new(0));
    let poison_calls = Arc::new(AtomicUsize::new(0));
    let mut reg = JobRegistry::new();
    reg.register_consumer(TypedConsumer {
        name: "poison.cons",
        types: &["poison.event"],
        fail: true,
        calls: poison_calls.clone(),
    });
    reg.register_consumer(TypedConsumer {
        name: "good.cons",
        types: &["good.event"],
        fail: false,
        calls: good_calls.clone(),
    });
    let mut r = Runner::new(db.clone(), reg);
    // Long backoff so the observed failures stay in active backoff.
    r.backoff_base = Duration::from_secs(300);

    for _ in 0..100 {
        emit(&db, ws, "poison.event").await;
    }
    // Tick 1 observes all 100 poison events failing → backoff established,
    // events remain unpublished.
    r.dispatch_workspace(ws).await.unwrap();
    assert_eq!(poison_calls.load(Ordering::SeqCst), 100);

    // The good event arrives behind 100 unpublished poison events.
    emit(&db, ws, "good.event").await;

    // R17: one tick — the 100 fully-backed-off events are excluded from the
    // claim, so the batch contains the good event.
    r.dispatch_workspace(ws).await.unwrap();
    assert_eq!(
        good_calls.load(Ordering::SeqCst),
        1,
        "good event must be delivered in one tick"
    );
    assert_eq!(
        poison_calls.load(Ordering::SeqCst),
        100,
        "backed-off consumers are not re-invoked"
    );
}

// ---------------------------------------------------------------- R14

struct PanicHandler;

#[async_trait]
impl jobs::JobHandler for PanicHandler {
    fn kind(&self) -> &'static str {
        "test.panic"
    }
    async fn handle(
        &self,
        _ctx: &JobCtx,
        _tx: &mut Tx<'_>,
        _payload: Value,
    ) -> Result<(), AppError> {
        panic!("scripted panic");
    }
}

/// R14: a panicking handler is converted to a failed attempt
/// (HANDLER_PANIC) — the runner survives and the job is retryable.
#[tokio::test]
async fn handler_panic_becomes_failed_attempt() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    {
        let mut tx = db.begin(ws).await.unwrap();
        jobs::enqueue(
            &mut tx,
            "test.panic",
            1,
            json!({}),
            chrono::Utc::now() - chrono::Duration::seconds(1),
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let mut reg = JobRegistry::new();
    reg.register(PanicHandler);
    let r = Runner::new(db.clone(), reg);
    r.run_once().await.unwrap();

    let mut tx = db.begin(ws).await.unwrap();
    let (state, code): (String, Option<String>) = sqlx::query_as(
        "SELECT state, last_error_code FROM jobs
          WHERE workspace_id = $1 AND kind = 'test.panic'",
    )
    .bind(ws)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(state, "retry_wait", "panic → retryable failure");
    assert_eq!(code.as_deref(), Some("HANDLER_PANIC"));
}

// ---------------------------------------------------------------- R13

/// Records handled job ids — kind-scoped claims (F1) mean a stray
/// same-kind leftover from a crashed earlier run can legitimately be
/// picked up by this runner; assertions filter to this test's job.
struct SlowHandler {
    seen: Arc<std::sync::Mutex<Vec<Uuid>>>,
    sleep: Duration,
}

#[async_trait]
impl jobs::JobHandler for SlowHandler {
    fn kind(&self) -> &'static str {
        "test.slow"
    }
    async fn handle(
        &self,
        ctx: &JobCtx,
        _tx: &mut Tx<'_>,
        _payload: Value,
    ) -> Result<(), AppError> {
        self.seen.lock().unwrap().push(ctx.job_id);
        tokio::time::sleep(self.sleep).await;
        Ok(())
    }
}

/// R13: heartbeats keep a long-running job's lease fresh — a concurrent
/// reclaimer cannot steal it, so the handler runs exactly once and the job
/// completes.
#[tokio::test]
async fn heartbeat_prevents_lease_steal() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    let job_id = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = jobs::enqueue(
            &mut tx,
            "test.slow",
            1,
            json!({}),
            chrono::Utc::now() - chrono::Duration::seconds(1),
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    };

    let seen = Arc::new(std::sync::Mutex::new(Vec::<Uuid>::new()));
    let mut reg = JobRegistry::new();
    reg.register(SlowHandler {
        seen: seen.clone(),
        sleep: Duration::from_millis(1500),
    });
    let mut r = Runner::new(db.clone(), reg);
    r.lease_seconds = 1; // sub-second heartbeats: lease/3 ≈ 333ms

    // A competing reclaimer polling while the handler runs.
    let db2 = db.clone();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop2 = stop.clone();
    let reclaimer = tokio::spawn(async move {
        while !stop2.load(Ordering::SeqCst) {
            let _ = jobs::reclaim_expired(&db2).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    r.run_once().await.unwrap();
    stop.store(true, Ordering::SeqCst);
    reclaimer.await.unwrap();

    let ours = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|id| **id == job_id)
        .count();
    assert_eq!(
        ours, 1,
        "handler must run exactly once — heartbeat held the lease"
    );
    let mut tx = db.begin(ws).await.unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE workspace_id = $1 AND kind = 'test.slow'")
            .bind(ws)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(state, "completed");
}
