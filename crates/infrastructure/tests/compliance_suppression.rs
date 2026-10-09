//! Compliance repository + compliance_core behaviour tests.
//! Spec refs: §11, A.6.4, B.4.4, B.5.
//! Uses the same seeded world + RLS-enforcing Db as the other infra tests.

mod common;

use infrastructure::db::Tx;
use infrastructure::repo::compliance as repo;
use infrastructure::repo::compliance::test_fixtures as fixtures;
use infrastructure::repo::compliance_core;
use infrastructure::testing::{TestWorld, seed_workspace};
use uuid::Uuid;

async fn insert_email_address(tx: &mut Tx<'_>, local: &str) -> Uuid {
    fixtures::insert_email_address(tx, &format!("{local}@example.com"))
        .await
        .unwrap()
}

async fn insert_outbound(tx: &mut Tx<'_>, world: &TestWorld, to: Uuid) -> Uuid {
    fixtures::insert_outbound(
        tx,
        world.mail_account_id,
        world.connection_id,
        world.admin.principal_id,
        to,
    )
    .await
    .unwrap()
}

/// Insert a suppression via the shared core (lock order A.6.4).
async fn suppress(tx: &mut Tx<'_>, email_address_id: Uuid, scope: &str, reason: &str) -> Uuid {
    assert!(
        compliance_core::lock_email_address(tx, email_address_id)
            .await
            .unwrap()
    );
    compliance_core::insert_suppression(
        tx,
        email_address_id,
        scope,
        reason,
        "unsubscribe_link",
        None,
        None,
    )
    .await
    .unwrap()
}

/// T11 — suppressions are workspace-confined: a second workspace's
/// suppression must not leak through RLS, and `active_suppressions` only
/// returns the matching purpose scope.
#[tokio::test]
async fn t11_suppression_blocks_and_scope_confined() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let a = seed_workspace(&db).await.unwrap();
    let b = seed_workspace(&db).await.unwrap();

    let mut tx = db.begin(a.workspace_id).await.unwrap();
    let addr_a = insert_email_address(&mut tx, "victim").await;
    suppress(&mut tx, addr_a, "marketing", "unsubscribe").await;
    tx.commit().await.unwrap();

    // Workspace B sees nothing of A's data — not even the same address text.
    let mut tx = db.begin(b.workspace_id).await.unwrap();
    let rows = repo::list_suppressions(&mut tx, &repo::SuppressionFilter::default(), 200)
        .await
        .unwrap();
    assert!(rows.is_empty(), "B must not see A's suppressions");
    tx.rollback().await.unwrap();

    // marketing scope does not block correspondence.
    let mut tx = db.begin(a.workspace_id).await.unwrap();
    assert!(
        compliance_core::active_suppressions(&mut tx, addr_a, "correspondence")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        compliance_core::active_suppressions(&mut tx, addr_a, "marketing")
            .await
            .unwrap()
            .len(),
        1
    );
    tx.rollback().await.unwrap();
}

/// T11 — `insert_suppression` is dedup-idempotent: creating twice for the
/// same (address, scope, reason) reaffirms the existing row.
#[tokio::test]
async fn t11_insert_suppression_idempotent_reaffirm() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "vip").await;

    let first = suppress(&mut tx, addr, "marketing", "unsubscribe").await;
    let second = suppress(&mut tx, addr, "marketing", "unsubscribe").await;
    assert_eq!(first, second, "repeat insert must reaffirm, not conflict");

    // Both events land in the same tx → identical occurred_at/created_at;
    // compare as a multiset (one created + one reaffirmed).
    let mut actions: Vec<String> =
        sqlx::query_scalar("SELECT action FROM suppression_events WHERE suppression_id = $1")
            .bind(first)
            .fetch_all(tx.conn())
            .await
            .unwrap();
    actions.sort();
    assert_eq!(actions, vec!["created", "reaffirmed"]);

    // A different reason is a distinct row (§11.1 keeps each reason).
    let third = suppress(&mut tx, addr, "marketing", "complaint").await;
    assert_ne!(first, third);
    tx.rollback().await.unwrap();
}

/// T12 — suppression committed while a submit is in-flight: the durable
/// row records immediately and the *final* dispatch-time permit check —
/// the same `active_suppressions` read the send worker re-runs after the
/// submitting boundary (§8.4, A.6.4) — sees it. No new submits pass.
#[tokio::test]
async fn t12_suppression_committed_midflight_blocks_permit_check() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();

    // Setup: the address exists before any submit is attempted.
    let mut setup = db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut setup, "midflight").await;
    setup.commit().await.unwrap();

    // t1 — submit authorization: the permit check passes, so a submit is
    // now "in-flight" toward the external boundary.
    let mut auth = db.begin(world.workspace_id).await.unwrap();
    assert!(
        compliance_core::active_suppressions(&mut auth, addr, "marketing")
            .await
            .unwrap()
            .is_empty(),
        "no suppression at submit authorization"
    );
    auth.rollback().await.unwrap();

    // t2 — a concurrent unsubscribe commits durably in its own tx (the
    // suppression never waits on the in-flight submit to finish).
    let mut unsub = db.begin(world.workspace_id).await.unwrap();
    assert!(
        compliance_core::lock_email_address(&mut unsub, addr)
            .await
            .unwrap()
    );
    compliance_core::insert_suppression(
        &mut unsub,
        addr,
        "marketing",
        "unsubscribe",
        "unsubscribe_link",
        None,
        None,
    )
    .await
    .unwrap();
    unsub.commit().await.unwrap();

    // t3 — the dispatch-time re-check (same `active_suppressions` read the
    // send worker re-runs under the address lock) sees the committed row:
    // no new submits for this address pass (T12).
    let mut dispatch = db.begin(world.workspace_id).await.unwrap();
    assert!(
        compliance_core::lock_email_address(&mut dispatch, addr)
            .await
            .unwrap()
    );
    assert_eq!(
        compliance_core::active_suppressions(&mut dispatch, addr, "marketing")
            .await
            .unwrap()
            .len(),
        1,
        "final suppression check must deny new submits"
    );
    dispatch.rollback().await.unwrap();
}

/// T12 — `release_suppression` clears the block, requires evidence +
/// releaser (DDL), records the `released` event, and is idempotent.
#[tokio::test]
async fn t12_release_clears_active() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "vip").await;
    let sid = suppress(&mut tx, addr, "marketing", "manual").await;
    assert_eq!(
        compliance_core::active_suppressions(&mut tx, addr, "marketing")
            .await
            .unwrap()
            .len(),
        1
    );

    assert!(
        compliance_core::release_suppression(&mut tx, sid, world.admin.principal_id, "ticket-123")
            .await
            .unwrap()
    );
    assert!(
        compliance_core::active_suppressions(&mut tx, addr, "marketing")
            .await
            .unwrap()
            .is_empty()
    );
    // Second release is a no-op — no duplicate event.
    assert!(
        !compliance_core::release_suppression(&mut tx, sid, world.admin.principal_id, "ticket-123")
            .await
            .unwrap()
    );
    let row = repo::get_suppression(&mut tx, sid).await.unwrap().unwrap();
    assert!(row.released_at.is_some());
    assert_eq!(row.released_by, Some(world.admin.principal_id));
    assert_eq!(row.release_evidence_ref.as_deref(), Some("ticket-123"));
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppression_events WHERE suppression_id = $1 AND action = 'released'",
    )
    .bind(sid)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(n, 1);
    tx.rollback().await.unwrap();
}

/// T32 — suppression survives contact delete/re-import at data level: the
/// row keys on `email_addresses`, not `contacts`, so contact churn never
/// drops the block (§11.2).
#[tokio::test]
async fn t32_suppression_survives_contact_delete_and_reimport() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "gone").await;
    let contact: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Gone Person') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, true)",
    )
    .bind(world.workspace_id)
    .bind(contact)
    .bind(addr)
    .execute(tx.conn())
    .await
    .unwrap();
    suppress(&mut tx, addr, "marketing", "unsubscribe").await;
    tx.commit().await.unwrap();

    // Contact leaves: the N:M link goes first (no cascade), then the contact.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("DELETE FROM contact_emails WHERE contact_id = $1")
        .bind(contact)
        .execute(tx.conn())
        .await
        .unwrap();
    sqlx::query("DELETE FROM contacts WHERE id = $1")
        .bind(contact)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    assert_eq!(
        compliance_core::active_suppressions(&mut tx, addr, "marketing")
            .await
            .unwrap()
            .len(),
        1,
        "suppression must survive contact deletion"
    );

    // Re-import as a brand-new contact linked to the same address — the
    // suppression still applies (it follows the address).
    let contact2: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Reimported') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, true)",
    )
    .bind(world.workspace_id)
    .bind(contact2)
    .bind(addr)
    .execute(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        compliance_core::active_suppressions(&mut tx, addr, "marketing")
            .await
            .unwrap()
            .len(),
        1,
        "suppression must survive contact re-import"
    );
    tx.rollback().await.unwrap();
}

/// Release-request roundtrip: pending approval row targets the suppression
/// and `release_context` exposes what the job needs to re-verify.
#[tokio::test]
async fn release_request_roundtrip() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "vip").await;
    let sid = suppress(&mut tx, addr, "marketing", "manual").await;

    let snapshot = infrastructure::crypto::sha256_bytes(b"snapshot");
    let req_id = infrastructure::repo::approvals::insert_request(
        &mut tx,
        world.admin.principal_id,
        infrastructure::repo::approvals::ApprovalTarget::SuppressionRelease(sid),
        &snapshot,
        chrono::Utc::now() + chrono::Duration::days(7),
    )
    .await
    .unwrap();
    let ctx = repo::release_context(&mut tx, req_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ctx.status, "pending");
    assert_eq!(ctx.action_type, "suppression_release");
    assert_eq!(ctx.suppression_id, Some(sid));
    assert!(ctx.decided_by.is_none());
    assert_eq!(
        repo::pending_release_request(&mut tx, sid).await.unwrap(),
        Some(req_id)
    );
    tx.rollback().await.unwrap();
}

/// Issue → resolve → consume token roundtrip (B.4.4): used tokens keep
/// resolving so a repeat POST is an idempotent success; revoked tokens
/// never resolve. The raw token is never persisted — only sha256.
#[tokio::test]
async fn unsubscribe_token_roundtrip() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let addr = insert_email_address(&mut tx, "unsub").await;
    let outbound = insert_outbound(&mut tx, &world, addr).await;
    let raw = compliance_core::issue_unsubscribe_token(&mut tx, addr, outbound)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let (token_tx, r1) = infrastructure::public_tokens::resolve_unsubscribe_token(&db, &raw.token)
        .await
        .unwrap();
    assert_eq!(r1.email_address_id, addr);
    assert_eq!(r1.outbound_message_id, outbound);
    assert!(r1.used_at.is_none());
    token_tx.rollback().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    assert!(
        compliance_core::consume_unsubscribe_token(&mut tx, &raw.token)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();

    // B.4.4: a used token must still resolve for idempotent repeat POSTs.
    let (token_tx, r2) = infrastructure::public_tokens::resolve_unsubscribe_token(&db, &raw.token)
        .await
        .unwrap();
    assert!(r2.used_at.is_some());
    token_tx.rollback().await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    sqlx::query("UPDATE unsubscribe_tokens SET revoked_at = now() WHERE id = $1")
        .bind(raw.token_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(
        infrastructure::public_tokens::resolve_unsubscribe_token(&db, &raw.token)
            .await
            .is_err(),
        "revoked token must not resolve"
    );
    // Unknown token → None (generalized 404 at the edge).
    assert!(
        infrastructure::public_tokens::resolve_unsubscribe_token(&db, "definitely-not-a-token")
            .await
            .is_err()
    );
}

/// §5.3 limited add — `address_visible_to`: the CRM path (contact link,
/// gated by the caller on `crm.read` for agents) and the mailbox path
/// (participant of a message, or the thread's linked contact, inside a
/// mailbox with a `can_read` grant). No link, no grant, or both scope
/// flags off → not visible.
#[tokio::test]
async fn address_visible_to_acl_paths() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let ws = world.workspace_id;

    let mut tx = db.begin(ws).await.unwrap();
    // CRM-visible: linked to a contact.
    let linked = insert_email_address(&mut tx, "acl-linked").await;
    let contact = fixtures::insert_contact(&mut tx, "Acl Person")
        .await
        .unwrap();
    fixtures::link_contact_email(&mut tx, contact, linked)
        .await
        .unwrap();
    // Mailbox-visible: participant on an inbound message in the shared
    // mailbox (member_a holds can_read via seed; member_b does not).
    let mail_addr = insert_email_address(&mut tx, "acl-mail").await;
    let thread: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id)
         VALUES ($1, $2) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let msg: Uuid = sqlx::query_scalar(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction, message_at)
         VALUES ($1, $2, $3, 'inbound', now()) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(thread)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO message_participants
            (workspace_id, email_message_id, role, address, email_address_id)
         VALUES ($1, $2, 'from', 'acl-mail@example.com', $3)",
    )
    .bind(ws)
    .bind(msg)
    .bind(mail_addr)
    .execute(tx.conn())
    .await
    .unwrap();
    // Invisible: exists but linked to nothing.
    let stray = insert_email_address(&mut tx, "acl-stray").await;
    tx.commit().await.unwrap();

    let mut tx = db.begin(ws).await.unwrap();
    macro_rules! vis {
        ($p:expr, $a:expr, $c:expr, $m:expr) => {
            repo::address_visible_to(&mut tx, $p, $a, $c, $m)
                .await
                .unwrap()
        };
    }
    assert!(vis!(world.member_a.principal_id, linked, true, false));
    assert!(vis!(world.member_a.principal_id, mail_addr, false, true));
    assert!(!vis!(world.member_a.principal_id, stray, true, true));
    // member_b: contact path still open (shared CRM), mailbox path denied.
    assert!(vis!(world.member_b.principal_id, linked, true, false));
    assert!(!vis!(world.member_b.principal_id, mail_addr, false, true));
    // Both scope flags off → nothing visible (e.g. agent without scopes).
    assert!(!vis!(world.member_a.principal_id, linked, false, false));
    assert!(!vis!(world.agent.principal_id, linked, false, false));
    tx.rollback().await.unwrap();
}
