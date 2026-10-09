//! Fix-round-2 worker coverage: F1 kind-scoped claiming — the runner only
//! claims kinds it has handlers for; jobs of unregistered kinds stay
//! `queued` and are never dead-lettered by a runner that lacks the
//! handler (previously NO_HANDLER → dead, a production hazard and the
//! source of flaky R13/R14 tests under nextest parallelism).

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::db::Tx;
use infrastructure::jobs::{self, JobCtx};
use infrastructure::testing::seed_workspace;
use serde_json::{Value, json};
use uuid::Uuid;
use worker::{JobRegistry, Runner};

/// Records the job ids it handled — assertions are per-job so a stray
/// leftover of the same kind from a crashed earlier run cannot flake us.
struct NoopHandler {
    kind: &'static str,
    seen: Arc<Mutex<Vec<Uuid>>>,
}

#[async_trait]
impl jobs::JobHandler for NoopHandler {
    fn kind(&self) -> &'static str {
        self.kind
    }
    async fn handle(
        &self,
        ctx: &JobCtx,
        _tx: &mut Tx<'_>,
        _payload: Value,
    ) -> Result<(), AppError> {
        self.seen.lock().unwrap().push(ctx.job_id);
        Ok(())
    }
}

fn noop_runner(db: &infrastructure::db::Db, kind: &'static str) -> (Runner, Arc<Mutex<Vec<Uuid>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut reg = JobRegistry::new();
    reg.register(NoopHandler {
        kind,
        seen: seen.clone(),
    });
    (Runner::new(db.clone(), reg), seen)
}

async fn job_state(
    db: &infrastructure::db::Db,
    ws: Uuid,
    id: Uuid,
) -> (String, i32, Option<String>) {
    let mut tx = db.begin(ws).await.unwrap();
    let row: (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, attempt_count, lease_owner FROM jobs
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    row
}

/// F1: a job whose kind the runner has no handler for is never claimed —
/// it stays `queued` with its attempt unconsumed, available for a runner
/// (or deploy generation) that does handle the kind.
#[tokio::test]
async fn unregistered_kind_stays_queued() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    let job_id = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = jobs::enqueue(
            &mut tx,
            "test.unregistered",
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

    // This runner handles only its own sentinel kind — test.unregistered is invisible
    // to its scan and its claim.
    let (r, seen) = noop_runner(&db, "test.unregistered-sentinel");
    r.run_once().await.unwrap();

    let (state, attempts, owner) = job_state(&db, ws, job_id).await;
    assert_eq!(state, "queued", "unhandled kind must never be claimed");
    assert_eq!(attempts, 0, "no attempt may be consumed");
    assert!(owner.is_none(), "no lease may be held");
    assert!(!seen.lock().unwrap().contains(&job_id));
}

/// F1: the runner claims and completes registered kinds in the same
/// workspace while leaving foreign kinds alone.
#[tokio::test]
async fn registered_kind_is_claimed_foreign_untouched() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    let (mine, foreign) = {
        let mut tx = db.begin(ws).await.unwrap();
        let mine = jobs::enqueue(
            &mut tx,
            "test.noop",
            1,
            json!({}),
            chrono::Utc::now() - chrono::Duration::seconds(1),
            Some("mine"),
        )
        .await
        .unwrap();
        let foreign = jobs::enqueue(
            &mut tx,
            "test.unregistered",
            1,
            json!({}),
            chrono::Utc::now() - chrono::Duration::seconds(1),
            Some("foreign"),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (mine, foreign)
    };

    let (r, seen) = noop_runner(&db, "test.noop");
    r.run_once().await.unwrap();

    let ours = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|id| **id == mine)
        .count();
    assert_eq!(ours, 1, "our registered-kind job ran exactly once");
    let (state, _, _) = job_state(&db, ws, mine).await;
    assert_eq!(state, "completed");

    let (state, attempts, _) = job_state(&db, ws, foreign).await;
    assert_eq!(state, "queued", "foreign kind left untouched");
    assert_eq!(attempts, 0);
}
