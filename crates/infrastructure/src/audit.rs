//! Append-only audit logging (spec §4.8, table `audit_logs`).
//! `record` runs inside the caller's business transaction — the audit row and
//! the business row commit or roll back together.
//!
//! `details` must be allowlist-built: entity ids, normalized keys, outcome
//! labels, metric names — never message bodies, tokens, or secrets (B.1.3,
//! B.6.1).

use contracts::AppError;
use serde::Serialize;
use uuid::Uuid;

use crate::db::Tx;
use crate::error::map_sqlx;

/// `actor_principal_id` is the acting principal (authenticated principal,
/// agent as `acted_on_behalf_of`, or the designated system principal);
/// NULL only for pre-auth system actions such as a failed login.
/// `requested_by` = the initiating human; `approved_by` = approver when the
/// action flowed through approvals; `job_id` when performed by a worker.
#[derive(Debug)]
pub struct AuditEntry {
    pub actor_principal_id: Option<Uuid>,
    pub requested_by: Option<Uuid>,
    pub approved_by: Option<Uuid>,
    pub job_id: Option<Uuid>,
    /// Stable action key, e.g. "lead.accept" — never request-scoped text.
    pub action: String,
    /// e.g. "lead", "campaign", "approval".
    pub target_type: String,
    pub target_id: Option<Uuid>,
    /// "ok" | "denied:<error_code>" | "failed:<error_code>".
    pub outcome: String,
    /// Allowlisted, hash-referenced details. No bodies/tokens.
    pub metadata: serde_json::Value,
    pub request_id: String,
    pub trace_id: Option<String>,
}

impl AuditEntry {
    /// Convenience constructor for an actor-only entry.
    pub fn new(
        actor: Uuid,
        action: impl Into<String>,
        target_type: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            actor_principal_id: Some(actor),
            requested_by: None,
            approved_by: None,
            job_id: None,
            action: action.into(),
            target_type: target_type.into(),
            target_id: None,
            outcome: "ok".into(),
            metadata: serde_json::json!({}),
            request_id: request_id.into(),
            trace_id: None,
        }
    }

    pub fn denied(mut self, code: &contracts::ErrorCode) -> Self {
        self.outcome = format!("denied:{}", code.as_str());
        self
    }
}

pub async fn record(tx: &mut Tx<'_>, entry: AuditEntry) -> Result<(), AppError> {
    let mut details = entry.metadata;
    if !details.is_object() {
        details = serde_json::json!({});
    }
    details["outcome"] = serde_json::Value::String(entry.outcome);
    sqlx::query(
        "INSERT INTO audit_logs
            (workspace_id, actor_principal_id, requested_by, approved_by, job_id,
             action, target_type, target_id, request_id, trace_id, details)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(tx.workspace_id())
    .bind(entry.actor_principal_id)
    .bind(entry.requested_by)
    .bind(entry.approved_by)
    .bind(entry.job_id)
    .bind(&entry.action)
    .bind(&entry.target_type)
    .bind(entry.target_id)
    .bind(&entry.request_id)
    .bind(&entry.trace_id)
    .bind(&details)
    .execute(tx.conn())
    .await
    .map_err(map_sqlx)?;
    Ok(())
}

/// Helper: serialize an allowlisted metadata struct.
pub fn details<T: Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_else(|_| serde_json::json!({}))
}
