//! Shared compliance core (spec §4.8, A.6.4) — the single lock-order rule:
//! `email_addresses` row is locked `FOR UPDATE` first, before any
//! suppression or send-permit decision, for a given (workspace, address).
//! All functions run inside the caller's business tx so the compliance
//! decision commits atomically with the business change.

use chrono::{DateTime, Utc};
use contracts::AppError;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;
use crate::outbox;

/// Row shape returned by `active_suppressions`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Suppression {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email_address_id: Uuid,
    /// 'marketing' | 'all'
    pub purpose_scope: String,
    /// 'unsubscribe' | 'complaint' | 'hard_bounce' | 'manual'
    pub reason: String,
    pub source_outbound_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Valid communication basis row (`communication_bases.status='valid'`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Basis {
    pub id: Uuid,
    pub email_address_id: Uuid,
    /// 'marketing' | 'correspondence' | 'transactional'
    pub purpose: String,
    pub jurisdiction: String,
    /// 'explicit_consent' | 'requested_reply' | 'requested_transaction' |
    /// 'reviewed_exception'
    pub basis_type: String,
    pub evidence_ref: String,
    pub valid_until: Option<DateTime<Utc>>,
}

/// A one-time raw token; only its sha256 hash is stored (A.4
/// `unsubscribe_tokens`, B.1.3).
#[derive(Debug, Clone)]
pub struct RawToken {
    /// The plaintext to embed in the link — never persisted.
    pub token: String,
    pub token_id: Uuid,
}

/// The single lock-order rule of A.6.4: lock the `email_addresses` row
/// `FOR UPDATE` before any suppression/send-permit decision. Returns false
/// when the address doesn't exist in this workspace.
pub async fn lock_email_address(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<bool, AppError> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM email_addresses
          WHERE workspace_id = $1 AND id = $2
          FOR UPDATE",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(found.is_some())
}

/// Active (unreleased) suppressions that block `purpose`. A `purpose_scope`
/// of 'all' blocks every purpose; 'marketing' blocks marketing sends.
/// Call after `lock_email_address`.
pub async fn active_suppressions(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose: &str,
) -> Result<Vec<Suppression>, AppError> {
    sqlx::query_as::<_, Suppression>(
        "SELECT id, workspace_id, email_address_id, purpose_scope, reason,
                source_outbound_id, created_by, created_at
           FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2
            AND released_at IS NULL
            AND (purpose_scope = 'all' OR purpose_scope = $3)
          ORDER BY created_at, id",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose)
    .fetch_all(tx.conn())
    .await
    .map_err(map_sqlx)
}

/// Insert a suppression row + its append-only `suppression_events` record +
/// `suppression.created` outbox event + a job to pause active enrollments —
/// all in the caller's tx (A.6.4: the suppression commits durably first;
/// bulk enrollment updates happen in the job).
///
/// `purpose_scope`: 'marketing'|'all'. `reason`: 'unsubscribe'|'complaint'|
/// 'hard_bounce'|'manual'. `source` populates `suppression_events.source_kind`
/// ('unsubscribe_link'|'webhook'|'manual'|'bounce'|…). `actor` is the acting
/// principal id, if any. Returns the suppression id; on a per-reason
/// duplicate the existing row id is returned after a `reaffirmed` event.
pub async fn insert_suppression(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose_scope: &str,
    reason: &str,
    source: &str,
    actor: Option<Uuid>,
    source_outbound_id: Option<Uuid>,
) -> Result<Uuid, AppError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM suppressions
          WHERE workspace_id = $1 AND email_address_id = $2
            AND purpose_scope = $3 AND reason = $4 AND released_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose_scope)
    .bind(reason)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;

    let (suppression_id, action) = match existing {
        Some(id) => (id, "reaffirmed"),
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO suppressions
                    (workspace_id, email_address_id, purpose_scope, reason, source_outbound_id, created_by)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 RETURNING id",
            )
            .bind(tx.workspace_id())
            .bind(email_address_id)
            .bind(purpose_scope)
            .bind(reason)
            .bind(source_outbound_id)
            .bind(actor)
            .fetch_one(tx.conn())
            .await
            .map_err(map_sqlx)?;
            (id, "created")
        }
    };

    sqlx::query(
        "INSERT INTO suppression_events
            (workspace_id, suppression_id, action, actor_principal_id, source_kind, evidence_ref)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .bind(action)
    .bind(actor)
    .bind(source)
    .bind(source_outbound_id.map(|id| id.to_string()))
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;

    outbox::emit(
        tx,
        outbox::event_types::SUPPRESSION_CREATED,
        "suppression",
        suppression_id,
        1,
        serde_json::json!({
            "email_address_id": email_address_id,
            "purpose_scope": purpose_scope,
            "reason": reason,
        }),
    )
    .await?;

    // A.6.4: pause active enrollments asynchronously — never block the
    // unsubscribe response on bulk updates.
    crate::jobs::enqueue(
        tx,
        "compliance.pause_enrollments",
        1,
        serde_json::json!({
            "email_address_id": email_address_id,
            "suppression_id": suppression_id,
        }),
        Utc::now(),
        Some(&format!("pause_enrollments:{email_address_id}")),
    )
    .await?;

    Ok(suppression_id)
}

/// Release a suppression (human-authorized only — callers enforce authz).
/// The DDL CHECK requires `released_by` + `release_evidence_ref`. Emits a
/// `released` event row.
pub async fn release_suppression(
    tx: &mut Tx<'_>,
    suppression_id: Uuid,
    released_by: Uuid,
    release_evidence_ref: &str,
) -> Result<bool, AppError> {
    let n = sqlx::query(
        "UPDATE suppressions
            SET released_at = now(), released_by = $3, release_evidence_ref = $4,
                version = version + 1
          WHERE workspace_id = $1 AND id = $2 AND released_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(suppression_id)
    .bind(released_by)
    .bind(release_evidence_ref)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    if n == 1 {
        sqlx::query(
            "INSERT INTO suppression_events
                (workspace_id, suppression_id, action, actor_principal_id, source_kind, evidence_ref)
             VALUES ($1, $2, 'released', $3, 'release_request', $4)",
        )
        .bind(tx.workspace_id())
        .bind(suppression_id)
        .bind(released_by)
        .bind(release_evidence_ref)
        .execute(tx.conn())
        .await
        .map_err(map_sqlx)?;
    }
    Ok(n == 1)
}

/// Issue a 256-bit one-time unsubscribe token; only the sha256 hash is
/// stored (purpose_scope is 'marketing' per DDL CHECK).
pub async fn issue_unsubscribe_token(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    outbound_message_id: Uuid,
) -> Result<RawToken, AppError> {
    let token = crate::crypto::generate_token();
    let hash = crate::crypto::sha256_bytes(token.as_bytes());
    let token_id: Uuid = sqlx::query_scalar(
        "INSERT INTO unsubscribe_tokens
            (workspace_id, token_hash, email_address_id, outbound_message_id)
         VALUES ($1, $2, $3, $4)
         RETURNING id",
    )
    .bind(tx.workspace_id())
    .bind(&hash[..])
    .bind(email_address_id)
    .bind(outbound_message_id)
    .fetch_one(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(RawToken { token, token_id })
}

/// Resolve a token to (email_address_id, outbound_message_id) if unused and
/// not revoked — used by the public unsubscribe endpoints.
pub async fn resolve_unsubscribe_token(
    tx: &mut Tx<'_>,
    token: &str,
) -> Result<Option<(Uuid, Uuid)>, AppError> {
    let hash = crate::crypto::sha256_bytes(token.as_bytes());
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT email_address_id, outbound_message_id FROM unsubscribe_tokens
          WHERE workspace_id = $1 AND token_hash = $2
            AND used_at IS NULL AND revoked_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(&hash[..])
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(row)
}

/// Mark a token used (one-time semantics).
pub async fn consume_unsubscribe_token(tx: &mut Tx<'_>, token: &str) -> Result<bool, AppError> {
    let hash = crate::crypto::sha256_bytes(token.as_bytes());
    let n = sqlx::query(
        "UPDATE unsubscribe_tokens SET used_at = now()
          WHERE workspace_id = $1 AND token_hash = $2
            AND used_at IS NULL AND revoked_at IS NULL",
    )
    .bind(tx.workspace_id())
    .bind(&hash[..])
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?
    .rows_affected();
    Ok(n == 1)
}

/// Valid (unexpired, unrevoked, status='valid') communication basis for
/// `purpose` under `jurisdiction`. `purpose` ∈
/// {marketing, correspondence, transactional}; `jurisdiction` is the
/// policy's jurisdiction key (e.g. 'KR', 'US', 'default').
pub async fn valid_communication_basis(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
    purpose: &str,
    jurisdiction: &str,
) -> Result<Option<Basis>, AppError> {
    sqlx::query_as::<_, Basis>(
        "SELECT id, email_address_id, purpose, jurisdiction, basis_type,
                evidence_ref, valid_until
           FROM communication_bases
          WHERE workspace_id = $1 AND email_address_id = $2
            AND purpose = $3 AND jurisdiction = $4
            AND status = 'valid'
            AND (valid_until IS NULL OR valid_until > now())
          ORDER BY created_at DESC
          LIMIT 1",
    )
    .bind(tx.workspace_id())
    .bind(email_address_id)
    .bind(purpose)
    .bind(jurisdiction)
    .fetch_optional(tx.conn())
    .await
    .map_err(map_sqlx)
}
