//! Task 3 org/mailboxes security invariants at the repo layer:
//!
//! - T02 helper: `mailboxes::grant_allows` — no grant ⇒ no action, admins
//!   get nothing implicit (content access is grant-only, §5.6/A.7).
//! - service credentials: `token_hash` stores sha256(token), never
//!   plaintext; `key_prefix` matches the issued `osk_…` secret.
//! - pending-work cancellation: `cancel_pending_jobs_for_{principal,
//!   credential}` touch only `queued`/`retry_wait` — running/unknown/
//!   finished jobs are never modified (§5.5).
//! - `emergency_stop_active` fails closed and tracks the flag.
//! - `reassign_principal_ownership` moves companies/contacts/open
//!   tasks/threads only.

mod common;

use infrastructure::crypto;
use infrastructure::repo::{mailboxes as mrepo, org as repo};
use infrastructure::testing::seed_workspace;
use uuid::Uuid;

/// Insert a job row in a given state sourced from a credential.
async fn insert_job(
    tx: &mut infrastructure::Tx<'_>,
    kind: &str,
    state: &str,
    requested_by: Option<Uuid>,
    cred: Option<Uuid>,
) -> Uuid {
    // jobs_check: 'running' rows must carry lease fields.
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO jobs
            (workspace_id, kind, payload_version, payload, state, dedupe_key,
             requested_by, source_credential_id,
             lease_owner, lease_token, lease_expires_at)
         VALUES ($1, $2, 1, '{}', $3, $4, $5, $6,
                 CASE WHEN $3 = 'running' THEN 'w-test' END,
                 CASE WHEN $3 = 'running' THEN gen_random_uuid() END,
                 CASE WHEN $3 = 'running' THEN now() + interval '60 seconds' END)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(kind)
    .bind(state)
    .bind(format!("t3-{state}-{}", Uuid::new_v4()))
    .bind(requested_by)
    .bind(cred)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    id
}

async fn job_state(tx: &mut infrastructure::Tx<'_>, id: Uuid) -> String {
    sqlx::query_scalar::<_, String>("SELECT state FROM jobs WHERE workspace_id = $1 AND id = $2")
        .bind(tx.workspace_id())
        .bind(id)
        .fetch_one(tx.conn())
        .await
        .unwrap()
}

async fn fresh_human(db: &infrastructure::Db, ws: Uuid, role: &str) -> Uuid {
    // Principal without any mailbox grant (kind human — no user needed for
    // repo-level tests? the DDL CHECK requires user_id for humans).
    let email = format!("h-{}@test.dev", &Uuid::new_v4().simple().to_string()[..8]);
    let user = infrastructure::repo::auth::create_user(db, &email, "NoGrant")
        .await
        .unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    let pid = infrastructure::repo::auth::insert_principal(
        &mut tx,
        "human",
        Some(user.id),
        "NoGrant",
        role,
        "active",
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    pid
}

// --------------------------------------------------------------------------
// T02 — grant_allows: grant-only mailbox access, no implicit admin
// --------------------------------------------------------------------------

#[tokio::test]
async fn t02_grant_allows_denies_without_grant_even_for_admin() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let acct = world.mail_account_id;

    // Granted member (member_a got the full seed grant).
    for action in ["read", "compose", "send", "approve", "assign"] {
        assert!(
            mrepo::grant_allows(&mut tx, world.member_a.principal_id, acct, action)
                .await
                .unwrap(),
            "member_a must hold {action}"
        );
    }
    // `mail.*` wire names normalize too.
    assert!(
        mrepo::grant_allows(&mut tx, world.member_a.principal_id, acct, "mail.send")
            .await
            .unwrap()
    );

    // member_b (no grant) → every action denied.
    for action in ["read", "compose", "send", "approve", "assign"] {
        assert!(
            !mrepo::grant_allows(&mut tx, world.member_b.principal_id, acct, action)
                .await
                .unwrap(),
            "member_b must not hold {action}"
        );
    }

    // A grantless ADMIN is equally denied — no implicit content access.
    let admin2 = fresh_human(&db, world.workspace_id, "admin").await;
    let mut tx2 = db.begin(world.workspace_id).await.unwrap();
    for action in ["read", "send", "approve"] {
        assert!(
            !mrepo::grant_allows(&mut tx2, admin2, acct, action)
                .await
                .unwrap(),
            "grantless admin must not hold {action} (T02)"
        );
    }
    let _ = tx2.rollback().await;

    // Unknown action names are caller bugs → validation error, not false.
    let err = mrepo::grant_allows(&mut tx, world.member_a.principal_id, acct, "hack")
        .await
        .unwrap_err();
    assert_eq!(err.code, contracts::ErrorCode::ValidationFailed);
    let _ = tx.rollback().await;
}

// --------------------------------------------------------------------------
// service credentials — sha256 at rest, never plaintext
// --------------------------------------------------------------------------

#[tokio::test]
async fn service_credential_stores_sha256_not_plaintext() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();

    let (cred_id, token) = infrastructure::repo::auth::create_service_credential(
        &mut tx,
        world.agent.principal_id,
        "ci",
        &["mail.read".to_string()],
        chrono::Duration::days(30),
    )
    .await
    .unwrap();
    assert!(token.starts_with("osk_"));

    let (hash, prefix): (Vec<u8>, String) = sqlx::query_as(
        "SELECT token_hash, key_prefix FROM service_credentials
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(cred_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let expected = crypto::sha256_bytes(token.as_bytes());
    assert_eq!(hash, expected.to_vec(), "token_hash must be sha256(token)");
    assert_ne!(hash, token.as_bytes(), "plaintext must never be stored");
    assert!(
        token.starts_with(&prefix),
        "key_prefix must match the token"
    );
}

// --------------------------------------------------------------------------
// pending-work cancellation scoping (§5.5)
// --------------------------------------------------------------------------

#[tokio::test]
async fn cancel_pending_jobs_for_credential_touches_only_pending() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();

    // A credential on the agent to attribute jobs to.
    let mut setup = db.begin(world.workspace_id).await.unwrap();
    let (cred_id, _tok) = infrastructure::repo::auth::create_service_credential(
        &mut setup,
        world.agent.principal_id,
        "ci",
        &[],
        chrono::Duration::days(30),
    )
    .await
    .unwrap();

    let mut pending = vec![];
    for state in ["queued", "retry_wait"] {
        pending.push(insert_job(&mut setup, "some.job", state, None, Some(cred_id)).await);
    }
    let mut untouched = vec![];
    for state in ["running", "unknown", "completed", "dead", "canceled"] {
        untouched.push(insert_job(&mut setup, "some.job", state, None, Some(cred_id)).await);
    }
    // Another credential's queued job must survive.
    let (other_cred, _t) = infrastructure::repo::auth::create_service_credential(
        &mut setup,
        world.agent.principal_id,
        "other",
        &[],
        chrono::Duration::days(30),
    )
    .await
    .unwrap();
    let foreign = insert_job(&mut setup, "some.job", "queued", None, Some(other_cred)).await;
    setup.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let n = repo::cancel_pending_jobs_for_credential(&mut tx, cred_id)
        .await
        .unwrap();
    assert_eq!(n, 2, "only queued+retry_wait cancel");
    for id in pending {
        assert_eq!(job_state(&mut tx, id).await, "canceled");
    }
    for (id, state) in untouched
        .iter()
        .zip(["running", "unknown", "completed", "dead", "canceled"])
    {
        assert_eq!(
            job_state(&mut tx, *id).await,
            state,
            "{state} job must be untouched"
        );
    }
    assert_eq!(job_state(&mut tx, foreign).await, "queued");
    let _ = tx.rollback().await;
}

#[tokio::test]
async fn principal_and_credential_revocation_cancel_only_pending_jobs() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();

    let mut setup = db.begin(world.workspace_id).await.unwrap();
    let (cred_id, _t) = infrastructure::repo::auth::create_service_credential(
        &mut setup,
        world.agent.principal_id,
        "agent-key",
        &[],
        chrono::Duration::days(30),
    )
    .await
    .unwrap();
    let requested = insert_job(
        &mut setup,
        "some.job",
        "queued",
        Some(world.agent.principal_id),
        None,
    )
    .await;
    let cred_sourced = insert_job(&mut setup, "some.job", "queued", None, Some(cred_id)).await;
    let running = insert_job(
        &mut setup,
        "some.job",
        "running",
        Some(world.agent.principal_id),
        None,
    )
    .await;
    let someone_elses = insert_job(
        &mut setup,
        "some.job",
        "queued",
        Some(world.member_a.principal_id),
        None,
    )
    .await;
    setup.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let n = repo::cancel_pending_jobs_for_principal(&mut tx, world.agent.principal_id)
        .await
        .unwrap();
    assert_eq!(n, 1, "principal revocation cancels directly requested work");
    assert_eq!(job_state(&mut tx, requested).await, "canceled");
    assert_eq!(
        job_state(&mut tx, cred_sourced).await,
        "queued",
        "worker principal cleanup does not read credentials"
    );
    let n = repo::cancel_pending_jobs_for_credential(&mut tx, cred_id)
        .await
        .unwrap();
    assert_eq!(n, 1, "credential revocation cancels its own pending work");
    assert_eq!(job_state(&mut tx, cred_sourced).await, "canceled");
    assert_eq!(job_state(&mut tx, running).await, "running");
    assert_eq!(job_state(&mut tx, someone_elses).await, "queued");
    let _ = tx.rollback().await;
}

// --------------------------------------------------------------------------
// emergency stop repository gate (exposed for other modules, §8.4)
// --------------------------------------------------------------------------

#[tokio::test]
async fn emergency_stop_active_defaults_true_and_follows_flag() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();

    assert!(
        repo::emergency_stop_active(&mut tx).await.unwrap(),
        "DDL default is engaged"
    );
    let (enabled, v) = repo::get_workspace_state(&mut tx).await.unwrap().unwrap();
    assert!(enabled);

    let v2 = repo::set_emergency_stop(&mut tx, false, v)
        .await
        .unwrap()
        .unwrap();
    assert!(!repo::emergency_stop_active(&mut tx).await.unwrap());
    // Stale version → None (optimistic ETag miss).
    assert!(
        repo::set_emergency_stop(&mut tx, true, v)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        repo::set_emergency_stop(&mut tx, true, v2)
            .await
            .unwrap()
            .is_some()
    );
    assert!(repo::emergency_stop_active(&mut tx).await.unwrap());
    let _ = tx.rollback().await;
}

// --------------------------------------------------------------------------
// reassignment (A.7 hand-off inside org.disable_principal)
// --------------------------------------------------------------------------

#[tokio::test]
async fn reassign_moves_owned_records_not_meetings() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let from = world.member_b.principal_id;
    let to = world.manager.principal_id;

    let mut setup = db.begin(world.workspace_id).await.unwrap();
    let company: Uuid = sqlx::query_scalar(
        "INSERT INTO companies (workspace_id, name, owner_principal_id)
         VALUES ($1, 'Acme', $2) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(from)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    let contact: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name, owner_principal_id)
         VALUES ($1, 'Ann', $2) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(from)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    let open_task: Uuid = sqlx::query_scalar(
        "INSERT INTO tasks (workspace_id, title, owner_principal_id, status)
         VALUES ($1, 'follow up', $2, 'open') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(from)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    let done_task: Uuid = sqlx::query_scalar(
        "INSERT INTO tasks (workspace_id, title, owner_principal_id, status)
         VALUES ($1, 'done thing', $2, 'done') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(from)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    let thread: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id, assigned_principal_id)
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(from)
    .fetch_one(setup.conn())
    .await
    .unwrap();
    setup.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let (companies, contacts, tasks, threads) =
        repo::reassign_principal_ownership(&mut tx, from, to)
            .await
            .unwrap();
    assert_eq!((companies, contacts, tasks, threads), (1, 1, 1, 1));

    // Owners actually moved; closed tasks stay put.
    let owners: Vec<Option<Uuid>> = sqlx::query_scalar(
        "SELECT owner_principal_id FROM companies WHERE workspace_id = $1 AND id = $2
         UNION ALL
         SELECT owner_principal_id FROM contacts WHERE workspace_id = $1 AND id = $3
         UNION ALL
         SELECT owner_principal_id FROM tasks WHERE workspace_id = $1 AND id = $4
         UNION ALL
         SELECT assigned_principal_id FROM email_threads WHERE workspace_id = $1 AND id = $5",
    )
    .bind(world.workspace_id)
    .bind(company)
    .bind(contact)
    .bind(open_task)
    .bind(thread)
    .fetch_all(tx.conn())
    .await
    .unwrap();
    assert!(owners.iter().all(|o| *o == Some(to)));
    let done_owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT owner_principal_id FROM tasks WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(done_task)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(done_owner, Some(from), "closed tasks keep their owner");
    let _ = tx.rollback().await;
}
