//! inbox module — HTTP tests (Task 8): mailbox ACL visibility (§5.4/§5.6),
//! thread assignment/patch/read-state (If-Match/ETag), reply drafts incl.
//! T16 re-review on new inbound + idempotent replay, classification review
//! safety stop, attachment download capability (§15.2), and the
//! `/webhooks/v1/mail` ingress boundary (§14.3).
//!
//! Runs only when `DATABASE_URL` points at the dedicated test DB.

use api::AppState;
use api::testing::TestApp;
use chrono::{DateTime, Utc};
use infrastructure::config::Config;
use infrastructure::crypto;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::providers::ProviderMessage;
use infrastructure::repo::inbox as repo;
use infrastructure::secrets::{FileSecretStore, SecretStore};
use infrastructure::testing::{TEST_PASSWORD, TestWorld, seed_workspace};
use reqwest::StatusCode;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

async fn test_app() -> Option<(TestApp, TestWorld)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("DATABASE_URL not set — skipping API test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&db).await.unwrap();
    let db = db.with_default_workspace(Some(world.workspace_id));
    let app = TestApp::spawn_with_state(AppState::with_fake_providers(
        db,
        Arc::new(Config::test()),
        None,
    ))
    .await;
    Some((app, world))
}

/// Ingest one inbound provider message (creates thread/message/participants
/// + `message.received` outbox) inside a workspace tx. `provider_thread_id`
///   controls which thread the message lands in (same pt → same thread).
async fn ingest(
    db: &Db,
    world: &TestWorld,
    provider_message_id: &str,
    provider_thread_id: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
) -> (Uuid, Uuid) {
    let msg = ProviderMessage {
        provider_message_id: provider_message_id.into(),
        provider_thread_id: Some(provider_thread_id.to_string()),
        internet_message_id: Some(format!("<{provider_message_id}@fake.test>")),
        in_reply_to: None,
        references: vec![],
        from: "sender@example.com".into(),
        to: vec!["shared@test.dev".into()],
        subject: Some(subject.into()),
        body_text: Some(body_text.into()),
        body_html: body_html.map(str::to_string),
        message_at: Utc::now(),
    };
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let out = application::inbox::ingest::ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &msg,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (out.thread_id, out.message_id.expect("new message"))
}

async fn ingest_at(
    db: &Db,
    world: &TestWorld,
    provider_message_id: &str,
    provider_thread_id: &str,
    message_at: DateTime<Utc>,
) -> (Uuid, Uuid) {
    let message = ProviderMessage {
        provider_message_id: provider_message_id.into(),
        provider_thread_id: Some(provider_thread_id.into()),
        internet_message_id: Some(format!("<{provider_message_id}@fake.test>")),
        in_reply_to: None,
        references: vec![],
        from: "sender@example.com".into(),
        to: vec!["shared@test.dev".into()],
        subject: Some("Timestamp tie".into()),
        body_text: Some(provider_message_id.into()),
        body_html: None,
        message_at,
    };
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let out = application::inbox::ingest::ingest_provider_message(
        &mut tx,
        None,
        world.mail_account_id,
        &message,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    (out.thread_id, out.message_id.expect("new message"))
}

struct Session {
    csrf: String,
}

async fn login(app: &TestApp, email: &str) -> Session {
    let res = app.login(email, TEST_PASSWORD).await;
    assert_eq!(res.status(), StatusCode::OK);
    Session {
        csrf: TestApp::csrf_token_from(&res).unwrap(),
    }
}

async fn put(
    app: &TestApp,
    s: &Session,
    path: &str,
    body: serde_json::Value,
    if_match: Option<i64>,
) -> reqwest::Response {
    let mut r = app
        .client
        .put(app.url(path))
        .header("x-csrf-token", &s.csrf)
        .json(&body);
    if let Some(v) = if_match {
        r = r.header("if-match", format!("\"{v}\""));
    }
    r.send().await.unwrap()
}

async fn patch(
    app: &TestApp,
    s: &Session,
    path: &str,
    body: serde_json::Value,
    if_match: Option<i64>,
) -> reqwest::Response {
    let mut r = app
        .client
        .patch(app.url(path))
        .header("x-csrf-token", &s.csrf)
        .json(&body);
    if let Some(v) = if_match {
        r = r.header("if-match", format!("\"{v}\""));
    }
    r.send().await.unwrap()
}

async fn post(
    app: &TestApp,
    s: &Session,
    path: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    app.post(path, body, Some(&s.csrf)).await
}

// ------------------------------------------------------------- ACL (§5.4)

#[tokio::test]
async fn thread_detail_returns_authoritative_latest_inbound_id_when_times_tie() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let tied_at = Utc::now();
    let (thread_id, first_id) = ingest_at(
        &app.state.db,
        &world,
        "anchor-tie-0",
        "pt-anchor-tie",
        tied_at,
    )
    .await;
    let mut latest_arrival_id = first_id;
    let mut max_tied_id = first_id;
    for attempt in 1..=64 {
        let (_, message_id) = ingest_at(
            &app.state.db,
            &world,
            &format!("anchor-tie-{attempt}"),
            "pt-anchor-tie",
            tied_at,
        )
        .await;
        latest_arrival_id = message_id;
        max_tied_id = max_tied_id.max(message_id);
        if latest_arrival_id < max_tied_id {
            break;
        }
    }
    assert!(
        latest_arrival_id < max_tied_id,
        "fixture needs an arrival-ordered anchor below the timestamp/UUID maximum"
    );

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let stored_anchor: Uuid = sqlx::query_scalar(
        "SELECT last_inbound_message_id FROM email_threads WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(thread_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(stored_anchor, latest_arrival_id);

    let _session = login(&app, world.admin.email.as_deref().unwrap()).await;
    let response = app.get(&format!("/api/v1/email-threads/{thread_id}")).await;
    let data = TestApp::data(response).await;
    assert_eq!(
        data["last_inbound_message_id"].as_str().unwrap(),
        stored_anchor.to_string()
    );
    assert_ne!(
        data["last_inbound_message_id"].as_str().unwrap(),
        max_tied_id.to_string()
    );
}

#[tokio::test]
async fn mailbox_acl_hides_ungranted_thread_and_content() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (thread_id, _) = ingest(
        &app.state.db,
        &world,
        "pm-acl-1",
        "pt-acl",
        "Pricing",
        "hello",
        None,
    )
    .await;

    // NOTE: one cookie jar per TestApp client — the most recent login wins.
    // Sequence positive cases while member_a's session is active.
    let a = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = app.get("/api/v1/email-threads").await;
    let data = TestApp::data(res).await;
    let ids: Vec<String> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&thread_id.to_string()), "member_a list");
    let res = app.get(&format!("/api/v1/email-threads/{thread_id}")).await;
    assert_eq!(res.status(), StatusCode::OK);
    drop(a);

    // member_b (no grant): list omits it; detail/mutations hide existence.
    let b = login(&app, world.member_b.email.as_deref().unwrap()).await;
    let res = app.get("/api/v1/email-threads").await;
    let data = TestApp::data(res).await;
    let ids: Vec<String> = data
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!ids.contains(&thread_id.to_string()), "member_b list");

    let res = app.get(&format!("/api/v1/email-threads/{thread_id}")).await;
    // Read/detail paths hide the thread when the caller has no mailbox grant.
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = put(
        &app,
        &b,
        &format!("/api/v1/email-threads/{thread_id}/read-state"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = post(
        &app,
        &b,
        &format!("/api/v1/email-threads/{thread_id}/reply-drafts"),
        json!({"body_text": "hi"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = put(
        &app,
        &b,
        &format!("/api/v1/email-threads/{thread_id}/assignment"),
        json!({"assigned_principal_id": null}),
        Some(1),
    )
    .await;
    // Members lack the `mail.assign` scope, so assignment is rejected before
    // the resource lookup. The read/detail paths above remain hidden as 404.
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

// ------------------------------------------------- thread mutations + ETag

#[tokio::test]
async fn thread_assignment_patch_and_read_state_flow() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (thread_id, _msg) = ingest(
        &app.state.db,
        &world,
        "pm-flow-1",
        "pt-flow",
        "Hello",
        "body",
        None,
    )
    .await;
    let a = login(&app, world.admin.email.as_deref().unwrap()).await;

    // Detail returns ETag = current row version.
    let res = app.get(&format!("/api/v1/email-threads/{thread_id}")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let v0: i64 = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .trim_matches('"')
        .parse()
        .unwrap();

    // Assignment: missing If-Match → 428; stale → 412; fresh → 200 + ETag.
    let res = put(
        &app,
        &a,
        &format!("/api/v1/email-threads/{thread_id}/assignment"),
        json!({"assigned_principal_id": world.member_b.principal_id}),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::PRECONDITION_REQUIRED);
    let res = put(
        &app,
        &a,
        &format!("/api/v1/email-threads/{thread_id}/assignment"),
        json!({"assigned_principal_id": world.member_b.principal_id}),
        Some(v0 - 1),
    )
    .await;
    let status = res.status();
    let body = res.text().await.unwrap();
    assert_eq!(status, StatusCode::PRECONDITION_FAILED, "{body}");
    let res = put(
        &app,
        &a,
        &format!("/api/v1/email-threads/{thread_id}/assignment"),
        json!({"assigned_principal_id": world.member_b.principal_id}),
        Some(v0),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let etag2: i64 = res
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap()
        .trim_matches('"')
        .parse()
        .unwrap();
    assert_eq!(etag2, v0 + 1);
    let data = TestApp::data(res).await;
    assert_eq!(
        data["assigned_principal_id"],
        world.member_b.principal_id.to_string()
    );

    // Read state: PUT stores the caller's mark; GET exposes it.
    let res = put(
        &app,
        &a,
        &format!("/api/v1/email-threads/{thread_id}/read-state"),
        json!({"last_read_at": Utc::now()}),
        None,
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = app.get(&format!("/api/v1/email-threads/{thread_id}")).await;
    let data = TestApp::data(res).await;
    assert!(data["my_last_read_at"].is_string());

    // Patch: stale If-Match → 412; fresh → 200 resolved.
    let res = patch(
        &app,
        &a,
        &format!("/api/v1/email-threads/{thread_id}"),
        json!({"status": "resolved"}),
        Some(v0),
    )
    .await;
    assert_eq!(res.status(), StatusCode::PRECONDITION_FAILED);
    let res = patch(
        &app,
        &a,
        &format!("/api/v1/email-threads/{thread_id}"),
        json!({"status": "resolved"}),
        Some(etag2),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let data = TestApp::data(res).await;
    assert_eq!(data["status"], "resolved");
}

// ------------------------------------------------------ reply drafts (T16)

#[tokio::test]
async fn t16_reply_draft_requires_fresh_inbound_context() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (thread_id, msg_id) = ingest(
        &app.state.db,
        &world,
        "pm-rd-1",
        "pt-rd",
        "Question",
        "how much?",
        None,
    )
    .await;
    let a = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let path = format!("/api/v1/email-threads/{thread_id}/reply-drafts");

    // Stale expectation → 409 REPLY_CONTEXT_CHANGED.
    let res = post(
        &app,
        &a,
        &path,
        json!({
            "expected_last_inbound_id": Uuid::new_v4(),
            "body_text": "answer",
        }),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "REPLY_CONTEXT_CHANGED");

    // Fresh expectation → 201; draft rows carry the reply linkage.
    let res = post(
        &app,
        &a,
        &path,
        json!({
            "expected_last_inbound_id": msg_id,
            "body_text": "answer",
            "visibility": "mailbox",
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED, "draft create");
    let data = TestApp::data(res).await;
    assert_eq!(data["thread_id"], thread_id.to_string());
    assert_eq!(data["reply_to_message_id"], msg_id.to_string());
    assert_eq!(data["expected_last_inbound_id"], msg_id.to_string());
    assert_eq!(data["status"], "draft");
    assert_eq!(data["visibility"], "mailbox");
    assert_eq!(data["subject"], "Re: Question");

    // New inbound on the SAME provider thread → the previous expected id
    // is now stale.
    let (t2, msg2) = ingest(
        &app.state.db,
        &world,
        "pm-rd-2",
        "pt-rd",
        "Re: Question",
        "following up",
        None,
    )
    .await;
    assert_eq!(t2, thread_id);
    let res = post(
        &app,
        &a,
        &path,
        json!({
            "expected_last_inbound_id": msg_id,
            "body_text": "stale answer",
        }),
    )
    .await;
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "REPLY_CONTEXT_CHANGED");
    let res = post(
        &app,
        &a,
        &path,
        json!({
            "expected_last_inbound_id": msg2,
            "body_text": "fresh answer",
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::CREATED);

    // Validation: empty body → 422.
    let res = post(&app, &a, &path, json!({"body_text": "   "})).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn reply_draft_idempotency_key_replays_and_conflicts() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (thread_id, msg_id) = ingest(
        &app.state.db,
        &world,
        "pm-idem-1",
        "pt-idem",
        "Hi",
        "body",
        None,
    )
    .await;
    let a = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let path = format!("/api/v1/email-threads/{thread_id}/reply-drafts");
    let body = json!({"expected_last_inbound_id": msg_id, "body_text": "ok"});

    let res = app
        .client
        .post(app.url(&path))
        .header("x-csrf-token", &a.csrf)
        .header("idempotency-key", "idem-1")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let first = TestApp::data(res).await["id"].as_str().unwrap().to_string();

    // Replay: same key + same request → same draft, no second row.
    let res = app
        .client
        .post(app.url(&path))
        .header("x-csrf-token", &a.csrf)
        .header("idempotency-key", "idem-1")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let second = TestApp::data(res).await["id"].as_str().unwrap().to_string();
    assert_eq!(first, second);

    // Same key, different request → 409 IDEMPOTENCY_KEY_REUSED.
    let res = app
        .client
        .post(app.url(&path))
        .header("x-csrf-token", &a.csrf)
        .header("idempotency-key", "idem-1")
        .json(&json!({"expected_last_inbound_id": msg_id, "body_text": "different"}))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::CONFLICT).await;
    assert_eq!(p["code"], "IDEMPOTENCY_KEY_REUSED");
}

#[tokio::test]
async fn reply_draft_replay_requires_mail_read_scope_and_current_read_grant() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (thread_id, message_id) = ingest(
        &app.state.db,
        &world,
        "pm-reply-scope-1",
        "pt-reply-scope",
        "Private inbound subject",
        "private inbound content",
        None,
    )
    .await;
    let member = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let path = format!("/api/v1/email-threads/{thread_id}/reply-drafts");
    let body = json!({
        "expected_last_inbound_id": message_id,
        "body_text": "reply",
    });
    let key = "reply-read-gated-replay-0001";

    // A normal human with both mailbox permissions can create the reply. The
    // successful response contains the derived `Re:` subject and is cached
    // under the member's principal id.
    let created = app
        .client
        .post(app.url(&path))
        .header("x-csrf-token", &member.csrf)
        .header("idempotency-key", key)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: serde_json::Value = created.json().await.unwrap();
    assert_eq!(created["data"]["subject"], "Re: Private inbound subject");

    let compose_only_token = {
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        let (_, token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.member_a.principal_id,
            "compose without read",
            &["mail.compose".to_string()],
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        token
    };

    // The same principal/key must not retrieve the cached derived subject
    // when its current credential lacks mail.read.
    let response = app
        .client
        .post(app.url(&path))
        .header("authorization", format!("Bearer {compose_only_token}"))
        .header("idempotency-key", key)
        .json(&body)
        .send()
        .await
        .unwrap();
    let denied = TestApp::problem(response, StatusCode::FORBIDDEN).await;
    assert_eq!(denied["code"], "SCOPE_REQUIRED");
    assert!(denied.to_string().find("Private inbound subject").is_none());

    let compose_and_read_token = {
        let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
        let (_, token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.member_a.principal_id,
            "compose and read",
            &["mail.compose".to_string(), "mail.read".to_string()],
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        token
    };
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants
            SET can_read = false, can_compose = false, can_send = false,
                can_approve = false, can_assign = false, version = version + 1
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(world.workspace_id)
    .bind(world.mail_account_id)
    .bind(world.member_a.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // The schema revokes compose whenever read is revoked; a credential that
    // still has both scopes cannot recover the cached subject without the
    // current mailbox grant.
    let response = app
        .client
        .post(app.url(&path))
        .header("authorization", format!("Bearer {compose_and_read_token}"))
        .header("idempotency-key", key)
        .json(&body)
        .send()
        .await
        .unwrap();
    let denied = TestApp::problem(response, StatusCode::FORBIDDEN).await;
    assert_eq!(denied["code"], "MAILBOX_ACCESS_DENIED");
    assert!(denied.to_string().find("Private inbound subject").is_none());
}

// --------------------------------------------- classification review (§9.4)

#[tokio::test]
async fn classification_review_records_and_enqueues_pause() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (_thread_id, msg_id) = ingest(
        &app.state.db,
        &world,
        "pm-rev-1",
        "pt-rev",
        "stop",
        "please unsubscribe me",
        None,
    )
    .await;

    // Ungranted principal → 404 (login member_b first — the jar holds the
    // latest session only).
    let b = login(&app, world.member_b.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        &b,
        &format!("/api/v1/email-messages/{msg_id}/classification-reviews"),
        json!({"classification": "unsubscribe_request"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    drop(b);

    // member_a review: unsubscribe_request → pause_address job coalesced in.
    let a = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        &a,
        &format!("/api/v1/email-messages/{msg_id}/classification-reviews"),
        json!({"classification": "unsubscribe_request", "reason": "explicit ask"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let data = TestApp::data(res).await;
    assert_eq!(data["classification"], "unsubscribe_request");
    assert_eq!(
        data["classification_reviewed_by"],
        world.member_a.principal_id.to_string()
    );

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let n = repo::count_jobs(&mut tx, "campaigns.pause_address")
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(n >= 1, "expected campaigns.pause_address job");

    // A second review is allowed (latest human decision wins).
    let res = post(
        &app,
        &a,
        &format!("/api/v1/email-messages/{msg_id}/classification-reviews"),
        json!({"classification": "unknown", "reason": "unclear"}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
}

// ------------------------------------------------------- attachments (§15.2)

#[tokio::test]
async fn attachment_download_capability_flow() {
    let Some((app, world)) = test_app().await else {
        return;
    };
    let (_thread_id, msg_id) = ingest(
        &app.state.db,
        &world,
        "pm-att-1",
        "pt-att",
        "files",
        "see attached",
        None,
    )
    .await;

    // Fixture: encrypted object + attachment row.
    let store = infrastructure::storage::LocalObjectStore::new(
        &app.state.config.objects_dir,
        [3u8; 32],
        app.state.config.hmac_key.clone(),
    );
    let storage_key = format!("obj-{}", Uuid::new_v4().simple());
    infrastructure::storage::ObjectStore::put(&store, &storage_key, b"PDFBYTES")
        .await
        .unwrap();
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let object_id = repo::insert_stored_object(
        &mut tx,
        &storage_key,
        "application/pdf",
        8,
        &crypto::sha256_bytes(b"PDFBYTES"),
        "clean",
    )
    .await
    .unwrap();
    let attachment_id =
        repo::insert_message_attachment(&mut tx, msg_id, object_id, "spec.pdf", false)
            .await
            .unwrap();
    tx.commit().await.unwrap();

    // Ungranted → 404 (existence hidden). Login member_b first — one jar.
    let b = login(&app, world.member_b.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        &b,
        &format!("/api/v1/message-attachments/{attachment_id}/download-requests"),
        json!({}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    drop(b);

    // Granted → capability URL.
    let a = login(&app, world.member_a.email.as_deref().unwrap()).await;
    let res = post(
        &app,
        &a,
        &format!("/api/v1/message-attachments/{attachment_id}/download-requests"),
        json!({}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let data = TestApp::data(res).await;
    let url = data["download_url"].as_str().unwrap().to_string();
    assert!(url.contains("token="));

    // Download serves the decrypted bytes.
    let res = app.get(&url).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("content-disposition")
            .and_then(|v| v.to_str().ok()),
        Some("attachment; filename=\"spec.pdf\"")
    );
    assert_eq!(res.bytes().await.unwrap().as_ref(), b"PDFBYTES");

    // Tampered/missing token → 403.
    let res = app
        .get(&format!(
            "/api/v1/message-attachments/{attachment_id}/download"
        ))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = app
        .get(&format!(
            "/api/v1/message-attachments/{attachment_id}/download?token=bad.token.here"
        ))
        .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Quarantined object → 422 on the request endpoint.
    let storage_key2 = format!("obj-{}", Uuid::new_v4().simple());
    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let obj2 = repo::insert_stored_object(
        &mut tx,
        &storage_key2,
        "application/pdf",
        8,
        &crypto::sha256_bytes(b"X"),
        "rejected",
    )
    .await
    .unwrap();
    let att2 = repo::insert_message_attachment(&mut tx, msg_id, obj2, "bad.pdf", false)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let res = post(
        &app,
        &a,
        &format!("/api/v1/message-attachments/{att2}/download-requests"),
        json!({}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

// --------------------------------------------------------- webhook ingress

#[tokio::test]
async fn webhook_ingress_verifies_signature_and_dedupes() {
    let Some((app, world)) = test_app().await else {
        return;
    };

    // The connection's secret_ref ('test-secret-ref') resolves via the
    // file secret store configured in Config::test().
    let secrets = FileSecretStore::new(&app.state.config.secrets_file, [3u8; 32]);
    SecretStore::put(&secrets, "test-secret-ref", "whsec-test")
        .await
        .unwrap();

    let path = format!("/webhooks/v1/mail/{}", world.connection_id);
    let payload = serde_json::json!({
        "event_id": "evt-1",
        "provider_message_id": "pm-wh-1",
        "body": "full body must never persist",
        "subject": "secret subject",
    });
    let raw = serde_json::to_vec(&payload).unwrap();
    let sig = format!(
        "sha256={}",
        crypto::b64_encode(&crypto::hmac_sha256(b"whsec-test", &raw))
    );

    // No signature → 401.
    let res = app
        .client
        .post(app.url(&path))
        .header("content-type", "application/json")
        .body(raw.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Wrong signature → 401, nothing stored.
    let res = app
        .client
        .post(app.url(&path))
        .header("x-webhook-signature", "sha256=AAAA")
        .body(raw.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Valid → 200 received; durable event + process job exist.
    let res = app
        .client
        .post(app.url(&path))
        .header("x-webhook-signature", &sig)
        .body(raw.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let data = TestApp::data(res).await;
    assert_eq!(data["duplicate"], false);
    let event_id: Uuid = data["event_id"].as_str().unwrap().parse().unwrap();

    // Replay the identical event → dedupe ACK, no second row/job.
    let res = app
        .client
        .post(app.url(&path))
        .header("x-webhook-signature", &sig)
        .body(raw.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let data = TestApp::data(res).await;
    assert_eq!(data["duplicate"], true);

    let mut tx = app.state.db.begin(world.workspace_id).await.unwrap();
    let ev = repo::get_webhook_event(&mut tx, event_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ev.status, "received");
    assert_eq!(ev.provider_event_key, "evt-1");
    // B.6.1: sensitive fields never persist.
    assert!(ev.redacted_payload.get("body").is_none());
    assert!(ev.redacted_payload.get("subject").is_none());
    assert!(ev.redacted_payload.get("provider_message_id").is_some());
    let jobs = repo::count_jobs(&mut tx, "inbox.process_webhook")
        .await
        .unwrap();
    assert_eq!(jobs, 1);
    tx.rollback().await.unwrap();

    // Unknown connection → 404.
    let res = app
        .client
        .post(app.url(&format!("/webhooks/v1/mail/{}", Uuid::new_v4())))
        .header("x-webhook-signature", &sig)
        .body(raw.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
