//! inbox module — DB-level tests (Task 8). T07: the composite FK refuses a
//! message whose thread lives under a different mail account. T17:
//! `thread_read_states` isolation + monotonicity. T26: HTML sanitization.

mod common;

use chrono::{Duration, Utc};
use common::test_db;
use infrastructure::repo::inbox as repo;
use infrastructure::testing::seed_workspace;
use uuid::Uuid;

/// Second mail account on the seeded connection (a different mailbox under
/// the same workspace — needed for cross-mailbox FK assertions).
async fn insert_mail_account(
    tx: &mut infrastructure::db::Tx<'_>,
    connection_id: Uuid,
    provider_account_id: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO mail_accounts
            (workspace_id, connection_id, provider_account_id, primary_address, name, status)
         VALUES ($1, $2, $3, $4, 'Second', 'active')
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(connection_id)
    .bind(provider_account_id)
    .bind(format!("{provider_account_id}@test.dev"))
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

#[tokio::test]
async fn t07_thread_message_mailbox_mismatch_fk_fails() {
    let Some(db) = test_db().await else { return };
    let world = seed_workspace(&db).await.unwrap();

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let account_b = insert_mail_account(&mut tx, world.connection_id, "fake-acct-b").await;
    let thread_a = repo::create_thread(&mut tx, world.mail_account_id, Some("pt-a"), None)
        .await
        .unwrap();

    // A message claiming mailbox B but thread in mailbox A violates
    // fk_070 (workspace_id, mail_account_id, thread_id) → threads.
    let res = sqlx::query(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction,
             provider_message_id, message_at, classification)
         VALUES ($1, $2, $3, 'inbound', 'pm-mismatch', now(), 'unknown')",
    )
    .bind(world.workspace_id)
    .bind(account_b)
    .bind(thread_a)
    .execute(tx.conn())
    .await;
    let err = res.expect_err("mismatched mailbox insert must fail");
    let msg = format!("{err}");
    assert!(
        msg.contains("fk_070") || msg.contains("foreign key") || msg.contains("violates"),
        "expected FK violation, got: {msg}"
    );
    let _ = tx.rollback().await;

    // Nothing persisted — the bad row never exists.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let found = repo::message_by_provider_id(&mut tx, account_b, "pm-mismatch")
        .await
        .unwrap();
    assert!(found.is_none());
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn t17_read_state_is_per_principal_and_monotonic() {
    let Some(db) = test_db().await else { return };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();

    let thread = repo::create_thread(&mut tx, world.mail_account_id, None, None)
        .await
        .unwrap();
    let now = Utc::now();
    let earlier = now - Duration::hours(2);

    // A marks now; B marks earlier — marks are per-principal.
    repo::upsert_read_state(&mut tx, thread, world.member_a.principal_id, now)
        .await
        .unwrap();
    repo::upsert_read_state(&mut tx, thread, world.member_b.principal_id, earlier)
        .await
        .unwrap();
    assert_eq!(
        repo::get_read_state(&mut tx, thread, world.member_a.principal_id)
            .await
            .unwrap(),
        Some(now)
    );
    assert_eq!(
        repo::get_read_state(&mut tx, thread, world.member_b.principal_id)
            .await
            .unwrap(),
        Some(earlier)
    );

    // Monotonic (B.1.4): an older mark never lowers A's stored position.
    repo::upsert_read_state(&mut tx, thread, world.member_a.principal_id, earlier)
        .await
        .unwrap();
    assert_eq!(
        repo::get_read_state(&mut tx, thread, world.member_a.principal_id)
            .await
            .unwrap(),
        Some(now)
    );

    // A's mark does not exist for B and vice versa; a third principal has none.
    assert!(
        repo::get_read_state(&mut tx, thread, world.manager.principal_id)
            .await
            .unwrap()
            .is_none()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn message_search_treats_percent_and_underscore_as_literal_text() {
    let Some(db) = test_db().await else { return };
    let world = seed_workspace(&db).await.unwrap();
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let exact_thread = repo::create_thread(&mut tx, world.mail_account_id, None, None)
        .await
        .unwrap();
    let wildcard_thread = repo::create_thread(&mut tx, world.mail_account_id, None, None)
        .await
        .unwrap();
    let exact_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction,
             subject, body_text, message_at, classification)
         VALUES ($1, $2, $3, 'inbound', 'Deal 50%_complete', 'note', now(), 'unknown')
         RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(exact_thread)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction,
             subject, body_text, message_at, classification)
         VALUES ($1, $2, $3, 'inbound', 'Deal 50XXcomplete', 'note', now(), 'unknown')",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(wildcard_thread)
    .execute(tx.conn())
    .await
    .unwrap();

    let results = repo::search_messages(
        &mut tx,
        world.member_a.principal_id,
        Some(world.mail_account_id),
        "%_",
        None,
        101,
    )
    .await
    .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, exact_id);

    let denied = repo::search_messages(
        &mut tx,
        world.member_b.principal_id,
        Some(world.mail_account_id),
        "%_",
        None,
        101,
    )
    .await
    .unwrap();
    assert!(denied.is_empty(), "mailbox ACL must gate matching rows");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn t26_inbound_html_strips_scripts_forms_and_remote_images() {
    let dirty = r#"<div onclick="x()"><script>alert(1)</script>
        <style>.a{}</style><form action="/x"><input name="q"></form>
        <img src="https://tracker.example/p.gif" alt="pix" onerror="y()">
        <img src="cid:local" alt="inline">
        <a href="javascript:alert(1)">bad</a>
        <a href="https://ok.example" onmouseover="z()">ok</a>
        <iframe src="https://evil.example"></iframe>
        <p style="color:red">hi</p></div>"#;
    let out = repo::sanitize_html(dirty);
    for bad in [
        "<script",
        "alert(1)",
        "<style",
        "<form",
        "<iframe",
        "onclick",
        "onerror",
        "onmouseover",
        "style=",
        "javascript:",
        "tracker.example",
        "cid:",
        "src=",
    ] {
        assert!(!out.contains(bad), "sanitizer leaked {bad}: {out}");
    }
    assert!(out.contains("hi"), "{out}");
    assert!(out.contains("href=\"https://ok.example\""), "{out}");
    assert!(out.contains("noopener"), "{out}");
    // Images keep alt text only — never a loadable source.
    assert!(out.contains("alt=\"pix\""), "{out}");
}
