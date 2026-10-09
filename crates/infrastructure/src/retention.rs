//! Audited, workspace-scoped privacy maintenance (source §6.6/§15, A.5).
//!
//! Erasure is deliberately an operator-only path. Contact rows and owned
//! content are removed or scrubbed in one RLS transaction, while an
//! all-purpose suppression and its normalized email key remain so an import
//! cannot recreate a sendable identity. Object-store bytes are deleted after
//! the transaction; deleted object rows remain as retry tombstones until that
//! succeeds. Callers supply cache invalidation because no person-data cache
//! is currently configured in infrastructure.

use async_trait::async_trait;
use contracts::AppError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    audit::{self, AuditEntry},
    db::{Db, ROLE_RETENTION, Tx},
    error::map_sqlx,
    repo::compliance_core,
    storage::ObjectStore,
};

const RETENTION_ACTION: &str = "privacy.contact_erased";
const SUPPRESSION_SOURCE: &str = "privacy_erasure";
const PRIVACY_TOMBSTONE_MEDIA_TYPE: &str = "application/x-privacy-erasure-tombstone";

/// The authenticated request or maintenance job responsible for the erasure.
/// Request identifiers are opaque diagnostic ids and must not contain PII.
#[derive(Debug, Clone)]
pub struct ErasureRequest {
    pub workspace_id: Uuid,
    pub contact_id: Uuid,
    pub actor_principal_id: Option<Uuid>,
    pub request_id: String,
    pub trace_id: Option<String>,
}

/// Cache adapter seam. Implementations should evict person/contact, email,
/// search and attachment authorization entries for these exact identifiers.
#[async_trait]
pub trait CacheInvalidator: Send + Sync {
    async fn invalidate_contact(
        &self,
        workspace_id: Uuid,
        contact_id: Uuid,
        email_address_ids: &[Uuid],
    ) -> Result<(), AppError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErasureReport {
    pub contact_id: Uuid,
    pub email_addresses_suppressed: u64,
    pub messages_scrubbed: u64,
    /// Native Campaigns exports whose local snapshots were redacted. This
    /// does not claim that a provider-accepted campaign was recalled.
    pub marketing_exports_redacted: u64,
    /// True when local exports were redacted without requesting cancellation
    /// from the external provider.
    pub provider_cancel_not_requested: bool,
    pub attachments_removed: u64,
    /// Shared source objects whose application access was revoked because
    /// they contain a row belonging to the erased contact.
    pub source_objects_access_revoked: u64,
    /// Number of deleted-object tombstones still awaiting physical cleanup.
    pub object_cleanup_pending: u64,
    /// True when the supplied cache adapter failed; call it again with the
    /// request identifiers to retry invalidation.
    pub cache_invalidation_pending: bool,
}

/// Erase one exact contact. Cross-workspace ids are indistinguishable from
/// missing contacts because the target lookup is workspace-bound and RLS
/// remains enabled under the retention role.
pub async fn erase_contact(
    db: &Db,
    request: ErasureRequest,
    object_store: &dyn ObjectStore,
    cache: &dyn CacheInvalidator,
) -> Result<ErasureReport, AppError> {
    if request.request_id.trim().is_empty() {
        return Err(AppError::validation("request_id is required", vec![]));
    }

    let mut tx = db.begin(request.workspace_id).await?;
    begin_retention_operation(&mut tx).await?;

    let contact: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM contacts
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    if contact.is_none() {
        return Err(AppError::not_found("contact not found"));
    }

    let linked_email_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT email_address_id FROM contact_emails
          WHERE workspace_id = $1 AND contact_id = $2
          ORDER BY email_address_id",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;

    // Native send reservation locks the export before it locks recipient
    // identities. Prelock a conservative set of exports before the email
    // address locks below to preserve that order. This is only a lock superset:
    // ownership and the final redaction set are still recomputed from exclusive
    // addresses and frozen outbound IDs after those identities are locked.
    let potential_thread_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT t.id
           FROM email_threads t
          WHERE t.workspace_id = $1
            AND (t.contact_id = $2
                 OR t.id IN (
                    SELECT o.thread_id FROM outbound_messages o
                     WHERE o.workspace_id = $1 AND o.thread_id IS NOT NULL
                       AND o.to_email_address_id = ANY($3))
                 OR t.id IN (
                    SELECT m.thread_id FROM email_messages m
                    JOIN message_participants p
                      ON p.workspace_id = m.workspace_id AND p.email_message_id = m.id
                     WHERE m.workspace_id = $1 AND p.email_address_id = ANY($3)))
          ORDER BY t.id",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .bind(&linked_email_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let potential_outbound_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT o.id FROM outbound_messages o
          WHERE o.workspace_id = $1
            AND (o.thread_id = ANY($2)
                 OR o.to_email_address_id = ANY($3)
                 OR o.enrollment_id IN (
                    SELECT e.id FROM campaign_enrollments e
                     WHERE e.workspace_id = $1 AND e.contact_id = $4))
          ORDER BY o.id",
    )
    .bind(request.workspace_id)
    .bind(&potential_thread_ids)
    .bind(&linked_email_ids)
    .bind(request.contact_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let potential_export_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT e.id FROM marketing_exports e
          WHERE e.workspace_id = $1
            AND (EXISTS (
                    SELECT 1 FROM campaign_enrollments ce
                     WHERE ce.workspace_id = e.workspace_id
                       AND ce.launch_id = e.launch_id
                       AND (ce.contact_id = $2
                            OR ce.email_address_id = ANY($3)))
                 OR EXISTS (
                    SELECT 1 FROM marketing_export_recipients r
                     WHERE r.workspace_id = e.workspace_id
                       AND r.export_id = e.id
                       AND (r.email_address_id = ANY($3)
                            OR r.outbound_message_id = ANY($4))))
          ORDER BY e.id",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .bind(&linked_email_ids)
    .bind(&potential_outbound_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let prelocked_export_ids: Vec<Uuid> = if potential_export_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar(
            "SELECT id FROM marketing_exports
              WHERE workspace_id = $1 AND id = ANY($2)
              ORDER BY id FOR UPDATE",
        )
        .bind(request.workspace_id)
        .bind(&potential_export_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };

    // Lock every linked identity before checking which addresses are shared.
    // Contact-email inserts take a key-share lock on these rows, so a new
    // owner cannot race the exclusivity decision or suppression write.
    for email_id in &linked_email_ids {
        compliance_core::lock_email_address(&mut tx, *email_id).await?;
    }

    // Only an address exclusively linked to this contact is owned by this
    // erasure. A shared address stays usable by its remaining contact.
    let exclusive_email_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT ce.email_address_id
           FROM contact_emails ce
          WHERE ce.workspace_id = $1 AND ce.contact_id = $2
            AND NOT EXISTS (
                SELECT 1 FROM contact_emails other
                 WHERE other.workspace_id = ce.workspace_id
                   AND other.email_address_id = ce.email_address_id
                   AND other.contact_id <> ce.contact_id)
          ORDER BY ce.email_address_id",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;

    // Ensure an all-scope suppression. Keeping this normalized identity is
    // the deliberate minimal exception that makes T32 re-imports remain
    // blocked.
    let mut suppressed_count = 0u64;
    for email_id in &exclusive_email_ids {
        compliance_core::insert_suppression(
            &mut tx,
            *email_id,
            "all",
            "manual",
            SUPPRESSION_SOURCE,
            request.actor_principal_id,
            None,
        )
        .await?;
        suppressed_count += 1;
    }

    let candidate_thread_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT t.id
           FROM email_threads t
          WHERE t.workspace_id = $1
            AND (t.contact_id = $2
                 OR t.id IN (
                    SELECT o.thread_id FROM outbound_messages o
                     WHERE o.workspace_id = $1 AND o.thread_id IS NOT NULL
                       AND o.to_email_address_id = ANY($3))
                 OR t.id IN (
                    SELECT m.thread_id FROM email_messages m
                    JOIN message_participants p
                      ON p.workspace_id = m.workspace_id AND p.email_message_id = m.id
                     WHERE m.workspace_id = $1 AND p.email_address_id = ANY($3)))
          ORDER BY t.id",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .bind(&exclusive_email_ids)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    let thread_ids: Vec<Uuid> = if candidate_thread_ids.is_empty() {
        Vec::new()
    } else {
        // Prevent new messages from being attached to an erased thread while
        // its message rows and attachments are being scrubbed.
        sqlx::query_scalar(
            "SELECT id FROM email_threads
              WHERE workspace_id = $1 AND id = ANY($2)
              ORDER BY id FOR UPDATE",
        )
        .bind(request.workspace_id)
        .bind(&candidate_thread_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };

    let message_ids: Vec<Uuid> = if thread_ids.is_empty() && exclusive_email_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar(
            "SELECT DISTINCT m.id
               FROM email_messages m
              WHERE m.workspace_id = $1
                AND (m.thread_id = ANY($2)
                     OR EXISTS (
                        SELECT 1 FROM message_participants p
                         WHERE p.workspace_id = m.workspace_id
                           AND p.email_message_id = m.id
                           AND p.email_address_id = ANY($3)))
              ORDER BY m.id",
        )
        .bind(request.workspace_id)
        .bind(&thread_ids)
        .bind(&exclusive_email_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };

    let message_ids = if message_ids.is_empty() {
        message_ids
    } else {
        // Lock before reading attachment rows. A new attachment references
        // its message through a key-share FK lock and must wait for erasure.
        sqlx::query_scalar(
            "SELECT id FROM email_messages
              WHERE workspace_id = $1 AND id = ANY($2)
              ORDER BY id FOR UPDATE",
        )
        .bind(request.workspace_id)
        .bind(&message_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };

    // Frozen campaign enrollments may retain the erased contact even when
    // their email address is shared and has no thread. Include those owned
    // outbounds explicitly; another enrollment for the same shared address
    // but a different contact remains outside this set.
    let outbound_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT o.id FROM outbound_messages o
          WHERE o.workspace_id = $1
            AND (o.thread_id = ANY($2)
                 OR o.to_email_address_id = ANY($3)
                 OR EXISTS (
                    SELECT 1 FROM campaign_enrollments e
                     WHERE e.workspace_id = o.workspace_id
                       AND e.id = o.enrollment_id
                       AND e.contact_id = $4))
          ORDER BY o.id",
    )
    .bind(request.workspace_id)
    .bind(&thread_ids)
    .bind(&exclusive_email_ids)
    .bind(request.contact_id)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;

    // Redact any native Campaigns export whose frozen recipient map contains
    // an exclusively erased address or an outbound scrubbed above. The export
    // rows are already locked ahead of the email-address locks; read their
    // operation states before deciding whether erasure may proceed.
    let export_candidates: Vec<Uuid> = if exclusive_email_ids.is_empty() && outbound_ids.is_empty()
    {
        Vec::new()
    } else {
        sqlx::query_scalar(
            "SELECT DISTINCT export_id
               FROM marketing_export_recipients
              WHERE workspace_id = $1
                AND (email_address_id = ANY($2) OR outbound_message_id = ANY($3))
              ORDER BY export_id",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .bind(&outbound_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };
    if export_candidates
        .iter()
        .any(|export_id| !prelocked_export_ids.contains(export_id))
    {
        return Err(AppError::conflict(
            "native marketing export targets changed during privacy erasure; retry the request",
        ));
    }
    let marketing_export_rows: Vec<(Uuid, String)> = if export_candidates.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT id, state FROM marketing_exports
              WHERE workspace_id = $1 AND id = ANY($2)
              ORDER BY id FOR UPDATE",
        )
        .bind(request.workspace_id)
        .bind(&export_candidates)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };
    let marketing_export_ids: Vec<Uuid> = marketing_export_rows.iter().map(|(id, _)| *id).collect();
    let operation_states: Vec<(Uuid, String, String)> = if marketing_export_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as(
            "SELECT export_id, kind, status FROM marketing_operations
              WHERE workspace_id = $1 AND export_id = ANY($2)
              ORDER BY export_id, id",
        )
        .bind(request.workspace_id)
        .bind(&marketing_export_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };
    let unresolved_export = marketing_export_rows.iter().find(|(export_id, state)| {
        let has_in_flight_write =
            operation_states
                .iter()
                .any(|(operation_export_id, kind, status)| {
                    operation_export_id == export_id
                        && matches!(
                            kind.as_str(),
                            "create_list"
                                | "add_contacts"
                                | "subscribe"
                                | "create_campaign"
                                | "send_campaign"
                        )
                        && matches!(status.as_str(), "started" | "unknown")
                });
        let has_completed_send_before_terminal_export =
            !matches!(state.as_str(), "completed" | "rejected" | "canceled")
                && operation_states
                    .iter()
                    .any(|(operation_export_id, kind, status)| {
                        operation_export_id == export_id
                            && kind == "send_campaign"
                            && status == "completed"
                    });
        matches!(state.as_str(), "submitting" | "in_progress")
            || has_in_flight_write
            || has_completed_send_before_terminal_export
    });
    if unresolved_export.is_some() {
        return Err(AppError::conflict(
            "native Campaigns provider operation must be reconciled before privacy erasure",
        ));
    }

    // Launch values are recipient-level data. Exclusive identities are
    // scrubbed across launches; outbounds selected through an erased thread
    // scrub only the matching launch/address values.
    if !exclusive_email_ids.is_empty() || !outbound_ids.is_empty() {
        sqlx::query(
            "UPDATE marketing_launch_values v
                SET variables = '{}'::jsonb
              WHERE v.workspace_id = $1
                AND (v.email_address_id = ANY($2)
                     OR EXISTS (
                        SELECT 1 FROM outbound_messages o
                         WHERE o.workspace_id = v.workspace_id
                           AND o.launch_id = v.launch_id
                           AND o.to_email_address_id = v.email_address_id
                           AND o.id = ANY($3)))",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .bind(&outbound_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    let marketing_exports_redacted = if marketing_export_ids.is_empty() {
        0
    } else {
        sqlx::query(
            "UPDATE marketing_export_recipients
                SET variables = '{}'::jsonb,
                    normalized_address = 'privacy-erased:' || email_address_id::text,
                    sync_status = 'excluded', updated_at = now()
              WHERE workspace_id = $1 AND export_id = ANY($2)
                AND (email_address_id = ANY($3) OR outbound_message_id = ANY($4))",
        )
        .bind(request.workspace_id)
        .bind(&marketing_export_ids)
        .bind(&exclusive_email_ids)
        .bind(&outbound_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE marketing_exports
                SET state = 'canceled',
                    source_manifest_hash = decode(repeat('00', 32), 'hex'),
                    content_hash = decode(repeat('00', 32), 'hex'),
                    audience_hash = decode(repeat('00', 32), 'hex'),
                    export_hash = decode(repeat('00', 32), 'hex'),
                    subject = '', body_text = '', body_html = '', field_map = '{}'::jsonb,
                    report = '{\"privacy_erased\":true,\"provider_cancel_not_requested\":true}'::jsonb,
                    recipient_count = 0, version = version + 1
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&marketing_export_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?
        .rows_affected()
    };

    // Collect only objects exclusively referenced by the messages being
    // erased. Imports and messages outside this target keep shared objects.
    let attachment_object_ids: Vec<Uuid> = if message_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_scalar(
            "SELECT DISTINCT stored_object_id FROM message_attachments
              WHERE workspace_id = $1 AND email_message_id = ANY($2)
              ORDER BY stored_object_id",
        )
        .bind(request.workspace_id)
        .bind(&message_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };
    let locked_object_ids: Vec<Uuid> = if attachment_object_ids.is_empty() {
        Vec::new()
    } else {
        // Lock separately, then check for remaining references in a fresh
        // READ COMMITTED statement snapshot. New attachment/import references
        // use FK key-share locks and wait until this transaction commits.
        sqlx::query_scalar(
            "SELECT id FROM stored_objects
              WHERE workspace_id = $1 AND id = ANY($2)
              ORDER BY id FOR UPDATE",
        )
        .bind(request.workspace_id)
        .bind(&attachment_object_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
    };
    let object_rows: Vec<(Uuid, String)> = if locked_object_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query(
            "SELECT so.id, so.storage_key
               FROM stored_objects so
              WHERE so.workspace_id = $1 AND so.id = ANY($2)
                AND NOT EXISTS (
                    SELECT 1 FROM message_attachments other
                     WHERE other.workspace_id = so.workspace_id
                       AND other.stored_object_id = so.id
                       AND NOT (other.email_message_id = ANY($3)))
                AND NOT EXISTS (
                    SELECT 1 FROM import_jobs ij
                     WHERE ij.workspace_id = so.workspace_id
                       AND ij.stored_object_id = so.id)
              ORDER BY so.id FOR UPDATE OF so",
        )
        .bind(request.workspace_id)
        .bind(&locked_object_ids)
        .bind(&message_ids)
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)?
        .into_iter()
        .map(|row| {
            Ok((
                row.try_get("id").map_err(map_sqlx)?,
                row.try_get("storage_key").map_err(map_sqlx)?,
            ))
        })
        .collect::<Result<Vec<_>, AppError>>()?
    };
    let object_ids: Vec<Uuid> = object_rows.iter().map(|(id, _)| *id).collect();

    let attachments_removed = if message_ids.is_empty() {
        0
    } else {
        sqlx::query(
            "DELETE FROM message_attachments
              WHERE workspace_id = $1 AND email_message_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&message_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?
        .rows_affected()
    };

    if !object_ids.is_empty() {
        sqlx::query(
            "UPDATE stored_objects
                SET media_type = $3, byte_size = 0,
                    content_hash = decode(repeat('00', 32), 'hex'),
                    scan_status = 'rejected', created_by = NULL,
                    expires_at = COALESCE(expires_at, now()), deleted_at = now()
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&object_ids)
        .bind(PRIVACY_TOMBSTONE_MEDIA_TYPE)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    let notes_removed = sqlx::query(
        "DELETE FROM notes
          WHERE workspace_id = $1
            AND (contact_id = $2 OR thread_id = ANY($3))",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .bind(&thread_ids)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    let evidence_removed =
        sqlx::query("DELETE FROM research_evidence WHERE workspace_id = $1 AND contact_id = $2")
            .bind(request.workspace_id)
            .bind(request.contact_id)
            .execute(tx.conn())
            .await
            .map_err(map_sqlx)?
            .rows_affected();
    let tasks_removed = sqlx::query(
        "DELETE FROM tasks
          WHERE workspace_id = $1
            AND (contact_id = $2 OR thread_id = ANY($3))",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .bind(&thread_ids)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    let mut activities_removed = sqlx::query(
        "DELETE FROM activities
          WHERE workspace_id = $1
            AND (contact_id = $2 OR thread_id = ANY($3))",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .bind(&thread_ids)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    // A bulk CSV may contain unrelated contacts. Keep its bytes and other
    // import rows under the configured retention policy, but revoke the
    // application capability to fetch or reuse the source object.
    let source_objects_access_revoked = sqlx::query(
        "UPDATE stored_objects so
            SET deleted_at = COALESCE(so.deleted_at, now())
          WHERE so.workspace_id = $1 AND so.deleted_at IS NULL
            AND EXISTS (
                SELECT 1
                  FROM import_jobs ij
                  JOIN import_rows ir
                    ON ir.workspace_id = ij.workspace_id
                   AND ir.import_job_id = ij.id
                 WHERE ij.workspace_id = so.workspace_id
                   AND ij.stored_object_id = so.id
                   AND ir.contact_id = $2)",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    let imported_rows_removed =
        sqlx::query("DELETE FROM import_rows WHERE workspace_id = $1 AND contact_id = $2")
            .bind(request.workspace_id)
            .bind(request.contact_id)
            .execute(tx.conn())
            .await
            .map_err(map_sqlx)?
            .rows_affected();

    // Stop frozen or active campaign enrollments without altering their
    // recipient snapshots. The guarded maintenance role is the only caller
    // allowed to clear the contact link from a frozen enrollment.
    sqlx::query(
        "UPDATE campaign_enrollments
            SET contact_id = NULL, status = 'suppressed', current_step_id = NULL,
                next_send_at = NULL, stop_reason = 'privacy_erasure'
          WHERE workspace_id = $1
            AND (contact_id = $2 OR email_address_id = ANY($3))",
    )
    .bind(request.workspace_id)
    .bind(request.contact_id)
    .bind(&exclusive_email_ids)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;

    if !exclusive_email_ids.is_empty() {
        sqlx::query(
            "UPDATE communication_bases
                SET status = 'revoked', evidence_ref = 'privacy_erasure',
                    reviewed_by = NULL, reviewed_at = NULL, revoked_at = now(),
                    version = version + 1
              WHERE workspace_id = $1 AND email_address_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;

        sqlx::query(
            "DELETE FROM unsubscribe_tokens
              WHERE workspace_id = $1 AND email_address_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;

        sqlx::query(
            "UPDATE booking_links
                SET bound_email_address_id = NULL,
                    revoked_at = COALESCE(revoked_at, now())
              WHERE workspace_id = $1 AND bound_email_address_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;

        sqlx::query(
            "UPDATE bookings
                SET guest_name = '[erased]', guest_timezone = 'UTC',
                    guest_memo = NULL, outcome_note = NULL,
                    guest_email_verification_status = 'unverified',
                    privacy_notice_version = NULL, privacy_acknowledged_at = NULL,
                    provider_event_id = NULL, provider_revision = NULL,
                    confirmation_verified_at = NULL, version = version + 1
              WHERE workspace_id = $1 AND guest_email_address_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE booking_changes bc
                SET proposed_data = '{}'::jsonb,
                    status = CASE WHEN bc.status IN ('proposed','approved') THEN 'canceled' ELSE bc.status END,
                    version = bc.version + 1
               FROM bookings b
              WHERE bc.workspace_id = $1 AND bc.booking_id = b.id
                AND b.workspace_id = bc.workspace_id
                AND b.guest_email_address_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE booking_manage_tokens token
                SET revoked_at = COALESCE(token.revoked_at, now())
               FROM bookings b
              WHERE token.workspace_id = $1 AND token.booking_id = b.id
                AND b.workspace_id = token.workspace_id
                AND b.guest_email_address_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        activities_removed += sqlx::query(
            "DELETE FROM activities
              WHERE workspace_id = $1
                AND booking_id IN (
                    SELECT id FROM bookings
                     WHERE workspace_id = $1 AND guest_email_address_id = ANY($2))",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?
        .rows_affected();

        sqlx::query(
            "UPDATE email_addresses
                SET kind = 'person', verification_status = 'invalid',
                    verified_at = NULL, last_seen_at = NULL
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    if !thread_ids.is_empty() {
        sqlx::query(
            "UPDATE email_threads
                SET contact_id = NULL, provider_thread_id = NULL,
                    version = version + 1, updated_at = now()
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&thread_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    if !outbound_ids.is_empty() {
        sqlx::query(
            "DELETE FROM send_attempts
              WHERE workspace_id = $1 AND outbound_message_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&outbound_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE outbound_messages
                SET to_address_snapshot = '[erased]', subject = '[erased]',
                    body_text = '', body_html = NULL,
                    communication_basis_id = NULL,
                    payload_hash = decode(repeat('00', 32), 'hex'),
                    version = version + 1
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&outbound_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE delivery_events
                SET provider_event_key = 'privacy-erased:' || id::text,
                    details = '{}'::jsonb
              WHERE workspace_id = $1 AND outbound_message_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&outbound_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    if !message_ids.is_empty() {
        sqlx::query(
            "DELETE FROM message_participants
              WHERE workspace_id = $1 AND email_message_id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&message_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        sqlx::query(
            "UPDATE email_messages
                SET provider_message_id = NULL, internet_message_id = NULL,
                    in_reply_to = NULL, reference_ids = '{}', subject = NULL,
                    body_text = NULL, body_html = NULL, classification = 'unknown',
                    classification_reviewed_by = NULL
              WHERE workspace_id = $1 AND id = ANY($2)",
        )
        .bind(request.workspace_id)
        .bind(&message_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    if !thread_ids.is_empty() || !exclusive_email_ids.is_empty() {
        sqlx::query(
            "UPDATE message_drafts
                SET subject = '[erased]', body_text = '', body_html = NULL,
                    variables = '{}'::jsonb, status = 'discarded',
                    version = version + 1, updated_at = now()
              WHERE workspace_id = $1
                AND (thread_id = ANY($2) OR to_email_address_id = ANY($3))",
        )
        .bind(request.workspace_id)
        .bind(&thread_ids)
        .bind(&exclusive_email_ids)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }

    // Remove direct contact links last so all referenced rows can be handled
    // in the same transaction. Shared email-address records remain attached
    // to their other contacts.
    sqlx::query("DELETE FROM contact_emails WHERE workspace_id = $1 AND contact_id = $2")
        .bind(request.workspace_id)
        .bind(request.contact_id)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    sqlx::query("DELETE FROM contacts WHERE workspace_id = $1 AND id = $2")
        .bind(request.workspace_id)
        .bind(request.contact_id)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;

    let messages_scrubbed = (message_ids.len() as u64) + (outbound_ids.len() as u64);
    let metadata = json!({
        "email_addresses_suppressed": suppressed_count,
        "messages_scrubbed": messages_scrubbed,
        "marketing_exports_redacted": marketing_exports_redacted,
        "provider_cancel_not_requested": marketing_exports_redacted > 0,
        "attachments_removed": attachments_removed,
        "source_objects_access_revoked": source_objects_access_revoked,
        "notes_removed": notes_removed,
        "research_evidence_removed": evidence_removed,
        "tasks_removed": tasks_removed,
        "activities_removed": activities_removed,
        "import_rows_removed": imported_rows_removed,
    });
    audit::record(
        &mut tx,
        AuditEntry {
            actor_principal_id: request.actor_principal_id,
            requested_by: request.actor_principal_id,
            approved_by: None,
            job_id: None,
            action: RETENTION_ACTION.into(),
            target_type: "contact".into(),
            target_id: Some(request.contact_id),
            request_id: request.request_id.clone(),
            trace_id: request.trace_id.clone(),
            outcome: "ok".into(),
            metadata,
        },
    )
    .await?;
    tx.commit().await?;

    let pending = retry_object_ids(db, request.workspace_id, &object_ids, object_store).await?;
    let cache_invalidation_pending = cache
        .invalidate_contact(request.workspace_id, request.contact_id, &linked_email_ids)
        .await
        .is_err();
    if cache_invalidation_pending {
        tracing::warn!(contact_id = %request.contact_id, "privacy erasure cache invalidation is pending");
    }

    Ok(ErasureReport {
        contact_id: request.contact_id,
        email_addresses_suppressed: suppressed_count,
        messages_scrubbed,
        marketing_exports_redacted,
        provider_cancel_not_requested: marketing_exports_redacted > 0,
        attachments_removed,
        source_objects_access_revoked,
        object_cleanup_pending: pending,
        cache_invalidation_pending,
    })
}

/// Retry physical object deletion for every tombstone in this workspace.
/// The DB mapping is already gone, so these objects remain inaccessible if a
/// store outage prevents immediate byte deletion.
pub async fn retry_pending_object_deletions(
    db: &Db,
    workspace_id: Uuid,
    object_store: &dyn ObjectStore,
) -> Result<u64, AppError> {
    let mut tx = db.begin(workspace_id).await?;
    begin_retention_operation(&mut tx).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM stored_objects
          WHERE workspace_id = $1 AND deleted_at IS NOT NULL
            AND media_type = $2
          ORDER BY id",
    )
    .bind(workspace_id)
    .bind(PRIVACY_TOMBSTONE_MEDIA_TYPE)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    retry_object_ids(db, workspace_id, &ids, object_store).await
}

async fn retry_object_ids(
    db: &Db,
    workspace_id: Uuid,
    ids: &[Uuid],
    object_store: &dyn ObjectStore,
) -> Result<u64, AppError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let mut tx = db.begin(workspace_id).await?;
    begin_retention_operation(&mut tx).await?;
    let rows: Vec<(Uuid, String)> = sqlx::query(
        "SELECT id, storage_key FROM stored_objects
          WHERE workspace_id = $1 AND id = ANY($2) AND deleted_at IS NOT NULL
            AND media_type = $3
          ORDER BY id",
    )
    .bind(workspace_id)
    .bind(ids)
    .bind(PRIVACY_TOMBSTONE_MEDIA_TYPE)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)?
    .into_iter()
    .map(|row| {
        Ok((
            row.try_get("id").map_err(map_sqlx)?,
            row.try_get("storage_key").map_err(map_sqlx)?,
        ))
    })
    .collect::<Result<Vec<_>, AppError>>()?;
    tx.commit().await?;

    let mut deleted_ids = Vec::new();
    for (id, key) in rows {
        if object_store.delete(&key).await.is_ok() {
            deleted_ids.push(id);
        } else {
            tracing::warn!(object_id = %id, "privacy erasure object cleanup is pending");
        }
    }
    if !deleted_ids.is_empty() {
        let mut tx = db.begin(workspace_id).await?;
        begin_retention_operation(&mut tx).await?;
        sqlx::query(
            "DELETE FROM stored_objects so
              WHERE so.workspace_id = $1 AND so.id = ANY($2) AND so.deleted_at IS NOT NULL
                AND so.media_type = $3
                AND NOT EXISTS (
                    SELECT 1 FROM message_attachments ma
                     WHERE ma.workspace_id = so.workspace_id AND ma.stored_object_id = so.id)
                AND NOT EXISTS (
                    SELECT 1 FROM import_jobs ij
                     WHERE ij.workspace_id = so.workspace_id AND ij.stored_object_id = so.id)",
        )
        .bind(workspace_id)
        .bind(&deleted_ids)
        .bind(PRIVACY_TOMBSTONE_MEDIA_TYPE)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        tx.commit().await?;
    }

    let mut tx = db.begin(workspace_id).await?;
    begin_retention_operation(&mut tx).await?;
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM stored_objects
          WHERE workspace_id = $1 AND id = ANY($2) AND deleted_at IS NOT NULL
            AND media_type = $3",
    )
    .bind(workspace_id)
    .bind(ids)
    .bind(PRIVACY_TOMBSTONE_MEDIA_TYPE)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    tx.commit().await?;
    Ok(remaining as u64)
}

async fn begin_retention_operation(tx: &mut Tx<'_>) -> Result<(), AppError> {
    let current_user: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)?;
    if current_user != ROLE_RETENTION {
        return Err(AppError::forbidden(
            "privacy maintenance requires the retention_operator role",
        ));
    }
    sqlx::query("SELECT set_config('app.retention_operator', 'on', true)")
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    Ok(())
}
