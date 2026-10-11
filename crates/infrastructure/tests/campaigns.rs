//! campaigns module DB-level tests (spec A.6.2 + wave-b Task 7 matrix):
//! composite-FK integrity across revision/launch/enrollment/step (T06),
//! freeze-immutability + post-approval snapshot pinning (T08), the
//! self-approval trigger (T09), and the one-enrollment-per-address-per-
//! launch / `RECIPIENT_ACTIVE_ELSEWHERE` anchors (T31).

mod common;

use chrono::{Duration, Utc};
use infrastructure::db::{Db, Tx};
use infrastructure::repo::approvals::{self, ApprovalTarget};
use infrastructure::repo::campaigns as repo;
use infrastructure::testing::{TestWorld, seed_workspace};
use uuid::Uuid;

/// Marketing delivery channel (zoho_mail — matches the seeded
/// integration_connection provider for fk_151; disabled/unverified is fine
/// at the DB layer — the guards under test don't inspect channel state).
async fn insert_marketing_channel(tx: &mut Tx<'_>, world: &TestWorld) -> (Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    let ident: Uuid = sqlx::query_scalar(
        "INSERT INTO sender_identities (workspace_id, mail_account_id, address, display_name, status, verified_at)
         VALUES ($1, $2, 'sender@test.dev', 'Sender', 'verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let channel: Uuid = sqlx::query_scalar(
        "INSERT INTO delivery_channels
            (workspace_id, mail_account_id, sender_identity_id, connection_id,
             provider, purpose, name)
         VALUES ($1, $2, $3, $4, 'zoho_mail', 'marketing', 'Bulk')
         RETURNING id",
    )
    .bind(ws)
    .bind(world.mail_account_id)
    .bind(ident)
    .bind(world.connection_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    (ident, channel)
}

async fn insert_template_version(tx: &mut Tx<'_>, created_by: Uuid) -> Uuid {
    let template: Uuid = sqlx::query_scalar(
        "INSERT INTO email_templates (workspace_id, name, created_by)
         VALUES ($1, 'tpl', $2) RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO template_versions
            (workspace_id, template_id, version_no, subject, body_text, variable_schema,
             content_hash, created_by)
         VALUES ($1, $2, 1, 'Hi {{contact.first_name}}', 'body {{company.name}}', '{}',
                 decode(md5(random()::text), 'hex'), $3)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(template)
    .bind(created_by)
    .fetch_one(tx.conn())
    .await
    .unwrap()
}

/// email_address + contact + link; returns (email_address_id, contact_id).
async fn insert_recipient(tx: &mut Tx<'_>, local: &str) -> (Uuid, Uuid) {
    let ws = tx.workspace_id().unwrap();
    let addr: Uuid = sqlx::query_scalar(
        "INSERT INTO email_addresses (workspace_id, address_original, normalized_key, kind, verification_status, verified_at)
         VALUES ($1, $2, $2, 'person', 'verified', now()) RETURNING id",
    )
    .bind(ws)
    .bind(format!("{local}@example.com"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let contact: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, $2) RETURNING id",
    )
    .bind(ws)
    .bind(format!("Contact {local}"))
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id, is_primary)
         VALUES ($1, $2, $3, true)",
    )
    .bind(ws)
    .bind(contact)
    .bind(addr)
    .execute(tx.conn())
    .await
    .unwrap();
    (addr, contact)
}

struct Fixture {
    campaign: Uuid,
    revision: Uuid,
    launch: Uuid,
    channel: Uuid,
    sender_identity: Uuid,
    step: Uuid,
    address: Uuid,
    contact: Uuid,
}

/// campaign + revision (draft) + one step + building launch + one recipient.
async fn launch_fixture(db: &Db, world: &TestWorld) -> Fixture {
    let ws = world.workspace_id;
    let mut tx = db.begin(ws).await.unwrap();
    let (ident, channel) = insert_marketing_channel(&mut tx, world).await;
    let tv = insert_template_version(&mut tx, world.admin.principal_id).await;
    let campaign = repo::insert_campaign(
        &mut tx,
        world.mail_account_id,
        "Fixture campaign",
        None,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let revision = repo::insert_revision(
        &mut tx,
        campaign,
        world.mail_account_id,
        channel,
        1,
        "UTC",
        &serde_json::json!({}),
        true,
        true,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let step = repo::insert_step(&mut tx, revision, tv, 1, 0)
        .await
        .unwrap();
    let launch = repo::insert_launch(
        &mut tx,
        revision,
        channel,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let (address, contact) = insert_recipient(&mut tx, "fixture").await;
    tx.commit().await.unwrap();
    Fixture {
        campaign,
        revision,
        launch,
        channel,
        sender_identity: ident,
        step,
        address,
        contact,
    }
}

/// FK/unique violations surface as `AppError::conflict` via map_sqlx.
fn assert_conflict(err: &contracts::AppError, what: &str) {
    assert_eq!(
        err.code,
        contracts::ErrorCode::Conflict,
        "expected conflict ({what}), got: {err}"
    );
}

async fn second_revision(f: &Fixture, db: &Db, world: &TestWorld) -> (Uuid, Uuid) {
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let tv = insert_template_version(&mut tx, world.admin.principal_id).await;
    let rev2 = repo::insert_revision(
        &mut tx,
        f.campaign,
        world.mail_account_id,
        f.channel,
        2,
        "UTC",
        &serde_json::json!({}),
        true,
        true,
        world.admin.principal_id,
    )
    .await
    .unwrap();
    let step2 = repo::insert_step(&mut tx, rev2, tv, 1, 0).await.unwrap();
    tx.commit().await.unwrap();
    (rev2, step2)
}

/// T06 — an enrollment whose `revision_id` disagrees with its launch's
/// revision must fail the composite FK
/// (workspace_id, revision_id, launch_id) → campaign_launches.
#[tokio::test]
async fn t06_enrollment_on_different_revision_fails_fk() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let f = launch_fixture(&db, &world).await;
    let (rev2, _step2) = second_revision(&f, &db, &world).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let err = repo::insert_enrollment(
        &mut tx,
        f.launch,
        rev2, // launch belongs to f.revision — composite FK mismatch
        f.address,
        Some(f.contact),
        "pending",
        None,
    )
    .await
    .expect_err("enrollment bound to a different revision must violate fk");
    assert_conflict(&err, "enrollment on wrong revision");
    tx.rollback().await.unwrap();
}

/// T06 — `current_step_id` must reference a step of the enrollment's own
/// revision (fk: (workspace_id, revision_id, current_step_id) →
/// campaign_steps (workspace_id, revision_id, id)).
#[tokio::test]
async fn t06_enrollment_step_from_wrong_revision_fails_fk() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let f = launch_fixture(&db, &world).await;
    let (_rev2, step2) = second_revision(&f, &db, &world).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let enr = repo::insert_enrollment(
        &mut tx,
        f.launch,
        f.revision,
        f.address,
        Some(f.contact),
        "pending",
        None,
    )
    .await
    .unwrap();
    // Launch still 'building' → the frozen-enrollment guard passes; the
    // composite FK on (revision_id, current_step_id) must refuse a step of
    // another revision.
    let err = sqlx::query(
        "UPDATE campaign_enrollments SET current_step_id = $3
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(enr)
    .bind(step2)
    .execute(tx.conn())
    .await
    .expect_err("current_step_id from another revision must violate fk");
    assert!(
        err.to_string().contains("foreign key") || err.to_string().contains("violates"),
        "expected FK violation, got: {err}"
    );
    tx.rollback().await.unwrap();
}

/// T06 — an outbound row's `step_id` must belong to the same revision as
/// `revision_id` on the row (composite FK to campaign_steps).
#[tokio::test]
async fn t06_outbound_step_from_wrong_revision_fails_fk() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let f = launch_fixture(&db, &world).await;
    let (_rev2, step2) = second_revision(&f, &db, &world).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let enr = repo::insert_enrollment(
        &mut tx,
        f.launch,
        f.revision,
        f.address,
        Some(f.contact),
        "pending",
        None,
    )
    .await
    .unwrap();
    let err = repo::insert_step_outbound(
        &mut tx,
        world.mail_account_id,
        f.channel,
        f.sender_identity,
        f.address,
        None,
        f.launch,
        f.revision,
        enr,
        step2, // belongs to rev2 — must violate the (revision_id, step_id) FK
        world.admin.principal_id,
        "sender@test.dev",
        "fixture@example.com",
        "hi",
        "body",
        None,
        &[7u8; 32],
    )
    .await
    .expect_err("outbound step from another revision must violate fk");
    assert_conflict(&err, "outbound step on wrong revision");
    tx.rollback().await.unwrap();
}

/// T08 — after freeze + approval request creation, the pinned snapshot and
/// frozen structural fields are immutable at the DB level.
#[tokio::test]
async fn t08_frozen_rows_and_request_snapshot_are_immutable() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let f = launch_fixture(&db, &world).await;
    let manifest = [42u8; 32];

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    repo::insert_enrollment(
        &mut tx,
        f.launch,
        f.revision,
        f.address,
        Some(f.contact),
        "pending",
        None,
    )
    .await
    .unwrap();
    assert!(repo::freeze_revision(&mut tx, f.revision).await.unwrap());
    assert_eq!(
        repo::freeze_launch_row(&mut tx, f.launch, &manifest)
            .await
            .unwrap(),
        1
    );
    let request_id = approvals::insert_request(
        &mut tx,
        world.admin.principal_id,
        ApprovalTarget::CampaignLaunch(f.launch),
        &manifest,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // 1. manifest_hash pinned — any change to it would invalidate the
    //    approval's snapshot; the frozen-launch guard refuses.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let err = sqlx::query(
        "UPDATE campaign_launches SET manifest_hash = decode('aa', 'hex')
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(f.launch)
    .execute(tx.conn())
    .await
    .expect_err("manifest_hash of a frozen launch must be immutable");
    assert!(err.to_string().contains("immutable"), "{err}");
    tx.rollback().await.unwrap();

    // 2. revision/requested_by/expires_at likewise.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let err = sqlx::query(
        "UPDATE campaign_launches SET expires_at = now() + interval '9 days'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(f.launch)
    .execute(tx.conn())
    .await
    .expect_err("expires_at of a frozen launch must be immutable");
    assert!(err.to_string().contains("immutable"), "{err}");
    tx.rollback().await.unwrap();

    // 3. The pinned snapshot hash on the approval request cannot be
    //    swapped — T08's "post-approval change → unusable" anchor.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let err = sqlx::query(
        "UPDATE approval_requests SET snapshot_hash = decode('aa', 'hex')
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(request_id)
    .execute(tx.conn())
    .await
    .expect_err("approval snapshot_hash must be immutable");
    assert!(err.to_string().contains("immutable"), "{err}");
    tx.rollback().await.unwrap();

    // 4. No enrollment may be added to a frozen launch.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let (addr2, contact2) = insert_recipient(&mut tx, "late").await;
    let err = repo::insert_enrollment(
        &mut tx,
        f.launch,
        f.revision,
        addr2,
        Some(contact2),
        "pending",
        None,
    )
    .await
    .expect_err("enrollment insert on a frozen launch must be refused");
    assert!(
        err.to_string().contains("frozen") || err.to_string().contains("immutable"),
        "{err}"
    );
    tx.rollback().await.unwrap();

    // 5. No step may be added to (or removed from) a frozen revision.
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let tv = insert_template_version(&mut tx, world.admin.principal_id).await;
    let err = repo::insert_step(&mut tx, f.revision, tv, 2, 0)
        .await
        .expect_err("step insert on a frozen revision must be refused");
    assert!(err.to_string().contains("frozen"), "{err}");
    tx.rollback().await.unwrap();
}

/// A permitted human requester can approve their own current snapshot.
#[tokio::test]
async fn requester_approval_allowed_by_trigger() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let f = launch_fixture(&db, &world).await;
    let hash = [1u8; 32];

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let request_id = approvals::insert_request(
        &mut tx,
        world.admin.principal_id,
        ApprovalTarget::CampaignLaunch(f.launch),
        &hash,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let result = sqlx::query(
        "INSERT INTO approval_decisions
            (workspace_id, approval_request_id, decision, decided_by,
             snapshot_hash, version_at_decision)
         VALUES ($1, $2, 'approved', $3, $4, 1)",
    )
    .bind(world.workspace_id)
    .bind(request_id)
    .bind(world.admin.principal_id) // == requested_by → self approval
    .bind(&hash[..])
    .execute(tx.conn())
    .await
    .unwrap();
    assert_eq!(result.rows_affected(), 1);
    tx.rollback().await.unwrap();
}

/// T31 — one email address may back N contacts, but a launch holds at most
/// one enrollment per address (UNIQUE (workspace_id, launch_id,
/// email_address_id)).
#[tokio::test]
async fn t31_one_enrollment_per_address_per_launch() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let f = launch_fixture(&db, &world).await;

    // A second contact sharing the same address (role addresses are legal:
    // contact_emails is N:M).
    let mut tx = db.begin(world.workspace_id).await.unwrap();
    let contact2: Uuid = sqlx::query_scalar(
        "INSERT INTO contacts (workspace_id, name) VALUES ($1, 'Second') RETURNING id",
    )
    .bind(world.workspace_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id)
         VALUES ($1, $2, $3)",
    )
    .bind(world.workspace_id)
    .bind(contact2)
    .bind(f.address)
    .execute(tx.conn())
    .await
    .unwrap();
    repo::insert_enrollment(
        &mut tx,
        f.launch,
        f.revision,
        f.address,
        Some(f.contact),
        "pending",
        None,
    )
    .await
    .unwrap();
    // A second enrollment for the same address — regardless of which
    // contact it cites — must violate the unique key.
    let err = repo::insert_enrollment(
        &mut tx,
        f.launch,
        f.revision,
        f.address,
        Some(contact2),
        "pending",
        None,
    )
    .await
    .expect_err("duplicate address enrollment in one launch must violate unique");
    assert_conflict(&err, "second enrollment for same address in launch");
    tx.rollback().await.unwrap();
}

/// T31/RECIPIENT_ACTIVE_ELSEWHERE — the app-level `activate_enrollment`
/// NOT EXISTS guard plus the `active_campaign_address_uq` partial index
/// together pin "one ACTIVE enrollment per address workspace-wide".
#[tokio::test]
async fn t31_second_active_enrollment_for_address_is_blocked() {
    let Some(db) = common::test_db().await else {
        return;
    };
    let world = seed_workspace(&db).await.unwrap();
    let f = launch_fixture(&db, &world).await;

    let mut tx = db.begin(world.workspace_id).await.unwrap();
    // Two launches (same revision is legal — launch is per-attempt).
    let launch_b = repo::insert_launch(
        &mut tx,
        f.revision,
        f.channel,
        world.admin.principal_id,
        Utc::now() + Duration::days(1),
    )
    .await
    .unwrap();
    let enr_a = repo::insert_enrollment(
        &mut tx,
        f.launch,
        f.revision,
        f.address,
        Some(f.contact),
        "pending",
        None,
    )
    .await
    .unwrap();
    let enr_b = repo::insert_enrollment(
        &mut tx,
        launch_b,
        f.revision,
        f.address,
        Some(f.contact),
        "pending",
        None,
    )
    .await
    .unwrap();
    // Freeze both launches (enrollments stay pending but set is locked).
    repo::freeze_launch_row(&mut tx, f.launch, &[3u8; 32])
        .await
        .unwrap();
    repo::freeze_launch_row(&mut tx, launch_b, &[4u8; 32])
        .await
        .unwrap();

    // First activation succeeds; second must be refused (returns 0 rows).
    assert!(
        repo::activate_enrollment(&mut tx, enr_a, f.step, Utc::now())
            .await
            .unwrap(),
        "first activation should succeed"
    );
    assert!(
        !repo::activate_enrollment(&mut tx, enr_b, f.step, Utc::now())
            .await
            .unwrap(),
        "RECIPIENT_ACTIVE_ELSEWHERE: second activation must no-op"
    );

    // The partial unique index is the last line of defence even for raw
    // writes that skip the guarded helper.
    let err = sqlx::query(
        "UPDATE campaign_enrollments SET status = 'active'
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(enr_b)
    .execute(tx.conn())
    .await
    .expect_err("raw activation of a second enrollment must violate the uq index");
    assert!(
        err.to_string().contains("active_campaign_address_uq")
            || err.to_string().contains("duplicate")
            || err.to_string().contains("unique"),
        "expected active_campaign_address_uq violation, got: {err}"
    );
    tx.rollback().await.unwrap();
}
