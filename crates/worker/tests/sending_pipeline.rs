//! sending module pipeline tests (B.3.10/B.3.11, §8.3, A.6.5):
//! draft → freeze → approval → submit → queued/dispatch-job, cancel.
//!
//! T-rows: T04 (idempotent submit), T15 (unique reply claim), T16 (new
//! inbound → re-review), T34-read-side (queued state reads). Dispatch-side
//! rows (T10–T13, accepted/failed/unknown) live in `sending_dispatch.rs`.
//!
//! These use raw SQL fixtures (`sqlx`), so they live in the worker test
//! crate — api tests can't link sqlx.

use std::collections::BTreeSet;

use application::sending;
use contracts::ErrorCode;
use contracts::sending::{
    CancelOutboundRequest, CreateMessageDraftRequest, CreateOutboundApprovalRequest,
    FreezeMessageDraftRequest, PatchMessageDraftRequest, SubmitOutboundRequest,
};
use contracts::templates::{CreateEmailTemplateRequest, CreateTemplateVersionRequest};
use infrastructure::db::{Db, ROLE_API, ROLE_WORKER, Tx};
use infrastructure::testing::{TestPrincipal, TestWorld, ctx_for, seed_workspace};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

async fn test_db() -> Option<(Db, TestWorld)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("DATABASE_URL not set — skipping pipeline test");
            return None;
        }
    };
    let owner = Db::connect(&url, None).await.unwrap();
    owner.migrate().await.unwrap();
    let seed_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let world = seed_workspace(&seed_db).await.unwrap();
    let db = Db::connect(&url, Some(ROLE_WORKER)).await.unwrap();
    Some((db, world))
}

async fn api_runtime_db() -> Db {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    Db::connect(&url, Some(ROLE_API)).await.unwrap()
}

// ---------------------------------------------------------------------------
// SQL fixtures — workspace tx (RLS path), same as production code.
// ---------------------------------------------------------------------------

async fn enable_sending(_db: &Db, ws: Uuid) {
    set_emergency_stop(ws, false).await;
}

async fn set_emergency_stop(ws: Uuid, enabled: bool) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let control_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let mut tx = control_db.begin(ws).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = $2 WHERE id = $1")
        .bind(ws)
        .bind(enabled)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn set_public_base_url(ws: Uuid, base_url: &str) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let control_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let mut tx = control_db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE workspaces
            SET settings = settings || jsonb_build_object('public_base_url', $2::text)
          WHERE id = $1",
    )
    .bind(ws)
    .bind(base_url)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

async fn meeting_type_for_host(db: &Db, world: &TestWorld, host: Uuid) -> Uuid {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let integration_id: Uuid = sqlx::query_scalar(
        "INSERT INTO integration_connections
            (workspace_id, provider, region, secret_ref, status, connected_by)
         VALUES ($1,'zoho_calendar','us',$2,'active',$3) RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(format!("calendar-test-{}", Uuid::new_v4()))
    .bind(world.admin.principal_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let calendar_id: Uuid = sqlx::query_scalar(
        "INSERT INTO calendar_connections
            (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
         VALUES ($1,$2,$3,$4,'active') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(host)
    .bind(integration_id)
    .bind(format!("calendar-{}", Uuid::new_v4()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let meeting_type_id: Uuid = sqlx::query_scalar(
        "INSERT INTO meeting_types
            (workspace_id, host_principal_id, calendar_connection_id, name)
         VALUES ($1,$2,$3,'Freeze test meeting') RETURNING id",
    )
    .bind(world.workspace_id)
    .bind(host)
    .bind(calendar_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    meeting_type_id
}

async fn booking_template_version(db: &Db, ctx: &contracts::Ctx) -> Uuid {
    let template = application::templates::create(
        db,
        ctx,
        &CreateEmailTemplateRequest {
            name: format!("booking-{}", Uuid::new_v4()),
        },
    )
    .await
    .unwrap();
    application::templates::create_version(
        db,
        ctx,
        template.template_id,
        &CreateTemplateVersionRequest {
            subject: "Your meeting link".into(),
            body_text: "Book here: {{meeting.booking_url}}".into(),
            body_html: None,
            variable_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "meeting.booking_url": {"type": "string", "format": "uri"}
                },
                "required": ["meeting.booking_url"],
                "additionalProperties": false
            }),
        },
    )
    .await
    .unwrap()
    .template_version_id
}

/// Verified sender + enabled/policy-allowed channel for `purpose`.
/// zoho_mail may only enable `correspondence` (DDL CHECK) — other purposes
/// route through `delivery_provider`.
async fn sender_and_channel(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    connection_id: Uuid,
    purpose: &str,
) -> (Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    let provider = if purpose == "correspondence" {
        "zoho_mail"
    } else {
        "delivery_provider"
    };
    // fk_151: (connection_id, provider) must match an integration_connections
    // row — non-correspondence channels get their own provider connection.
    let connection_id = if provider == "zoho_mail" {
        connection_id
    } else {
        let owner: Uuid = sqlx::query_scalar(
            "SELECT connected_by FROM integration_connections
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(connection_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id, provider, region, secret_ref, status, connected_by)
             VALUES ($1, $2, 'us', 'test-secret-ref', 'active', $3)
             RETURNING id",
        )
        .bind(ws)
        .bind(provider)
        .bind(owner)
        .fetch_one(tx.conn())
        .await
        .unwrap()
    };
    let ident: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities
            (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1,$2,$3,'Sender','verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(mail_account_id)
    .bind(format!("sender-{}@test.dev", Uuid::new_v4().simple()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name, policy_status, enabled)
         VALUES ($1,$2,$3,$4,$5,$6,'ch','allowed',true) RETURNING id",
    )
    .bind(ws)
    .bind(mail_account_id)
    .bind(ident)
    .bind(connection_id)
    .bind(provider)
    .bind(purpose)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    (ident, channel)
}

async fn email_address(tx: &mut Tx<'_>, local: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
         VALUES ($1,$2,$2) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(format!("{local}-{}@example.com", Uuid::new_v4().simple()))
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

/// (thread_id, last_inbound_message_id, from_address_id) with `n` inbound messages.
async fn thread_with_inbound(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    n: usize,
) -> (Uuid, Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    let from_address_id = email_address(tx, "inbound-from").await;
    let (from_address, _) = infrastructure::repo::sending::email_address_row(tx, from_address_id)
        .await
        .unwrap()
        .unwrap();
    let thread: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id)
         VALUES ($1,$2) RETURNING id",
    )
    .bind(ws)
    .bind(mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let mut last = Uuid::nil();
    for i in 0..n {
        last = sqlx::query_scalar(
            "INSERT INTO email_messages
                (workspace_id, mail_account_id, thread_id, direction,
                 internet_message_id, subject, message_at)
             VALUES ($1,$2,$3,'inbound',$4,'q',$5) RETURNING id",
        )
        .bind(ws)
        .bind(mail_account_id)
        .bind(thread)
        .bind(format!("<m{i}-{thread}@test>"))
        .bind(chrono::Utc::now() + chrono::Duration::seconds(i as i64))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        infrastructure::repo::inbox::insert_participant(
            tx,
            last,
            "from",
            &from_address,
            None,
            Some(from_address_id),
        )
        .await
        .unwrap();
    }
    sqlx::query(
        "UPDATE email_threads SET last_inbound_message_id = $3
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(thread)
    .bind(last)
    .execute(tx.conn())
    .await
    .unwrap();
    (thread, last, from_address_id)
}

async fn thread_with_from_participant(
    tx: &mut Tx<'_>,
    mail_account_id: Uuid,
    from_address: &str,
    from_email_address_id: Option<Uuid>,
) -> (Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    let thread_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id)
         VALUES ($1,$2) RETURNING id",
    )
    .bind(ws)
    .bind(mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let message_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction,
             internet_message_id, subject, message_at)
         VALUES ($1,$2,$3,'inbound',$4,'reply recipient fixture',now()) RETURNING id",
    )
    .bind(ws)
    .bind(mail_account_id)
    .bind(thread_id)
    .bind(format!("<reply-{}@test>", Uuid::new_v4().simple()))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    infrastructure::repo::inbox::insert_participant(
        tx,
        message_id,
        "from",
        from_address,
        None,
        from_email_address_id,
    )
    .await
    .unwrap();
    sqlx::query(
        "UPDATE email_threads SET last_inbound_message_id=$3
          WHERE workspace_id=$1 AND id=$2",
    )
    .bind(ws)
    .bind(thread_id)
    .bind(message_id)
    .execute(tx.conn())
    .await
    .unwrap();
    (thread_id, message_id)
}

/// `send_basis` at dispatch needs a valid basis for the purpose (a reply
/// counts too — see policy). Attach one up front for every fixture.
async fn valid_basis(tx: &mut Tx<'_>, email_address_id: Uuid, reviewer: Uuid, purpose: &str) {
    sqlx::query(
        "INSERT INTO communication_bases
            (workspace_id, email_address_id, purpose, jurisdiction, basis_type,
             status, evidence_ref, collected_at, reviewed_by, reviewed_at)
         VALUES ($1,$2,$3,'KR','explicit_consent','valid','test',now(),$4,now())",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose)
    .bind(reviewer)
    .execute(tx.conn())
    .await
    .unwrap();
}

fn draft_req(
    world: &TestWorld,
    to: Uuid,
    purpose: &str,
    extra: impl FnOnce(&mut CreateMessageDraftRequest),
) -> CreateMessageDraftRequest {
    let mut r = CreateMessageDraftRequest {
        mail_account_id: world.mail_account_id,
        to_email_address_id: to,
        purpose: purpose.to_string(),
        subject: "Re: hi".into(),
        body_text: "hello back".into(),
        body_html: None,
        template_version_id: None,
        variables: None,
        thread_id: None,
        reply_to_message_id: None,
        expected_last_inbound_id: None,
        visibility: None,
    };
    extra(&mut r);
    r
}

/// channel + recipient (+ basis) for `purpose`.
async fn send_fixture(db: &Db, world: &TestWorld, purpose: &str, local: &str) -> (Uuid, Uuid) {
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let (_, channel) =
        sender_and_channel(&mut tx, world.mail_account_id, world.connection_id, purpose).await;
    let to = email_address(&mut tx, local).await;
    valid_basis(&mut tx, to, world.admin.principal_id, purpose).await;
    tx.commit().await.unwrap();
    (channel, to)
}

/// create + freeze → (outbound_id, payload_hash).
async fn frozen_outbound(
    db: &Db,
    world: &TestWorld,
    author: &TestPrincipal,
    channel: Uuid,
    req: &CreateMessageDraftRequest,
) -> (Uuid, String) {
    let ctx = ctx_for(world, author, "fixture");
    let draft = sending::create_draft(db, &ctx, req).await.unwrap();
    let frozen = sending::freeze_draft(
        db,
        &ctx,
        draft.draft_id,
        1,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    (frozen.outbound_message_id, frozen.payload_hash)
}

async fn submit(
    db: &Db,
    world: &TestWorld,
    author: &TestPrincipal,
    outbound: Uuid,
    hash: &str,
    key: &str,
) -> contracts::sending::SubmitOutboundResponse {
    let ctx = ctx_for(world, author, "submit");
    sending::submit(
        db,
        &ctx,
        outbound,
        &SubmitOutboundRequest {
            expected_payload_hash: hash.to_string(),
            not_before: None,
        },
        key,
    )
    .await
    .unwrap()
    .body
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn draft_lifecycle_freeze_immutable() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, to) = send_fixture(&db, &world, "correspondence", "life").await;
    let ctx = ctx_for(&world, &world.admin, "life");

    let draft = sending::create_draft(&db, &ctx, &draft_req(&world, to, "correspondence", |_| {}))
        .await
        .unwrap();
    assert_eq!(draft.status, "draft");
    assert_eq!(draft.version, 1);

    // List contains exactly this draft.
    let items = sending::list_drafts(&db, &ctx, 50, None).await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].draft_id, draft.draft_id);

    // Stale version → VERSION_MISMATCH.
    let err = sending::patch_draft(
        &db,
        &ctx,
        draft.draft_id,
        99,
        &PatchMessageDraftRequest {
            subject: Some("new".into()),
            body_text: None,
            body_html: None,
            variables: None,
            to_email_address_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::VersionMismatch);

    // Good version → patched, version bumped.
    let patched = sending::patch_draft(
        &db,
        &ctx,
        draft.draft_id,
        1,
        &PatchMessageDraftRequest {
            subject: Some("new subject".into()),
            body_text: None,
            body_html: None,
            variables: None,
            to_email_address_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(patched.version, 2);
    assert_eq!(patched.subject, "new subject");

    // Freeze → immutable outbound snapshot + payload hash.
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        2,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(frozen.status, "draft");
    assert_eq!(frozen.payload_hash.len(), 64);

    // The draft row is now `frozen` — no second freeze, no patch.
    let err = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        2,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let err = sending::patch_draft(
        &db,
        &ctx,
        draft.draft_id,
        2,
        &PatchMessageDraftRequest {
            subject: Some("x".into()),
            body_text: None,
            body_html: None,
            variables: None,
            to_email_address_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);

    // The frozen snapshot is immutable even under direct SQL — a
    // DB-level guard (trigger) rejects edits to frozen columns.
    let mut tx = db.begin(ws).await.unwrap();
    let res = sqlx::query(
        "UPDATE outbound_messages SET subject = 'tampered'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(frozen.outbound_message_id)
    .execute(tx.conn())
    .await;
    assert!(res.is_err(), "frozen snapshot must reject edits: {res:?}");
    let _ = tx.rollback().await;
}

#[tokio::test]
async fn freeze_mints_recipient_bound_booking_url_and_denies_foreign_host() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    set_public_base_url(ws, "https://booking.example").await;
    let ctx = ctx_for(&world, &world.admin, "booking-freeze");
    let host_meeting_type = meeting_type_for_host(&db, &world, world.admin.principal_id).await;
    let template_version_id = booking_template_version(&db, &ctx).await;
    let (channel, to) = send_fixture(&db, &world, "correspondence", "booking-freeze").await;
    let mut request = draft_req(&world, to, "correspondence", |_| {});
    request.template_version_id = Some(template_version_id);
    let draft = sending::create_draft(&db, &ctx, &request).await.unwrap();

    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: Some(host_meeting_type),
            tracking_requested: false,
        },
    )
    .await
    .unwrap();

    let mut tx = db.begin(ws).await.unwrap();
    let outbound = infrastructure::repo::sending::get_outbound(&mut tx, frozen.outbound_message_id)
        .await
        .unwrap()
        .unwrap();
    let url = outbound.body_text.strip_prefix("Book here: ").unwrap();
    let token = url
        .strip_prefix("https://booking.example/public/book/")
        .expect("booking URL uses the configured public origin");
    assert!(token.len() >= 43, "token must carry at least 256 bits");
    assert!(
        !token.contains('/'),
        "raw capability must be one opaque path segment"
    );

    let token_hash = infrastructure::crypto::sha256_bytes(token.as_bytes());
    let (stored_meeting_type, bound_email, stored_hash, max_uses): (
        Uuid,
        Option<Uuid>,
        Vec<u8>,
        i32,
    ) = sqlx::query_as(
        "SELECT meeting_type_id, bound_email_address_id, token_hash, max_uses
           FROM booking_links WHERE workspace_id = $1 AND token_hash = $2",
    )
    .bind(ws)
    .bind(token_hash.to_vec())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(stored_meeting_type, host_meeting_type);
    assert_eq!(bound_email, Some(to));
    assert_eq!(stored_hash, token_hash);
    assert_eq!(max_uses, 1);

    let mail_account_id = outbound.mail_account_id.to_string();
    let delivery_channel_id = outbound.delivery_channel_id.to_string();
    let sender_identity_id = outbound.sender_identity_id.to_string();
    let to_email_address_id = outbound.to_email_address_id.to_string();
    let template_version_id_text = template_version_id.to_string();
    let preimage = domain::sending::payload_preimage(
        &outbound.purpose,
        &mail_account_id,
        &delivery_channel_id,
        &sender_identity_id,
        &outbound.from_address_snapshot,
        &to_email_address_id,
        &outbound.to_address_snapshot,
        &outbound.subject,
        &outbound.body_text,
        outbound.body_html.as_deref(),
        outbound.thread_id.map(|id| id.to_string()).as_deref(),
        outbound
            .reply_to_message_id
            .map(|id| id.to_string())
            .as_deref(),
        outbound
            .communication_basis_id
            .map(|id| id.to_string())
            .as_deref(),
        Some(&template_version_id_text),
        outbound.internet_message_id.as_deref(),
    );
    let expected_hash = infrastructure::crypto::sha256_bytes(&preimage);
    assert_eq!(outbound.payload_hash, expected_hash);
    assert_eq!(
        frozen.payload_hash,
        infrastructure::crypto::sha256_hex(&preimage)
    );
    tx.rollback().await.unwrap();

    // A member with mail.compose but no booking-host authority cannot mint a
    // capability for another host's meeting type.
    let foreign_meeting_type =
        meeting_type_for_host(&db, &world, world.member_a.principal_id).await;
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "INSERT INTO mail_account_grants
            (workspace_id, mail_account_id, principal_id,
             can_read, can_compose, can_send, granted_by)
         VALUES ($1,$2,$3,true,true,true,$4)
         ON CONFLICT (workspace_id, mail_account_id, principal_id)
         DO UPDATE SET can_read = true, can_compose = true, can_send = true",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(world.member_b.principal_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let member_ctx = ctx_for(&world, &world.member_b, "foreign-host-freeze");
    let mut member_request = draft_req(&world, to, "correspondence", |_| {});
    member_request.template_version_id = Some(template_version_id);
    let member_draft = sending::create_draft(&db, &member_ctx, &member_request)
        .await
        .unwrap();
    let before_links: i64 = {
        let mut tx = db.begin(ws).await.unwrap();
        let count =
            sqlx::query_scalar("SELECT count(*) FROM booking_links WHERE workspace_id = $1")
                .bind(ws)
                .fetch_one(tx.conn())
                .await
                .unwrap();
        tx.rollback().await.unwrap();
        count
    };
    let err = sending::freeze_draft(
        &db,
        &member_ctx,
        member_draft.draft_id,
        member_draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: Some(foreign_meeting_type),
            tracking_requested: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);
    let after_links: i64 = {
        let mut tx = db.begin(ws).await.unwrap();
        let count =
            sqlx::query_scalar("SELECT count(*) FROM booking_links WHERE workspace_id = $1")
                .bind(ws)
                .fetch_one(tx.conn())
                .await
                .unwrap();
        tx.rollback().await.unwrap();
        count
    };
    assert_eq!(
        after_links, before_links,
        "denied freeze must not mint a link"
    );
}

#[tokio::test]
async fn marketing_freeze_mints_confirmation_link_and_adds_only_missing_footers() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    set_public_base_url(ws, "https://mail.example").await;
    let ctx = ctx_for(&world, &world.admin, "marketing-unsubscribe-footer");

    // A raw marketing draft still gets a durable unsubscribe capability even
    // when no template declares the system placeholder.
    let (channel, to) = send_fixture(&db, &world, "marketing", "marketing-raw-footer").await;
    let mut request = draft_req(&world, to, "marketing", |draft| {
        draft.body_text = "Hello there".into();
        draft.body_html = Some("<p>Hello there</p>".into());
    });
    let draft = sending::create_draft(&db, &ctx, &request).await.unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();

    let mut tx = db.begin(ws).await.unwrap();
    let outbound = infrastructure::repo::sending::get_outbound(&mut tx, frozen.outbound_message_id)
        .await
        .unwrap()
        .unwrap();
    let footer = outbound
        .body_text
        .strip_prefix("Hello there\n\nUnsubscribe: ")
        .expect("plain marketing body has a visible unsubscribe footer");
    assert!(footer.starts_with("https://mail.example/public/unsubscribe/"));
    let token = footer
        .strip_prefix("https://mail.example/public/unsubscribe/")
        .unwrap();
    assert_eq!(token.len(), 43);
    let html = outbound.body_html.as_deref().unwrap();
    assert!(html.contains(&format!("href=\"{footer}\">Unsubscribe</a>")));
    let (stored_email, stored_outbound, stored_hash): (Uuid, Uuid, Vec<u8>) = sqlx::query_as(
        "SELECT email_address_id, outbound_message_id, token_hash
           FROM unsubscribe_tokens
          WHERE workspace_id = $1 AND token_hash = $2",
    )
    .bind(ws)
    .bind(infrastructure::crypto::sha256_bytes(token.as_bytes()).to_vec())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(stored_email, to);
    assert_eq!(stored_outbound, outbound.id);
    assert_eq!(
        stored_hash,
        infrastructure::crypto::sha256_bytes(token.as_bytes()).to_vec()
    );
    tx.rollback().await.unwrap();

    // A template that already renders the generated URL in both body forms
    // keeps one footer, rather than receiving the automatic footer again.
    let template = application::templates::create(
        &db,
        &ctx,
        &CreateEmailTemplateRequest {
            name: format!("unsubscribe-{}", Uuid::new_v4()),
        },
    )
    .await
    .unwrap();
    let template_version = application::templates::create_version(
        &db,
        &ctx,
        template.template_id,
        &CreateTemplateVersionRequest {
            subject: "A note".into(),
            body_text: "Hello\nUnsubscribe: {{compliance.unsubscribe_url}}".into(),
            body_html: Some(
                "<p>Hello</p><p><a href=\"{{compliance.unsubscribe_url}}\">Unsubscribe</a></p>"
                    .into(),
            ),
            variable_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "compliance.unsubscribe_url": {"type": "string", "format": "uri"}
                },
                "required": ["compliance.unsubscribe_url"],
                "additionalProperties": false
            }),
        },
    )
    .await
    .unwrap();
    let (channel, to) = send_fixture(&db, &world, "marketing", "marketing-template-footer").await;
    request = draft_req(&world, to, "marketing", |draft| {
        draft.template_version_id = Some(template_version.template_version_id);
    });
    let draft = sending::create_draft(&db, &ctx, &request).await.unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    let outbound = infrastructure::repo::sending::get_outbound(&mut tx, frozen.outbound_message_id)
        .await
        .unwrap()
        .unwrap();
    let text_link = outbound.body_text.split("Unsubscribe: ").nth(1).unwrap();
    let url = text_link.trim();
    assert!(url.starts_with("https://mail.example/public/unsubscribe/"));
    assert_eq!(outbound.body_text.matches(url).count(), 1);
    assert_eq!(
        outbound.body_html.as_deref().unwrap().matches(url).count(),
        1
    );
    assert_eq!(
        outbound
            .body_html
            .as_deref()
            .unwrap()
            .matches("Unsubscribe</a>")
            .count(),
        1
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn private_draft_owner_loses_content_access_after_mailbox_grant_revocation() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    let (_channel, to) = send_fixture(&db, &world, "correspondence", "draft-revoke").await;
    let ctx = ctx_for(&world, &world.admin, "draft-revoke");
    let draft = sending::create_draft(&db, &ctx, &draft_req(&world, to, "correspondence", |_| {}))
        .await
        .unwrap();

    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants
            SET can_read = false, can_compose = false, can_send = false,
                can_approve = false, can_assign = false, version = version + 1
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(world.admin.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let err = sending::get_draft(&db, &ctx, draft.draft_id)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let list = sending::list_drafts(&db, &ctx, 50, None).await.unwrap();
    assert!(
        list.is_empty(),
        "revoked owners must not see draft subjects"
    );
    let err = sending::patch_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &PatchMessageDraftRequest {
            subject: Some("should not be editable".into()),
            body_text: None,
            body_html: None,
            variables: None,
            to_email_address_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::MailboxAccessDenied);
}

#[tokio::test]
async fn preflight_reports_full_chain_nonbinding() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, to) = send_fixture(&db, &world, "correspondence", "pre").await;
    let ctx = ctx_for(&world, &world.admin, "pre");
    let (outbound, _) = frozen_outbound(
        &db,
        &world,
        &world.admin,
        channel,
        &draft_req(&world, to, "correspondence", |_| {}),
    )
    .await;

    let pre = sending::preflight(&db, &ctx, outbound).await.unwrap();
    assert!(!pre.is_execution_authorization);
    let names: Vec<&str> = pre.checks.iter().map(|c| c.name.as_str()).collect();
    for required in [
        "channel_exists",
        "channel_enabled",
        "mail_account_active",
        "connection_active",
        "provider_purpose_policy",
        "requester_permission",
        "approval_state",
        "emergency_stop",
        "send_workers_locked",
        "recipient_not_suppressed",
        "reply_recipient",
        "send_basis",
        "reply_context",
        "sender_verified",
        "quiet_hours",
        "daily_cap",
        "payload_integrity",
    ] {
        assert!(names.contains(&required), "preflight missing {required}");
    }
    let failed: Vec<_> = pre.checks.iter().filter(|c| !c.passed).collect();
    assert!(failed.is_empty(), "unexpected failures: {failed:?}");
    assert!(pre.global_blocking_reasons.is_empty());
}

#[tokio::test]
async fn submit_guards_snapshot_approval_agent() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, to) = send_fixture(&db, &world, "correspondence", "guard").await;
    let ctx = ctx_for(&world, &world.admin, "guard");
    let (outbound, hash) = frozen_outbound(
        &db,
        &world,
        &world.admin,
        channel,
        &draft_req(&world, to, "correspondence", |_| {}),
    )
    .await;

    // Snapshot mismatch → 409 SNAPSHOT_MISMATCH.
    let err = sending::submit(
        &db,
        &ctx,
        outbound,
        &SubmitOutboundRequest {
            expected_payload_hash: "f".repeat(64),
            not_before: None,
        },
        "k-bad",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::SnapshotMismatch);

    // Agents can never submit — denial is audited (§5.4/T03).
    let agent_ctx = ctx_for(&world, &world.agent, "agent-submit");
    let err = sending::submit(
        &db,
        &agent_ctx,
        outbound,
        &SubmitOutboundRequest {
            expected_payload_hash: hash.clone(),
            not_before: None,
        },
        "k-agent",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::AgentDirectSendForbidden);
    let mut tx = db.begin(ws).await.unwrap();
    let denied: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs
          WHERE workspace_id = $1 AND action = 'outbound.submit'
            AND details->>'outcome' = 'denied:AGENT_DIRECT_SEND_FORBIDDEN'
            AND target_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(denied, 1);
    tx.rollback().await.unwrap();

    // Emergency stop rejects new submissions (B.5) — put it back after.
    set_emergency_stop(ws, true).await;
    let err = sending::submit(
        &db,
        &ctx,
        outbound,
        &SubmitOutboundRequest {
            expected_payload_hash: hash.clone(),
            not_before: None,
        },
        "k-stop",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::WorkspaceEmergencyStop);
    set_emergency_stop(ws, false).await;

    // Marketing without an approved request → conflict.
    let (mchannel, mto) = send_fixture(&db, &world, "marketing", "guard-m").await;
    let (moutbound, mhash) = frozen_outbound(
        &db,
        &world,
        &world.admin,
        mchannel,
        &draft_req(&world, mto, "marketing", |_| {}),
    )
    .await;
    let err = sending::submit(
        &db,
        &ctx,
        moutbound,
        &SubmitOutboundRequest {
            expected_payload_hash: mhash.clone(),
            not_before: None,
        },
        "k-m",
    )
    .await
    .unwrap_err();
    assert_eq!(
        err.code,
        ErrorCode::Conflict,
        "expected approval-required conflict: {err:?}"
    );

    // Approve the marketing outbound properly (ApprovalTarget::OutboundMessage).
    let req = sending::create_approval_request(
        &db,
        &ctx,
        moutbound,
        &CreateOutboundApprovalRequest {
            reason: "test".into(),
            expected_payload_hash: mhash.clone(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    // The request must be a message_submit targeting this outbound
    // (ApprovalTarget::OutboundMessage).
    let (action, target): (String, Uuid) = sqlx::query_as(
        "SELECT action_type, outbound_message_id FROM approval_requests
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(req.approval_request_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(action, "message_submit");
    assert_eq!(target, moutbound);
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision,
             snapshot_hash, version_at_decision)
         VALUES ($1,$2,$3,'approved',$4,1)",
    )
    .bind(ws)
    .bind(req.approval_request_id)
    .bind(world.manager.principal_id)
    .bind(hex_bytes(&mhash))
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE approval_requests SET status = 'approved'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(req.approval_request_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Now submit succeeds → queued + dispatch job.
    let res = submit(&db, &world, &world.admin, moutbound, &mhash, "k-m2").await;
    assert_eq!(res.status, "queued");
    let mut tx = db.begin(ws).await.unwrap();
    let kind: String =
        sqlx::query_scalar("SELECT kind FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(res.job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(kind, "sending.dispatch_outbound");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn t04_concurrent_idempotent_submit_one_execution() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, to) = send_fixture(&db, &world, "correspondence", "t04").await;
    let (outbound, hash) = frozen_outbound(
        &db,
        &world,
        &world.admin,
        channel,
        &draft_req(&world, to, "correspondence", |_| {}),
    )
    .await;

    // 8 racing submits with the SAME key+body — the idempotency claim
    // makes exactly one execute; followers replay its stored response.
    let mut handles = Vec::new();
    for i in 0..8 {
        let db = db.clone();
        let ctx = ctx_for(&world, &world.admin, &format!("t04-{i}"));
        let h = hash.clone();
        handles.push(tokio::spawn(async move {
            sending::submit(
                &db,
                &ctx,
                outbound,
                &SubmitOutboundRequest {
                    expected_payload_hash: h,
                    not_before: None,
                },
                "t04-key",
            )
            .await
            .unwrap()
            .body
        }));
    }
    let mut job_ids = BTreeSet::new();
    for h in handles {
        job_ids.insert(h.await.unwrap().job_id);
    }
    assert_eq!(job_ids.len(), 1, "replays must return the leader's job_id");

    let mut tx = db.begin(ws).await.unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id = $1 AND kind = 'sending.dispatch_outbound'
            AND payload->>'outbound_message_id' = $2",
    )
    .bind(ws)
    .bind(outbound.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(n, 1);
    tx.rollback().await.unwrap();

    // Same key + different request → 409 IDEMPOTENCY_KEY_REUSED.
    let ctx = ctx_for(&world, &world.admin, "t04-reuse");
    let err = sending::submit(
        &db,
        &ctx,
        outbound,
        &SubmitOutboundRequest {
            expected_payload_hash: "0".repeat(64),
            not_before: None,
        },
        "t04-key",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::IdempotencyKeyReused);

    // A different key is a new request — but the outbound is no longer
    // `draft`, so it conflicts rather than double-executing.
    let ctx = ctx_for(&world, &world.admin, "t04-new");
    let err = sending::submit(
        &db,
        &ctx,
        outbound,
        &SubmitOutboundRequest {
            expected_payload_hash: hash.clone(),
            not_before: None,
        },
        "t04-other-key",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[tokio::test]
async fn t15_two_users_one_reply_claim() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, _to) = send_fixture(&db, &world, "correspondence", "t15").await;

    // A thread with one inbound — both users reply to it.
    let mut tx = db.begin(ws).await.unwrap();
    let (thread, last, reply_recipient) =
        thread_with_inbound(&mut tx, world.mail_account_id, 1).await;
    tx.commit().await.unwrap();

    let req_a = draft_req(&world, reply_recipient, "correspondence", |r| {
        r.thread_id = Some(thread);
        r.reply_to_message_id = Some(last);
        r.expected_last_inbound_id = Some(last);
    });
    let (oa, ha) = frozen_outbound(&db, &world, &world.member_a, channel, &req_a).await;
    let (ob, hb) = frozen_outbound(&db, &world, &world.manager, channel, &req_a).await;

    // First submit wins the unique active claim (A.6.5).
    let res = submit(&db, &world, &world.member_a, oa, &ha, "t15-a").await;
    assert_eq!(res.status, "queued");
    let ctx = ctx_for(&world, &world.manager, "t15-b");
    let err = sending::submit(
        &db,
        &ctx,
        ob,
        &SubmitOutboundRequest {
            expected_payload_hash: hb,
            not_before: None,
        },
        "t15-b",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::ReplyAlreadySubmitted);

    let mut tx = db.begin(ws).await.unwrap();
    let claims: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reply_claims
          WHERE workspace_id = $1 AND inbound_message_id = $2
            AND state IN ('reserved','submitted','accepted')",
    )
    .bind(ws)
    .bind(last)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(claims, 1);
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn t16_new_inbound_during_draft_forces_rereview() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, _to) = send_fixture(&db, &world, "correspondence", "t16").await;
    let ctx = ctx_for(&world, &world.admin, "t16");

    let mut tx = db.begin(ws).await.unwrap();
    let (thread, last1, reply_recipient) =
        thread_with_inbound(&mut tx, world.mail_account_id, 1).await;
    tx.commit().await.unwrap();

    let req = draft_req(&world, reply_recipient, "correspondence", |r| {
        r.thread_id = Some(thread);
        r.reply_to_message_id = Some(last1);
        r.expected_last_inbound_id = Some(last1);
    });
    let draft = sending::create_draft(&db, &ctx, &req).await.unwrap();

    // A newer inbound lands while the draft is open — the reviewed
    // anchor is stale; freeze must refuse (T16).
    let mut tx = db.begin(ws).await.unwrap();
    let m2: Uuid = sqlx::query_scalar(
        "INSERT INTO email_messages
            (workspace_id, mail_account_id, thread_id, direction,
             internet_message_id, subject, message_at)
         VALUES ($1,$2,$3,'inbound','<m2@test>','re: q', now()) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(thread)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE email_threads SET last_inbound_message_id = $3
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(thread)
    .bind(m2)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let err = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        1,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::ReplyContextChanged);
}

#[tokio::test]
async fn reply_draft_create_rejects_recipient_different_from_inbound_from() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    let (_channel, unrelated_recipient) =
        send_fixture(&db, &world, "correspondence", "reply-create").await;
    let (reply_recipient, thread_id, inbound_id, from_address) = {
        let mut tx = db.begin(ws).await.unwrap();
        let reply_recipient = email_address(&mut tx, "actual-sender").await;
        valid_basis(
            &mut tx,
            reply_recipient,
            world.admin.principal_id,
            "correspondence",
        )
        .await;
        let (from_address, _) =
            infrastructure::repo::sending::email_address_row(&mut tx, reply_recipient)
                .await
                .unwrap()
                .unwrap();
        let (thread_id, inbound_id) = thread_with_from_participant(
            &mut tx,
            world.mail_account_id,
            &from_address,
            Some(reply_recipient),
        )
        .await;
        tx.commit().await.unwrap();
        (reply_recipient, thread_id, inbound_id, from_address)
    };
    assert_ne!(reply_recipient, unrelated_recipient);

    let ctx = ctx_for(&world, &world.admin, "reply-create-mismatch");
    let req = draft_req(&world, unrelated_recipient, "correspondence", |req| {
        req.thread_id = Some(thread_id);
        req.reply_to_message_id = Some(inbound_id);
        req.expected_last_inbound_id = Some(inbound_id);
    });
    let error = sending::create_draft(&db, &ctx, &req).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::ValidationFailed);
    assert!(
        error.violations.iter().any(|violation| {
            violation.field == "to_email_address_id" && violation.code == "REPLY_RECIPIENT_MISMATCH"
        }),
        "unexpected violations: {:?}; inbound From was {from_address}",
        error.violations
    );
}

#[tokio::test]
async fn reply_draft_patch_rejects_retargeting_away_from_inbound_from() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    let (_channel, unrelated_recipient) =
        send_fixture(&db, &world, "correspondence", "reply-patch").await;
    let (reply_recipient, thread_id, inbound_id) = {
        let mut tx = db.begin(ws).await.unwrap();
        let reply_recipient = email_address(&mut tx, "actual-sender-patch").await;
        valid_basis(
            &mut tx,
            reply_recipient,
            world.admin.principal_id,
            "correspondence",
        )
        .await;
        let (from_address, _) =
            infrastructure::repo::sending::email_address_row(&mut tx, reply_recipient)
                .await
                .unwrap()
                .unwrap();
        let (thread_id, inbound_id) = thread_with_from_participant(
            &mut tx,
            world.mail_account_id,
            &from_address,
            Some(reply_recipient),
        )
        .await;
        tx.commit().await.unwrap();
        (reply_recipient, thread_id, inbound_id)
    };
    let ctx = ctx_for(&world, &world.admin, "reply-patch-mismatch");
    let req = draft_req(&world, reply_recipient, "correspondence", |req| {
        req.thread_id = Some(thread_id);
        req.reply_to_message_id = Some(inbound_id);
        req.expected_last_inbound_id = Some(inbound_id);
    });
    let draft = sending::create_draft(&db, &ctx, &req).await.unwrap();
    let patch = PatchMessageDraftRequest {
        subject: None,
        body_text: Some("retargeted reply".into()),
        body_html: None,
        variables: None,
        to_email_address_id: Some(unrelated_recipient),
        expected_last_inbound_id: None,
        visibility: None,
    };
    let error = sending::patch_draft(&db, &ctx, draft.draft_id, draft.version, &patch)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ValidationFailed);
    assert!(error.violations.iter().any(|violation| {
        violation.field == "to_email_address_id" && violation.code == "REPLY_RECIPIENT_MISMATCH"
    }));
}

#[tokio::test]
async fn case_normalized_inbound_from_can_be_frozen_as_reply_recipient() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    let (channel, _unrelated) =
        send_fixture(&db, &world, "correspondence", "reply-normalized").await;
    let (recipient, thread_id, inbound_id, address_variant) = {
        let mut tx = db.begin(ws).await.unwrap();
        let recipient = email_address(&mut tx, "mixed-case-sender").await;
        valid_basis(
            &mut tx,
            recipient,
            world.admin.principal_id,
            "correspondence",
        )
        .await;
        let (address, _) = infrastructure::repo::sending::email_address_row(&mut tx, recipient)
            .await
            .unwrap()
            .unwrap();
        let address_variant = address.to_ascii_uppercase();
        let (thread_id, inbound_id) =
            thread_with_from_participant(&mut tx, world.mail_account_id, &address_variant, None)
                .await;
        tx.commit().await.unwrap();
        (recipient, thread_id, inbound_id, address_variant)
    };
    let ctx = ctx_for(&world, &world.admin, "reply-normalized");
    let req = draft_req(&world, recipient, "correspondence", |req| {
        req.thread_id = Some(thread_id);
        req.reply_to_message_id = Some(inbound_id);
        req.expected_last_inbound_id = Some(inbound_id);
    });
    let draft = sending::create_draft(&db, &ctx, &req).await.unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    assert!(!address_variant.is_empty());
    assert_eq!(frozen.status, "draft");
}

#[tokio::test]
async fn cancel_queued_releases_job_and_claim() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, _to) = send_fixture(&db, &world, "correspondence", "cancel").await;
    let ctx = ctx_for(&world, &world.admin, "cancel");

    let mut tx = db.begin(ws).await.unwrap();
    let (thread, last, reply_recipient) =
        thread_with_inbound(&mut tx, world.mail_account_id, 1).await;
    tx.commit().await.unwrap();
    let req = draft_req(&world, reply_recipient, "correspondence", |r| {
        r.thread_id = Some(thread);
        r.reply_to_message_id = Some(last);
        r.expected_last_inbound_id = Some(last);
    });
    let (outbound, hash) = frozen_outbound(&db, &world, &world.admin, channel, &req).await;
    let res = submit(&db, &world, &world.admin, outbound, &hash, "k-cancel").await;

    // Read-side state while queued (T34 pre-dispatch shape).
    let api_db = api_runtime_db().await;
    let dto = sending::get_outbound(&api_db, &ctx, outbound)
        .await
        .unwrap();
    assert_eq!(dto.status, "queued");
    assert_eq!(dto.delivery_status, "unknown");
    assert_eq!(dto.provider_accepted, None);
    assert!(!dto.automatic_resubmit_allowed);
    assert_eq!(dto.next_action, "wait_for_dispatch");

    // Versioned cancel releases the reply claim + the dispatch job.
    let canceled = sending::cancel(
        &db,
        &ctx,
        outbound,
        dto.version,
        &CancelOutboundRequest {
            reason: Some("changed my mind".into()),
        },
    )
    .await
    .unwrap();
    assert_eq!(canceled.status, "canceled");

    let mut tx = db.begin(ws).await.unwrap();
    let job_state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE workspace_id = $1 AND id = $2")
            .bind(ws)
            .bind(res.job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(job_state, "canceled");
    let claim: Option<String> = sqlx::query_scalar(
        "SELECT state FROM reply_claims
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_optional(tx.conn())
    .await
    .unwrap();
    assert_eq!(claim.as_deref(), Some("released"));
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn thread_filtered_draft_resumes_to_read_authorized_frozen_snapshot() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (channel, _to) = send_fixture(&db, &world, "correspondence", "reply-resume").await;
    let mut tx = db.begin(ws).await.unwrap();
    let (thread, last_inbound, reply_recipient) =
        thread_with_inbound(&mut tx, world.mail_account_id, 1).await;
    tx.commit().await.unwrap();
    let ctx = ctx_for(&world, &world.admin, "reply-resume");
    let req = draft_req(&world, reply_recipient, "correspondence", |r| {
        r.thread_id = Some(thread);
        r.reply_to_message_id = Some(last_inbound);
        r.expected_last_inbound_id = Some(last_inbound);
    });
    let draft = sending::create_draft(&db, &ctx, &req).await.unwrap();

    let filtered = sending::list_drafts_filtered(&db, &ctx, 50, Some(thread), None)
        .await
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].draft_id, draft.draft_id);
    assert_eq!(filtered[0].thread_id, Some(thread));
    assert_eq!(filtered[0].outbound_message_id, None);

    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
            tracking_requested: false,
        },
    )
    .await
    .unwrap();
    let resumed = sending::get_draft(&db, &ctx, draft.draft_id).await.unwrap();
    assert_eq!(
        resumed.outbound_message_id,
        Some(frozen.outbound_message_id)
    );
    let filtered = sending::list_drafts_filtered(&db, &ctx, 50, Some(thread), None)
        .await
        .unwrap();
    assert_eq!(
        filtered[0].outbound_message_id,
        Some(frozen.outbound_message_id)
    );

    let api_db = api_runtime_db().await;
    let outbound = sending::get_outbound(&api_db, &ctx, frozen.outbound_message_id)
        .await
        .unwrap();
    assert_eq!(outbound.payload_hash, frozen.payload_hash);
    assert!(outbound.from_address.starts_with("sender-"));
    assert!(outbound.from_address.ends_with("@test.dev"));
    assert_eq!(outbound.subject, "Re: hi");
    assert_eq!(outbound.body_text, "hello back");
    assert_eq!(outbound.body_html, None);

    let no_grant = ctx_for(&world, &world.member_b, "reply-resume-no-read");
    let err = sending::get_outbound(&api_db, &no_grant, frozen.outbound_message_id)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::MailboxAccessDenied);
}

#[tokio::test]
async fn cancel_without_mail_read_returns_no_subject_or_recipient() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;

    let (channel_a, to_a) = send_fixture(&db, &world, "correspondence", "cancel-admin").await;
    let (admin_outbound, admin_hash) = frozen_outbound(
        &db,
        &world,
        &world.admin,
        channel_a,
        &draft_req(&world, to_a, "correspondence", |_| {}),
    )
    .await;
    submit(
        &db,
        &world,
        &world.admin,
        admin_outbound,
        &admin_hash,
        "cancel-admin",
    )
    .await;

    let (channel_m, to_m) = send_fixture(&db, &world, "correspondence", "cancel-manager").await;
    let (manager_outbound, manager_hash) = frozen_outbound(
        &db,
        &world,
        &world.admin,
        channel_m,
        &draft_req(&world, to_m, "correspondence", |_| {}),
    )
    .await;
    submit(
        &db,
        &world,
        &world.admin,
        manager_outbound,
        &manager_hash,
        "cancel-manager",
    )
    .await;

    let admin_ctx = ctx_for(&world, &world.admin, "cancel-admin-read");
    let manager_ctx = ctx_for(&world, &world.manager, "cancel-manager-read");
    let api_db = api_runtime_db().await;
    let admin_version = sending::get_outbound(&api_db, &admin_ctx, admin_outbound)
        .await
        .unwrap()
        .version;
    let manager_version = sending::get_outbound(&api_db, &admin_ctx, manager_outbound)
        .await
        .unwrap()
        .version;

    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants
            SET can_read = false, can_compose = false, can_send = false,
                can_approve = false, can_assign = false, version = version + 1
          WHERE workspace_id = $1 AND mail_account_id = $2
            AND principal_id = ANY($3)",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(vec![world.admin.principal_id, world.manager.principal_id])
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Control authority remains available to Admin/Manager, while the
    // response cannot reveal message bodies, subject, or recipient address.
    let admin_result = sending::cancel(
        &db,
        &admin_ctx,
        admin_outbound,
        admin_version,
        &CancelOutboundRequest { reason: None },
    )
    .await
    .unwrap();
    let manager_result = sending::cancel(
        &db,
        &manager_ctx,
        manager_outbound,
        manager_version,
        &CancelOutboundRequest { reason: None },
    )
    .await
    .unwrap();
    for result in [admin_result, manager_result] {
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["status"], "canceled");
        assert!(value.get("subject").is_none());
        assert!(value.get("to_address").is_none());
        assert!(value.get("body_text").is_none());
    }
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
