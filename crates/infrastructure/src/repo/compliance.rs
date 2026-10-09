//! compliance module repository (spec §6 — owned by the module worker).
//!
//! Owns: `communication_bases` CRUD (reviews/revoke under optimistic
//! locking per A.6.1), `suppressions`/`suppression_events` reads (writes go
//! through `compliance_core`, never reimplemented), and the
//! `compliance.release_suppression` job's read side.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

// =====================================================================
// communication_bases
// =====================================================================

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BasisRow {
    pub id: Uuid,
    pub email_address_id: Uuid,
    pub purpose: String,
    pub jurisdiction: String,
    pub basis_type: String,
    pub status: String,
    pub evidence_ref: String,
    pub collected_at: DateTime<Utc>,
    pub reviewed_by: Option<Uuid>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub valid_until: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    /// Joined `email_addresses.address_original` (display only).
    pub email_address: Option<String>,
}

const BASIS_COLS: &str = "b.id, b.email_address_id, b.purpose, b.jurisdiction,
    b.basis_type, b.status, b.evidence_ref, b.collected_at, b.reviewed_by,
    b.reviewed_at, b.valid_until, b.revoked_at, b.version, b.created_at,
    a.address_original AS email_address";

/// Insert a new basis. Status is always 'pending' on creation (B.2).
#[allow(clippy::too_many_arguments)]
pub async fn insert_basis(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose: &str,
    jurisdiction: &str,
    basis_type: &str,
    evidence_ref: &str,
    collected_at: DateTime<Utc>,
    valid_until: Option<DateTime<Utc>>,
) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "INSERT INTO communication_bases
            (workspace_id, email_address_id, purpose, jurisdiction, basis_type,
             evidence_ref, collected_at, valid_until)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose)
    .bind(jurisdiction)
    .bind(basis_type)
    .bind(evidence_ref)
    .bind(collected_at)
    .bind(valid_until)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)
}

pub async fn get_basis(tx: &mut Tx<'_>, id: Uuid) -> Result<Option<BasisRow>, AppError> {
    sqlx::query_as::<_, BasisRow>(&format!(
        "SELECT {BASIS_COLS} FROM communication_bases b
          LEFT JOIN email_addresses a
            ON a.workspace_id = b.workspace_id AND a.id = b.email_address_id
         WHERE b.workspace_id = $1 AND b.id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// List filters — every field is also bound into the cursor's filter hash.
#[derive(Debug, Clone, Default)]
pub struct BasisFilter {
    pub email_address_id: Option<Uuid>,
    pub purpose: Option<String>,
    pub status: Option<String>,
    pub jurisdiction: Option<String>,
    /// Keyset cursor: rows strictly older than (created_at, id).
    pub after: Option<(DateTime<Utc>, Uuid)>,
}

/// Cursor-paginated list (`created_at DESC, id DESC`, B.1.5). The caller
/// fetches `limit + 1` rows to detect `has_more`.
pub async fn list_bases(
    tx: &mut Tx<'_>,
    filter: &BasisFilter,
    limit: i64,
) -> Result<Vec<BasisRow>, AppError> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(&format!(
        "SELECT {BASIS_COLS} FROM communication_bases b
          LEFT JOIN email_addresses a
            ON a.workspace_id = b.workspace_id AND a.id = b.email_address_id
         WHERE b.workspace_id = "
    ));
    qb.push_bind(tx.workspace_id());
    if let Some(v) = filter.email_address_id {
        qb.push(" AND b.email_address_id = ").push_bind(v);
    }
    if let Some(v) = &filter.purpose {
        qb.push(" AND b.purpose = ").push_bind(v);
    }
    if let Some(v) = &filter.status {
        qb.push(" AND b.status = ").push_bind(v);
    }
    if let Some(v) = &filter.jurisdiction {
        qb.push(" AND b.jurisdiction = ").push_bind(v);
    }
    if let Some((ts, id)) = filter.after {
        qb.push(" AND (b.created_at, b.id) < (")
            .push_bind(ts)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    qb.push(" ORDER BY b.created_at DESC, b.id DESC LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<BasisRow>()
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

/// Human review — optimistic locking on `version`, and only a `pending`
/// basis may be decided (a decided basis is final; use revoke instead).
/// Returns `Ok(Some)` the updated row; `Ok(None)` when the optimistic
/// precondition failed (caller distinguishes 404 vs 412 vs 409).
pub async fn apply_basis_review(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
    decision: &str,
    reviewed_by: Uuid,
    valid_until: Option<DateTime<Utc>>,
) -> Result<Option<BasisRow>, AppError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE communication_bases
            SET status = $4, reviewed_by = $5, reviewed_at = now(),
                valid_until = $6, version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $3
            AND status = 'pending'
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .bind(decision)
    .bind(reviewed_by)
    .bind(valid_until)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    match updated {
        Some(_) => get_basis(tx, id).await,
        None => Ok(None),
    }
}

/// Revoke a basis — optimistic locking; revoking an already-revoked row is
/// reported as `Ok(None)` for the caller's 404/412/409 split.
pub async fn revoke_basis(
    tx: &mut Tx<'_>,
    id: Uuid,
    expected_version: i64,
) -> Result<Option<BasisRow>, AppError> {
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE communication_bases
            SET status = 'revoked', revoked_at = now(), version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND version = $3
            AND status <> 'revoked'
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(id)
    .bind(expected_version)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    match updated {
        Some(_) => get_basis(tx, id).await,
        None => Ok(None),
    }
}

// =====================================================================
// suppressions (reads; writes stay in compliance_core)
// =====================================================================

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SuppressionRow {
    pub id: Uuid,
    pub email_address_id: Uuid,
    pub purpose_scope: String,
    pub reason: String,
    pub source_outbound_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub released_at: Option<DateTime<Utc>>,
    pub released_by: Option<Uuid>,
    pub release_evidence_ref: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    /// Joined `email_addresses.address_original` (display only).
    pub email_address: Option<String>,
}

const SUPPRESSION_COLS: &str = "s.id, s.email_address_id, s.purpose_scope,
    s.reason, s.source_outbound_id, s.created_by, s.released_at,
    s.released_by, s.release_evidence_ref, s.version, s.created_at,
    a.address_original AS email_address";

pub async fn get_suppression(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<SuppressionRow>, AppError> {
    sqlx::query_as::<_, SuppressionRow>(&format!(
        "SELECT {SUPPRESSION_COLS} FROM suppressions s
          LEFT JOIN email_addresses a
            ON a.workspace_id = s.workspace_id AND a.id = s.email_address_id
         WHERE s.workspace_id = $1 AND s.id = $2"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Lock a suppression row after its email address has been locked. This is
/// used to serialize competing release-request submissions in A.6.4 order.
pub async fn get_suppression_for_update(
    tx: &mut Tx<'_>,
    id: Uuid,
) -> Result<Option<SuppressionRow>, AppError> {
    sqlx::query_as::<_, SuppressionRow>(&format!(
        "SELECT {SUPPRESSION_COLS} FROM suppressions s
          LEFT JOIN email_addresses a
            ON a.workspace_id = s.workspace_id AND a.id = s.email_address_id
         WHERE s.workspace_id = $1 AND s.id = $2
         FOR UPDATE OF s"
    ))
    .bind(tx.workspace_id())
    .bind(id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// List filters for `GET /suppressions` (B.2 "제한된 검색" — allowlisted
/// fields only). `released`: None → all, Some(false) → active only,
/// Some(true) → released only.
#[derive(Debug, Clone, Default)]
pub struct SuppressionFilter {
    pub email_address_id: Option<Uuid>,
    pub purpose_scope: Option<String>,
    pub reason: Option<String>,
    pub released: Option<bool>,
    /// Keyset cursor: rows strictly older than (created_at, id).
    pub after: Option<(DateTime<Utc>, Uuid)>,
}

pub async fn list_suppressions(
    tx: &mut Tx<'_>,
    filter: &SuppressionFilter,
    limit: i64,
) -> Result<Vec<SuppressionRow>, AppError> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(&format!(
        "SELECT {SUPPRESSION_COLS} FROM suppressions s
          LEFT JOIN email_addresses a
            ON a.workspace_id = s.workspace_id AND a.id = s.email_address_id
         WHERE s.workspace_id = "
    ));
    qb.push_bind(tx.workspace_id());
    if let Some(v) = filter.email_address_id {
        qb.push(" AND s.email_address_id = ").push_bind(v);
    }
    if let Some(v) = &filter.purpose_scope {
        qb.push(" AND s.purpose_scope = ").push_bind(v);
    }
    if let Some(v) = &filter.reason {
        qb.push(" AND s.reason = ").push_bind(v);
    }
    if let Some(released) = filter.released {
        if released {
            qb.push(" AND s.released_at IS NOT NULL");
        } else {
            qb.push(" AND s.released_at IS NULL");
        }
    }
    if let Some((ts, id)) = filter.after {
        qb.push(" AND (s.created_at, s.id) < (")
            .push_bind(ts)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    qb.push(" ORDER BY s.created_at DESC, s.id DESC LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<SuppressionRow>()
        .fetch_all(tx.conn())
        .await
        .map_err(map_sqlx)
}

/// Latest `suppression_events.action` for a suppression — lets callers
/// report "created" vs "reaffirmed" after `compliance_core::insert_suppression`.
pub async fn last_suppression_action(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
) -> Result<Option<String>, AppError> {
    sqlx::query_scalar(
        "SELECT action FROM suppression_events
          WHERE workspace_id = $1 AND suppression_id = $2
          ORDER BY occurred_at DESC, created_at DESC, id DESC
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Verify an outbound message belongs to this email address before callers
/// attach it as `source_outbound_id` (the composite FK also enforces it).
pub async fn outbound_belongs_to_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    outbound_message_id: Uuid,
) -> Result<bool, AppError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbound_messages
          WHERE workspace_id = $1 AND id = $2 AND to_email_address_id = $3",
    )
    .bind(tx.workspace_id())
    .bind(outbound_message_id)
    .bind(email_address_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(n == 1)
}

// =====================================================================
// TODO(rebase) workaround — jobs::enqueue is not yet idempotent
// =====================================================================

/// `compliance_core::insert_suppression` always enqueues a
/// `compliance.pause_enrollments` job with dedupe key
/// `pause_enrollments:{email_address_id}`. The framework fix landing on
/// feat/foundation makes `jobs::enqueue` idempotent on (kind, dedupe_key)
/// and the pause key per-suppression; until then a second suppression for
/// the same address dies on `UNIQUE (workspace_id, kind, dedupe_key)`.
///
/// TODO(rebase): drop this helper (and its call sites) after the rebase.
/// Freeing the canonical key keeps the post-fix semantics — every
/// suppression gets its own pause job — while the superseded job row stays
/// untouched (pausing is idempotent).
pub async fn free_pause_enrollments_key(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<u64, AppError> {
    let n = sqlx::query(
        "UPDATE jobs
            SET dedupe_key = dedupe_key || ':superseded:' || id::text
          WHERE workspace_id = $1
            AND kind = 'compliance.pause_enrollments'
            AND dedupe_key = $2",
    )
    .bind(tx.workspace_id())
    .bind(format!("pause_enrollments:{email_address_id}"))
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n)
}

// =====================================================================
// release requests / release job helpers
// =====================================================================

/// An already-pending suppression-release approval request for this
/// suppression, if any (dedupe of POST release-requests).
pub async fn pending_release_request(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM approval_requests
          WHERE workspace_id = $1 AND suppression_id = $2
            AND action_type = 'suppression_release' AND status = 'pending'
          ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// What the `compliance.release_suppression` job needs to re-verify inside
/// its own transaction: the request's status/target plus the deciding
/// human and their recorded reason.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReleaseContext {
    pub status: String,
    pub action_type: String,
    pub suppression_id: Option<Uuid>,
    pub requested_by: Uuid,
    pub decided_by: Option<Uuid>,
    pub decision_reason: Option<String>,
}

pub async fn release_context(
    tx: &mut Tx<'_>,
    approval_request_id: Uuid,
) -> Result<Option<ReleaseContext>, AppError> {
    sqlx::query_as::<_, ReleaseContext>(
        "SELECT r.status, r.action_type, r.suppression_id, r.requested_by,
                d.decided_by, d.reason AS decision_reason
           FROM approval_requests r
           LEFT JOIN approval_decisions d
             ON d.workspace_id = r.workspace_id AND d.approval_request_id = r.id
          WHERE r.workspace_id = $1 AND r.id = $2",
    )
    .bind(tx.workspace_id())
    .bind(approval_request_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// The workspace's system principal — audit `actor` for worker-side
/// compliance actions (A.7).
pub async fn system_principal_id(tx: &mut Tx<'_>) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM principals
          WHERE workspace_id = $1 AND kind = 'system'
          ORDER BY created_at, id LIMIT 1",
    )
    .bind(tx.workspace_id())
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// §5.3 limited-add scope check: is `email_address_id` visible to
/// `viewer_principal_id` through the allowed paths? `via_contact` — the
/// address is linked to a contact (shared CRM read for humans; callers
/// gate agents on the `crm.read` scope). `via_mailbox` — the address is a
/// participant on a message, or the linked contact of a thread, in a
/// mailbox where the viewer holds `can_read` (callers gate agents on
/// `mail.read`). Runs inside the caller's tx; RLS scopes the rows.
#[doc(hidden)]
pub async fn address_visible_to(
    tx: &mut Tx<'_>,
    viewer_principal_id: Uuid,
    email_address_id: Uuid,
    via_contact: bool,
    via_mailbox: bool,
) -> Result<bool, AppError> {
    let visible = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (
             SELECT 1 FROM contact_emails ce
             WHERE $4 AND ce.email_address_id = $2
         ) OR EXISTS (
             SELECT 1
             FROM mail_account_grants g
             WHERE $5
               AND g.principal_id = $1
               AND g.can_read
               AND (
                   EXISTS (
                       SELECT 1
                       FROM message_participants mp
                       JOIN email_messages em
                         ON em.id = mp.email_message_id
                        AND em.workspace_id = mp.workspace_id
                       WHERE mp.workspace_id = $3
                         AND mp.email_address_id = $2
                         AND em.mail_account_id = g.mail_account_id
                   )
                   OR EXISTS (
                       SELECT 1
                       FROM email_threads t
                       JOIN contact_emails ce
                         ON ce.contact_id = t.contact_id
                        AND ce.workspace_id = t.workspace_id
                       WHERE t.workspace_id = $3
                         AND ce.email_address_id = $2
                         AND t.mail_account_id = g.mail_account_id
                   )
               )
         )",
    )
    .bind(viewer_principal_id)
    .bind(email_address_id)
    .bind(tx.workspace_id())
    .bind(via_contact)
    .bind(via_mailbox)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(visible)
}

// =====================================================================
// Cross-crate test fixtures (test support only).
// =====================================================================

#[doc(hidden)]
/// Fixtures shared by this module's tests across crates — the api crate's
/// tests cannot depend on `sqlx` (crates' Cargo.toml are shared, so the
/// raw INSERTs live here, same pattern as `crate::testing`). Not public
/// API; call sites are `crates/*/tests` only.
pub mod test_fixtures {
    use chrono::{DateTime, Utc};
    use contracts::AppError;
    use uuid::Uuid;

    use crate::db::Tx;
    use crate::error::map_sqlx;

    /// Minimal `email_addresses` row (`normalized_key` = the address).
    pub async fn insert_email_address(tx: &mut Tx<'_>, address: &str) -> Result<Uuid, AppError> {
        sqlx::query_scalar(
            "INSERT INTO email_addresses (workspace_id, address_original, normalized_key)
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(tx.workspace_id())
        .bind(address)
        .bind(address)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)
    }

    /// Minimal `contacts` row — enough to satisfy `contact_emails` FK.
    pub async fn insert_contact(tx: &mut Tx<'_>, name: &str) -> Result<Uuid, AppError> {
        sqlx::query_scalar("INSERT INTO contacts (workspace_id, name) VALUES ($1, $2) RETURNING id")
            .bind(tx.workspace_id())
            .bind(name)
            .fetch_one(tx.conn())
            .await
            .map_err(map_sqlx)
    }

    /// Link `email_address_id` to `contact_id` (makes the address
    /// CRM-visible for the §5.3 limited-add scope check).
    pub async fn link_contact_email(
        tx: &mut Tx<'_>,
        contact_id: Uuid,
        email_address_id: Uuid,
    ) -> Result<(), AppError> {
        sqlx::query(
            "INSERT INTO contact_emails (workspace_id, contact_id, email_address_id)
             VALUES ($1, $2, $3)",
        )
        .bind(tx.workspace_id())
        .bind(contact_id)
        .bind(email_address_id)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
        Ok(())
    }

    /// The minimum outbound chain an `unsubscribe_tokens` row references:
    /// sender_identity + delivery_channel + outbound_message to `to`.
    pub async fn insert_outbound(
        tx: &mut Tx<'_>,
        mail_account_id: Uuid,
        connection_id: Uuid,
        requested_by: Uuid,
        to_email_address_id: Uuid,
    ) -> Result<Uuid, AppError> {
        let ws = tx.workspace_id();
        let ident: Uuid = sqlx::query_scalar(
            "INSERT INTO sender_identities (workspace_id, mail_account_id, address, display_name)
             VALUES ($1, $2, 'shared@test.dev', 'Shared') RETURNING id",
        )
        .bind(ws)
        .bind(mail_account_id)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)?;
        let channel: Uuid = sqlx::query_scalar(
            "INSERT INTO delivery_channels
                (workspace_id, mail_account_id, sender_identity_id, connection_id, provider, purpose, name)
             VALUES ($1, $2, $3, $4, 'zoho_mail', 'marketing', 'Marketing') RETURNING id",
        )
        .bind(ws)
        .bind(mail_account_id)
        .bind(ident)
        .bind(connection_id)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)?;
        sqlx::query_scalar(
            "INSERT INTO outbound_messages
                (workspace_id, mail_account_id, delivery_channel_id, sender_identity_id,
                 to_email_address_id, requested_by, purpose,
                 from_address_snapshot, to_address_snapshot, subject, body_text, payload_hash)
             VALUES ($1, $2, $3, $4, $5, $6, 'marketing',
                     'shared@test.dev', 'rcpt@example.com', 'hi', 'body',
                     decode(md5(random()::text), 'hex'))
             RETURNING id",
        )
        .bind(ws)
        .bind(mail_account_id)
        .bind(channel)
        .bind(ident)
        .bind(to_email_address_id)
        .bind(requested_by)
        .fetch_one(tx.conn())
        .await
        .map_err(map_sqlx)
    }

    /// `unsubscribe_tokens.used_at` for a raw token — `Ok(None)` when the
    /// token hash is unknown, `Ok(Some(None))` issued-but-unused.
    pub async fn token_used_at(
        tx: &mut Tx<'_>,
        raw_token: &str,
    ) -> Result<Option<Option<DateTime<Utc>>>, AppError> {
        let hash = crate::crypto::sha256_bytes(raw_token.as_bytes());
        sqlx::query_scalar("SELECT used_at FROM unsubscribe_tokens WHERE token_hash = $1")
            .bind(&hash[..])
            .fetch_optional(tx.conn())
            .await
            .map_err(map_sqlx)
    }

    /// Count of `suppressions` rows in the tx's workspace.
    pub async fn suppression_count(tx: &mut Tx<'_>) -> Result<i64, AppError> {
        sqlx::query_scalar("SELECT count(*) FROM suppressions")
            .fetch_one(tx.conn())
            .await
            .map_err(map_sqlx)
    }
}
