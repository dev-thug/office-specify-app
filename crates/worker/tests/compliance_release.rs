//! `compliance.release_suppression` job tests (integration contract:
//! payload `{approval_request_id, suppression_id}`). The handler re-verifies
//! the human approval inside its own tx, releases via
//! `compliance_core::release_suppression`, emits `suppression.released`
//! outbox + audit — all atomically. Idempotent and no-retry-safe.

mod common;

use infrastructure::db::{Db, Tx};
use infrastructure::jobs;
use infrastructure::repo::approvals::{ApprovalTarget, insert_request};
use infrastructure::repo::compliance_core;
use infrastructure::testing::{TestWorld, seed_workspace};
use serde_json::json;
use uuid::Uuid;
use worker::{Runner, registry};

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

async fn suppress(tx: &mut Tx<'_>, addr: Uuid) -> Uuid {
    compliance_core::lock_email_address(tx, addr).await.unwrap();
    compliance_core::insert_suppression(tx, addr, "marketing", "manual", "manual", None, None)
        .await
        .unwrap()
}

/// Approved `suppression_release` request + its human decision — what the
/// approvals module leaves behind before enqueueing this job.
async fn approved_request(tx: &mut Tx<'_>, world: &TestWorld, suppression_id: Uuid) -> Uuid {
    let snapshot = infrastructure::crypto::sha256_bytes(b"snapshot");
    let req = insert_request(
        tx,
        world.admin.principal_id,
        ApprovalTarget::SuppressionRelease(suppression_id),
        &snapshot,
        chrono::Utc::now() + chrono::Duration::days(7),
    )
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision,
             snapshot_hash, reason, version_at_decision)
         VALUES ($1, $2, $3, 'approved', $4, 'recipient re-consented', 1)",
    )
    .bind(tx.workspace_id())
    .bind(req)
    .bind(world.manager.principal_id)
    .bind(&snapshot[..])
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query("UPDATE approval_requests SET status = 'approved' WHERE id = $1")
        .bind(req)
        .execute(tx.conn())
        .await
        .unwrap();
    req
}

/// Enqueue the release job under the canonical dedupe key
/// `release_suppression:{approval_request_id}` (the approvals module owns
/// the real enqueue on decision — this mirrors its contract).
async fn enqueue_release(tx: &mut Tx<'_>, req: Uuid, sid: Uuid) -> Uuid {
    jobs::enqueue(
        tx,
        "compliance.release_suppression",
        1,
        json!({"approval_request_id": req, "suppression_id": sid}),
        chrono::Utc::now(),
        Some(&format!("release_suppression:{req}")),
    )
    .await
    .unwrap()
}

async fn suppression_released(db: &Db, ws: Uuid, sid: Uuid) -> Option<Uuid> {
    let mut tx = db.begin(ws).await.unwrap();
    let released_by: Option<Uuid> =
        sqlx::query_scalar("SELECT released_by FROM suppressions WHERE id = $1")
            .bind(sid)
            .fetch_optional(tx.conn())
            .await
            .unwrap()
            .flatten();
    tx.rollback().await.unwrap();
    released_by
}

/// Approved request → job releases, emits outbox + audit, completes.
#[tokio::test]
async fn release_job_releases_and_audits() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    let mut tx = db.begin(ws).await.unwrap();
    let addr = insert_email_address(&mut tx, "rel").await;
    let sid = suppress(&mut tx, addr).await;
    let req = approved_request(&mut tx, &world, sid).await;
    enqueue_release(&mut tx, req, sid).await;
    tx.commit().await.unwrap();

    let r = Runner::new(db.clone(), registry());
    let handled = r.run_once().await.unwrap();
    assert!(handled >= 1, "job must be claimed");

    assert_eq!(
        suppression_released(&db, ws, sid).await,
        Some(world.manager.principal_id),
        "released_by must be the deciding human"
    );

    let mut tx = db.begin(ws).await.unwrap();
    // suppression_events got 'released' once.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppression_events WHERE suppression_id = $1 AND action = 'released'",
    )
    .bind(sid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(n, 1);
    // outbox suppression.released emitted.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events
          WHERE event_type = 'suppression.released' AND aggregate_id = $1",
    )
    .bind(sid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(n, 1);
    // audit suppression.released recorded with the job id.
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs
          WHERE action = 'suppression.released' AND target_id = $1 AND job_id IS NOT NULL",
    )
    .bind(sid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(n, 1);
    // job completed.
    let state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE kind = 'compliance.release_suppression'")
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(state, "completed");
    tx.rollback().await.unwrap();
}

/// A second job for the same decision is a no-op — released rows stay
/// released, events/outbox are not duplicated.
#[tokio::test]
async fn release_job_idempotent_on_replay() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    let mut tx = db.begin(ws).await.unwrap();
    let addr = insert_email_address(&mut tx, "rel2").await;
    let sid = suppress(&mut tx, addr).await;
    let req = approved_request(&mut tx, &world, sid).await;
    enqueue_release(&mut tx, req, sid).await;
    tx.commit().await.unwrap();

    let r = Runner::new(db.clone(), registry());
    r.run_once().await.unwrap();
    assert!(suppression_released(&db, ws, sid).await.is_some());

    // Replay: a duplicate job (different dedupe key) arrives later.
    let mut tx = db.begin(ws).await.unwrap();
    jobs::enqueue(
        &mut tx,
        "compliance.release_suppression",
        1,
        json!({"approval_request_id": req, "suppression_id": sid}),
        chrono::Utc::now(),
        Some(&format!("release_suppression:{req}:retry")),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    r.run_once().await.unwrap();

    let mut tx = db.begin(ws).await.unwrap();
    let released_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppression_events WHERE suppression_id = $1 AND action = 'released'",
    )
    .bind(sid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(released_events, 1, "replay must not re-release");
    let released_outbox: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events
          WHERE event_type = 'suppression.released' AND aggregate_id = $1",
    )
    .bind(sid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(released_outbox, 1);
    tx.rollback().await.unwrap();
}

/// Jobs referencing pending/rejected requests or wrong targets complete
/// without side effects — never retried, never released (§16.4 unknown→no
/// implicit retry; the job outcome is a definitive no-op).
#[tokio::test]
async fn release_job_noops_on_unauthorized_payloads() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&common::seed_db().await).await.unwrap();
    let ws = world.workspace_id;

    let mut tx = db.begin(ws).await.unwrap();
    let addr = insert_email_address(&mut tx, "rel3").await;
    let sid = suppress(&mut tx, addr).await;
    // A still-PENDING request.
    let snapshot = infrastructure::crypto::sha256_bytes(b"snap");
    let pending = insert_request(
        &mut tx,
        world.admin.principal_id,
        ApprovalTarget::SuppressionRelease(sid),
        &snapshot,
        chrono::Utc::now() + chrono::Duration::days(7),
    )
    .await
    .unwrap();
    enqueue_release(&mut tx, pending, sid).await;
    // An unknown request id — the dedupe key stays canonical
    // (`release_suppression:{approval_request_id}`).
    let unknown = Uuid::new_v4();
    enqueue_release(&mut tx, unknown, sid).await;
    // A mismatched suppression id: a pending request for a *different*
    // suppression enqueued under its own canonical key, but the payload
    // points at an id the request does not target.
    let addr2 = insert_email_address(&mut tx, "rel3b").await;
    let sid2 = suppress(&mut tx, addr2).await;
    let pending2 = insert_request(
        &mut tx,
        world.admin.principal_id,
        ApprovalTarget::SuppressionRelease(sid2),
        &snapshot,
        chrono::Utc::now() + chrono::Duration::days(7),
    )
    .await
    .unwrap();
    enqueue_release(&mut tx, pending2, Uuid::new_v4()).await;
    tx.commit().await.unwrap();

    let r = Runner::new(db.clone(), registry());
    r.run_once().await.unwrap();

    assert_eq!(
        suppression_released(&db, ws, sid).await,
        None,
        "no valid approval → nothing released"
    );
    let mut tx = db.begin(ws).await.unwrap();
    let states: Vec<String> = sqlx::query_scalar(
        "SELECT state FROM jobs WHERE kind = 'compliance.release_suppression' ORDER BY created_at",
    )
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert!(
        states.iter().all(|s| s == "completed"),
        "unauthorized payloads must complete, not retry: {states:?}"
    );
    tx.rollback().await.unwrap();
}
