//! sending dispatch/policy tests (§8.4 chain at send time, §8.5 unknown,
//! §8.6 in-flight suppression, A.6.4 submitting boundary, ADR-006).
//!
//! T-rows: T10 (requester/approver perms revoked before dispatch), T11
//! (committed suppression → no new send permit), T12 (suppression during
//! in-flight send), T13 (crash after provider accept → unknown → lookup
//! reconcile), T34 (accepted ≠ delivered/read).
//!
//! Jobs are claimed through `jobs::claim` (real lease tokens) and run via
//! `application::sending::dispatch::{run_dispatch,run_reconcile}` — the
//! same path the registered `JobHandler`s call (`registered_handlers`
//! asserts that wiring + `external_effect`).

use std::sync::{Arc, Mutex};

use application::sending;
use application::sending::dispatch::{
    DISPATCH_KIND, Providers, RECONCILE_KIND, run_dispatch, run_reconcile,
};
use contracts::ErrorCode;
use contracts::sending::{
    CreateMessageDraftRequest, CreateOutboundApprovalRequest, FreezeMessageDraftRequest,
    SubmitOutboundRequest,
};
use infrastructure::db::{Db, ROLE_API, ROLE_WORKER, Tx};
use infrastructure::jobs::{self, JobCtx};
use infrastructure::providers::{
    DeliveryCapabilities, DeliveryProvider, MailCapabilities, MailProvider, ProviderError,
    ProviderMessage, SendOutcome, SendRequest, SentLookup,
};
use infrastructure::repo::compliance_core;
use infrastructure::testing::{
    TestPrincipal, TestWorld, ctx_for, ctx_for_credential, seed_workspace,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// plumbing
// ---------------------------------------------------------------------------

async fn test_db() -> Option<(Db, TestWorld)> {
    let url = match std::env::var("DATABASE_URL") {
        Ok(u) if !u.is_empty() => u,
        _ => {
            eprintln!("DATABASE_URL not set — skipping dispatch test");
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

/// Recording + scriptable MailProvider for dispatch tests.
#[derive(Default)]
struct StubMail {
    sends: Mutex<Vec<SendRequest>>,
    send_calls: Mutex<usize>,
    configured_for_send: Option<bool>,
    send_outcome: Mutex<Option<SendOutcome>>,
    send_error: Mutex<Option<ProviderError>>,
    lookup: Mutex<Option<Result<Option<SentLookup>, ProviderError>>>,
    /// (db, ws, email_address_id, actor) — commit a suppression mid-flight.
    suppress: Mutex<Option<(Db, Uuid, Uuid, Uuid)>>,
}

impl StubMail {
    fn sent(&self) -> Vec<SendRequest> {
        self.sends.lock().unwrap().clone()
    }

    fn send_calls(&self) -> usize {
        *self.send_calls.lock().unwrap()
    }
}

#[async_trait::async_trait]
impl MailProvider for StubMail {
    fn capabilities(&self) -> MailCapabilities {
        MailCapabilities {
            supports_send: self.configured_for_send.unwrap_or(true),
            supports_delta_sync: false,
        }
    }
    async fn list_messages(
        &self,
        _c: Option<String>,
        _l: u32,
    ) -> Result<(Vec<ProviderMessage>, Option<String>), ProviderError> {
        Ok((vec![], None))
    }
    async fn get_message(&self, _id: &str) -> Result<ProviderMessage, ProviderError> {
        Err(ProviderError::unavailable("no messages"))
    }
    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError> {
        *self.send_calls.lock().unwrap() += 1;
        // Optional §8.6 script: the suppression commits in its own tx
        // while this send is in flight — the exact T12 race.
        let script = self.suppress.lock().unwrap().clone();
        if let Some((db, ws, addr, actor)) = script {
            let mut tx = db.begin(ws).await.unwrap();
            compliance_core::insert_suppression(
                &mut tx,
                addr,
                "all",
                "manual",
                "manual",
                Some(actor),
                None,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
        }
        if let Some(err) = self.send_error.lock().unwrap().take() {
            return Err(err);
        }
        self.sends.lock().unwrap().push(req);
        Ok(self
            .send_outcome
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| SendOutcome::Accepted {
                provider_message_id: format!("stub-{}", Uuid::new_v4()),
            }))
    }
    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        match self.lookup.lock().unwrap().take() {
            Some(r) => r,
            None => Ok(None),
        }
    }
}

/// Recording delivery provider (marketing/transactional path).
#[derive(Default)]
struct StubDelivery {
    sends: Mutex<Vec<SendRequest>>,
    send_outcome: Mutex<Option<SendOutcome>>,
    lookup: Mutex<Option<Result<Option<SentLookup>, ProviderError>>>,
}

#[async_trait::async_trait]
impl DeliveryProvider for StubDelivery {
    fn capabilities(&self) -> DeliveryCapabilities {
        DeliveryCapabilities {
            supports_send: true,
            ..Default::default()
        }
    }
    async fn send(&self, req: SendRequest) -> Result<SendOutcome, ProviderError> {
        self.sends.lock().unwrap().push(req);
        Ok(self
            .send_outcome
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| SendOutcome::Accepted {
                provider_message_id: format!("del-{}", Uuid::new_v4()),
            }))
    }
    async fn lookup_sent(
        &self,
        _correlation_id: &str,
    ) -> Result<Option<SentLookup>, ProviderError> {
        match self.lookup.lock().unwrap().take() {
            Some(r) => r,
            None => Ok(None),
        }
    }
}

fn providers(mail: Arc<StubMail>, delivery: Arc<StubDelivery>) -> Providers {
    Providers { mail, delivery }
}

/// Claim runnable jobs in this workspace and run the sending handlers
/// through `run_dispatch`/`run_reconcile` with the REAL job lease ctx —
/// the same path `DispatchOutbound`/`ReconcileUnknown` take. Foreign job
/// kinds (e.g. `compliance.pause_enrollments` from fixture suppressions)
/// are completed silently.
async fn run_claimed_jobs(db: &Db, ws: Uuid, providers: &Providers) -> usize {
    let mut tx = db.begin(ws).await.unwrap();
    let jobs = jobs::claim(&mut tx, "test-worker", 50, &[DISPATCH_KIND, RECONCILE_KIND])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut ran = 0;
    for job in jobs {
        let ctx = JobCtx {
            job_id: job.id,
            workspace_id: ws,
            lease_token: job.lease_token.unwrap_or_default(),
            attempt_count: job.attempt_count,
            lease_owner: "test-worker".into(),
        };
        let outbound = job
            .payload
            .get("outbound_message_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());
        let result = match (job.kind.as_str(), outbound) {
            (DISPATCH_KIND, Some(id)) => run_dispatch(db, providers, &ctx, id).await,
            (RECONCILE_KIND, Some(id)) => run_reconcile(db, providers, &ctx, id).await,
            _ => Ok(()), // foreign kinds: no handler — just complete
        };
        result.unwrap();
        let mut tx = db.begin(ws).await.unwrap();
        jobs::complete(&mut tx, job.id, ctx.lease_token)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        ran += 1;
    }
    ran
}

/// Deferred jobs (reconcile rearm, dispatch rearm) become claimable now.
async fn arm_jobs(db: &Db, ws: Uuid) {
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE jobs SET available_at = now()
          WHERE workspace_id = $1 AND state IN ('queued','retry_wait')",
    )
    .bind(ws)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

// ---------------------------------------------------------------------------
// SQL fixtures
// ---------------------------------------------------------------------------

async fn enable_sending(_db: &Db, ws: Uuid) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let control_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let mut tx = control_db.begin(ws).await.unwrap();
    sqlx::query("UPDATE workspaces SET emergency_stop = false WHERE id = $1")
        .bind(ws)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

async fn set_send_workers_locked(ws: Uuid, locked: bool) {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let control_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let mut tx = control_db.begin(ws).await.unwrap();
    infrastructure::repo::operations::set_send_workers_locked(&mut tx, locked)
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

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

/// channel + recipient + basis; create+freeze → (outbound, hash, to, channel).
async fn frozen_outbound_for(
    db: &Db,
    world: &TestWorld,
    author: &TestPrincipal,
    purpose: &str,
    local: &str,
) -> (Uuid, String, Uuid, Uuid) {
    let ws = world.workspace_id;
    let ctx = ctx_for(world, author, "fixture");
    let mut tx = db.begin(ws).await.unwrap();
    let (_, channel) =
        sender_and_channel(&mut tx, world.mail_account_id, world.connection_id, purpose).await;
    let to = email_address(&mut tx, local).await;
    valid_basis(&mut tx, to, world.admin.principal_id, purpose).await;
    tx.commit().await.unwrap();

    let draft = sending::create_draft(
        db,
        &ctx,
        &CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: to,
            purpose: purpose.to_string(),
            subject: "hi".into(),
            body_text: "body".into(),
            body_html: None,
            template_version_id: None,
            variables: None,
            thread_id: None,
            reply_to_message_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    let frozen = sending::freeze_draft(
        db,
        &ctx,
        draft.draft_id,
        1,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
        },
    )
    .await
    .unwrap();
    (frozen.outbound_message_id, frozen.payload_hash, to, channel)
}

async fn submit_now(
    db: &Db,
    world: &TestWorld,
    author: &TestPrincipal,
    outbound: Uuid,
    hash: &str,
) {
    let ctx = ctx_for(world, author, "submit");
    sending::submit(
        db,
        &ctx,
        outbound,
        &SubmitOutboundRequest {
            expected_payload_hash: hash.to_string(),
            not_before: None,
        },
        &format!("k-{}", Uuid::new_v4()),
    )
    .await
    .unwrap();
}

/// create+freeze+submit → (outbound_id, payload_hash, to_address_id).
async fn submitted_outbound(
    db: &Db,
    world: &TestWorld,
    author: &TestPrincipal,
    purpose: &str,
    local: &str,
) -> (Uuid, String, Uuid) {
    let (outbound, hash, to, _chan) = frozen_outbound_for(db, world, author, purpose, local).await;
    submit_now(db, world, author, outbound, &hash).await;
    (outbound, hash, to)
}

async fn outbound_status(db: &Db, ws: Uuid, id: Uuid) -> (String, Option<String>) {
    let mut tx = db.begin(ws).await.unwrap();
    let row: (String, Option<String>) = sqlx::query_as(
        "SELECT status, failure_code FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    row
}

async fn audit_count(db: &Db, ws: Uuid, action: &str, target: Uuid) -> i64 {
    let mut tx = db.begin(ws).await.unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs
          WHERE workspace_id = $1 AND action = $2 AND target_id = $3",
    )
    .bind(ws)
    .bind(action)
    .bind(target)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    n
}

async fn outbox_count(db: &Db, ws: Uuid, event_type: &str, aggregate: Uuid) -> i64 {
    let mut tx = db.begin(ws).await.unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events
          WHERE workspace_id = $1 AND event_type = $2 AND aggregate_id = $3",
    )
    .bind(ws)
    .bind(event_type)
    .bind(aggregate)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    n
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// handler registration
// ---------------------------------------------------------------------------

#[test]
fn registered_handlers() {
    let mut reg = worker::JobRegistry::new();
    worker::handlers::sending::register(&mut reg);
    let dispatch = reg.get(DISPATCH_KIND).expect("dispatch handler missing");
    assert_eq!(dispatch.kind(), DISPATCH_KIND);
    // Spec: sending.dispatch_outbound is the external-effect job.
    assert!(
        dispatch.external_effect(),
        "dispatch must be external_effect"
    );
    let reconcile = reg.get(RECONCILE_KIND).expect("reconcile handler missing");
    assert_eq!(reconcile.kind(), RECONCILE_KIND);
    assert!(!reconcile.external_effect(), "reconcile is lookup-only");
}

// ---------------------------------------------------------------------------
// outcome paths
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_accepted_path() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _hash, _to) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "acc").await;

    let mail = Arc::new(StubMail::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;

    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "accepted");
    assert_eq!(code, None);
    assert_eq!(mail.sent().len(), 1);
    assert_eq!(mail.sent()[0].correlation_id, outbound.to_string());
    assert!(
        mail.sent()[0]
            .headers
            .iter()
            .any(|(k, _)| k == "X-Outreach-Correlation")
    );
    assert_eq!(audit_count(&db, ws, "send.accepted", outbound).await, 1);
    assert_eq!(
        outbox_count(&db, ws, "outbound.accepted", outbound).await,
        1
    );

    // Attempt + provider evidence + the 'accepted' delivery event.
    let mut tx = db.begin(ws).await.unwrap();
    let att: (String, Option<String>) = sqlx::query_as(
        "SELECT status, provider_message_id FROM send_attempts
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(att.0, "accepted");
    assert!(att.1.is_some());
    assert!(
        mail.sent()[0]
            .headers
            .iter()
            .all(|(name, _)| name != "List-Unsubscribe" && name != "List-Unsubscribe-Post"),
        "non-marketing sends must not advertise unsubscribe"
    );
    let ev: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM delivery_events
          WHERE workspace_id = $1 AND outbound_message_id = $2 AND event_type = 'accepted'",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(ev, 1);
    tx.rollback().await.unwrap();

    // A rerun is an idempotent no-op — no second provider call.
    run_dispatch(
        &db,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
        &JobCtx {
            job_id: Uuid::new_v4(),
            workspace_id: ws,
            lease_token: Uuid::new_v4(),
            attempt_count: 1,
            lease_owner: "test-worker".into(),
        },
        outbound,
    )
    .await
    .unwrap();
    assert_eq!(mail.sent().len(), 1);
}

#[tokio::test]
async fn marketing_list_unsubscribe_uses_bound_one_click_api_route() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;

    let ctx = ctx_for(&world, &world.admin, "one-click-header");
    let (channel, to) = {
        let mut tx = db.begin(ws).await.unwrap();
        let (_, channel) = sender_and_channel(
            &mut tx,
            world.mail_account_id,
            world.connection_id,
            "marketing",
        )
        .await;
        let to = email_address(&mut tx, "one-click-header").await;
        valid_basis(&mut tx, to, world.admin.principal_id, "marketing").await;
        tx.commit().await.unwrap();
        (channel, to)
    };
    let fake_token = "z".repeat(43);
    let draft = sending::create_draft(
        &db,
        &ctx,
        &CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: to,
            purpose: "marketing".into(),
            subject: "Newsletter".into(),
            body_text: format!("Hello\nhttps://attacker.example/public/unsubscribe/{fake_token}"),
            body_html: None,
            template_version_id: None,
            variables: None,
            thread_id: None,
            reply_to_message_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
        },
    )
    .await
    .unwrap();

    let (token, origin) = {
        let mut tx = db.begin(ws).await.unwrap();
        let outbound =
            infrastructure::repo::sending::get_outbound(&mut tx, frozen.outbound_message_id)
                .await
                .unwrap()
                .unwrap();
        let body_link = outbound
            .body_text
            .split("Unsubscribe: ")
            .nth(1)
            .expect("marketing freeze adds the visible confirmation link")
            .trim();
        assert!(body_link.starts_with("http://localhost:3000/public/unsubscribe/"));
        let token = body_link.rsplit('/').next().unwrap().to_string();
        let origin = infrastructure::repo::sending::public_base_url(&mut tx)
            .await
            .unwrap();
        (token, origin)
    };
    let request = sending::create_approval_request(
        &db,
        &ctx,
        frozen.outbound_message_id,
        &CreateOutboundApprovalRequest {
            reason: "test marketing send".into(),
            expected_payload_hash: frozen.payload_hash.clone(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision,
             snapshot_hash, version_at_decision)
         VALUES ($1,$2,$3,'approved',$4,1)",
    )
    .bind(ws)
    .bind(request.approval_request_id)
    .bind(world.manager.principal_id)
    .bind(hex_bytes(&frozen.payload_hash))
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE approval_requests SET status = 'approved'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(request.approval_request_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    submit_now(
        &db,
        &world,
        &world.admin,
        frozen.outbound_message_id,
        &frozen.payload_hash,
    )
    .await;

    let delivery = Arc::new(StubDelivery::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(Arc::new(StubMail::default()), delivery.clone()),
    )
    .await;
    let sent = delivery.sends.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].headers.iter().any(|(name, value)| {
        name == "List-Unsubscribe"
            && value
                == &format!(
                    "<{}/public/v1/unsubscribe/{token}/one-click>",
                    origin.trim_end_matches('/')
                )
    }));
    assert!(sent[0].headers.iter().any(|(name, value)| {
        name == "List-Unsubscribe-Post" && value == "List-Unsubscribe=One-Click"
    }));
    assert!(
        sent[0]
            .headers
            .iter()
            .all(|(_, value)| !value.contains("attacker.example")),
        "message-controlled origins must never reach headers"
    );
}

#[tokio::test]
async fn legacy_marketing_without_bound_unsubscribe_capability_is_canceled_pre_attempt() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let ctx = ctx_for(&world, &world.admin, "legacy-unsubscribe-missing");
    let (outbound, hash, _to, _channel) =
        frozen_outbound_for(&db, &world, &world.admin, "marketing", "legacy-no-unsub").await;
    let approval = sending::create_approval_request(
        &db,
        &ctx,
        outbound,
        &CreateOutboundApprovalRequest {
            reason: "test legacy outbound".into(),
            expected_payload_hash: hash.clone(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision,
             snapshot_hash, version_at_decision)
         VALUES ($1,$2,$3,'approved',$4,1)",
    )
    .bind(ws)
    .bind(approval.approval_request_id)
    .bind(world.manager.principal_id)
    .bind(hex_bytes(&hash))
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE approval_requests SET status = 'approved'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(approval.approval_request_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // Simulate a legacy/restored marketing snapshot whose body still has an
    // old-looking link but whose capability record is missing.
    let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL for DB tests");
    let control_db = Db::connect(&database_url, None).await.unwrap();
    let mut tx = control_db.begin(ws).await.unwrap();
    sqlx::query(
        "DELETE FROM unsubscribe_tokens
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    submit_now(&db, &world, &world.admin, outbound, &hash).await;
    let delivery = Arc::new(StubDelivery::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(Arc::new(StubMail::default()), delivery.clone()),
    )
    .await;

    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "canceled");
    assert_eq!(code.as_deref(), Some("UNSUBSCRIBE_CAPABILITY_MISSING"));
    assert!(delivery.sends.lock().unwrap().is_empty());
    let mut tx = db.begin(ws).await.unwrap();
    let attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM send_attempts WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        attempts, 0,
        "missing unsubscribe capability fails pre-attempt"
    );
}

#[tokio::test]
async fn dispatch_rejected_permanent_fails() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _h, _t) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "rej").await;

    let mail = Arc::new(StubMail::default());
    mail.send_outcome
        .lock()
        .unwrap()
        .replace(SendOutcome::Rejected {
            permanent: true,
            code: "MAILBOX_NOT_FOUND".into(),
        });
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;

    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "failed");
    assert_eq!(code.as_deref(), Some("MAILBOX_NOT_FOUND"));
    assert_eq!(outbox_count(&db, ws, "outbound.failed", outbound).await, 1);
    let mut tx = db.begin(ws).await.unwrap();
    let att: String = sqlx::query_scalar(
        "SELECT status FROM send_attempts
          WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(att, "definitive_failed");
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn unknown_never_resubmits_reconcile_lookup_only() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _h, _t) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "unk").await;

    let mail = Arc::new(StubMail::default());
    mail.send_outcome
        .lock()
        .unwrap()
        .replace(SendOutcome::Unknown {
            reason: "timeout after write".into(),
        });
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;

    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "unknown");
    assert_eq!(code.as_deref(), Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"));
    assert_eq!(outbox_count(&db, ws, "outbound.unknown", outbound).await, 1);
    assert_eq!(mail.sent().len(), 1, "send attempted exactly once");

    // Reconcile enqueued (deferred) — arm it, run it: provider has NO
    // evidence → stays unknown, polls again, still never resubmits.
    arm_jobs(&db, ws).await;
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "unknown");
    assert_eq!(mail.sent().len(), 1, "lookup never resubmits");
    assert_eq!(
        audit_count(&db, ws, "send.reconcile_no_evidence", outbound).await,
        1
    );

    // Evidence appears on the next poll → accepted via lookup only.
    mail.lookup.lock().unwrap().replace(Ok(Some(SentLookup {
        provider_message_id: "prov-rec-1".into(),
        sent_at: chrono::Utc::now(),
    })));
    arm_jobs(&db, ws).await;
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "accepted");
    let mut tx = db.begin(ws).await.unwrap();
    let pmid: Option<String> = sqlx::query_scalar(
        "SELECT provider_message_id FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(pmid.as_deref(), Some("prov-rec-1"));
    tx.rollback().await.unwrap();
    assert_eq!(mail.sent().len(), 1, "still exactly one provider send");
}

#[tokio::test]
async fn provider_transport_error_becomes_unknown_and_never_resubmits() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _hash, _to) = submitted_outbound(
        &db,
        &world,
        &world.admin,
        "correspondence",
        "transport-unknown",
    )
    .await;

    let mail = Arc::new(StubMail::default());
    *mail.send_error.lock().unwrap() = Some(ProviderError::unavailable("simulated transport loss"));
    let providers = providers(mail.clone(), Arc::new(StubDelivery::default()));
    run_claimed_jobs(&db, ws, &providers).await;

    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "unknown");
    assert_eq!(code.as_deref(), Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"));
    assert_eq!(mail.send_calls(), 1);

    // Reconcile is a provider lookup only; no-evidence leaves the state
    // unknown and cannot trigger another send.
    arm_jobs(&db, ws).await;
    run_claimed_jobs(&db, ws, &providers).await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "unknown");
    assert_eq!(mail.send_calls(), 1);
}

#[tokio::test]
async fn unconfigured_provider_defers_before_submitting() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _hash, _to) = submitted_outbound(
        &db,
        &world,
        &world.admin,
        "correspondence",
        "provider-config",
    )
    .await;

    let mail = Arc::new(StubMail {
        configured_for_send: Some(false),
        ..Default::default()
    });
    let providers = providers(mail.clone(), Arc::new(StubDelivery::default()));
    run_claimed_jobs(&db, ws, &providers).await;

    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "queued");
    assert_eq!(mail.send_calls(), 0);
    let mut tx = db.begin(ws).await.unwrap();
    let attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM send_attempts WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        attempts, 0,
        "missing provider config must not cross submitting"
    );
}

#[tokio::test]
async fn channel_disabled_cancels_at_dispatch() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _h, _t) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "dis").await;

    // Channel disabled between submit and dispatch.
    let mut tx = db.begin(ws).await.unwrap();
    let chan: Uuid = sqlx::query_scalar(
        "SELECT delivery_channel_id FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "UPDATE delivery_channels SET enabled = false
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(chan)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mail = Arc::new(StubMail::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;
    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "canceled");
    assert_eq!(code.as_deref(), Some("CHANNEL_DISABLED"));
    assert!(mail.sent().is_empty());
}

#[tokio::test]
async fn t29_daily_cap_uses_durable_attempt_rows_and_keeps_job_runnable() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, hash, _to, channel) =
        frozen_outbound_for(&db, &world, &world.admin, "correspondence", "t29").await;
    submit_now(&db, &world, &world.admin, outbound, &hash).await;

    // One accepted provider attempt already consumed the channel's durable
    // daily allowance. The queued outbound must not rely on a Redis counter.
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE delivery_channels SET internal_daily_cap = 1
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(channel)
    .execute(tx.conn())
    .await
    .unwrap();
    let history_to = email_address(&mut tx, "daily-history").await;
    let (sender_identity_id, from_address): (Uuid, String) = sqlx::query_as(
        "SELECT si.id, si.address
           FROM delivery_channels dc
           JOIN sender_identities si
             ON si.workspace_id = dc.workspace_id AND si.id = dc.sender_identity_id
          WHERE dc.workspace_id = $1 AND dc.id = $2",
    )
    .bind(ws)
    .bind(channel)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let to_address: String = sqlx::query_scalar(
        "SELECT address_original FROM email_addresses WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(history_to)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let history_id: Uuid = sqlx::query_scalar(
        "INSERT INTO outbound_messages
            (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
             to_email_address_id, requested_by, purpose, from_address_snapshot,
             to_address_snapshot, subject, body_text, payload_hash, status, accepted_at)
         VALUES ($1,$2,$3,$4,$5,$6,'correspondence',$7,$8,'history','accepted',
                 $9,'accepted',now()) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(channel)
    .bind(sender_identity_id)
    .bind(history_to)
    .bind(world.admin.principal_id)
    .bind(from_address)
    .bind(to_address)
    .bind(infrastructure::crypto::sha256_bytes(b"prior-accepted-send").to_vec())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO send_attempts
            (workspace_id, outbound_message_id, attempt_no, lease_token, status,
             provider_message_id, started_at, finished_at)
         VALUES ($1,$2,1,$3,'accepted','history-provider',now(),now())",
    )
    .bind(ws)
    .bind(history_id)
    .bind(Uuid::new_v4())
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mail = Arc::new(StubMail::default());
    let providers = providers(mail.clone(), Arc::new(StubDelivery::default()));
    run_claimed_jobs(&db, ws, &providers).await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "queued");
    assert_eq!(mail.send_calls(), 0);

    let mut tx = db.begin(ws).await.unwrap();
    let pending_dispatch: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id = $1 AND kind = $2
            AND payload->>'outbound_message_id' = $3
            AND state IN ('queued','retry_wait')",
    )
    .bind(ws)
    .bind(DISPATCH_KIND)
    .bind(outbound.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(pending_dispatch, 1, "the durable job must be rearmed");

    // Once the durable quota allows another attempt, the rearmed DB job
    // proceeds without depending on Redis state.
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE delivery_channels SET internal_daily_cap = 2
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(channel)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();
    arm_jobs(&db, ws).await;
    run_claimed_jobs(&db, ws, &providers).await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "accepted");
    assert_eq!(mail.send_calls(), 1);
}

#[tokio::test]
async fn t30_restore_lock_defers_a_prequeued_send_then_resumes_it() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _hash, _to) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "t30").await;

    set_send_workers_locked(ws, true).await;

    let mail = Arc::new(StubMail::default());
    let providers = providers(mail.clone(), Arc::new(StubDelivery::default()));
    run_claimed_jobs(&db, ws, &providers).await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "queued");
    assert_eq!(mail.send_calls(), 0);
    let mut tx = db.begin(ws).await.unwrap();
    let attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM send_attempts WHERE workspace_id = $1 AND outbound_message_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(attempts, 0, "a restore lock must stop before submitting");

    set_send_workers_locked(ws, false).await;
    arm_jobs(&db, ws).await;
    run_claimed_jobs(&db, ws, &providers).await;

    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "accepted");
    assert_eq!(mail.send_calls(), 1);
}

#[tokio::test]
async fn t13_expired_dispatch_lease_isolated_and_reconciled_without_resubmit() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _hash, _to) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "lease-expired").await;

    // Acquire a real running lease, then persist a submitting boundary with
    // that token as if the worker crashed during provider I/O.
    let (job_id, lease_token) = {
        let mut tx = db.begin(ws).await.unwrap();
        let claimed = jobs::claim(&mut tx, "crashed-worker", 1, &[DISPATCH_KIND])
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        let job = &claimed[0];
        let token = job.lease_token.unwrap();
        sqlx::query(
            "INSERT INTO send_attempts
                (workspace_id, outbound_message_id, attempt_no, lease_token, status)
             VALUES ($1,$2,1,$3,'started')",
        )
        .bind(ws)
        .bind(outbound)
        .bind(token)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE outbound_messages SET status = 'submitting'
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(outbound)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() - interval '1 minute'
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(job.id)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (job.id, token)
    };

    assert!(jobs::reclaim_expired(&db).await.unwrap() >= 1);
    let mut tx = db.begin(ws).await.unwrap();
    let job_state: String = sqlx::query_scalar("SELECT state FROM jobs WHERE id = $1")
        .bind(job_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    let (outbound_state, failure_code): (String, Option<String>) = sqlx::query_as(
        "SELECT status, failure_code FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let (attempt_state, attempt_finished): (String, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as(
            "SELECT status, finished_at FROM send_attempts
              WHERE workspace_id = $1 AND outbound_message_id = $2 AND lease_token = $3",
        )
        .bind(ws)
        .bind(outbound)
        .bind(lease_token)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    let reconcile_jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs
          WHERE workspace_id = $1 AND kind = $2
            AND payload->>'outbound_message_id' = $3
            AND state IN ('queued','retry_wait')",
    )
    .bind(ws)
    .bind(RECONCILE_KIND)
    .bind(outbound.to_string())
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let unknown_events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox_events
          WHERE workspace_id = $1 AND event_type = 'outbound.unknown'
            AND aggregate_id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(job_state, "unknown");
    assert_eq!(outbound_state, "unknown");
    assert_eq!(
        failure_code.as_deref(),
        Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN")
    );
    assert_eq!(attempt_state, "unknown");
    assert!(attempt_finished.is_some());
    assert_eq!(reconcile_jobs, 1);
    assert_eq!(unknown_events, 1);

    // Reconciliation is lookup-only. No evidence keeps the outbound unknown
    // and does not invoke provider send.
    let mail = Arc::new(StubMail::default());
    let providers = providers(mail.clone(), Arc::new(StubDelivery::default()));
    arm_jobs(&db, ws).await;
    run_claimed_jobs(&db, ws, &providers).await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "unknown");
    assert_eq!(mail.send_calls(), 0);
}

#[tokio::test]
async fn queued_legacy_campaign_follow_up_is_canceled_before_provider_send() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;

    let (outbound_id, job_id) = {
        let mut tx = db.begin(ws).await.unwrap();
        let (sender_identity_id, channel_id) = sender_and_channel(
            &mut tx,
            world.mail_account_id,
            world.connection_id,
            "marketing",
        )
        .await;
        let address_id = email_address(&mut tx, "legacy-follow-up").await;
        valid_basis(&mut tx, address_id, world.admin.principal_id, "marketing").await;
        let template_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_templates (workspace_id, name, created_by)
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(ws)
        .bind(format!("legacy-step-{}", Uuid::new_v4()))
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let template_version_id: Uuid = sqlx::query_scalar(
            "INSERT INTO template_versions
                (workspace_id, template_id, version_no, subject, body_text,
                 variable_schema, content_hash, created_by)
             VALUES ($1, $2, 1, 'Follow-up', 'Body', '{}', decode(md5(random()::text), 'hex'), $3)
             RETURNING id",
        )
        .bind(ws)
        .bind(template_id)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let campaign_id = infrastructure::repo::campaigns::insert_campaign(
            &mut tx,
            world.mail_account_id,
            "Legacy campaign",
            None,
            world.admin.principal_id,
        )
        .await
        .unwrap();
        let revision_id = infrastructure::repo::campaigns::insert_revision(
            &mut tx,
            campaign_id,
            world.mail_account_id,
            channel_id,
            1,
            "UTC",
            &serde_json::json!({"weekdays":[1,2,3,4,5,6,7],"window_start":"00:00","window_end":"23:59"}),
            true,
            true,
            world.admin.principal_id,
        )
        .await
        .unwrap();
        let _first_step_id = infrastructure::repo::campaigns::insert_step(
            &mut tx,
            revision_id,
            template_version_id,
            1,
            0,
        )
        .await
        .unwrap();
        let follow_up_step_id = infrastructure::repo::campaigns::insert_step(
            &mut tx,
            revision_id,
            template_version_id,
            2,
            24,
        )
        .await
        .unwrap();
        let launch_id = infrastructure::repo::campaigns::insert_launch(
            &mut tx,
            revision_id,
            channel_id,
            world.admin.principal_id,
            chrono::Utc::now() + chrono::Duration::days(1),
        )
        .await
        .unwrap();
        let enrollment_id = infrastructure::repo::campaigns::insert_enrollment(
            &mut tx,
            launch_id,
            revision_id,
            address_id,
            None,
            "pending",
            None,
        )
        .await
        .unwrap();
        infrastructure::repo::campaigns::freeze_revision(&mut tx, revision_id)
            .await
            .unwrap();
        infrastructure::repo::campaigns::freeze_launch_row(&mut tx, launch_id, &[7_u8; 32])
            .await
            .unwrap();
        for status in ["awaiting_approval", "approved", "running"] {
            infrastructure::repo::campaigns::transition_launch(&mut tx, launch_id, status, None)
                .await
                .unwrap();
        }
        infrastructure::repo::campaigns::set_enrollment_state(
            &mut tx,
            enrollment_id,
            "active",
            Some(follow_up_step_id),
            Some(chrono::Utc::now()),
            None,
        )
        .await
        .unwrap();
        let basis_id = infrastructure::repo::campaigns::valid_marketing_basis(&mut tx, address_id)
            .await
            .unwrap()
            .unwrap();
        let outbound_id = infrastructure::repo::campaigns::insert_step_outbound(
            &mut tx,
            world.mail_account_id,
            channel_id,
            sender_identity_id,
            address_id,
            Some(basis_id),
            launch_id,
            revision_id,
            enrollment_id,
            follow_up_step_id,
            world.admin.principal_id,
            "sender@example.test",
            "legacy-follow-up@example.test",
            "Follow-up",
            "Follow-up body",
            None,
            &[9_u8; 32],
        )
        .await
        .unwrap();
        infrastructure::repo::campaigns::queue_outbound_at(
            &mut tx,
            outbound_id,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        let job_id = infrastructure::repo::campaigns::enqueue_dedup(
            &mut tx,
            DISPATCH_KIND,
            serde_json::json!({"outbound_message_id": outbound_id}),
            chrono::Utc::now(),
            &infrastructure::repo::campaigns::dispatch_dedupe_key(outbound_id),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (outbound_id, job_id)
    };

    let mail = Arc::new(StubMail::default());
    run_dispatch(
        &db,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
        &JobCtx {
            job_id,
            workspace_id: ws,
            lease_token: Uuid::new_v4(),
            attempt_count: 1,
            lease_owner: "test-worker".into(),
        },
        outbound_id,
    )
    .await
    .unwrap();

    let (status, code) = outbound_status(&db, ws, outbound_id).await;
    assert_eq!(status, "canceled");
    assert_eq!(code.as_deref(), Some("CAMPAIGN_FOLLOW_UP_DISABLED"));
    assert_eq!(
        mail.send_calls(),
        0,
        "legacy follow-ups must not reach a provider"
    );
    let mut tx = db.begin(ws).await.unwrap();
    let attempts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM send_attempts WHERE workspace_id=$1 AND outbound_message_id=$2",
    )
    .bind(ws)
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let state: String =
        sqlx::query_scalar("SELECT state FROM jobs WHERE workspace_id=$1 AND id=$2")
            .bind(ws)
            .bind(job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(attempts, 0);
    assert_eq!(state, "canceled");
    tx.rollback().await.unwrap();
}

// ---------------------------------------------------------------------------
// T10 — requester / approver permissions revoked before dispatch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t10_requester_grant_revoked_before_dispatch_cancels() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _h, _t) =
        submitted_outbound(&db, &world, &world.member_a, "correspondence", "t10").await;

    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants SET can_send = false, can_approve = false
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(world.member_a.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mail = Arc::new(StubMail::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;

    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "canceled");
    assert_eq!(code.as_deref(), Some("MAILBOX_ACCESS_DENIED"));
    assert!(mail.sent().is_empty(), "provider must never be called");
    assert_eq!(audit_count(&db, ws, "send.canceled", outbound).await, 1);
}

#[tokio::test]
async fn t10_revoked_source_credential_cancels_before_provider_call() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for DB tests");
    let control_db = Db::connect(&url, Some(ROLE_API)).await.unwrap();
    let credential_id = {
        let mut tx = control_db.begin(ws).await.unwrap();
        let scopes = vec![
            "mail.read".to_string(),
            "mail.compose".to_string(),
            "mail.send".to_string(),
        ];
        let (credential_id, _token) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.member_a.principal_id,
            "worker send test",
            &scopes,
            chrono::Duration::days(1),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        credential_id
    };
    let scopes = contracts::ScopeSet::from_strings(["mail.read", "mail.compose", "mail.send"]);
    let ctx = ctx_for_credential(
        &world,
        &world.member_a,
        credential_id,
        scopes,
        "credential-send",
    );

    let (channel, to) = {
        let mut tx = db.begin(ws).await.unwrap();
        let (_, channel) = sender_and_channel(
            &mut tx,
            world.mail_account_id,
            world.connection_id,
            "correspondence",
        )
        .await;
        let to = email_address(&mut tx, "credential-send").await;
        valid_basis(&mut tx, to, world.admin.principal_id, "correspondence").await;
        tx.commit().await.unwrap();
        (channel, to)
    };
    let draft = sending::create_draft(
        &db,
        &ctx,
        &contracts::sending::CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: to,
            purpose: "correspondence".into(),
            subject: "hi".into(),
            body_text: "body".into(),
            body_html: None,
            template_version_id: None,
            variables: None,
            thread_id: None,
            reply_to_message_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        draft.version,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel,
            sender_identity_id: None,
            meeting_type_id: None,
        },
    )
    .await
    .unwrap();
    sending::submit(
        &db,
        &ctx,
        frozen.outbound_message_id,
        &SubmitOutboundRequest {
            expected_payload_hash: frozen.payload_hash,
            not_before: None,
        },
        "credential-send-submit",
    )
    .await
    .unwrap();

    let mut tx = control_db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE service_credentials SET revoked_at = now()
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(credential_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mail = Arc::new(StubMail::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;
    let (status, code) = outbound_status(&db, ws, frozen.outbound_message_id).await;
    assert_eq!(status, "canceled");
    assert_eq!(code.as_deref(), Some("CREDENTIAL_REVOKED"));
    assert_eq!(mail.send_calls(), 0);
}

#[tokio::test]
async fn t10_approver_grant_revoked_before_dispatch_cancels() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    // create+freeze (no submit yet — marketing needs approval first).
    let (outbound, hash, _t, _chan) =
        frozen_outbound_for(&db, &world, &world.member_a, "marketing", "t10b").await;
    let ctx = ctx_for(&world, &world.member_a, "req-approval");
    let req = sending::create_approval_request(
        &db,
        &ctx,
        outbound,
        &CreateOutboundApprovalRequest {
            reason: "test".into(),
            expected_payload_hash: hash.clone(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decided_by, decision,
             snapshot_hash, version_at_decision)
         VALUES ($1,$2,$3,'approved',$4,1)",
    )
    .bind(ws)
    .bind(req.approval_request_id)
    .bind(world.manager.principal_id)
    .bind(hex_bytes(&hash))
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

    submit_now(&db, &world, &world.member_a, outbound, &hash).await;

    // NOW revoke the approver — the queued dispatch re-checks at send
    // time (T10): the approval was valid at submit but must not ship.
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE mail_account_grants SET can_approve = false
          WHERE workspace_id = $1 AND mail_account_id = $2 AND principal_id = $3",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(world.manager.principal_id)
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mail = Arc::new(StubMail::default());
    let delivery = Arc::new(StubDelivery::default());
    run_claimed_jobs(&db, ws, &providers(mail.clone(), delivery.clone())).await;

    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "canceled");
    assert_eq!(code.as_deref(), Some("APPROVER_PERMISSION_REVOKED"));
    assert!(delivery.sends.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// T11 / T12 — suppression
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t11_suppression_denies_at_dispatch_and_submit() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;

    // (a) Dispatch-side: a queued send meets a committed suppression.
    let (outbound, _h, to) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "t11").await;
    let mut tx = db.begin(ws).await.unwrap();
    compliance_core::insert_suppression(
        &mut tx,
        to,
        "all",
        "manual",
        "manual",
        Some(world.admin.principal_id),
        None,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mail = Arc::new(StubMail::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;
    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "canceled");
    assert_eq!(code.as_deref(), Some("RECIPIENT_SUPPRESSED"));
    assert!(mail.sent().is_empty());

    // (b) Submit-side: a fresh send to the suppressed address never
    //     gets a permit.
    let ctx = ctx_for(&world, &world.admin, "t11b");
    let mut tx = db.begin(ws).await.unwrap();
    let (_, channel2) = sender_and_channel(
        &mut tx,
        world.mail_account_id,
        world.connection_id,
        "correspondence",
    )
    .await;
    tx.commit().await.unwrap();
    let draft = sending::create_draft(
        &db,
        &ctx,
        &CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: to,
            purpose: "correspondence".into(),
            subject: "hi".into(),
            body_text: "body".into(),
            body_html: None,
            template_version_id: None,
            variables: None,
            thread_id: None,
            reply_to_message_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        1,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel2,
            sender_identity_id: None,
            meeting_type_id: None,
        },
    )
    .await
    .unwrap();
    let err = sending::submit(
        &db,
        &ctx,
        frozen.outbound_message_id,
        &SubmitOutboundRequest {
            expected_payload_hash: frozen.payload_hash,
            not_before: None,
        },
        "k-t11b",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::RecipientSuppressed);
}

#[tokio::test]
async fn t12_suppression_during_inflight_is_recorded() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _h, to) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "t12").await;

    // The provider call itself commits the suppression mid-flight.
    let mail = Arc::new(StubMail::default());
    mail.suppress
        .lock()
        .unwrap()
        .replace((db.clone(), ws, to, world.admin.principal_id));
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;

    // The in-flight send is beyond recall — recorded as accepted, and
    // the mid-flight suppression is audited (§8.6).
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "accepted");
    assert_eq!(mail.sent().len(), 1);
    assert_eq!(
        audit_count(&db, ws, "send.in_flight_at_suppression", outbound).await,
        1,
        "in-flight suppression must be audited"
    );

    // The NEXT send to the same address is denied — no new submits.
    let ctx = ctx_for(&world, &world.admin, "t12-next");
    let mut tx = db.begin(ws).await.unwrap();
    let (_, channel2) = sender_and_channel(
        &mut tx,
        world.mail_account_id,
        world.connection_id,
        "correspondence",
    )
    .await;
    tx.commit().await.unwrap();
    let draft = sending::create_draft(
        &db,
        &ctx,
        &CreateMessageDraftRequest {
            mail_account_id: world.mail_account_id,
            to_email_address_id: to,
            purpose: "correspondence".into(),
            subject: "again".into(),
            body_text: "body".into(),
            body_html: None,
            template_version_id: None,
            variables: None,
            thread_id: None,
            reply_to_message_id: None,
            expected_last_inbound_id: None,
            visibility: None,
        },
    )
    .await
    .unwrap();
    let frozen = sending::freeze_draft(
        &db,
        &ctx,
        draft.draft_id,
        1,
        &FreezeMessageDraftRequest {
            delivery_channel_id: channel2,
            sender_identity_id: None,
            meeting_type_id: None,
        },
    )
    .await
    .unwrap();
    let err = sending::submit(
        &db,
        &ctx,
        frozen.outbound_message_id,
        &SubmitOutboundRequest {
            expected_payload_hash: frozen.payload_hash,
            not_before: None,
        },
        "k-t12-next",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::RecipientSuppressed);
}

// ---------------------------------------------------------------------------
// T13 — crash after provider accept → unknown → reconcile (lookup only)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t13_crash_after_accept_isolated_to_unknown_then_reconciled() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _h, _t) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "t13").await;

    // Crash: the row crossed `submitting` (attempt 'started') but the
    // owning job died — its lease token maps to no live job.
    let mut tx = db.begin(ws).await.unwrap();
    sqlx::query(
        "UPDATE outbound_messages SET status = 'submitting'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .execute(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO send_attempts
            (workspace_id, outbound_message_id, attempt_no, lease_token, status)
         VALUES ($1,$2,1,$3,'started')",
    )
    .bind(ws)
    .bind(outbound)
    .bind(Uuid::new_v4())
    .execute(tx.conn())
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // The still-queued dispatch job runs → sees the dead owner →
    // isolates to `unknown` (NEVER resubmits — ADR-006) + reconcile job.
    let mail = Arc::new(StubMail::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;
    let (status, code) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "unknown");
    assert_eq!(code.as_deref(), Some("PROVIDER_SUBMISSION_OUTCOME_UNKNOWN"));
    assert!(mail.sent().is_empty(), "isolation must not re-send");
    assert_eq!(outbox_count(&db, ws, "outbound.unknown", outbound).await, 1);
    assert_eq!(
        audit_count(&db, ws, "send.isolated_submitting", outbound).await,
        1
    );

    // The reconcile job (deferred) polls `lookup_sent` — the provider
    // DID accept it before the crash → converges to `accepted`.
    mail.lookup.lock().unwrap().replace(Ok(Some(SentLookup {
        provider_message_id: "rec-1".into(),
        sent_at: chrono::Utc::now(),
    })));
    arm_jobs(&db, ws).await;
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;
    let (status, _) = outbound_status(&db, ws, outbound).await;
    assert_eq!(status, "accepted");
    let mut tx = db.begin(ws).await.unwrap();
    let pmid: Option<String> = sqlx::query_scalar(
        "SELECT provider_message_id FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(ws)
    .bind(outbound)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(pmid.as_deref(), Some("rec-1"));
    tx.rollback().await.unwrap();
    assert!(mail.sent().is_empty(), "reconcile is lookup-only");
}

// ---------------------------------------------------------------------------
// T34 — accepted ≠ delivered/read in the DTO
// ---------------------------------------------------------------------------

#[tokio::test]
async fn t34_accepted_is_not_delivered() {
    let Some((db, world)) = test_db().await else {
        return;
    };
    let ws = world.workspace_id;
    enable_sending(&db, ws).await;
    let (outbound, _h, _t) =
        submitted_outbound(&db, &world, &world.admin, "correspondence", "t34").await;

    let mail = Arc::new(StubMail::default());
    run_claimed_jobs(
        &db,
        ws,
        &providers(mail.clone(), Arc::new(StubDelivery::default())),
    )
    .await;

    let ctx = ctx_for(&world, &world.admin, "t34-get");
    let dto = sending::get_outbound(&db, &ctx, outbound).await.unwrap();
    assert_eq!(dto.status, "accepted");
    assert_eq!(dto.payload_hash.len(), 64);
    assert!(dto.payload_hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(dto.provider_accepted, Some(true));
    // T34: accepted is NOT delivery — the claim stays "accepted" only.
    assert_eq!(dto.delivery_status, "accepted");
    assert_ne!(dto.delivery_status, "delivered");
    assert!(!dto.automatic_resubmit_allowed);
    assert_eq!(dto.last_attempt.as_ref().unwrap().status, "accepted");
    assert_eq!(dto.next_action, "none");

    // Only a provider-confirmed `delivered` event may move the status.
    let mut tx = db.begin(ws).await.unwrap();
    infrastructure::repo::sending::insert_delivery_event(
        &mut tx,
        outbound,
        "prov:msg-1:delivered",
        "delivered",
        chrono::Utc::now(),
        &serde_json::json!({}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let dto = sending::get_outbound(&db, &ctx, outbound).await.unwrap();
    assert_eq!(dto.delivery_status, "delivered");
}
