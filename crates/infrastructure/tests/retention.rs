//! T36 — privacy erasure is workspace scoped, scrubs person-owned search and
//! attachment data, keeps the minimum suppression identity, and invalidates
//! caches. All records are synthetic rows seeded into the dedicated test DB.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use contracts::{AppError, ErrorCode};
use infrastructure::{
    db::{Db, ROLE_RETENTION},
    repo::compliance::test_fixtures as compliance_fixtures,
    retention::{CacheInvalidator, ErasureRequest, erase_contact, retry_pending_object_deletions},
    storage::{LocalObjectStore, ObjectStore},
    testing::seed_workspace,
};
use uuid::Uuid;

#[derive(Default)]
struct CacheSpy(AtomicUsize);

#[async_trait]
impl CacheInvalidator for CacheSpy {
    async fn invalidate_contact(
        &self,
        _workspace_id: Uuid,
        _contact_id: Uuid,
        _email_address_ids: &[Uuid],
    ) -> Result<(), AppError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

async fn retention_db() -> Db {
    let url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must target the synthetic test DB");
    let owner = Db::connect(&url, None)
        .await
        .expect("test DB connect failed");
    owner.migrate().await.expect("test DB migrations failed");
    Db::connect(&url, Some(ROLE_RETENTION))
        .await
        .expect("retention role connect failed")
}

fn request(workspace_id: Uuid, contact_id: Uuid, actor_id: Uuid) -> ErasureRequest {
    ErasureRequest {
        workspace_id,
        contact_id,
        actor_principal_id: Some(actor_id),
        request_id: format!("t36-{}", Uuid::new_v4()),
        trace_id: None,
    }
}

#[tokio::test]
async fn t36_erases_contact_owned_content_keeps_suppression_and_preserves_other_contact() {
    let Some(api_db) = common::test_db().await else {
        return;
    };
    let maintenance_db = retention_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let (
        contact_id,
        other_contact_id,
        email_id,
        other_email_id,
        shared_contact_id,
        shared_email_id,
        outbound_id,
        engagement_token,
        engagement_capability_id,
        booking_link_id,
        booking_id,
        message_id,
        object_id,
        source_import_id,
        source_object_id,
        source_object_key,
    ) = {
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let contact_id = compliance_fixtures::insert_contact(&mut tx, "T36 Synthetic Person")
            .await
            .unwrap();
        let other_contact_id =
            compliance_fixtures::insert_contact(&mut tx, "Keep Synthetic Person")
                .await
                .unwrap();
        let shared_contact_id =
            compliance_fixtures::insert_contact(&mut tx, "Keep Shared Address Person")
                .await
                .unwrap();
        let email_id = compliance_fixtures::insert_email_address(
            &mut tx,
            &format!("t36+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let other_email_id = compliance_fixtures::insert_email_address(
            &mut tx,
            &format!("keep+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        let shared_email_id = compliance_fixtures::insert_email_address(
            &mut tx,
            &format!("shared+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        compliance_fixtures::link_contact_email(&mut tx, contact_id, email_id)
            .await
            .unwrap();
        compliance_fixtures::link_contact_email(&mut tx, other_contact_id, other_email_id)
            .await
            .unwrap();
        compliance_fixtures::link_contact_email(&mut tx, contact_id, shared_email_id)
            .await
            .unwrap();
        compliance_fixtures::link_contact_email(&mut tx, shared_contact_id, shared_email_id)
            .await
            .unwrap();
        let source_object_key = format!("t36-source-csv-{}", Uuid::new_v4());
        let source_object_id: Uuid = sqlx::query_scalar(
            "INSERT INTO stored_objects
                (workspace_id, storage_key, media_type, byte_size, content_hash, scan_status)
             VALUES ($1, $2, 'text/csv', 96, decode(repeat('33', 32), 'hex'), 'clean')
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(&source_object_key)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let source_import_id: Uuid = sqlx::query_scalar(
            "INSERT INTO import_jobs (workspace_id, stored_object_id, requested_by, mapping, status)
             VALUES ($1, $2, $3, '{\"job_kind\":\"import\"}', 'completed') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(source_object_id)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO import_rows
                (workspace_id, import_job_id, row_no, normalized_data, status, contact_id)
             VALUES ($1, $2, 1, '{\"name\":\"T36 Synthetic Person\"}', 'imported', $3),
                    ($1, $2, 2, '{\"name\":\"Keep Synthetic Person\"}', 'imported', $4)",
        )
        .bind(world.workspace_id)
        .bind(source_import_id)
        .bind(contact_id)
        .bind(other_contact_id)
        .execute(tx.conn())
        .await
        .unwrap();
        let outbound_id = compliance_fixtures::insert_outbound(
            &mut tx,
            world.mail_account_id,
            world.connection_id,
            world.admin.principal_id,
            email_id,
        )
        .await
        .unwrap();
        let engagement_token = infrastructure::crypto::generate_token();
        infrastructure::repo::engagement::insert_capability(
            &mut tx,
            outbound_id,
            email_id,
            "click",
            &engagement_token,
            Some("https://private.example.test/person?email=t36@example.test"),
        )
        .await
        .unwrap();
        let engagement_capability_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM outbound_engagement_capabilities
              WHERE workspace_id = $1 AND token_hash = $2",
        )
        .bind(world.workspace_id)
        .bind(infrastructure::crypto::sha256_bytes(engagement_token.as_bytes()).to_vec())
        .fetch_one(tx.conn())
        .await
        .unwrap();
        infrastructure::repo::engagement::record_request(
            &mut tx,
            engagement_capability_id,
            "click",
        )
        .await
        .unwrap();
        let calendar_connection_id: Uuid = sqlx::query_scalar(
            "INSERT INTO integration_connections
                (workspace_id, provider, region, secret_ref, status, connected_by)
             VALUES ($1, 'zoho_calendar', 'us', 't36-calendar-secret', 'active', $2)
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let calendar_id: Uuid = sqlx::query_scalar(
            "INSERT INTO calendar_connections
                (workspace_id, host_principal_id, connection_id, provider_calendar_id, status)
             VALUES ($1, $2, $3, 't36-calendar', 'active') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(calendar_connection_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let meeting_type_id: Uuid = sqlx::query_scalar(
            "INSERT INTO meeting_types
                (workspace_id, host_principal_id, calendar_connection_id, name)
             VALUES ($1, $2, $3, 'T36 meeting') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.admin.principal_id)
        .bind(calendar_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let booking_link_id: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_links
                (workspace_id, meeting_type_id, host_principal_id, token_hash, bound_email_address_id)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(meeting_type_id)
        .bind(world.admin.principal_id)
        .bind(Uuid::new_v4().as_bytes().to_vec())
        .bind(email_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let booking_id: Uuid = sqlx::query_scalar(
            "INSERT INTO bookings
                (workspace_id, booking_link_id, host_principal_id, guest_email_address_id,
                 guest_name, guest_memo, guest_timezone, starts_at, ends_at,
                 occupied_start, occupied_end, status, attendance_status, outcome_note)
             VALUES ($1, $2, $3, $4, 'T36 Synthetic Person', 'T36 guest memo', 'Asia/Seoul',
                     now() + interval '2 days', now() + interval '2 days 30 minutes',
                     now() + interval '1 day 23 hours 50 minutes', now() + interval '2 days 40 minutes',
                     'canceled', 'attended', 'T36 personal outcome note') RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(booking_link_id)
        .bind(world.admin.principal_id)
        .bind(email_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO notes (workspace_id, contact_id, created_by, body)
             VALUES ($1, $2, $3, 'T36 private note marker')",
        )
        .bind(world.workspace_id)
        .bind(contact_id)
        .bind(world.admin.principal_id)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO notes (workspace_id, contact_id, created_by, body)
             VALUES ($1, $2, $3, 'Keep other contact note')",
        )
        .bind(world.workspace_id)
        .bind(other_contact_id)
        .bind(world.admin.principal_id)
        .execute(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO research_evidence
                (workspace_id, contact_id, field_path, source_url, observed_at, excerpt,
                 value, evidence_type, created_by)
             VALUES ($1, $2, 'name', 'https://example.test/profile', now(),
                     'T36 evidence marker', '{\"name\":\"T36 Synthetic Person\"}',
                     'observed', $3)",
        )
        .bind(world.workspace_id)
        .bind(contact_id)
        .bind(world.agent.principal_id)
        .execute(tx.conn())
        .await
        .unwrap();

        let thread_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_threads (workspace_id, mail_account_id, provider_thread_id, contact_id)
             VALUES ($1, $2, 't36-provider-thread', $3) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .bind(contact_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let message_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_messages
                (workspace_id, mail_account_id, thread_id, direction, provider_message_id,
                 internet_message_id, subject, body_text, message_at)
             VALUES ($1, $2, $3, 'inbound', 't36-provider-message', '<t36@example.test>',
                     'T36 private subject', 'T36 private message body', now())
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .bind(thread_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO message_participants
                (workspace_id, email_message_id, role, address, display_name, email_address_id)
             VALUES ($1, $2, 'from', $3, 'T36 Synthetic Person', $4)",
        )
        .bind(world.workspace_id)
        .bind(message_id)
        .bind(format!("t36+{}@example.test", Uuid::new_v4()))
        .bind(email_id)
        .execute(tx.conn())
        .await
        .unwrap();
        let object_id: Uuid = sqlx::query_scalar(
            "INSERT INTO stored_objects
                (workspace_id, storage_key, media_type, byte_size, content_hash, scan_status)
             VALUES ($1, $2, 'text/plain', 23, decode(repeat('11', 32), 'hex'), 'clean')
             RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(format!("t36-object-{}", Uuid::new_v4()))
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO message_attachments (workspace_id, email_message_id, stored_object_id, filename)
             VALUES ($1, $2, $3, 'private.txt')",
        )
        .bind(world.workspace_id)
        .bind(message_id)
        .bind(object_id)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (
            contact_id,
            other_contact_id,
            email_id,
            other_email_id,
            shared_contact_id,
            shared_email_id,
            outbound_id,
            engagement_token,
            engagement_capability_id,
            booking_link_id,
            booking_id,
            message_id,
            object_id,
            source_import_id,
            source_object_id,
            source_object_key,
        )
    };

    let storage_dir = tempfile::tempdir().unwrap();
    let object_store = LocalObjectStore::new(storage_dir.path(), [7; 32], b"test-key".to_vec());
    let object_key: String = {
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let key: String =
            sqlx::query_scalar("SELECT storage_key FROM stored_objects WHERE id = $1")
                .bind(object_id)
                .fetch_one(tx.conn())
                .await
                .unwrap();
        tx.rollback().await.unwrap();
        key
    };
    object_store
        .put(&object_key, b"T36 attachment body")
        .await
        .unwrap();
    let source_csv_bytes =
        b"name,email\nT36 Synthetic Person,t36@example.test\nKeep Synthetic Person,keep@example.test\n";
    object_store
        .put(&source_object_key, source_csv_bytes)
        .await
        .unwrap();
    let cache = CacheSpy::default();

    let report = erase_contact(
        &maintenance_db,
        request(world.workspace_id, contact_id, world.admin.principal_id),
        &object_store,
        &cache,
    )
    .await
    .unwrap();

    assert_eq!(report.contact_id, contact_id);
    assert_eq!(report.email_addresses_suppressed, 1);
    assert_eq!(report.source_objects_access_revoked, 1);
    assert_eq!(report.object_cleanup_pending, 0);
    assert_eq!(cache.0.load(Ordering::SeqCst), 1);
    assert!(object_store.get(&object_key).await.is_err());
    assert_eq!(
        object_store.get(&source_object_key).await.unwrap(),
        source_csv_bytes,
        "shared source bytes remain under retention policy"
    );

    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    assert!(
        infrastructure::repo::engagement::resolve_token(&api_db, &engagement_token)
            .await
            .unwrap()
            .is_none(),
        "erasure must revoke the old click capability"
    );
    let capability_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbound_engagement_capabilities
          WHERE workspace_id = $1 AND id = $2",
    )
    .bind(world.workspace_id)
    .bind(engagement_capability_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let request_bucket_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbound_engagement_request_buckets
          WHERE workspace_id = $1 AND capability_id = $2",
    )
    .bind(world.workspace_id)
    .bind(engagement_capability_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(capability_count, 0);
    assert_eq!(
        request_bucket_count, 0,
        "capability events cascade on erasure"
    );
    let contact_count: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts WHERE id = $1")
        .bind(contact_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(
        contact_count, 0,
        "target contact should no longer be searchable"
    );
    let other_count: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts WHERE id = $1")
        .bind(other_contact_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(other_count, 1, "unrelated contact must remain intact");
    let source_object = infrastructure::repo::imports::get_stored_object(&mut tx, source_object_id)
        .await
        .unwrap();
    assert!(
        source_object.is_none(),
        "deleted source object cannot be fetched or re-imported"
    );
    let (source_deleted_at, source_media_type): (Option<chrono::DateTime<chrono::Utc>>, String) =
        sqlx::query_as("SELECT deleted_at, media_type FROM stored_objects WHERE id = $1")
            .bind(source_object_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert!(source_deleted_at.is_some());
    assert_eq!(
        source_media_type, "text/csv",
        "shared bytes are not a cleanup tombstone"
    );
    let target_import_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM import_rows WHERE workspace_id = $1 AND import_job_id = $2 AND contact_id = $3",
    )
    .bind(world.workspace_id)
    .bind(source_import_id)
    .bind(contact_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    let preserved_import_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM import_rows WHERE workspace_id = $1 AND import_job_id = $2 AND contact_id = $3",
    )
    .bind(world.workspace_id)
    .bind(source_import_id)
    .bind(other_contact_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(target_import_rows, 0);
    assert_eq!(
        preserved_import_rows, 1,
        "other canonical import row remains"
    );
    let shared_link_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM contact_emails
          WHERE workspace_id = $1 AND contact_id = $2 AND email_address_id = $3",
    )
    .bind(world.workspace_id)
    .bind(shared_contact_id)
    .bind(shared_email_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        shared_link_count, 1,
        "shared address remains linked to its other owner"
    );
    let shared_suppressions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM suppressions WHERE workspace_id = $1 AND email_address_id = $2",
    )
    .bind(world.workspace_id)
    .bind(shared_email_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        shared_suppressions, 0,
        "erasing one contact must not suppress a shared address"
    );
    let sensitive_notes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notes WHERE body ILIKE '%T36 private note marker%'",
    )
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        sensitive_notes, 0,
        "contact note must be removed from search"
    );
    let preserved_notes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notes WHERE body = 'Keep other contact note'")
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(preserved_notes, 1);
    let evidence_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM research_evidence WHERE contact_id = $1 OR excerpt ILIKE '%T36 evidence marker%'",
    )
    .bind(contact_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        evidence_count, 0,
        "personal research evidence must be removed"
    );
    let (subject, body): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT subject, body_text FROM email_messages WHERE id = $1")
            .bind(message_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert!(
        subject.is_none() && body.is_none(),
        "message content must be scrubbed"
    );
    let (outbound_subject, outbound_body, outbound_to): (String, String, String) = sqlx::query_as(
        "SELECT subject, body_text, to_address_snapshot
               FROM outbound_messages WHERE id = $1",
    )
    .bind(outbound_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(outbound_subject, "[erased]");
    assert!(outbound_body.is_empty());
    assert_eq!(outbound_to, "[erased]");
    let (booking_guest, guest_memo, outcome_note): (String, Option<String>, Option<String>) =
        sqlx::query_as("SELECT guest_name, guest_memo, outcome_note FROM bookings WHERE id = $1")
            .bind(booking_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(booking_guest, "[erased]");
    assert!(guest_memo.is_none() && outcome_note.is_none());
    let (bound_email, link_revoked): (Option<Uuid>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as(
            "SELECT bound_email_address_id, revoked_at FROM booking_links WHERE id = $1",
        )
        .bind(booking_link_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert!(bound_email.is_none() && link_revoked.is_some());
    let attachments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM message_attachments WHERE email_message_id = $1")
            .bind(message_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(attachments, 0, "attachment access mapping must be removed");
    let (suppressed, kept_email): (i64, String) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE s.released_at IS NULL AND s.purpose_scope = 'all'),
                a.address_original
           FROM email_addresses a
           LEFT JOIN suppressions s ON s.workspace_id = a.workspace_id AND s.email_address_id = a.id
          WHERE a.id = $1
          GROUP BY a.address_original",
    )
    .bind(email_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(suppressed, 1, "re-import must remain blocked");
    assert!(
        kept_email.contains("@example.test"),
        "minimal suppression identity must survive"
    );
    let unrelated_email: String =
        sqlx::query_scalar("SELECT address_original FROM email_addresses WHERE id = $1")
            .bind(other_email_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert!(unrelated_email.starts_with("keep+"));
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_logs WHERE action = 'privacy.contact_erased' AND target_id = $1",
    )
    .bind(contact_id)
    .fetch_one(tx.conn())
    .await
    .unwrap();
    assert_eq!(
        audit_count, 1,
        "the erase and its audit must commit together"
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
async fn retention_requires_operator_role_and_cannot_cross_workspaces() {
    let Some(api_db) = common::test_db().await else {
        return;
    };
    let maintenance_db = retention_db().await;
    let owner = seed_workspace(&api_db).await.unwrap();
    let other = seed_workspace(&api_db).await.unwrap();
    let contact_id = {
        let mut tx = api_db.begin(owner.workspace_id).await.unwrap();
        let id = compliance_fixtures::insert_contact(&mut tx, "T36 Scope Person")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        id
    };
    let storage_dir = tempfile::tempdir().unwrap();
    let object_store = LocalObjectStore::new(storage_dir.path(), [7; 32], b"test-key".to_vec());
    let cache = CacheSpy::default();

    let err = erase_contact(
        &api_db,
        request(owner.workspace_id, contact_id, owner.admin.principal_id),
        &object_store,
        &cache,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Forbidden);

    let err = erase_contact(
        &maintenance_db,
        request(other.workspace_id, contact_id, other.admin.principal_id),
        &object_store,
        &cache,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);

    let mut tx = api_db.begin(owner.workspace_id).await.unwrap();
    let remains: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts WHERE id = $1")
        .bind(contact_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
    assert_eq!(remains, 1, "denied attempts must not mutate target data");
    tx.rollback().await.unwrap();
    assert_eq!(cache.0.load(Ordering::SeqCst), 0);
}

struct FailOnceStore(AtomicUsize);

#[async_trait]
impl ObjectStore for FailOnceStore {
    async fn put(&self, _key: &str, _plaintext: &[u8]) -> Result<(), AppError> {
        Ok(())
    }
    async fn get(&self, _key: &str) -> Result<Vec<u8>, AppError> {
        Ok(Vec::new())
    }
    async fn delete(&self, _key: &str) -> Result<(), AppError> {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(AppError::dependency_unavailable(
                "synthetic storage failure",
            ))
        } else {
            Ok(())
        }
    }
    fn sign_access(&self, _key: &str, _ttl: std::time::Duration) -> String {
        String::new()
    }
    fn verify_access(&self, _key: &str, _token: &str) -> bool {
        false
    }
}

#[tokio::test]
async fn t36_failed_object_delete_leaves_retryable_tombstone() {
    let Some(api_db) = common::test_db().await else {
        return;
    };
    let maintenance_db = retention_db().await;
    let world = seed_workspace(&api_db).await.unwrap();
    let (contact_id, object_id, key) = {
        let mut tx = api_db.begin(world.workspace_id).await.unwrap();
        let contact_id = compliance_fixtures::insert_contact(&mut tx, "T36 Retry Person")
            .await
            .unwrap();
        let email_id = compliance_fixtures::insert_email_address(
            &mut tx,
            &format!("retry+{}@example.test", Uuid::new_v4()),
        )
        .await
        .unwrap();
        compliance_fixtures::link_contact_email(&mut tx, contact_id, email_id)
            .await
            .unwrap();
        let thread_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_threads (workspace_id, mail_account_id, contact_id)
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .bind(contact_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let message_id: Uuid = sqlx::query_scalar(
            "INSERT INTO email_messages (workspace_id, mail_account_id, thread_id, direction, message_at)
             VALUES ($1, $2, $3, 'inbound', now()) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(world.mail_account_id)
        .bind(thread_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let key = format!("t36-retry-object-{}", Uuid::new_v4());
        let object_id: Uuid = sqlx::query_scalar(
            "INSERT INTO stored_objects
                (workspace_id, storage_key, media_type, byte_size, content_hash)
             VALUES ($1, $2, 'text/plain', 1, decode(repeat('22', 32), 'hex')) RETURNING id",
        )
        .bind(world.workspace_id)
        .bind(&key)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO message_attachments (workspace_id, email_message_id, stored_object_id, filename)
             VALUES ($1, $2, $3, 'retry.txt')",
        )
        .bind(world.workspace_id)
        .bind(message_id)
        .bind(object_id)
        .execute(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        (contact_id, object_id, key)
    };
    let store = FailOnceStore(AtomicUsize::new(0));
    let report = erase_contact(
        &maintenance_db,
        request(world.workspace_id, contact_id, world.admin.principal_id),
        &store,
        &CacheSpy::default(),
    )
    .await
    .unwrap();
    assert_eq!(report.object_cleanup_pending, 1);

    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let tombstone_deleted_at: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT deleted_at FROM stored_objects WHERE id = $1")
            .bind(object_id)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert!(
        tombstone_deleted_at.is_some(),
        "retry metadata must survive the failed delete"
    );
    tx.rollback().await.unwrap();

    assert_eq!(
        retry_pending_object_deletions(&maintenance_db, world.workspace_id, &store)
            .await
            .unwrap(),
        0,
        "successful retry leaves no object cleanup pending"
    );
    let mut tx = api_db.begin(world.workspace_id).await.unwrap();
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM stored_objects WHERE storage_key = $1")
            .bind(key)
            .fetch_one(tx.conn())
            .await
            .unwrap();
    assert_eq!(remaining, 0, "successful retry removes the tombstone");
    tx.rollback().await.unwrap();
}
