//! Delivery channels + policy reviews (B.2, §4.9, T33).
//!
//! Channels start `unverified`/disabled. Enabling requires (a) an
//! `allowed` review that has not expired and (b) provider/purpose policy —
//! `zoho_mail` can never drive `marketing`/`transactional` (T33,
//! `PROVIDER_POLICY_BLOCKED`); missing/expired evidence is
//! `PROVIDER_POLICY_UNVERIFIED`. Reviews are append-only and move
//! `policy_status`; a non-`allowed` verdict force-disables the channel.

use chrono::{DateTime, Utc};
use contracts::AppError;
use contracts::Ctx;
use contracts::ErrorCode;
use contracts::Role;
use contracts::Violation;
use contracts::mailboxes::{
    ChannelReviewResponse, CreateChannelReviewRequest, CreateDeliveryChannelRequest,
    DeliveryChannelResponse, PatchDeliveryChannelRequest,
};
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::repo::mailboxes as repo;
use uuid::Uuid;

fn response(row: &repo::DeliveryChannelRow) -> DeliveryChannelResponse {
    DeliveryChannelResponse {
        id: row.id,
        mail_account_id: row.mail_account_id,
        sender_identity_id: row.sender_identity_id,
        connection_id: row.connection_id,
        provider: row.provider.clone(),
        purpose: row.purpose.clone(),
        name: row.name.clone(),
        policy_status: row.policy_status.clone(),
        enabled: row.enabled,
        capabilities: row.capabilities.clone(),
        internal_daily_cap: row.internal_daily_cap,
        version: row.version,
        created_at: row.created_at,
    }
}

fn review_response(row: &repo::ChannelReviewRow) -> ChannelReviewResponse {
    ChannelReviewResponse {
        id: row.id,
        delivery_channel_id: row.delivery_channel_id,
        reviewed_by: row.reviewed_by,
        result: row.result.clone(),
        purpose: row.purpose.clone(),
        policy_url: row.policy_url.clone(),
        contract_evidence_ref: row.contract_evidence_ref.clone(),
        notes: row.notes.clone(),
        reviewed_at: row.reviewed_at,
        expires_at: row.expires_at,
    }
}

pub struct ChannelPage {
    pub items: Vec<DeliveryChannelResponse>,
    pub has_more: bool,
    pub cursor_seed: Option<(DateTime<Utc>, Uuid)>,
}

/// `GET /delivery-channels` — mailbox ACL applies (§5.6): non-admins see
/// only channels on accounts they can read; Admin sees all (metadata).
pub async fn list_delivery_channels(
    db: &Db,
    ctx: &Ctx,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<ChannelPage, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let restricted =
        (!(ctx.role == Role::Admin || crate::authz::agent_admin(ctx))).then_some(ctx.principal_id);
    let rows = repo::list_delivery_channels(&mut tx, restricted, after, limit + 1).await?;
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<repo::DeliveryChannelRow> = rows.into_iter().take(limit as usize).collect();
    let cursor_seed = has_more
        .then(|| rows.last().map(|r| (r.created_at, r.id)))
        .flatten();
    Ok(ChannelPage {
        items: rows.iter().map(response).collect(),
        has_more,
        cursor_seed,
    })
}

/// `POST /delivery-channels` (Admin). Provider derives from the referenced
/// `integration_connections` row — the request never names a provider.
pub async fn create_delivery_channel(
    db: &Db,
    ctx: &Ctx,
    req: &CreateDeliveryChannelRequest,
) -> Result<DeliveryChannelResponse, AppError> {
    super::require_admin_audited(db, ctx, "delivery_channel.create", "delivery_channel", None)
        .await?;
    if req.name.trim().is_empty() {
        return Err(AppError::validation(
            "name is required",
            vec![Violation {
                field: "name".into(),
                code: "required".into(),
            }],
        ));
    }
    if req.internal_daily_cap.is_some_and(|c| c <= 0) {
        return Err(AppError::validation(
            "internal_daily_cap must be positive",
            vec![Violation {
                field: "internal_daily_cap".into(),
                code: "invalid".into(),
            }],
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    let conn = repo::get_connection(&mut tx, req.connection_id)
        .await?
        .ok_or_else(|| AppError::not_found("connection not found"))?;
    if conn.status != "active" {
        let _ = tx.rollback().await;
        return Err(AppError::conflict("connection is not active"));
    }
    if !matches!(
        conn.provider.as_str(),
        "zoho_mail" | "delivery_provider" | "transactional_provider"
    ) {
        let _ = tx.rollback().await;
        return Err(AppError::validation(
            "connection provider cannot back a delivery channel",
            vec![Violation {
                field: "connection_id".into(),
                code: "invalid".into(),
            }],
        ));
    }
    let (account, _g) = repo::get_mail_account(&mut tx, ctx.principal_id, req.mail_account_id)
        .await?
        .ok_or_else(|| AppError::not_found("mail account not found"))?;
    let sender = repo::get_sender_identity(&mut tx, req.sender_identity_id)
        .await?
        .ok_or_else(|| AppError::not_found("sender identity not found"))?;
    if sender.0 != account.id {
        let _ = tx.rollback().await;
        return Err(AppError::validation(
            "sender identity does not belong to the mail account",
            vec![Violation {
                field: "sender_identity_id".into(),
                code: "invalid".into(),
            }],
        ));
    }
    if sender.1 == "disabled" {
        let _ = tx.rollback().await;
        return Err(AppError::conflict("sender identity is disabled"));
    }

    let row = repo::insert_delivery_channel(
        &mut tx,
        account.id,
        req.sender_identity_id,
        conn.id,
        &conn.provider,
        req.purpose.as_str(),
        req.name.trim(),
        &req.capabilities
            .clone()
            .unwrap_or_else(|| serde_json::json!({})),
        req.internal_daily_cap,
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "delivery_channel.create",
            "delivery_channel",
            ctx.request_id.clone(),
        )
        .with_target(row.id),
    )
    .await?;
    tx.commit().await?;
    Ok(response(&row))
}

/// `POST /delivery-channels/{id}/reviews` (Admin/policy review) — append
/// only; the verdict moves `policy_status` and a non-`allowed` result
/// force-disables the channel (DDL CHECK).
pub async fn review_delivery_channel(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    req: &CreateChannelReviewRequest,
) -> Result<ChannelReviewResponse, AppError> {
    super::require_admin_audited(
        db,
        ctx,
        "delivery_channel.review",
        "delivery_channel",
        Some(id),
    )
    .await?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(channel) = repo::get_delivery_channel(&mut tx, id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("delivery channel not found"));
    };
    let row = repo::insert_channel_review(
        &mut tx,
        id,
        ctx.principal_id,
        req.result.as_str(),
        &channel.purpose,
        req.policy_url.as_deref(),
        req.contract_evidence_ref.as_deref(),
        req.notes.as_deref(),
        req.expires_at,
    )
    .await?;
    repo::apply_review_result(&mut tx, id, req.result.as_str()).await?;
    audit::record(
        &mut tx,
        AuditEntry {
            metadata: serde_json::json!({"result": req.result.as_str()}),
            ..AuditEntry::new(
                ctx.principal_id,
                "delivery_channel.review",
                "delivery_channel",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await?;
    tx.commit().await?;
    Ok(review_response(&row))
}

/// `PATCH /delivery-channels/{id}` (Admin, If-Match) — the only mutable
/// fields are `enabled` and `internal_daily_cap` (B.2 "검증된 범위").
/// Activation gates: policy_status must be `allowed` by an unexpired
/// review and provider/purpose policy must hold (T33).
pub async fn patch_delivery_channel(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &PatchDeliveryChannelRequest,
) -> Result<DeliveryChannelResponse, AppError> {
    super::require_admin_audited(
        db,
        ctx,
        "delivery_channel.update",
        "delivery_channel",
        Some(id),
    )
    .await?;
    if req.enabled.is_none() && req.internal_daily_cap.is_none() {
        return Err(AppError::validation("nothing to update", vec![]));
    }
    if req.internal_daily_cap.is_some_and(|c| c <= 0) {
        return Err(AppError::validation(
            "internal_daily_cap must be positive",
            vec![Violation {
                field: "internal_daily_cap".into(),
                code: "invalid".into(),
            }],
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(cur) = repo::get_delivery_channel(&mut tx, id).await? else {
        let _ = tx.rollback().await;
        return Err(AppError::not_found("delivery channel not found"));
    };
    if cur.version != expected_version {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "delivery channel changed; refetch and retry",
        ));
    }
    let new_enabled = req.enabled.unwrap_or(cur.enabled);
    let new_cap = req.internal_daily_cap.or(cur.internal_daily_cap);

    if new_enabled && !cur.enabled {
        // T33 / §4.9 / DDL: zoho_mail never drives non-correspondence.
        if domain::mailboxes::activation_policy_blocked(&cur.provider, &cur.purpose) {
            let _ = tx.rollback().await;
            return Err(AppError::new(
                ErrorCode::ProviderPolicyBlocked,
                "zoho_mail can only serve the correspondence purpose",
            ));
        }
        // Gate evidence: `allowed` policy_status set by an unexpired
        // review (release-gate evidence — §4.9).
        if cur.policy_status == "blocked" {
            let _ = tx.rollback().await;
            return Err(AppError::new(
                ErrorCode::ProviderPolicyBlocked,
                "channel is blocked by policy review",
            ));
        }
        let evidence = repo::latest_allow_review(&mut tx, id).await?;
        let evidence_valid = evidence.is_some_and(|r| r.expires_at.is_none_or(|e| e > Utc::now()));
        if cur.policy_status != "allowed" || !evidence_valid {
            let _ = tx.rollback().await;
            return Err(AppError::new(
                ErrorCode::ProviderPolicyUnverified,
                "channel activation requires an unexpired allowed review",
            ));
        }
    }

    let Some(row) =
        repo::update_delivery_channel(&mut tx, id, expected_version, new_enabled, new_cap).await?
    else {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "delivery channel changed; refetch and retry",
        ));
    };
    audit::record(
        &mut tx,
        AuditEntry {
            metadata: serde_json::json!({
                "enabled": new_enabled,
                "internal_daily_cap": new_cap,
            }),
            ..AuditEntry::new(
                ctx.principal_id,
                "delivery_channel.update",
                "delivery_channel",
                ctx.request_id.clone(),
            )
            .with_target(id)
        },
    )
    .await?;
    tx.commit().await?;
    Ok(response(&row))
}
