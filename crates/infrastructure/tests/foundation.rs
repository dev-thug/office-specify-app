//! Task 1 step-9 DB tests: migrations apply, T01 workspace isolation,
//! T04/T05 idempotency, lease fencing, append-only + freeze guards.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::Utc;
use contracts::ErrorCode;
use infrastructure::idempotency::{self, Outcome, Scope, Stored};
use infrastructure::jobs;
use infrastructure::repo::auth as auth_repo;
use infrastructure::testing::seed_workspace;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn migrations_applied_cleanly() {
    let Some(db) = common::test_db().await else {
        return;
    };
    // sqlx bookkeeping — every migration file applied without dirty state.
    let rows: Vec<(i64, bool)> =
        sqlx::query_as("SELECT version, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert!(rows.len() >= 4, "expected ≥4 migrations, got {rows:?}");
    assert!(rows.iter().all(|(_, ok)| *ok), "failed migration: {rows:?}");
}

#[tokio::test]
async fn t01_cross_workspace_isolation() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let a = seed_workspace(&db).await.unwrap();
    let b = seed_workspace(&db).await.unwrap();

    // Workspace A's tx sees only A's principals.
    let mut tx = db.begin(a.workspace_id).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM principals")
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(count, 6, "workspace A principals");

    // B's principal id is invisible — repo returns None (404 territory).
    let found = auth_repo::get_principal(&mut tx, b.admin.principal_id)
        .await
        .unwrap();
    assert!(found.is_none(), "cross-workspace principal leaked");
    tx.rollback().await.unwrap();

    // RLS without a workspace GUC returns zero rows — begin_global applies
    // the runtime role but never sets app.workspace_id.
    let mut tx = db.begin_global().await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM principals")
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(count, 0, "RLS without workspace must see nothing");
    tx.rollback().await.unwrap();

    // Context must not leak across pooled connections: a fresh workspace-B
    // tx on the same pool still cannot see A's rows.
    let mut tx = db.begin(b.workspace_id).await.unwrap();
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM principals WHERE id = $1")
        .bind(a.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(leaked, 0);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn t04_t05_idempotency_concurrency_and_reuse() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let scope = Scope {
        workspace_id: world.workspace_id,
        principal_or_capability: world.admin.principal_id.to_string(),
        endpoint: "POST /api/v1/test".into(),
        key: format!("key-{}", Uuid::new_v4()),
    };
    let request_hash = infrastructure::crypto::sha256_bytes(b"same-request");

    // T04: 20 concurrent identical calls → exactly one executes; the rest
    // replay the stored response.
    let ran = Arc::new(AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for _ in 0..20 {
        let db = db.clone();
        let scope = scope.clone();
        let ran = ran.clone();
        tasks.push(tokio::spawn(async move {
            idempotency::run(&db, &scope, &request_hash, {
                let ran = ran.clone();
                move |_tx| {
                    let ran = ran.clone();
                    async move {
                        // Hold the claim briefly so followers actually poll.
                        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                        ran.fetch_add(1, Ordering::SeqCst);
                        Ok(Stored {
                            status: 201,
                            body: json!({"id": 42}),
                        })
                    }
                }
            })
            .await
        }));
    }
    let mut fresh = 0;
    let mut replayed = 0;
    for t in tasks {
        match t.await.unwrap().unwrap() {
            Outcome::Fresh(s) => {
                fresh += 1;
                assert_eq!(s.status, 201);
            }
            Outcome::Replayed(s) => {
                replayed += 1;
                assert_eq!(s.body, json!({"id": 42}));
            }
        }
    }
    assert_eq!((fresh, replayed), (1, 19));
    assert_eq!(ran.load(Ordering::SeqCst), 1, "handler ran more than once");

    // T05: same key, different request hash → 409 IDEMPOTENCY_KEY_REUSED.
    let other_hash = infrastructure::crypto::sha256_bytes(b"different-request");
    let err = idempotency::run(&db, &scope, &other_hash, |_| async {
        Ok(Stored {
            status: 200,
            body: json!({}),
        })
    })
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::IdempotencyKeyReused);
    assert_eq!(err.status(), 409);
}

#[tokio::test]
async fn job_lease_fencing_and_reclaim() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let job_id = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = jobs::enqueue(&mut tx, "test.job", 1, json!({}), Utc::now(), None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        id
    };

    // Claim under the runner's lease owner.
    let claimed = {
        let mut tx = db.begin(ws).await.unwrap();
        let jobs = jobs::claim(&mut tx, "test-worker", 20, &["test.job"])
            .await
            .unwrap();
        tx.commit().await.unwrap();
        jobs
    };
    assert_eq!(claimed.len(), 1);
    let job = &claimed[0];
    assert_eq!(job.state, jobs::STATE_RUNNING);
    assert_eq!(job.attempt_count, 1);
    let token = job.lease_token.unwrap();

    // Fenced: a stale token cannot complete the job.
    let stale = Uuid::new_v4();
    let mut tx = db.begin(ws).await.unwrap();
    assert!(!jobs::complete(&mut tx, job_id, stale).await.unwrap());
    // Heartbeat + complete with the real token succeed.
    assert!(jobs::heartbeat(&mut tx, job_id, token).await.unwrap());
    assert!(jobs::complete(&mut tx, job_id, token).await.unwrap());
    tx.commit().await.unwrap();

    // Reclaim: enqueue, claim, force lease expiry inside the workspace tx,
    // then the definer scan returns it to queued.
    let job2 = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = jobs::enqueue(&mut tx, "test.job", 1, json!({}), Utc::now(), None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        id
    };
    {
        let mut tx = db.begin(ws).await.unwrap();
        let claimed = jobs::claim(&mut tx, "dead-worker", 20, &["test.job"])
            .await
            .unwrap();
        assert!(claimed.iter().any(|j| j.id == job2));
        sqlx::query("UPDATE jobs SET lease_expires_at = now() - interval '1s' WHERE id = $1")
            .bind(job2)
            .execute(tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let n = jobs::reclaim_expired(&db).await.unwrap();
    assert!(n >= 1, "expired lease not reclaimed");
    let mut tx = db.begin(ws).await.unwrap();
    let state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id = $1")
        .bind(job2)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    // R12: reclaimed non-external leases retry with backoff (retry_wait),
    // not a blind requeue.
    assert_eq!(state, jobs::STATE_RETRY_WAIT);
    // Pull the backoff expiry forward so the job is claimable for the rest
    // of the test.
    sqlx::query("UPDATE jobs SET available_at = now() - interval '1s' WHERE id = $1")
        .bind(job2)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // fail_or_retry → retry_wait; at MAX_ATTEMPTS → dead.
    let mut tx = db.begin(ws).await.unwrap();
    let claimed = jobs::claim(&mut tx, "test-worker", 20, &["test.job"])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let job = claimed.iter().find(|j| j.id == job2).unwrap().clone();
    let mut tx = db.begin(ws).await.unwrap();
    assert!(
        jobs::fail_or_retry(&mut tx, &job, job.lease_token.unwrap(), "TEST_ERR")
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    let (state, err): (String, Option<String>) =
        sqlx::query_as("SELECT state, last_error_code FROM jobs WHERE id = $1")
            .bind(job2)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(state, jobs::STATE_RETRY_WAIT);
    assert_eq!(err.as_deref(), Some("TEST_ERR"));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn append_only_guard_blocks_audit_mutation() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    infrastructure::audit::record(
        &mut tx,
        infrastructure::audit::AuditEntry::new(
            world.admin.principal_id,
            "test.action",
            "test",
            "req-1",
        ),
    )
    .await
    .unwrap();
    // api_runtime has no UPDATE/DELETE on audit_logs AND the trigger
    // rejects mutation — either way this must fail.
    let res = sqlx::query("UPDATE audit_logs SET action = 'tampered'")
        .execute(tx.conn())
        .await;
    assert!(res.is_err(), "audit update must be rejected");
    tx.rollback().await.unwrap();
}
