//! Fix-round-1 coverage: R4 invitation claim, R5 lockout reset, R11/R12
//! jobs dedupe + reclaim, R15 idempotency lease, R16 public-token
//! resolution.

mod common;

use chrono::Utc;
use contracts::ErrorCode;
use infrastructure::db::Tx;
use infrastructure::idempotency::{self, Outcome, Scope};
use infrastructure::jobs;
use infrastructure::public_tokens;
use infrastructure::repo::auth as auth_repo;
use infrastructure::repo::compliance_core;
use infrastructure::testing::{TestWorld, seed_workspace};
use serde_json::{Value, json};
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

// ------------------------------------------------------------- R4 / R5

/// R4: the single-use claim is atomic — two concurrent accepts see exactly
/// one winner even before the app-level flow runs.
#[tokio::test]
async fn invitation_claim_is_single_use() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let (inv_id, _token) = {
        let mut tx = db.begin(ws).await.unwrap();
        let r = auth_repo::create_invitation(
            &mut tx,
            "invitee@test.dev",
            "member",
            world.admin.principal_id,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        r
    };

    // Two concurrent claims — the row lock serializes them; only one wins.
    let (a, b) = tokio::join!(
        async {
            let mut tx = db.begin(ws).await.unwrap();
            let r = auth_repo::mark_invitation_accepted(&mut tx, inv_id)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            r
        },
        async {
            let mut tx = db.begin(ws).await.unwrap();
            let r = auth_repo::mark_invitation_accepted(&mut tx, inv_id)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            r
        }
    );
    assert_ne!(a, b, "exactly one concurrent accept must win");

    // A third attempt sees accepted_at → false.
    let mut tx = db.begin(ws).await.unwrap();
    assert!(
        !auth_repo::mark_invitation_accepted(&mut tx, inv_id)
            .await
            .unwrap()
    );
    tx.rollback().await.unwrap();
}

/// R5: an expired lock resets the counter on the next attempt evaluation;
/// it does not ratchet into a permanent lock.
#[tokio::test]
async fn lockout_counter_resets_after_expiry() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let user_id = world.member_b.user_id.unwrap();

    // Simulate an expired lock at the failure cap.
    {
        let mut tx = db.begin_global().await.unwrap();
        sqlx::query(
            "UPDATE user_password_credentials
                SET failed_attempts = $2, locked_until = now() - interval '1 second'
              WHERE user_id = $1",
        )
        .bind(user_id)
        .bind(auth_repo::LOGIN_MAX_FAILURES)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    // Next failure starts a fresh window: attempts=1, no lock.
    let locked = auth_repo::record_login_failure(&db, user_id).await.unwrap();
    assert!(locked.is_none(), "expired lock must not re-lock");
    let mut tx = db.begin_global().await.unwrap();
    let (attempts, locked): (i32, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "SELECT failed_attempts, locked_until
           FROM user_password_credentials WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(attempts, 1, "counter resets after lock expiry");
    assert!(locked.is_none());
}

// -------------------------------------------------------- R11 / R12 jobs

/// R11: same dedupe key returns the existing job id instead of 409.
#[tokio::test]
async fn enqueue_dedupe_returns_existing_id() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let first = jobs::enqueue(&mut tx, "test.kind", 1, json!({}), Utc::now(), Some("k1"))
        .await
        .unwrap();
    let second = jobs::enqueue(
        &mut tx,
        "test.kind",
        1,
        json!({"x": 1}),
        Utc::now(),
        Some("k1"),
    )
    .await
    .unwrap();
    assert_eq!(first, second);
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id = $1 AND dedupe_key = 'k1'",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(count, 1);
    tx.rollback().await.unwrap();
}

/// R11: suppress → release → suppress enqueues a second pause job (the
/// dedupe key is per-suppression, not per-address).
#[tokio::test]
async fn resuppression_enqueues_second_pause_job() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let mut tx = db.begin(ws).await.unwrap();
    let addr = insert_email_address(&mut tx, "sup").await;
    let s1 = compliance_core::insert_suppression(
        &mut tx,
        addr,
        "marketing",
        "unsubscribe",
        "unsubscribe_link",
        Some(world.admin.principal_id),
        None,
    )
    .await
    .unwrap();
    compliance_core::release_suppression(&mut tx, s1, world.admin.principal_id, "ev-1")
        .await
        .unwrap();
    let s2 = compliance_core::insert_suppression(
        &mut tx,
        addr,
        "marketing",
        "unsubscribe",
        "unsubscribe_link",
        Some(world.admin.principal_id),
        None,
    )
    .await
    .unwrap();
    assert_ne!(s1, s2);
    let jobs_for_addr: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id = $1 AND kind = 'compliance.pause_enrollments'
            AND payload->>'email_address_id' = $2",
    )
    .bind(ws)
    .bind(addr.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(jobs_for_addr, 2, "re-suppression must enqueue a new job");
    tx.rollback().await.unwrap();
}

/// R12: lease-expired external-effect jobs go to `unknown` (never
/// re-submitted); normal jobs retry or go `dead` at the attempt cap.
#[tokio::test]
async fn reclaim_external_effect_and_max_attempts() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    // External-effect job claimed, then lease expires.
    {
        let mut tx = db.begin(ws).await.unwrap();
        for kind in ["booking.confirm", "booking.apply_change"] {
            let id = jobs::enqueue(&mut tx, kind, 1, json!({}), Utc::now(), Some(kind))
                .await
                .unwrap();
            let flagged: bool = sqlx::query_scalar(
                "SELECT external_effect FROM jobs WHERE workspace_id = $1 AND id = $2",
            )
            .bind(ws)
            .bind(id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
            assert!(flagged, "{kind} must be persisted as an external effect");
        }

        let id = jobs::enqueue(
            &mut tx,
            "sending.dispatch_outbound",
            1,
            json!({}),
            // Past timestamp — `now()` in the claim is frozen at tx start,
            // so an in-tx Utc::now() would not be runnable.
            Utc::now() - chrono::Duration::seconds(1),
            Some("ext1"),
        )
        .await
        .unwrap();
        let flagged: bool = sqlx::query_scalar(
            "SELECT external_effect FROM jobs WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert!(flagged, "enqueue must persist external_effect");
        let claimed = jobs::claim_lease(&mut tx, "t", 10, 60, &["sending.dispatch_outbound"])
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() - interval '1 second'
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(id)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    jobs::reclaim_expired(&db).await.unwrap();
    {
        let mut tx = db.begin(ws).await.unwrap();
        let (state, code): (String, Option<String>) = sqlx::query_as(
            "SELECT state, last_error_code FROM jobs
              WHERE workspace_id = $1 AND dedupe_key = 'ext1'",
        )
        .bind(ws)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(state, "unknown");
        assert_eq!(code.as_deref(), Some("LEASE_EXPIRED_UNKNOWN"));
        tx.rollback().await.unwrap();
    }

    // Normal job at MAX_ATTEMPTS is moved to dead at claim time, not leased.
    {
        let mut tx = db.begin(ws).await.unwrap();
        jobs::enqueue(
            &mut tx,
            "test.kind",
            1,
            json!({}),
            Utc::now() - chrono::Duration::seconds(1),
            Some("max1"),
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE jobs SET attempt_count = $2
              WHERE workspace_id = $1 AND dedupe_key = 'max1'",
        )
        .bind(ws)
        .bind(jobs::MAX_ATTEMPTS)
        .execute(tx.conn())
        .await
        .unwrap();
        let claimed = jobs::claim_lease(&mut tx, "t", 10, 60, &["test.kind"])
            .await
            .unwrap();
        assert!(claimed.is_empty());
        let (state, code): (String, Option<String>) = sqlx::query_as(
            "SELECT state, last_error_code FROM jobs
              WHERE workspace_id = $1 AND dedupe_key = 'max1'",
        )
        .bind(ws)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(state, "dead");
        assert_eq!(code.as_deref(), Some("MAX_ATTEMPTS"));
        tx.commit().await.unwrap();
    }

    // Expired lease on a normal job under the cap → retry_wait, not dead.
    {
        let mut tx = db.begin(ws).await.unwrap();
        let id = jobs::enqueue(
            &mut tx,
            "test.kind",
            1,
            json!({}),
            Utc::now() - chrono::Duration::seconds(1),
            Some("norm1"),
        )
        .await
        .unwrap();
        let claimed = jobs::claim_lease(&mut tx, "t", 10, 60, &["test.kind"])
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() - interval '1 second'
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(id)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        jobs::reclaim_expired(&db).await.unwrap();
        let mut tx = db.begin(ws).await.unwrap();
        let (state, code): (String, Option<String>) = sqlx::query_as(
            "SELECT state, last_error_code FROM jobs
              WHERE workspace_id = $1 AND dedupe_key = 'norm1'",
        )
        .bind(ws)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        assert_eq!(state, "retry_wait");
        assert_eq!(code.as_deref(), Some("LEASE_EXPIRED"));
        tx.rollback().await.unwrap();
    }
}

/// An expired outbound dispatch that crossed the submitting boundary is
/// fenced to `unknown` and reconciled by lookup; a queued row with no
/// attempt is safely returned to the normal retry path.
#[tokio::test]
async fn expired_send_dispatch_lease_recovers_business_state_atomically() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let (submitted_outbound, queued_outbound, submitted_job, queued_job) = {
        let mut tx = db.begin(ws).await.unwrap();
        let sender_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sender_identities (workspace_id, mail_account_id, address, display_name, status)
             VALUES ($1, $2, $3, 'Recovery Sender', 'verified') RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(format!("recovery-{}@test.dev", Uuid::new_v4().simple()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let channel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO delivery_channels
                (workspace_id, mail_account_id, sender_identity_id, connection_id,
                 provider, purpose, name)
             VALUES ($1, $2, $3, $4, 'zoho_mail', 'correspondence', $5) RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(sender_id)
        .bind(world.connection_id)
        .bind(format!("recovery-{}", Uuid::new_v4().simple()))
        .fetch_one(tx.conn())
        .await
        .unwrap();

        async fn outbound(
            tx: &mut Tx<'_>,
            world: &TestWorld,
            sender_id: Uuid,
            channel_id: Uuid,
        ) -> Uuid {
            let ws = world.workspace_id;
            let local = Uuid::new_v4().simple().to_string();
            let address_id: Uuid = sqlx::query_scalar(
                "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
                 VALUES ($1, $2, $2) RETURNING id",
            )
            .bind(ws)
            .bind(format!("recovery-{local}@example.com"))
            .fetch_one(tx.conn())
            .await
            .unwrap();
            sqlx::query_scalar(
                "INSERT INTO outbound_messages
                    (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
                     to_email_address_id, requested_by, purpose, from_address_snapshot,
                     to_address_snapshot, subject, body_text, payload_hash, status)
                 VALUES ($1, $2, $3, $4, $5, $6, 'correspondence', $7, $8, 'Recovery', 'Body', $9, 'queued')
                 RETURNING id",
            )
            .bind(ws)
            .bind(world.mail_account_id)
            .bind(channel_id)
            .bind(sender_id)
            .bind(address_id)
            .bind(world.admin.principal_id)
            .bind(format!("recovery-{local}@test.dev"))
            .bind(format!("recovery-{local}@example.com"))
            .bind(vec![0_u8; 32])
            .fetch_one(tx.conn())
            .await
            .unwrap()
        }

        let submitted_outbound = outbound(&mut tx, &world, sender_id, channel_id).await;
        let queued_outbound = outbound(&mut tx, &world, sender_id, channel_id).await;
        let submitted_job = jobs::enqueue(
            &mut tx,
            "sending.dispatch_outbound",
            1,
            json!({"outbound_message_id": submitted_outbound}),
            Utc::now() - chrono::Duration::seconds(5),
            Some(&format!("dispatch:{submitted_outbound}")),
        )
        .await
        .unwrap();
        let queued_job = jobs::enqueue(
            &mut tx,
            "sending.dispatch_outbound",
            1,
            json!({"outbound_message_id": queued_outbound}),
            Utc::now() - chrono::Duration::seconds(5),
            Some(&format!("dispatch:{queued_outbound}")),
        )
        .await
        .unwrap();
        let claimed = jobs::claim_lease(
            &mut tx,
            "recovery-worker",
            10,
            60,
            &["sending.dispatch_outbound"],
        )
        .await
        .unwrap();
        assert_eq!(claimed.len(), 2);
        let submitted = claimed.iter().find(|j| j.id == submitted_job).unwrap();
        sqlx::query(
            "INSERT INTO send_attempts
                (workspace_id, outbound_message_id, attempt_no, lease_token, status)
             VALUES ($1, $2, 1, $3, 'started')",
        )
        .bind(ws)
        .bind(submitted_outbound)
        .bind(submitted.lease_token.unwrap())
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE outbound_messages SET status = 'submitting', version = version + 1
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(submitted_outbound)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() - interval '1 second'
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(ws)
        .bind(vec![submitted_job, queued_job])
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (
            submitted_outbound,
            queued_outbound,
            submitted_job,
            queued_job,
        )
    };

    // The test binary also has reclaim coverage running concurrently, and
    // the global definer may already have handled these rows by this call.
    jobs::reclaim_expired(&db).await.unwrap();

    let mut tx = db.begin(ws).await.unwrap();
    let (submitted_state, submitted_code): (String, Option<String>) = sqlx::query_as(
        "SELECT state, last_error_code FROM jobs WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(submitted_job)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (queued_state, queued_code): (String, Option<String>) = sqlx::query_as(
        "SELECT state, last_error_code FROM jobs WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(queued_job)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (submitted_status, failure_code, version): (String, Option<String>, i64) = sqlx::query_as(
        "SELECT status, failure_code, version FROM outbound_messages
              WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(submitted_outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let queued_status: String = sqlx::query_scalar(
        "SELECT status FROM outbound_messages WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(queued_outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let attempt_state: (String, Option<String>) = sqlx::query_as(
        "SELECT status, error_code FROM send_attempts
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(submitted_outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let reconcile: (serde_json::Value, String, bool, chrono::DateTime<Utc>) = sqlx::query_as(
        "SELECT payload, dedupe_key, external_effect, available_at FROM jobs
          WHERE workspace_id = $1 AND kind = 'sending.reconcile_unknown'
            AND payload->>'outbound_message_id' = $2",
    )
    .bind(ws)
    .bind(submitted_outbound.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let event: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM outbox_events
          WHERE workspace_id = $1 AND event_type = 'outbound.unknown' AND aggregate_id = $2",
    )
    .bind(ws)
    .bind(submitted_outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let audit: (Uuid, String, serde_json::Value) = sqlx::query_as(
        "SELECT job_id, request_id, details FROM audit_logs
          WHERE workspace_id = $1 AND action = 'send.isolated_submitting' AND target_id = $2",
    )
    .bind(ws)
    .bind(submitted_outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(
        (submitted_state.as_str(), submitted_code.as_deref()),
        ("unknown", Some("LEASE_EXPIRED_UNKNOWN"))
    );
    assert_eq!(
        (queued_state.as_str(), queued_code.as_deref()),
        ("retry_wait", Some("LEASE_EXPIRED"))
    );
    assert_eq!(submitted_status, "unknown");
    assert_eq!(
        failure_code.as_deref(),
        Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN")
    );
    assert_eq!(version, 3);
    assert_eq!(
        queued_status, "queued",
        "pre-submit recovery must not invent a provider outcome"
    );
    assert_eq!(attempt_state.0, "unknown");
    assert_eq!(
        attempt_state.1.as_deref(),
        Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN")
    );
    assert_eq!(
        reconcile.0,
        json!({"outbound_message_id": submitted_outbound})
    );
    assert_eq!(reconcile.1, format!("reconcile:{submitted_outbound}:0"));
    assert!(!reconcile.2, "reconcile is a lookup-only job");
    assert!(reconcile.3 > Utc::now() + chrono::Duration::minutes(4));
    assert_eq!(
        event,
        json!({"outbound_message_id": submitted_outbound, "reason": "submitting_owner_lost"})
    );
    assert_eq!(audit.0, submitted_job);
    assert_eq!(audit.1, format!("job:{submitted_job}"));
    assert_eq!(audit.2["reason"], "owning job lease lost");
    assert_eq!(audit.2["outcome"], "ok");
}

// ------------------------------------------------------ R15 idempotency

fn scope(world: &TestWorld, key: &str) -> Scope {
    Scope {
        workspace_id: world.workspace_id,
        principal_or_capability: world.admin.principal_id.to_string(),
        endpoint: "POST /test".into(),
        key: key.into(),
    }
}

fn stored_ok() -> idempotency::Stored<Value> {
    idempotency::Stored {
        status: 200,
        body: json!({"done": true}),
    }
}

/// R15: a stale `processing` claim (lease expired) is taken over by an
/// identical request — a crashed leader does not burn the key.
#[tokio::test]
async fn stale_processing_claim_is_taken_over() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let sc = scope(&world, "stale-1");
    let hash = infrastructure::crypto::sha256_bytes(b"request-body");

    // Simulate a crashed leader: processing row with an expired lease.
    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query(
            "INSERT INTO idempotency_records
                (workspace_id, scope_key, endpoint, idempotency_key, request_hash,
                 state, locked_until, expires_at)
             VALUES ($1, $2, 'POST /test', 'stale-1', $3,
                     'processing', now() - interval '1 minute',
                     now() + interval '72 hours')",
        )
        .bind(world.workspace_id)
        .bind(sc.scope_key())
        .bind(&hash[..])
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let out = idempotency::run(&db, &sc, &hash, |tx| async { Ok((tx, stored_ok())) })
        .await
        .unwrap();
    assert!(matches!(out, Outcome::Fresh(_)), "takeover must run f");
}

/// R15: if a second request takes over an expired lease while the old
/// business future is still running, only the new leader may commit writes.
#[tokio::test]
async fn taken_over_leader_cannot_commit_business_writes() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let sc = scope(&world, "takeover-fence");
    let hash = infrastructure::crypto::sha256_bytes(b"same-request");
    let (claimed_tx, claimed_rx) = tokio::sync::oneshot::channel();
    let (resume_tx, resume_rx) = tokio::sync::oneshot::channel();

    let old_db = db.clone();
    let old_scope = sc.clone();
    let old_leader = tokio::spawn(async move {
        idempotency::run(&old_db, &old_scope, &hash, move |mut tx| async move {
            let _ = claimed_tx.send(());
            let _ = resume_rx.await;
            jobs::enqueue(
                &mut tx,
                "test.idempotency.stale",
                1,
                json!({}),
                Utc::now(),
                Some("stale-leader"),
            )
            .await?;
            Ok((
                tx,
                idempotency::Stored {
                    status: 200,
                    body: json!({"leader": "old"}),
                },
            ))
        })
        .await
    });

    claimed_rx.await.unwrap();
    // Simulate lease expiry while the old leader's business tx is open.
    sqlx::query(
        "UPDATE idempotency_records SET locked_until = now() - interval '1 second'
          WHERE workspace_id = $1 AND scope_key = $2 AND endpoint = $3 AND idempotency_key = $4",
    )
    .bind(world.workspace_id)
    .bind(sc.scope_key())
    .bind(&sc.endpoint)
    .bind(&sc.key)
    .execute(db.pool())
    .await
    .unwrap();

    let winner = idempotency::run(&db, &sc, &hash, |mut tx| async move {
        jobs::enqueue(
            &mut tx,
            "test.idempotency.winner",
            1,
            json!({}),
            Utc::now(),
            Some("new-leader"),
        )
        .await?;
        Ok((
            tx,
            idempotency::Stored {
                status: 200,
                body: json!({"leader": "new"}),
            },
        ))
    })
    .await
    .unwrap();
    assert!(matches!(winner, Outcome::Fresh(_)));

    resume_tx.send(()).unwrap();
    let old_error = old_leader.await.unwrap().unwrap_err();
    assert_eq!(old_error.code, ErrorCode::RequestInProgress);

    let stale_writes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id = $1 AND dedupe_key = 'stale-leader'",
    )
    .bind(world.workspace_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let winner_writes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE workspace_id = $1 AND dedupe_key = 'new-leader'",
    )
    .bind(world.workspace_id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(
        stale_writes, 0,
        "fenced leader's business tx must roll back"
    );
    assert_eq!(
        winner_writes, 1,
        "takeover leader's business write must commit"
    );
}

/// R15: an unexpired processing claim still yields 409 REQUEST_IN_PROGRESS
/// (after the bounded poll), and a same-key different-hash request gets
/// IDEMPOTENCY_KEY_REUSED.
#[tokio::test]
async fn live_processing_claim_conflicts() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let sc = scope(&world, "live-1");
    let hash = infrastructure::crypto::sha256_bytes(b"body");

    // Keep the leader's claim alive by holding its record uncommitted? No —
    // claim commits; simulate by inserting a fresh-leased processing row.
    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query(
            "INSERT INTO idempotency_records
                (workspace_id, scope_key, endpoint, idempotency_key, request_hash,
                 state, locked_until, expires_at)
             VALUES ($1, $2, 'POST /test', 'live-1', $3,
                     'processing', now() + interval '5 minutes',
                     now() + interval '72 hours')",
        )
        .bind(world.workspace_id)
        .bind(sc.scope_key())
        .bind(&hash[..])
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let err = idempotency::run(&db, &sc, &hash, |tx| async { Ok((tx, stored_ok())) })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::RequestInProgress);

    let err = idempotency::run(&db, &sc, b"different-body", |tx| async {
        Ok((tx, stored_ok()))
    })
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::IdempotencyKeyReused);
}

/// R15: records past `expires_at` are ignored entirely — the key is
/// claimable again.
#[tokio::test]
async fn expired_record_is_ignored() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let sc = scope(&world, "exp-1");
    let hash = infrastructure::crypto::sha256_bytes(b"body");
    {
        let mut tx = db.begin(world.workspace_id).await.unwrap();
        sqlx::query(
            "INSERT INTO idempotency_records
                (workspace_id, scope_key, endpoint, idempotency_key, request_hash,
                 state, response_status, response_body, locked_until, expires_at)
             VALUES ($1, $2, 'POST /test', 'exp-1', $3,
                     'completed', 200, '{}', NULL, now() - interval '1 minute')",
        )
        .bind(world.workspace_id)
        .bind(sc.scope_key())
        .bind(&hash[..])
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    let out = idempotency::run(&db, &sc, &hash, |tx| async { Ok((tx, stored_ok())) })
        .await
        .unwrap();
    assert!(matches!(out, Outcome::Fresh(_)));
}

// ------------------------------------------------- R16 public tokens

/// R16: the unsubscribe definer function resolves (workspace, row) from
/// the hash alone, including already-used tokens (repeat POST = idempotent
/// success, T24).
#[tokio::test]
async fn unsubscribe_token_resolves_workspace() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    // Token rows need an outbound message — build the channel chain first.
    let (addr, token) = {
        let mut tx = db.begin(ws).await.unwrap();
        let ident: Uuid = sqlx::query_scalar(
            "INSERT INTO sender_identities (workspace_id, mail_account_id, address, display_name)
             VALUES ($1, $2, 'shared@test.dev', 'Shared') RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
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
        let addr = insert_email_address(&mut tx, "unsub").await;
        let outbound: Uuid = sqlx::query_scalar(
            "INSERT INTO outbound_messages
                (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
                 to_email_address_id, requested_by, purpose,
                 from_address_snapshot, to_address_snapshot, subject, body_text, payload_hash)
             VALUES ($1, $2, $3, $4, $5, $6, 'marketing',
                     'shared@test.dev', 'unsub@example.com', 'hi', 'body',
                     decode(md5(random()::text), 'hex'))
             RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(channel)
        .bind(ident)
        .bind(addr)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let raw = compliance_core::issue_unsubscribe_token(&mut tx, addr, outbound)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        (addr, raw)
    };

    let loc = public_tokens::unsubscribe_token(&db, &token.token)
        .await
        .unwrap();
    assert_eq!(loc.workspace_id, ws);
    assert_eq!(loc.token_id, token.token_id);

    // Full-resolution wrapper returns the scoped tx + row.
    let (tx, row) = public_tokens::resolve_unsubscribe_token(&db, &token.token)
        .await
        .unwrap();
    assert_eq!(row.email_address_id, addr);
    assert!(row.used_at.is_none());
    tx.rollback().await.unwrap();

    // Used tokens still resolve (T24 idempotent repeat).
    {
        let mut tx = db.begin(ws).await.unwrap();
        assert!(
            compliance_core::consume_unsubscribe_token(&mut tx, &token.token)
                .await
                .unwrap()
        );
        tx.commit().await.unwrap();
    }
    let (tx, row) = public_tokens::resolve_unsubscribe_token(&db, &token.token)
        .await
        .unwrap();
    assert!(row.used_at.is_some(), "used token must still resolve");
    tx.rollback().await.unwrap();

    // Unknown token → NotFound.
    let err = public_tokens::unsubscribe_token(&db, "no-such-token")
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// R16: booking-link and booking-manage definer lookups resolve their
/// workspaces the same way.
#[tokio::test]
async fn booking_tokens_resolve_workspace() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;
    // token_hash is globally UNIQUE and the test DB persists — derive
    // unique tokens per seeded workspace.
    let link_token = format!("bl-token-{}", ws.simple());
    let manage_token = format!("bm-token-{}", ws.simple());
    let link_hash = infrastructure::crypto::sha256_bytes(link_token.as_bytes());
    let manage_hash = infrastructure::crypto::sha256_bytes(manage_token.as_bytes());

    {
        let mut tx = db.begin(ws).await.unwrap();
        // calendar_connections.connection_id is a composite FK
        // (workspace_id, connection_id, provider) — needs a zoho_calendar
        // integration connection, not the seeded zoho_mail one.
        let cal_conn: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id, provider, region, secret_ref, status, connected_by)
             VALUES ($1, 'zoho_calendar', 'us', 'test-secret-ref', 'active', $2)
             RETURNING id",
        )
        .bind(ws)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let cal: Uuid = sqlx::query_scalar(
            "INSERT INTO calendar_connections
                (workspace_id, provider, host_principal_id, connection_id,
                 provider_calendar_id, status)
             VALUES ($1, 'zoho_calendar', $2, $3, 'cal-1', 'active') RETURNING id",
        )
        .bind(ws)
        .bind(world.admin.principal_id)
        .bind(cal_conn)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let mt: Uuid = sqlx::query_scalar(
            "INSERT INTO meeting_types
                (workspace_id, host_principal_id, calendar_connection_id, name)
             VALUES ($1, $2, $3, 'Intro') RETURNING id",
        )
        .bind(ws)
        .bind(world.admin.principal_id)
        .bind(cal)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let link: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_links
                (workspace_id, meeting_type_id, host_principal_id, token_hash)
             VALUES ($1, $2, $3, $4) RETURNING id",
        )
        .bind(ws)
        .bind(mt)
        .bind(world.admin.principal_id)
        .bind(&link_hash[..])
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let guest = insert_email_address(&mut tx, "guest").await;
        let booking: Uuid = sqlx::query_scalar(
            "INSERT INTO bookings
                (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
                 guest_name, guest_timezone, starts_at, ends_at, occupied_start,
                 occupied_end, hold_expires_at)
             VALUES ($1, $2, $3, $4, 'Guest', 'UTC',
                     now() + interval '1 day', now() + interval '1 day 30 minutes',
                     now() + interval '1 day', now() + interval '1 day 30 minutes',
                     now() + interval '1 hour')
             RETURNING id",
        )
        .bind(ws)
        .bind(link)
        .bind(world.admin.principal_id)
        .bind(guest)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO booking_manage_tokens
                (workspace_id, booking_id, token_hash, expires_at)
             VALUES ($1, $2, $3, now() + interval '1 day')",
        )
        .bind(ws)
        .bind(booking)
        .bind(&manage_hash[..])
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }

    let loc = public_tokens::booking_link_token(&db, &link_token)
        .await
        .unwrap();
    assert_eq!(loc.workspace_id, ws);
    let loc = public_tokens::booking_manage_token(&db, &manage_token)
        .await
        .unwrap();
    assert_eq!(loc.workspace_id, ws);
}
