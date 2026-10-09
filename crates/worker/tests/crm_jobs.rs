//! Task 4 (crm) worker tests: `crm.activity_projector` subscription,
//! idempotent re-delivery (same event → one activity), §5.6 ACL filtering
//! of mail-derived activities, lead.accepted actor mapping, and the
//! `crm.merge_companies` job applying a merge end-to-end.

mod common;

use std::time::Duration;

use infrastructure::db::Db;
use infrastructure::error::map_sqlx;
use infrastructure::jobs::JobCtx;
use infrastructure::outbox;
use infrastructure::repo::crm as repo;
use infrastructure::testing::{TestWorld, ctx_for, seed_workspace};
use serde_json::json;
use uuid::Uuid;
use worker::{Runner, registry};

/// Worker tests seed users, principals and CRM rows through `api_runtime`,
/// then exercise the actual job/event consumer through `worker_runtime`.
async fn test_world() -> Option<(Db, Db, TestWorld)> {
    let worker = common::test_db().await?;
    let api = common::seed_db().await;
    let world = seed_workspace(&api).await.unwrap();
    Some((api, worker, world))
}

fn runner(db: &Db) -> Runner {
    let mut r = Runner::new(db.clone(), registry());
    r.backoff_base = Duration::from_millis(5);
    r
}

fn company(name: &str, domain: Option<&str>) -> repo::CompanyInsert {
    repo::CompanyInsert {
        name: name.into(),
        domain: domain.map(Into::into),
        lifecycle_stage: "prospect".into(),
        custom_fields: json!({}),
        ..Default::default()
    }
}

async fn activity_count(db: &Db, ws: Uuid, event_id: Uuid) -> i64 {
    let mut tx = db.begin(ws).await.unwrap();
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM activities WHERE source_event_id = $1")
        .bind(event_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    n
}

/// The projector is registered for exactly the six CRM event types.
#[tokio::test]
async fn projector_registered_for_all_six_events() {
    let reg = registry();
    for ty in [
        "lead.accepted",
        "approval.decided",
        "outbound.accepted",
        "message.received",
        "suppression.created",
        "booking.confirmed",
    ] {
        let consumers = reg.consumers_for(ty);
        assert!(
            consumers
                .iter()
                .any(|c| c.name() == "crm.activity_projector"),
            "no crm.activity_projector subscriber for {ty}"
        );
    }
}

/// Same event delivered twice → exactly one activity row (processed_events
/// dedupe + source_event_id uniqueness).
#[tokio::test]
async fn projector_idempotent_same_event_one_activity() {
    let Some((api, worker, world)) = test_world().await else {
        return;
    };
    let ws = world.workspace_id;

    // Fixtures: company + thread on the seeded mailbox.
    let mut tx = api.begin(ws).await.unwrap();
    let company_row = repo::insert_company(&mut tx, &company("Proj Corp", None))
        .await
        .unwrap();
    let thread_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id, provider_thread_id)
         VALUES ($1, $2, 'pt-1') RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
    .unwrap();
    let event_id = outbox::emit(
        &mut tx,
        "message.received",
        "email_threads",
        thread_id,
        1,
        json!({
            "thread_id": thread_id,
            "company_id": company_row.id,
            "occurred_at": "2026-01-01T00:00:00Z",
        }),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let r = runner(&worker);
    assert!(r.dispatch_workspace(ws).await.unwrap() >= 1);
    assert_eq!(activity_count(&api, ws, event_id).await, 1);

    // Re-deliver the same event: un-publish and dispatch again. The
    // committed processed_events row must make this a no-op.
    let mut tx = worker.begin(ws).await.unwrap();
    sqlx::query("UPDATE outbox_events SET published_at = NULL WHERE id = $1")
        .bind(event_id)
        .execute(tx.conn())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    r.dispatch_workspace(ws).await.unwrap();
    assert_eq!(
        activity_count(&api, ws, event_id).await,
        1,
        "re-delivery created a second activity"
    );

    // And the insert itself is idempotent on (source_event_id, kind).
    let mut tx = api.begin(ws).await.unwrap();
    let dup = repo::insert_activity(
        &mut tx,
        &repo::ActivityInsert {
            kind: "message.received",
            source_event_id: event_id,
            occurred_at: chrono::Utc::now(),
            company_id: Some(company_row.id),
            thread_id: Some(thread_id),
            mail_account_id: Some(world.mail_account_id),
            summary: json!({"text": "dup"}),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(dup.is_none(), "duplicate insert should be swallowed");
    tx.rollback().await.unwrap();
}

/// §5.6: mail-derived activities are visible only to principals holding a
/// can_read grant on the activity's mailbox.
#[tokio::test]
async fn mail_derived_activities_hidden_without_grant() {
    let Some((api, worker, world)) = test_world().await else {
        return;
    };
    let ws = world.workspace_id;

    let mut tx = api.begin(ws).await.unwrap();
    let company_row = repo::insert_company(&mut tx, &company("ACL Corp", None))
        .await
        .unwrap();
    // The projector resolves company via thread -> contact -> company.
    let contact = repo::insert_contact(
        &mut tx,
        &repo::ContactInsert {
            name: "Acl Person".into(),
            company_id: Some(company_row.id),
            status: "active".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let thread_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads
            (workspace_id, mail_account_id, provider_thread_id, contact_id)
         VALUES ($1, $2, 'acl-1', $3) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(contact.id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
    .unwrap();
    outbox::emit(
        &mut tx,
        "message.received",
        "email_threads",
        thread_id,
        1,
        json!({"thread_id": thread_id, "mail_account_id": world.mail_account_id}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    runner(&worker).dispatch_workspace(ws).await.unwrap();

    // member_a holds can_read on the seeded mailbox → sees the activity.
    let mut tx = api.begin(ws).await.unwrap();
    let rows = repo::list_company_activities(
        &mut tx,
        company_row.id,
        world.member_a.principal_id,
        None,
        50,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "message.received");
    assert_eq!(rows[0].mail_account_id, Some(world.mail_account_id));
    tx.rollback().await.unwrap();

    // member_b has no grant → the same timeline is empty.
    let mut tx = api.begin(ws).await.unwrap();
    let rows = repo::list_company_activities(
        &mut tx,
        company_row.id,
        world.member_b.principal_id,
        None,
        50,
    )
    .await
    .unwrap();
    assert!(
        rows.is_empty(),
        "member without can_read saw mail-derived activity"
    );
    tx.rollback().await.unwrap();

    // Non-mail activities (mail_account_id NULL) are still visible.
    let mut tx = api.begin(ws).await.unwrap();
    repo::insert_activity(
        &mut tx,
        &repo::ActivityInsert {
            kind: "company.created",
            source_event_id: Uuid::new_v4(),
            occurred_at: chrono::Utc::now(),
            actor_principal_id: Some(world.admin.principal_id),
            company_id: Some(company_row.id),
            summary: json!({"text": "created"}),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut tx = api.begin(ws).await.unwrap();
    let rows = repo::list_company_activities(
        &mut tx,
        company_row.id,
        world.member_b.principal_id,
        None,
        50,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "company.created");
    tx.rollback().await.unwrap();
}

/// lead.accepted: actor resolves to the proposal's reviewer.
#[tokio::test]
async fn lead_accepted_actor_is_reviewer() {
    let Some((api, worker, world)) = test_world().await else {
        return;
    };
    let ws = world.workspace_id;

    let mut tx = api.begin(ws).await.unwrap();
    let company_row = repo::insert_company(&mut tx, &company("LeadCo", None))
        .await
        .unwrap();
    let proposal_id: Uuid = sqlx::query_scalar(
        "INSERT INTO lead_proposals
            (workspace_id, proposed_by, company_name, proposed_data,
             status, reviewed_by, reviewed_at, accepted_company_id)
         VALUES ($1, $2, 'LeadCo', '{}'::jsonb, 'accepted', $3, now(), $4)
         RETURNING id",
    )
    .bind(ws)
    .bind(world.agent.principal_id)
    .bind(world.admin.principal_id)
    .bind(company_row.id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
    .unwrap();
    let event_id = outbox::emit(
        &mut tx,
        "lead.accepted",
        "lead_proposals",
        proposal_id,
        1,
        json!({"lead_proposal_id": proposal_id, "company_id": company_row.id}),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    runner(&worker).dispatch_workspace(ws).await.unwrap();

    let mut tx = api.begin(ws).await.unwrap();
    let actor: Option<Uuid> =
        sqlx::query_scalar("SELECT actor_principal_id FROM activities WHERE source_event_id = $1")
            .bind(event_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(actor, Some(world.admin.principal_id));
}

/// `crm.merge_companies` applies the merge: source archived, field choices
/// applied, CRM references repointed — idempotent on re-run.
#[tokio::test]
async fn merge_companies_job_applies_and_is_idempotent() {
    let Some((api, worker, world)) = test_world().await else {
        return;
    };
    let ws = world.workspace_id;
    let ctx = ctx_for(&world, &world.admin, "t4-merge");

    let mut tx = api.begin(ws).await.unwrap();
    let src = repo::insert_company(&mut tx, &company("Src Better Name", Some("src.example")))
        .await
        .unwrap();
    let tgt = repo::insert_company(&mut tx, &company("Tgt Old Name", Some("tgt.example")))
        .await
        .unwrap();
    let contact = repo::insert_contact(
        &mut tx,
        &repo::ContactInsert {
            name: "C".into(),
            company_id: Some(src.id),
            status: "active".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    repo::insert_note(
        &mut tx,
        &repo::NoteInsert {
            company_id: Some(src.id),
            contact_id: None,
            thread_id: None,
            mail_account_id: None,
            created_by: ctx.principal_id,
            body: "on source".into(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let payload = json!({
        "merge_request": {
            "source_id": src.id,
            "target_id": tgt.id,
            "field_choices": {"name": "source", "domain": "target"},
        },
        "requested_by": world.admin.principal_id,
    });
    let handler = registry()
        .get("crm.merge_companies")
        .expect("job not registered");
    // audit_logs.job_id references jobs — the ctx must carry a real row.
    let job_id = {
        let mut tx = api.begin(ws).await.unwrap();
        let id = infrastructure::jobs::enqueue(
            &mut tx,
            "crm.merge_companies",
            1,
            payload.clone(),
            chrono::Utc::now() - chrono::Duration::seconds(1),
            None,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    };
    let job_ctx = JobCtx {
        job_id,
        workspace_id: ws,
        lease_token: Uuid::new_v4(),
        attempt_count: 1,
        lease_owner: "test".into(),
    };
    let mut tx = worker.begin(ws).await.unwrap();
    handler
        .handle(&job_ctx, &mut tx, payload.clone())
        .await
        .unwrap();
    tx.commit().await.unwrap();

    // Target got source's name, kept its own domain; source archived.
    let mut tx = api.begin(ws).await.unwrap();
    let tgt_row = repo::get_company(&mut tx, tgt.id).await.unwrap().unwrap();
    assert_eq!(tgt_row.name, "Src Better Name");
    assert_eq!(tgt_row.domain.as_deref(), Some("tgt.example"));
    let src_row = repo::get_company(&mut tx, src.id).await.unwrap().unwrap();
    assert!(src_row.archived_at.is_some());
    // Contact + note repointed to target.
    let c = repo::get_contact(&mut tx, contact.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(c.company_id, Some(tgt.id));
    let note_company: Option<Uuid> =
        sqlx::query_scalar("SELECT company_id FROM notes WHERE body = 'on source'")
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(note_company, Some(tgt.id));
    tx.commit().await.unwrap();

    // Re-run: source already archived → the merge is a no-op, not an error.
    let mut tx = worker.begin(ws).await.unwrap();
    handler.handle(&job_ctx, &mut tx, payload).await.unwrap();
    tx.commit().await.unwrap();
    let mut tx = api.begin(ws).await.unwrap();
    let tgt_row2 = repo::get_company(&mut tx, tgt.id).await.unwrap().unwrap();
    assert_eq!(tgt_row2.name, "Src Better Name");
    let merge_event = infrastructure::outbox::claim_unpublished(&mut tx, 100, &[])
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.event_type == "company.merged" && event.aggregate_id == tgt.id)
        .expect("merge job must emit a CRM outbox event");
    assert_eq!(merge_event.aggregate_type, "company");
    assert_eq!(merge_event.payload["source_company_id"], json!(src.id));
    assert_eq!(merge_event.payload["target_company_id"], json!(tgt.id));
    tx.rollback().await.unwrap();
}

/// The merge-request write path enqueues `crm.merge_companies` with the
/// integration-contract payload shape and replays on the same key.
#[tokio::test]
async fn request_merge_enqueues_contract_payload() {
    let Some((api, _worker, world)) = test_world().await else {
        return;
    };
    let ws = world.workspace_id;
    let ctx = ctx_for(&world, &world.manager, "t4-req");

    let mut tx = api.begin(ws).await.unwrap();
    let src = repo::insert_company(&mut tx, &company("S", None))
        .await
        .unwrap();
    let tgt = repo::insert_company(&mut tx, &company("T", None))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let mut choices = std::collections::BTreeMap::new();
    choices.insert("name".to_string(), "source".to_string());
    let first = application::crm::companies::request_merge(
        &api,
        &ctx,
        tgt.id,
        &contracts::crm::MergeCompanyRequest {
            source_id: src.id,
            field_choices: choices.clone(),
        },
        "req-key-1".to_string(),
    )
    .await
    .unwrap();
    // Same key + same body → replayed response with the same job id.
    let second = application::crm::companies::request_merge(
        &api,
        &ctx,
        tgt.id,
        &contracts::crm::MergeCompanyRequest {
            source_id: src.id,
            field_choices: choices,
        },
        "req-key-1".to_string(),
    )
    .await
    .unwrap();
    assert_eq!(first.body.job_id, second.body.job_id);

    let mut tx = api.begin(ws).await.unwrap();
    let (kind, payload): (String, serde_json::Value) =
        sqlx::query_as("SELECT kind, payload FROM jobs WHERE id = $1")
            .bind(first.body.job_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(kind, "crm.merge_companies");
    assert_eq!(
        payload["merge_request"]["source_id"],
        json!(src.id.to_string())
    );
    assert_eq!(
        payload["merge_request"]["target_id"],
        json!(tgt.id.to_string())
    );
    assert_eq!(
        payload["requested_by"],
        json!(world.manager.principal_id.to_string())
    );
    let event = infrastructure::outbox::claim_unpublished(&mut tx, 100, &[])
        .await
        .unwrap()
        .into_iter()
        .find(|event| {
            event.event_type == "company.merge_requested" && event.aggregate_id == first.body.job_id
        })
        .expect("merge request must emit an outbox event");
    assert_eq!(event.payload["source_company_id"], json!(src.id));
    assert_eq!(event.payload["target_company_id"], json!(tgt.id));
    tx.rollback().await.unwrap();
}

/// T31 groundwork: three contacts linking the same normalized address keep
/// a single `email_addresses` row (B.3.4, §6.3).
#[tokio::test]
async fn t31_shared_address_single_row() {
    let Some((api, _worker, world)) = test_world().await else {
        return;
    };
    let ws = world.workspace_id;
    let ctx = ctx_for(&world, &world.admin, "t4-t31");

    let mut address_ids = Vec::new();
    for name in ["Alice", "Bob", "Carol"] {
        let contact = application::crm::contacts::create(
            &api,
            &ctx,
            &contracts::crm::CreateContactRequest {
                name: name.into(),
                company_id: None,
                job_title: None,
                department: None,
                status: None,
                owner_principal_id: None,
            },
            Some(format!("c-{name}")),
        )
        .await
        .unwrap();
        let linked = application::crm::contacts::add_email(
            &api,
            &ctx,
            contact.body.id,
            &contracts::crm::AddContactEmailRequest {
                address: "Sales@Test.dev".into(),
                label: None,
                is_primary: Some(true),
                kind: None,
            },
            Some(format!("e-{name}")),
        )
        .await
        .unwrap();
        address_ids.push(linked.body.email_address_id);
    }
    assert!(
        address_ids.iter().all(|id| *id == address_ids[0]),
        "shared address produced multiple email_addresses rows"
    );

    let mut tx = api.begin(ws).await.unwrap();
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM email_addresses
          WHERE workspace_id = $1 AND normalized_key = 'sales@test.dev'",
    )
    .bind(ws)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    // And exactly one primary link per contact.
    let primaries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM contact_emails ce
          JOIN email_addresses ea ON ea.workspace_id = ce.workspace_id
             AND ea.id = ce.email_address_id
         WHERE ce.workspace_id = $1 AND ea.normalized_key = 'sales@test.dev'
           AND ce.is_primary",
    )
    .bind(ws)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(n, 1);
    assert_eq!(primaries, 3);
}

/// §5.6 for notes: creating a note on a thread in a mailbox the principal
/// cannot read fails closed (404), and listing hides mail-linked notes.
#[tokio::test]
async fn note_on_thread_requires_mailbox_read() {
    let Some((api, _worker, world)) = test_world().await else {
        return;
    };
    let ws = world.workspace_id;

    let mut tx = api.begin(ws).await.unwrap();
    let thread_id: Uuid = sqlx::query_scalar(
        "INSERT INTO email_threads (workspace_id, mail_account_id, provider_thread_id)
         VALUES ($1, $2, 'nt-1') RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
    .unwrap();
    tx.commit().await.unwrap();

    // member_b has no grant → thread existence hidden (404).
    let ctx_b = ctx_for(&world, &world.member_b, "t4-note-b");
    let err = application::crm::notes::create(
        &api,
        &ctx_b,
        &contracts::crm::CreateNoteRequest {
            company_id: None,
            contact_id: None,
            thread_id: Some(thread_id),
            body: "peek".into(),
        },
        Some("nb".into()),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status(), 404);

    // member_a holds can_read → creates the note.
    let ctx_a = ctx_for(&world, &world.member_a, "t4-note-a");
    let note = application::crm::notes::create(
        &api,
        &ctx_a,
        &contracts::crm::CreateNoteRequest {
            company_id: None,
            contact_id: None,
            thread_id: Some(thread_id),
            body: "real".into(),
        },
        Some("na".into()),
    )
    .await
    .unwrap();
    assert_eq!(note.body.thread_id, Some(thread_id));

    // Listing for member_b hides the mail-linked note entirely.
    let mut tx = api.begin(ws).await.unwrap();
    let rows = repo::list_notes(
        &mut tx,
        None,
        None,
        Some(thread_id),
        world.member_b.principal_id,
        None,
        50,
    )
    .await
    .unwrap();
    assert!(rows.is_empty());
    let rows = repo::list_notes(
        &mut tx,
        None,
        None,
        Some(thread_id),
        world.member_a.principal_id,
        None,
        50,
    )
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    tx.rollback().await.unwrap();
}
