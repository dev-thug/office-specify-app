//! Fix-round-1 DB coverage: R18 launch guard (UPDATE+DELETE, forward-only),
//! R19 OLD+NEW parent checks on steps/enrollments, R20 FOR SHARE locking,
//! R21 approval-request transitions, R22 explicit grant matrix.
//!
//! Tests run under `api_runtime` (common::test_db) — a role that can
//! actually fire the triggers through RLS.

mod common;

use chrono::Utc;
use infrastructure::db::{Db, ROLE_REPORT, Tx};
use infrastructure::testing::{TestWorld, seed_workspace};
use uuid::Uuid;

async fn email_addr(tx: &mut Tx<'_>, local: &str) -> Uuid {
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

/// campaign → revision → channel chain. Returns (campaign, revision, channel).
async fn campaign_chain(tx: &mut Tx<'_>, world: &TestWorld) -> (Uuid, Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    // Unique per call — sender_identities is UNIQUE on (ws, account, address).
    let address = format!(
        "shared-{}@test.dev",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let ident: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities (workspace_id, mail_account_id, address, display_name)
         VALUES ($1, $2, $3, 'Shared') RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(&address)
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
    let campaign: Uuid = sqlx::query_scalar(
        "INSERT INTO campaigns (workspace_id, mail_account_id, name, owner_principal_id)
         VALUES ($1, $2, 'Guard test', $3) RETURNING id",
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
    (campaign, revision, channel)
}

async fn template_version(tx: &mut Tx<'_>, world: &TestWorld) -> Uuid {
    let ws = tx.workspace_id().unwrap();
    let t: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, 'T', $2) RETURNING id",
    )
    .bind(ws)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text,
             variable_schema, content_hash, created_by)
         VALUES ($1, $2, 1, 's', 'b', '{}', decode(md5(random()::text),'hex'), $3)
         RETURNING id",
    )
    .bind(ws)
    .bind(t)
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn step(tx: &mut Tx<'_>, revision: Uuid, tv: Uuid) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO campaign_steps
            (workspace_id, revision_id, template_version_id, step_order)
         VALUES ($1, $2, $3, 1) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(revision)
    .bind(tv)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

async fn launch(tx: &mut Tx<'_>, world: &TestWorld) -> (Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    let (_c, revision, channel) = campaign_chain(tx, world).await;
    let launch: Uuid = sqlx::query_scalar(
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
    .unwrap();
    (launch, revision)
}

async fn freeze_launch(tx: &mut Tx<'_>, launch: Uuid) {
    sqlx::query(
        "UPDATE campaign_launches
            SET status = 'frozen', frozen_at = now(),
                manifest_hash = decode(md5('m'), 'hex')
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(tx.workspace_id())
    .bind(launch)
    .execute(tx.conn())
    .await
    .unwrap();
}

/// Run `sql` expecting a 23514 guard rejection. Wrapped in a savepoint —
/// a rejected statement would otherwise abort the test transaction.
async fn expect_guard(tx: &mut Tx<'_>, sql: &str, binds: &[Uuid]) {
    sqlx::query("SAVEPOINT guard_check")
        .execute(tx.conn())
        .await
        .unwrap();
    let mut q = sqlx::query(sql).bind(tx.workspace_id());
    for b in binds {
        q = q.bind(*b);
    }
    let res = q.execute(tx.conn()).await;
    sqlx::query("ROLLBACK TO SAVEPOINT guard_check")
        .execute(tx.conn())
        .await
        .unwrap();
    let err = res.expect_err("expected guard rejection, statement succeeded");
    let msg = err.to_string();
    assert!(
        err.as_database_error()
            .is_some_and(|d| d.code().as_deref() == Some("23514")),
        "expected 23514 guard rejection, got: {msg}"
    );
}

// ------------------------------------------------------------- R18

/// R18: a launch past 'building' cannot regress to building, cannot have
/// frozen columns mutated, and cannot be deleted.
#[tokio::test]
async fn launch_guard_forward_only_and_no_delete() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let (l, _rev) = launch(&mut tx, &world).await;
    freeze_launch(&mut tx, l).await;

    // Backward status → rejected.
    expect_guard(
        &mut tx,
        "UPDATE campaign_launches SET status = 'building' WHERE workspace_id = $1 AND id = $2",
        &[l],
    )
    .await;
    // Frozen column mutation → rejected.
    expect_guard(
        &mut tx,
        "UPDATE campaign_launches SET revision_id = gen_random_uuid() WHERE workspace_id = $1 AND id = $2",
        &[l],
    )
    .await;
    expect_guard(
        &mut tx,
        "UPDATE campaign_launches SET requested_by = gen_random_uuid() WHERE workspace_id = $1 AND id = $2",
        &[l],
    )
    .await;
    // DELETE of a frozen launch → rejected.
    expect_guard(
        &mut tx,
        "DELETE FROM campaign_launches WHERE workspace_id = $1 AND id = $2",
        &[l],
    )
    .await;
    // Forward move is fine.
    sqlx::query(
        "UPDATE campaign_launches SET status = 'awaiting_approval' WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(l)
    .execute(tx.conn())
    .await
    .unwrap();
    // And still cannot regress.
    expect_guard(
        &mut tx,
        "UPDATE campaign_launches SET status = 'frozen' WHERE workspace_id = $1 AND id = $2",
        &[l],
    )
    .await;
    tx.rollback().await.unwrap();
}

// ------------------------------------------------------------- R19

/// R19: under a frozen revision, step UPDATE/DELETE is rejected via the
/// OLD parent; repointing INTO a frozen revision is rejected via the NEW
/// parent.
#[tokio::test]
async fn step_guard_checks_old_and_new_parent() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let (_c, rev_frozen, _ch) = campaign_chain(&mut tx, &world).await;
    let (_c2, rev_live, _ch2) = campaign_chain(&mut tx, &world).await;
    let tv = template_version(&mut tx, &world).await;
    let step_frozen = step(&mut tx, rev_frozen, tv).await;
    let step_live = step(&mut tx, rev_live, tv).await;
    sqlx::query(
        "UPDATE campaign_revisions SET status = 'frozen' WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(rev_frozen)
    .execute(tx.conn())
    .await
    .unwrap();

    // Mutate a step under the frozen revision → OLD-parent rejection.
    expect_guard(
        &mut tx,
        "UPDATE campaign_steps SET delay_hours = 5 WHERE workspace_id = $1 AND id = $2",
        &[step_frozen],
    )
    .await;
    // Repoint the frozen step to a live revision → still the OLD parent.
    expect_guard(
        &mut tx,
        "UPDATE campaign_steps SET revision_id = $3 WHERE workspace_id = $1 AND id = $2",
        &[step_frozen, rev_live],
    )
    .await;
    // Repoint a live step INTO the frozen revision → NEW-parent rejection.
    expect_guard(
        &mut tx,
        "UPDATE campaign_steps SET revision_id = $3 WHERE workspace_id = $1 AND id = $2",
        &[step_live, rev_frozen],
    )
    .await;
    // DELETE under the frozen revision → rejected.
    expect_guard(
        &mut tx,
        "DELETE FROM campaign_steps WHERE workspace_id = $1 AND id = $2",
        &[step_frozen],
    )
    .await;
    // INSERT into the frozen revision → rejected.
    expect_guard(
        &mut tx,
        "INSERT INTO campaign_steps (workspace_id, revision_id, template_version_id, step_order)
         VALUES ($1, $2, $3, 2)",
        &[rev_frozen, tv],
    )
    .await;
    tx.rollback().await.unwrap();
}

/// R19: enrollment guard — execution-state updates stay legal under a
/// frozen launch; repointing, inserting and deleting do not.
#[tokio::test]
async fn enrollment_guard_allows_execution_updates_only() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let (l, rev) = launch(&mut tx, &world).await;
    let addr = email_addr(&mut tx, "enr").await;
    let enr: Uuid = sqlx::query_scalar(
        "INSERT INTO campaign_enrollments
            (workspace_id, launch_id, revision_id, email_address_id)
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(ws)
    .bind(l)
    .bind(rev)
    .bind(addr)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (l2, _r2) = launch(&mut tx, &world).await;
    freeze_launch(&mut tx, l).await;

    // Execution-state update is legal under the frozen launch.
    sqlx::query(
        "UPDATE campaign_enrollments SET status = 'active', next_send_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(enr)
    .execute(tx.conn())
    .await
    .unwrap();
    // Repoint to another (building) launch → rejected via OLD parent.
    expect_guard(
        &mut tx,
        "UPDATE campaign_enrollments SET launch_id = $3 WHERE workspace_id = $1 AND id = $2",
        &[enr, l2],
    )
    .await;
    // Change email_address_id → non-execution column → rejected.
    let addr2 = email_addr(&mut tx, "enr2").await;
    expect_guard(
        &mut tx,
        "UPDATE campaign_enrollments SET email_address_id = $3 WHERE workspace_id = $1 AND id = $2",
        &[enr, addr2],
    )
    .await;
    // DELETE under frozen launch → rejected.
    expect_guard(
        &mut tx,
        "DELETE FROM campaign_enrollments WHERE workspace_id = $1 AND id = $2",
        &[enr],
    )
    .await;
    // INSERT into the frozen launch → rejected.
    expect_guard(
        &mut tx,
        "INSERT INTO campaign_enrollments
            (workspace_id, launch_id, revision_id, email_address_id)
         VALUES ($1, $2, $3, $4)",
        &[l, rev, addr],
    )
    .await;
    tx.rollback().await.unwrap();
}

// ------------------------------------------------------------- R20

/// R20: the guard SELECTs the parent FOR SHARE — a step mutation blocks on
/// an in-flight freeze and is then rejected after it commits.
#[tokio::test]
async fn step_guard_serializes_against_freeze() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let (rev, step_id) = {
        let mut tx = db.begin(ws).await.unwrap();
        let (_c, rev, _ch) = campaign_chain(&mut tx, &world).await;
        let tv = template_version(&mut tx, &world).await;
        let s = step(&mut tx, rev, tv).await;
        tx.commit().await.unwrap();
        (rev, s)
    };

    // Tx A freezes the revision but holds it open.
    let mut tx_a = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE campaign_revisions SET status = 'frozen' WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(rev)
    .execute(tx_a.conn())
    .await
    .unwrap();

    // Tx B's guard SELECT ... FOR SHARE blocks on A's row lock; after A
    // commits the read returns 'frozen' and the guard rejects.
    let db2 = db.clone();
    let blocker = tokio::spawn(async move {
        let mut tx_b = db2.begin(ws).await.unwrap();
        let res = sqlx::query(
            "UPDATE campaign_steps SET delay_hours = 9 WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(step_id)
        .execute(tx_b.conn())
        .await;
        let _ = tx_b.rollback().await;
        res
    });
    // Let B reach its blocked SELECT, then commit the freeze.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    tx_a.commit().await.unwrap();
    let res = blocker.await.unwrap();
    let err = res.unwrap_err();
    let is_guard = err
        .as_database_error()
        .is_some_and(|d| d.code().as_deref() == Some("23514"));
    assert!(is_guard, "post-freeze step update must be rejected: {err}");
}

// ------------------------------------------------------------- R21

/// R21: approval request status may only leave 'pending' toward a terminal
/// state; 'approved' requires a same-tx decision row; frozen fields are
/// immutable.
#[tokio::test]
async fn approval_request_transitions_guarded() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();

    let (l, _rev) = launch(&mut tx, &world).await;
    let hash = [7u8; 32];
    let req = infrastructure::repo::approvals::insert_request(
        &mut tx,
        world.admin.principal_id,
        infrastructure::repo::approvals::ApprovalTarget::CampaignLaunch(l),
        &hash,
        Utc::now() + chrono::Duration::days(1),
    )
    .await
    .unwrap();

    // 'approved' without a decision row → rejected.
    expect_guard(
        &mut tx,
        "UPDATE approval_requests SET status = 'approved' WHERE workspace_id = $1 AND id = $2",
        &[req],
    )
    .await;

    // expires_at mutation → rejected.
    expect_guard(
        &mut tx,
        "UPDATE approval_requests SET expires_at = now() + interval '9 days'
          WHERE workspace_id = $1 AND id = $2",
        &[req],
    )
    .await;
    // snapshot_hash mutation → rejected.
    expect_guard(
        &mut tx,
        "UPDATE approval_requests SET snapshot_hash = decode(md5('x'),'hex')
          WHERE workspace_id = $1 AND id = $2",
        &[req],
    )
    .await;

    // With a decision row in the same tx, 'approved' is legal.
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision, snapshot_hash, version_at_decision)
         VALUES ($1, $2, $3, 'approved', $4, 1)",
    )
    .bind(ws)
    .bind(req)
    .bind(world.manager.principal_id)
    .bind(&hash[..])
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE approval_requests SET status = 'approved', version = 2
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(req)
    .execute(tx.conn())
    .await
    .unwrap();

    // Terminal state is immutable.
    expect_guard(
        &mut tx,
        "UPDATE approval_requests SET status = 'pending' WHERE workspace_id = $1 AND id = $2",
        &[req],
    )
    .await;
    tx.rollback().await.unwrap();
}

/// F2: the recorded decision must MATCH the outcome — a 'rejected'
/// decision cannot approve a request, and an 'approved' decision cannot
/// reject one (migration 0007).
#[tokio::test]
async fn approval_status_requires_matching_decision() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();

    let (l1, _r1) = launch(&mut tx, &world).await;
    let (l2, _r2) = launch(&mut tx, &world).await;
    let hash = [9u8; 32];
    let expires = Utc::now() + chrono::Duration::days(1);
    let req_rejected = infrastructure::repo::approvals::insert_request(
        &mut tx,
        world.admin.principal_id,
        infrastructure::repo::approvals::ApprovalTarget::CampaignLaunch(l1),
        &hash,
        expires,
    )
    .await
    .unwrap();
    let req_approved = infrastructure::repo::approvals::insert_request(
        &mut tx,
        world.admin.principal_id,
        infrastructure::repo::approvals::ApprovalTarget::CampaignLaunch(l2),
        &hash,
        expires,
    )
    .await
    .unwrap();

    // Same-tx decision rows while both requests are still pending.
    for (req, decision) in [(req_rejected, "rejected"), (req_approved, "approved")] {
        sqlx::query(
            "INSERT INTO approval_decisions
                (workspace_id, approval_request_id, decided_by, decision, snapshot_hash, version_at_decision)
             VALUES ($1, $2, $3, $4, $5, 1)",
        )
        .bind(ws)
        .bind(req)
        .bind(world.manager.principal_id)
        .bind(decision)
        .bind(&hash[..])
        .execute(tx.conn())
        .await
        .unwrap();
    }

    // A rejection on record cannot be flipped to approved (0006's bug).
    expect_guard(
        &mut tx,
        "UPDATE approval_requests SET status = 'approved' WHERE workspace_id = $1 AND id = $2",
        &[req_rejected],
    )
    .await;
    // Nor can an approval be recorded then rejected.
    expect_guard(
        &mut tx,
        "UPDATE approval_requests SET status = 'rejected' WHERE workspace_id = $1 AND id = $2",
        &[req_approved],
    )
    .await;
    // Matching transitions succeed.
    for (req, status) in [(req_rejected, "rejected"), (req_approved, "approved")] {
        sqlx::query("UPDATE approval_requests SET status = $3 WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(req)
            .bind(status)
            .execute(tx.conn())
            .await
            .unwrap();
    }
    tx.rollback().await.unwrap();
}

// ------------------------------------------------------------- R22

/// R22: the grant matrix — assert each ruled privilege via
/// has_table_privilege / has_column_privilege.
#[tokio::test]
async fn grant_matrix_matches_least_privilege() {
    let db = common::test_db().await.unwrap();

    async fn priv_of(db: &Db, role: &str, table: &str, priv_: &str) -> bool {
        sqlx::query_scalar(&format!(
            "SELECT has_table_privilege('{role}', '{table}', '{priv_}')"
        ))
        .fetch_one(db.pool())
        .await
        .unwrap()
    }

    // _sqlx_migrations: no access for any runtime role.
    for role in ["api_runtime", "worker_runtime", "report_reader"] {
        for p in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            assert!(
                !priv_of(&db, role, "_sqlx_migrations", p).await,
                "{role} must not have {p} on _sqlx_migrations"
            );
        }
    }

    // worker_runtime: no access to auth tables.
    for t in [
        "user_password_credentials",
        "user_sessions",
        "member_invitations",
        "service_credentials",
    ] {
        for p in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            assert!(
                !priv_of(&db, "worker_runtime", t, p).await,
                "worker_runtime must not have {p} on {t}"
            );
        }
    }

    // api_runtime: no DELETE on append-only / approval / queue tables (R22).
    // campaign_launches is not in that set — its trigger guards DELETE.
    for t in [
        "audit_logs",
        "approval_decisions",
        "suppression_events",
        "processed_events",
        "jobs",
        "outbox_events",
        "webhook_events",
        "approval_requests",
    ] {
        assert!(
            !priv_of(&db, "api_runtime", t, "DELETE").await,
            "api_runtime must not DELETE {t}"
        );
    }
    // …and audit trail is not rewritable.
    for t in [
        "audit_logs",
        "approval_decisions",
        "suppression_events",
        "processed_events",
    ] {
        assert!(
            !priv_of(&db, "api_runtime", t, "UPDATE").await,
            "api_runtime must not UPDATE {t}"
        );
    }

    // idempotency_records is the one queue-adjacent table needing DELETE.
    assert!(priv_of(&db, "api_runtime", "idempotency_records", "DELETE").await);

    // report_reader: no secret/credential/session/token tables.
    for t in [
        "user_password_credentials",
        "user_sessions",
        "service_credentials",
        "member_invitations",
        "unsubscribe_tokens",
        "booking_manage_tokens",
        "idempotency_records",
    ] {
        assert!(
            !priv_of(&db, "report_reader", t, "SELECT").await,
            "report_reader must not read {t}"
        );
    }
    // report_reader is SELECT-only on readable tables.
    for p in ["INSERT", "UPDATE", "DELETE"] {
        assert!(
            !priv_of(&db, "report_reader", "companies", p).await,
            "report_reader must not {p}"
        );
    }
    assert!(priv_of(&db, "report_reader", "companies", "SELECT").await);

    // email_messages: metadata readable, bodies excluded.
    for (col, expected) in [
        ("subject", true),
        ("body_text", false),
        ("body_html", false),
    ] {
        let has: bool = sqlx::query_scalar(&format!(
            "SELECT has_column_privilege('report_reader', 'email_messages', '{col}', 'SELECT')"
        ))
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(has, expected, "report_reader email_messages.{col}");
    }
}

// ------------------------------------------------------------- F3

/// F3: report_reader gets only column-level SELECT on outbound_messages
/// and message_drafts — metadata stays readable, content columns
/// (subject/body_text/body_html, and drafts' `variables` merge data) are
/// denied outright (migration 0007).
#[tokio::test]
async fn report_reader_cannot_read_message_bodies() {
    let db = common::test_db().await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    // Seed one outbound message + one draft as api_runtime.
    let (outbound_id, draft_id) = {
        let mut tx = db.begin(ws).await.unwrap();
        let (_c, _rev, channel) = campaign_chain(&mut tx, &world).await;
        let ident: Uuid = sqlx::query_scalar(
            "SELECT sender_identity_id FROM delivery_channels
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(channel)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let addr = email_addr(&mut tx, "f3").await;
        let outbound: Uuid = sqlx::query_scalar(
            "INSERT INTO outbound_messages
                (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
                 to_email_address_id, requested_by, purpose,
                 from_address_snapshot, to_address_snapshot, subject, body_text, payload_hash)
             VALUES ($1, $2, $3, $4, $5, $6, 'marketing',
                     'shared@test.dev', 'f3@example.com', 'secret subject', 'secret body',
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
        let draft: Uuid = sqlx::query_scalar(
            "INSERT INTO message_drafts
                (workspace_id, mail_account_id, owner_principal_id, to_email_address_id,
                 purpose, subject, body_text)
             VALUES ($1, $2, $3, $4, 'correspondence', 'draft subject', 'draft body')
             RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(world.admin.principal_id)
        .bind(addr)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (outbound, draft)
    };

    // Privilege matrix — column-level; content columns denied.
    for (table, col, expected) in [
        ("outbound_messages", "id", true),
        ("outbound_messages", "status", true),
        ("outbound_messages", "to_address_snapshot", true),
        ("outbound_messages", "subject", false),
        ("outbound_messages", "body_text", false),
        ("outbound_messages", "body_html", false),
        ("message_drafts", "id", true),
        ("message_drafts", "status", true),
        ("message_drafts", "subject", false),
        ("message_drafts", "body_text", false),
        ("message_drafts", "body_html", false),
        ("message_drafts", "variables", false),
    ] {
        let has: bool = sqlx::query_scalar(&format!(
            "SELECT has_column_privilege('report_reader', '{table}', '{col}', 'SELECT')"
        ))
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(has, expected, "report_reader {table}.{col}");
    }

    // Behavioral: a report_reader-scoped workspace tx reads id/status but
    // is denied content columns (42501 insufficient_privilege).
    let url = std::env::var("DATABASE_URL").unwrap();
    let rdb = Db::connect(&url, Some(ROLE_REPORT)).await.unwrap();
    let mut rtx = rdb.begin(ws).await.unwrap();
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, status FROM outbound_messages WHERE id = $1")
            .bind(outbound_id)
            .fetch_all(rtx.conn())
            .await
            .unwrap();
    assert_eq!(rows.len(), 1, "metadata must stay readable");
    let drafts: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, status FROM message_drafts WHERE id = $1")
            .bind(draft_id)
            .fetch_all(rtx.conn())
            .await
            .unwrap();
    assert_eq!(drafts.len(), 1);

    for sql in [
        "SELECT body_text FROM outbound_messages",
        "SELECT subject FROM outbound_messages",
        "SELECT body_text FROM message_drafts",
        "SELECT subject, variables FROM message_drafts",
    ] {
        // Savepoint per denied statement — a rejected query aborts the tx.
        sqlx::query("SAVEPOINT f3_check")
            .execute(rtx.conn())
            .await
            .unwrap();
        let res = sqlx::query(sql).fetch_all(rtx.conn()).await;
        sqlx::query("ROLLBACK TO SAVEPOINT f3_check")
            .execute(rtx.conn())
            .await
            .unwrap();
        let err = match res {
            Err(e) => e,
            Ok(_) => panic!("{sql} must be denied for report_reader"),
        };
        assert!(
            err.as_database_error()
                .is_some_and(|d| d.code().as_deref() == Some("42501")),
            "{sql}: expected 42501, got {err}"
        );
    }
    rtx.rollback().await.unwrap();
}
