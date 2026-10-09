//! sending module HTTP-level tests (B.3.10/B.3.11): route registration,
//! extractor contracts (Idempotency-Key, If-Match), validation envelopes,
//! and 404s. Fixture-heavy send-pipeline coverage (T04/T10–T16/T34) lives
//! in `crates/worker/tests/sending_{pipeline,dispatch}.rs` because those
//! need raw SQL fixtures (sqlx is not an api dependency).

use api::AppState;
use api::testing::TestApp;
use contracts::templates::{CreateEmailTemplateRequest, CreateTemplateVersionRequest};
use infrastructure::config::Config;
use infrastructure::db::{Db, ROLE_API};
use infrastructure::testing::{TEST_PASSWORD, ctx_for, seed_workspace};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

async fn test_app() -> Option<(TestApp, String, infrastructure::testing::TestWorld)> {
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
    let res = app
        .login(world.admin.email.as_deref().unwrap(), TEST_PASSWORD)
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let csrf = TestApp::csrf_token_from(&res).unwrap();
    Some((app, csrf, world))
}

async fn post(
    app: &TestApp,
    csrf: &str,
    path: &str,
    body: Value,
    extra: &[(&str, &str)],
) -> reqwest::Response {
    let mut req = app.client.post(app.url(path)).header("x-csrf-token", csrf);
    for (k, v) in extra {
        req = req.header(*k, *v);
    }
    req.json(&body).send().await.unwrap()
}

#[tokio::test]
async fn draft_routes_envelope_and_404s() {
    let Some((app, csrf, _world)) = test_app().await else {
        return;
    };

    // Empty list — route is registered and envelope-shaped.
    let res = app.get("/api/v1/message-drafts").await;
    let list = TestApp::data(res).await;
    assert!(list.as_array().unwrap().is_empty());

    // New draft creation requires an idempotency key before invoking the
    // business use case.
    let res = post(
        &app,
        &csrf,
        "/api/v1/message-drafts",
        json!({
            "mail_account_id": Uuid::new_v4(),
            "to_email_address_id": Uuid::new_v4(),
            "purpose": "correspondence",
            "subject": "s",
            "body_text": "b",
        }),
        &[],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // Pre-tx validation: bad purpose → 422 ENUM violation.
    let res = post(
        &app,
        &csrf,
        "/api/v1/message-drafts",
        json!({
            "mail_account_id": Uuid::new_v4(),
            "to_email_address_id": Uuid::new_v4(),
            "purpose": "bogus",
            "subject": "s",
            "body_text": "b",
        }),
        &[("idempotency-key", "draft-bad-purpose-001")],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");
    assert!(
        p["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["field"] == "purpose" && v["code"] == "ENUM")
    );

    // Header-injection guard on subject → 422 HEADER_BREAK.
    let res = post(
        &app,
        &csrf,
        "/api/v1/message-drafts",
        json!({
            "mail_account_id": Uuid::new_v4(),
            "to_email_address_id": Uuid::new_v4(),
            "purpose": "correspondence",
            "subject": "a\r\nBcc: x@y",
            "body_text": "b",
        }),
        &[("idempotency-key", "draft-header-break-001")],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert!(
        p["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["code"] == "HEADER_BREAK")
    );

    // Reply fields require thread_id → 422 REQUIRED.
    let res = post(
        &app,
        &csrf,
        "/api/v1/message-drafts",
        json!({
            "mail_account_id": Uuid::new_v4(),
            "to_email_address_id": Uuid::new_v4(),
            "purpose": "correspondence",
            "subject": "s",
            "body_text": "b",
            "reply_to_message_id": Uuid::new_v4(),
        }),
        &[("idempotency-key", "draft-reply-link-001")],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert!(
        p["violations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["field"] == "thread_id" && v["code"] == "REQUIRED")
    );

    // Missing rows → envelope errors, not panics.
    let res = app
        .get(&format!("/api/v1/message-drafts/{}", Uuid::new_v4()))
        .await;
    TestApp::problem(res, StatusCode::NOT_FOUND).await;
    let res = app
        .get(&format!("/api/v1/outbound-messages/{}", Uuid::new_v4()))
        .await;
    TestApp::problem(res, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn optimistic_concurrency_and_idempotency_extractors() {
    let Some((app, csrf, _world)) = test_app().await else {
        return;
    };
    let id = Uuid::new_v4();

    // PATCH without If-Match → 428 VERSION_PRECONDITION_REQUIRED.
    let res = app
        .client
        .patch(app.url(&format!("/api/v1/message-drafts/{id}")))
        .header("x-csrf-token", &csrf)
        .json(&json!({"subject": "x"}))
        .send()
        .await
        .unwrap();
    let p = TestApp::problem(res, StatusCode::from_u16(428).unwrap()).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Freeze with If-Match but without Idempotency-Key is also rejected.
    let res = post(
        &app,
        &csrf,
        &format!("/api/v1/message-drafts/{id}/freeze"),
        json!({"delivery_channel_id": Uuid::new_v4()}),
        &[("if-match", "\"1\"")],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // Freeze without If-Match → 428.
    let res = post(
        &app,
        &csrf,
        &format!("/api/v1/message-drafts/{id}/freeze"),
        json!({"delivery_channel_id": Uuid::new_v4()}),
        &[("idempotency-key", "freeze-without-version")],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::from_u16(428).unwrap()).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Cancel without If-Match → 428.
    let res = post(
        &app,
        &csrf,
        &format!("/api/v1/outbound-messages/{id}/cancel"),
        json!({}),
        &[],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::from_u16(428).unwrap()).await;
    assert_eq!(p["code"], "VERSION_PRECONDITION_REQUIRED");

    // Submit without Idempotency-Key → 422 (external-effect create).
    let res = post(
        &app,
        &csrf,
        &format!("/api/v1/outbound-messages/{id}/submit"),
        json!({"expected_payload_hash": "0".repeat(64)}),
        &[],
    )
    .await;
    let p = TestApp::problem(res, StatusCode::BAD_REQUEST).await;
    assert_eq!(p["code"], "VALIDATION_FAILED");

    // With both headers the flow reaches the use case → 404.
    let res = post(
        &app,
        &csrf,
        &format!("/api/v1/outbound-messages/{id}/submit"),
        json!({"expected_payload_hash": "0".repeat(64)}),
        &[("idempotency-key", "k-404")],
    )
    .await;
    TestApp::problem(res, StatusCode::NOT_FOUND).await;

    // Preflight exists and reaches the use case → 404.
    let res = post(
        &app,
        &csrf,
        &format!("/api/v1/outbound-messages/{id}/preflight"),
        json!({}),
        &[],
    )
    .await;
    TestApp::problem(res, StatusCode::NOT_FOUND).await;

    // Approval-requests exists → 404 (resource first).
    let res = post(
        &app,
        &csrf,
        &format!("/api/v1/outbound-messages/{id}/approval-requests"),
        json!({"reason": "r", "expected_payload_hash": "0".repeat(64)}),
        &[("idempotency-key", "approval-missing-target")],
    )
    .await;
    TestApp::problem(res, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn draft_thread_filter_binds_cursor_and_frozen_drafts_resume() {
    let Some((app, csrf, world)) = test_app().await else {
        return;
    };
    let ws = world.workspace_id;
    let mut tx = app.state.db.begin(ws).await.unwrap();
    let mut addresses = Vec::new();
    for n in 0..3 {
        let address = format!("reply-filter-{n}-{}@example.test", Uuid::new_v4());
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id,address_original,normalized_key)
             VALUES ($1,$2,$2) RETURNING id",
        )
        .bind(ws)
        .bind(address)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        addresses.push(id);
    }
    let thread_a: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id)
         VALUES ($1,$2) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let thread_b: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id)
         VALUES ($1,$2) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut draft_ids = Vec::new();
    for (n, (address_id, thread_id)) in [
        (addresses[0], Some(thread_a)),
        (addresses[1], Some(thread_a)),
        (addresses[2], Some(thread_b)),
    ]
    .into_iter()
    .enumerate()
    {
        let mut body = json!({
            "mail_account_id": world.mail_account_id,
            "to_email_address_id": address_id,
            "purpose": "correspondence",
            "subject": "Reply",
            "body_text": "Hello",
        });
        body["thread_id"] = serde_json::to_value(thread_id).unwrap();
        let key = format!("reply-filter-create-{n:02}");
        let response = post(
            &app,
            &csrf,
            "/api/v1/message-drafts",
            body,
            &[("idempotency-key", &key)],
        )
        .await;
        let draft = TestApp::data(response).await;
        draft_ids.push(draft["draft_id"].as_str().unwrap().to_string());
    }

    let first = app
        .get(&format!(
            "/api/v1/message-drafts?thread_id={thread_a}&limit=1"
        ))
        .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: Value = first.json().await.unwrap();
    assert_eq!(first["data"].as_array().unwrap().len(), 1);
    assert_eq!(first["data"][0]["thread_id"], thread_a.to_string());
    assert!(first["data"][0]["outbound_message_id"].is_null());
    assert_eq!(first["page"]["has_more"], true);
    let cursor = first["page"]["next_cursor"].as_str().unwrap();

    let second = app
        .get(&format!(
            "/api/v1/message-drafts?thread_id={thread_a}&limit=1&cursor={cursor}"
        ))
        .await;
    let second = TestApp::data(second).await;
    assert_eq!(second.as_array().unwrap().len(), 1);
    assert_ne!(first["data"][0]["draft_id"], second[0]["draft_id"]);

    let stale_filter = app
        .get(&format!(
            "/api/v1/message-drafts?thread_id={thread_b}&limit=1&cursor={cursor}"
        ))
        .await;
    let problem = TestApp::problem(stale_filter, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "VALIDATION_FAILED");

    let (sender_id, channel_id) = {
        let mut tx = app.state.db.begin(ws).await.unwrap();
        let sender_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sender_identities
                (workspace_id, mail_account_id, address, display_name, status, verified_at)
             VALUES ($1,$2,$3,'Reply sender','verified',now()) RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(format!("reply-sender-{}@test.dev", Uuid::new_v4()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let channel_id: Uuid = sqlx::query_scalar(
            "INSERT INTO delivery_channels
                (workspace_id,mail_account_id,sender_identity_id,connection_id,
                 provider,purpose,name,policy_status,enabled)
             VALUES ($1,$2,$3,$4,'zoho_mail','correspondence','Reply','allowed',true)
             RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(sender_id)
        .bind(world.connection_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (sender_id, channel_id)
    };
    let draft_id = Uuid::parse_str(&draft_ids[0]).unwrap();
    let freeze = post(
        &app,
        &csrf,
        &format!("/api/v1/message-drafts/{draft_id}/freeze"),
        json!({"delivery_channel_id": channel_id, "sender_identity_id": sender_id}),
        &[
            ("if-match", "\"1\""),
            ("idempotency-key", "reply-freeze-000001"),
        ],
    )
    .await;
    let freeze = TestApp::data(freeze).await;
    let outbound_id = freeze["outbound_message_id"].as_str().unwrap();
    let detail = TestApp::data(app.get(&format!("/api/v1/message-drafts/{draft_id}")).await).await;
    assert_eq!(detail["outbound_message_id"], outbound_id);

    let resumed = app
        .get(&format!(
            "/api/v1/message-drafts?thread_id={thread_a}&limit=10"
        ))
        .await;
    let resumed = TestApp::data(resumed).await;
    let item = resumed
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["draft_id"] == draft_id.to_string())
        .unwrap();
    assert_eq!(item["outbound_message_id"], outbound_id);
}

#[tokio::test]
async fn sending_create_freeze_and_approval_idempotency_replay_without_duplicate_capabilities() {
    let Some((app, csrf, world)) = test_app().await else {
        return;
    };
    let ws = world.workspace_id;
    let ctx = ctx_for(&world, &world.admin, "sending-idempotency");
    let (to, channel, meeting_type) = {
        let mut tx = app.state.db.begin(ws).await.unwrap();
        let to: Uuid = sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id,address_original,normalized_key)
             VALUES ($1,$2,$2) RETURNING id",
        )
        .bind(ws)
        .bind(format!("idempotency-{}@example.test", Uuid::new_v4()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO communication_bases
                (workspace_id,email_address_id,purpose,jurisdiction,basis_type,status,
                 evidence_ref,collected_at,reviewed_by,reviewed_at)
             VALUES ($1,$2,'marketing','KR','explicit_consent','valid','test',now(),$3,now())",
        )
        .bind(ws)
        .bind(to)
        .bind(world.admin.principal_id)
        .execute(tx.conn())
        .await
        .unwrap();

        let delivery_connection: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id,provider,region,secret_ref,status,connected_by)
             VALUES ($1,'delivery_provider','us','sending-idempotency-delivery','active',$2)
             RETURNING id",
        )
        .bind(ws)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let sender_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sender_identities
                (workspace_id,mail_account_id,address,display_name,status,verified_at)
             VALUES ($1,$2,$3,'Idempotency sender','verified',now()) RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(format!("sender-{}@test.dev", Uuid::new_v4()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let channel: Uuid = sqlx::query_scalar(
            "INSERT INTO delivery_channels
                (workspace_id,mail_account_id,sender_identity_id,connection_id,
                 provider,purpose,name,policy_status,enabled)
             VALUES ($1,$2,$3,$4,'delivery_provider','marketing','Idempotency','allowed',true)
             RETURNING id",
        )
        .bind(ws)
        .bind(world.mail_account_id)
        .bind(sender_id)
        .bind(delivery_connection)
        .fetch_one(tx.conn())
        .await
        .unwrap();

        let calendar_connection: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id,provider,region,secret_ref,status,connected_by)
             VALUES ($1,'zoho_calendar','us','sending-idempotency-calendar','active',$2)
             RETURNING id",
        )
        .bind(ws)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let calendar: Uuid = sqlx::query_scalar(
            "INSERT INTO calendar_connections
                (workspace_id,host_principal_id,connection_id,provider_calendar_id,status)
             VALUES ($1,$2,$3,$4,'active') RETURNING id",
        )
        .bind(ws)
        .bind(world.admin.principal_id)
        .bind(calendar_connection)
        .bind(format!("calendar-{}", Uuid::new_v4()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let meeting_type: Uuid = sqlx::query_scalar(
            "INSERT INTO meeting_types (workspace_id,host_principal_id,calendar_connection_id,name)
             VALUES ($1,$2,$3,'Idempotency meeting') RETURNING id",
        )
        .bind(ws)
        .bind(world.admin.principal_id)
        .bind(calendar)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (to, channel, meeting_type)
    };

    let template = application::templates::create(
        &app.state.db,
        &ctx,
        &CreateEmailTemplateRequest {
            name: format!("idem-template-{}", Uuid::new_v4()),
        },
    )
    .await
    .unwrap();
    let version = application::templates::create_version(
        &app.state.db,
        &ctx,
        template.template_id,
        &CreateTemplateVersionRequest {
            subject: "Hello".into(),
            body_text: "Hello {{compliance.unsubscribe_url}} {{meeting.booking_url}}".into(),
            body_html: None,
            variable_schema: json!({
                "type": "object",
                "properties": {
                    "compliance.unsubscribe_url": {"type": "string", "format": "uri"},
                    "meeting.booking_url": {"type": "string", "format": "uri"}
                },
                "required": ["compliance.unsubscribe_url", "meeting.booking_url"],
                "additionalProperties": false
            }),
        },
    )
    .await
    .unwrap();

    let create_body = json!({
        "mail_account_id": world.mail_account_id,
        "to_email_address_id": to,
        "purpose": "marketing",
        "subject": "Draft",
        "body_text": "Draft body",
        "template_version_id": version.template_version_id,
    });
    let created = post(
        &app,
        &csrf,
        "/api/v1/message-drafts",
        create_body.clone(),
        &[("idempotency-key", "sending-create-idem-0001")],
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.unwrap();
    let draft_id = created["data"]["draft_id"].as_str().unwrap();
    let replay = post(
        &app,
        &csrf,
        "/api/v1/message-drafts",
        create_body.clone(),
        &[("idempotency-key", "sending-create-idem-0001")],
    )
    .await;
    let replay: Value = replay.json().await.unwrap();
    assert_eq!(replay["data"]["draft_id"], draft_id);
    let mut changed_create = create_body;
    changed_create["subject"] = json!("Changed");
    let reused = post(
        &app,
        &csrf,
        "/api/v1/message-drafts",
        changed_create,
        &[("idempotency-key", "sending-create-idem-0001")],
    )
    .await;
    let reused = TestApp::problem(reused, StatusCode::CONFLICT).await;
    assert_eq!(reused["code"], "IDEMPOTENCY_KEY_REUSED");

    let freeze_path = format!("/api/v1/message-drafts/{draft_id}/freeze");
    let freeze_body = json!({
        "delivery_channel_id": channel,
        "meeting_type_id": meeting_type,
    });
    let freeze_headers = [
        ("if-match", "\"1\""),
        ("idempotency-key", "sending-freeze-idem-0001"),
    ];
    let frozen = post(
        &app,
        &csrf,
        &freeze_path,
        freeze_body.clone(),
        &freeze_headers,
    )
    .await;
    assert_eq!(frozen.status(), StatusCode::CREATED);
    let frozen: Value = frozen.json().await.unwrap();
    let outbound_id = frozen["data"]["outbound_message_id"].as_str().unwrap();
    let payload_hash = frozen["data"]["payload_hash"].as_str().unwrap();
    let replay = post(
        &app,
        &csrf,
        &freeze_path,
        freeze_body.clone(),
        &freeze_headers,
    )
    .await;
    let replay: Value = replay.json().await.unwrap();
    assert_eq!(replay["data"]["outbound_message_id"], outbound_id);
    assert_eq!(replay["data"]["payload_hash"], payload_hash);
    let changed_freeze = post(
        &app,
        &csrf,
        &freeze_path,
        json!({
            "delivery_channel_id": Uuid::new_v4(),
            "meeting_type_id": meeting_type,
        }),
        &freeze_headers,
    )
    .await;
    let changed_freeze = TestApp::problem(changed_freeze, StatusCode::CONFLICT).await;
    assert_eq!(changed_freeze["code"], "IDEMPOTENCY_KEY_REUSED");

    let approval_path = format!("/api/v1/outbound-messages/{outbound_id}/approval-requests");
    let approval_body = json!({
        "reason": "Review marketing copy",
        "expected_payload_hash": payload_hash,
    });
    let approval = post(
        &app,
        &csrf,
        &approval_path,
        approval_body.clone(),
        &[("idempotency-key", "sending-approval-idem-0001")],
    )
    .await;
    assert_eq!(approval.status(), StatusCode::CREATED);
    let approval: Value = approval.json().await.unwrap();
    let approval_id = approval["data"]["approval_request_id"].as_str().unwrap();
    let approval_replay = post(
        &app,
        &csrf,
        &approval_path,
        approval_body,
        &[("idempotency-key", "sending-approval-idem-0001")],
    )
    .await;
    let approval_replay: Value = approval_replay.json().await.unwrap();
    assert_eq!(approval_replay["data"]["approval_request_id"], approval_id);

    let mut tx = app.state.db.begin(ws).await.unwrap();
    let outbound_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbound_messages WHERE workspace_id = $1 AND source_draft_id = $2",
    )
    .bind(ws)
    .bind(Uuid::parse_str(draft_id).unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let unsubscribe_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM unsubscribe_tokens WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(Uuid::parse_str(outbound_id).unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let booking_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM booking_links WHERE workspace_id = $1
           AND meeting_type_id = $2 AND bound_email_address_id = $3",
    )
    .bind(ws)
    .bind(meeting_type)
    .bind(to)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let approval_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM approval_requests WHERE workspace_id = $1
           AND outbound_message_id = $2 AND action_type = 'message_submit'",
    )
    .bind(ws)
    .bind(Uuid::parse_str(outbound_id).unwrap())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(outbound_count, 1);
    assert_eq!(unsubscribe_count, 1);
    assert_eq!(booking_count, 1);
    assert_eq!(approval_count, 1);
}
