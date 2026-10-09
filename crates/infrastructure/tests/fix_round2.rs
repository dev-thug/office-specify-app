//! Fix-round-2 infrastructure coverage: F1 — kind-scoped job claims at
//! the jobs layer (foreign kinds untouched, foreign maxed jobs not swept)
//! and `release` returning a claimed job to `queued` with the attempt
//! refunded.

mod common;

use chrono::Utc;
use infrastructure::jobs;
use infrastructure::testing::seed_workspace;
use serde_json::json;

/// F1: claim_lease only leases kinds the runner registered — a runnable
/// job of a foreign kind is not claimed, and a maxed foreign-kind job is
/// not even swept to `dead` by this runner.
#[tokio::test]
async fn claim_is_scoped_to_registered_kinds() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let (beta_id, alpha_id) = {
        let mut tx = db.begin(ws).await.unwrap();
        let beta = jobs::enqueue(
            &mut tx,
            "test.beta",
            1,
            json!({}),
            Utc::now() - chrono::Duration::seconds(1),
            Some("beta1"),
        )
        .await
        .unwrap();
        let alpha = jobs::enqueue(
            &mut tx,
            "test.alpha",
            1,
            json!({}),
            Utc::now() - chrono::Duration::seconds(1),
            Some("alpha1"),
        )
        .await
        .unwrap();
        // A foreign-kind job already at the attempt cap.
        sqlx::query("UPDATE jobs SET attempt_count = $3 WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(alpha)
            .bind(jobs::MAX_ATTEMPTS)
            .execute(tx.conn())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (beta, alpha)
    };

    let mut tx = db.begin(ws).await.unwrap();
    let claimed = jobs::claim_lease(&mut tx, "t", 10, 60, &["test.beta"])
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1, "only the registered kind is claimed");
    assert_eq!(claimed[0].id, beta_id);

    // The foreign job is completely untouched — still queued, not dead.
    let state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(alpha_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(
        state, "queued",
        "maxed foreign-kind job must not be dead-lettered by this runner"
    );
    tx.commit().await.unwrap();

    // The workspace scan is kind-scoped too: a ws holding only jobs of
    // other kinds is not returned.
    let ws_list = jobs::runnable_workspaces(&db, &["test.never-registered"])
        .await
        .unwrap();
    assert!(!ws_list.contains(&ws), "no runnable jobs of that kind here");
}

/// F1: `release` is the defensive path for a claimed job that has no
/// handler — the job returns to `queued` and the claim's attempt is
/// refunded (fenced by the lease token).
#[tokio::test]
async fn release_requeues_without_consuming_attempt() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let job_id = {
        let mut tx = db.begin(ws).await.unwrap();
        let id = jobs::enqueue(
            &mut tx,
            "test.beta",
            1,
            json!({}),
            Utc::now() - chrono::Duration::seconds(1),
            Some("rel1"),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    };

    let token = {
        let mut tx = db.begin(ws).await.unwrap();
        let claimed = jobs::claim_lease(&mut tx, "t", 10, 60, &["test.beta"])
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        let job = &claimed[0];
        assert_eq!(job.state, "running");
        assert_eq!(job.attempt_count, 1);
        // A stale token cannot release the job.
        assert!(
            !jobs::release(&mut tx, job_id, uuid::Uuid::new_v4())
                .await
                .unwrap()
        );
        let token = job.lease_token.unwrap();
        assert!(jobs::release(&mut tx, job_id, token).await.unwrap());
        tx.commit().await.unwrap();
        token
    };

    let mut tx = db.begin(ws).await.unwrap();
    let (state, attempts, owner): (String, i32, Option<String>) = sqlx::query_as(
        "SELECT state, attempt_count, lease_owner FROM jobs
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(job_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(state, "queued", "released job is runnable again");
    assert_eq!(attempts, 0, "the claim's attempt is refunded");
    assert!(owner.is_none(), "lease is cleared");

    // And it is claimable again immediately.
    let mut tx = db.begin(ws).await.unwrap();
    let claimed = jobs::claim_lease(&mut tx, "t2", 10, 60, &["test.beta"])
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_ne!(
        claimed[0].lease_token,
        Some(token),
        "re-claim issues a fresh fencing token"
    );
    tx.rollback().await.unwrap();
}
