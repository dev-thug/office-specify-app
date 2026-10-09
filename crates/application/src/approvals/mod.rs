//! approvals module application layer (spec §5.5, A.6.3, B.3.9).
//! A decision locks the request row, re-verifies pending/expiry/version/
//! snapshot/requester-vs-approver/target-state, inserts the append-only
//! decision row, flips the request status and performs the integration
//! side effects — all inside the caller's single transaction.

use chrono::Utc;
use contracts::approvals::{
    ApprovalDecisionView, ApprovalRequestDetail, ApprovalRequestSummary, ApprovalTargetSummary,
    CreateDecisionRequest, DecisionResponse, RevokeResponse,
};
use contracts::{AppError, Ctx, ErrorCode, Page, Role, Scope, Violation};
use domain::approvals as d;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::repo::approvals as repo;
use uuid::Uuid;

use crate::authz::{self, MailboxPerm};

// ------------------------------------------------------------ helpers

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap_or(0))
            .collect(),
    )
}

fn summary(r: &repo::ApprovalRequestRow) -> ApprovalRequestSummary {
    ApprovalRequestSummary {
        id: r.id,
        action_type: r.action_type.clone(),
        target_id: r.target_id(),
        requested_by: r.requested_by,
        status: r.status.clone(),
        snapshot_hash: hex_of(&r.snapshot_hash),
        expires_at: r.expires_at,
        version: r.version,
        created_at: r.created_at,
    }
}

fn audited(
    ctx: &Ctx,
    action: &str,
    target_type: &str,
    target_id: Uuid,
    metadata: serde_json::Value,
) -> AuditEntry {
    let mut e = AuditEntry::new(
        ctx.principal_id,
        action,
        target_type,
        ctx.request_id.clone(),
    )
    .with_target(target_id);
    e.metadata = metadata;
    e
}

#[derive(sqlx::FromRow)]
struct OutboundPreviewRow {
    to_address_snapshot: String,
    from_address_snapshot: String,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    payload_hash: Vec<u8>,
    purpose: String,
}

/// Visibility rule (B.2 "승인자·본인 요청"): the requester, any
/// admin/manager, or a principal holding `can_approve` on the target's
/// mailbox (mail-bound targets only).
async fn can_view(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &repo::ApprovalRequestRow,
    target_mail_account: Option<Uuid>,
) -> Result<bool, AppError> {
    if req.requested_by == ctx.principal_id || matches!(ctx.role, Role::Admin | Role::Manager) {
        return Ok(true);
    }
    if let Some(ma) = target_mail_account {
        let grant = authz::mailbox_grant(tx, ctx, ma).await?.unwrap_or_default();
        return Ok(grant.can_approve);
    }
    Ok(false)
}

/// Resolve the target's mail account (mail-bound targets only).
async fn target_mail_account(
    tx: &mut Tx<'_>,
    req: &repo::ApprovalRequestRow,
) -> Result<Option<Uuid>, AppError> {
    match req.action_type.as_str() {
        d::action_type::CAMPAIGN_LAUNCH => Ok(repo::launch_state(tx, req.target_id())
            .await?
            .map(|(_, _, _, ma, _)| ma)),
        d::action_type::MESSAGE_SUBMIT => Ok(repo::outbound_state(tx, req.target_id())
            .await?
            .map(|(_, _, ma, _)| ma)),
        _ => Ok(None),
    }
}

/// `require_mailbox` enforces both the row grant and the caller's matching
/// credential scope. Only expected authorization failures are converted to
/// `false`; database errors still fail the request.
async fn mailbox_has_permission(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
    permission: MailboxPerm,
) -> Result<bool, AppError> {
    match authz::require_mailbox(tx, ctx, mail_account_id, permission).await {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.code,
                ErrorCode::MailboxAccessDenied | ErrorCode::ScopeRequired
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

// ------------------------------------------------------------------ list

/// GET /approvals — admins/managers see the workspace queue; everyone else
/// sees own requests plus mail-bound requests on mailboxes where they hold
/// `can_approve` (a member approver sees their actionable queue).
pub async fn list(
    db: &Db,
    ctx: &Ctx,
    status: Option<&str>,
    action_type: Option<&str>,
    cursor: Option<&str>,
    limit: i64,
    hmac: &[u8],
) -> Result<(Vec<ApprovalRequestSummary>, Page), AppError> {
    authz::require_scope(ctx, Scope::ApprovalsRead)?;
    for (v, allowed, field) in [
        (
            status,
            &["pending", "approved", "rejected", "expired", "revoked"][..],
            "status",
        ),
        (
            action_type,
            &[
                "campaign_launch",
                "message_submit",
                "booking_change",
                "suppression_release",
            ][..],
            "action_type",
        ),
    ] {
        if let Some(v) = v
            && !allowed.contains(&v)
        {
            return Err(AppError::validation(
                "invalid filter",
                vec![Violation {
                    field: field.to_string(),
                    code: "unknown_value".to_string(),
                }],
            ));
        }
    }
    let filter = serde_json::json!({
        "route": "GET /approvals",
        "status": status,
        "action_type": action_type,
    });
    let filter_hash = infrastructure::cursor::Cursor::hash_filter(&filter);
    let after = match cursor {
        Some(c) => {
            let cur =
                infrastructure::cursor::Cursor::decode(c, hmac, ctx.principal_id, &filter_hash)?;
            let (Some(t), Some(i)) = (cur.sort_values.first(), cur.sort_values.get(1)) else {
                return Err(AppError::validation("invalid pagination cursor", vec![]));
            };
            let t: chrono::DateTime<Utc> = serde_json::from_value(t.clone())
                .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
            let i: Uuid = serde_json::from_value(i.clone())
                .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
            Some((t, i))
        }
        None => None,
    };

    let mut tx = db.begin(ctx.workspace_id).await?;
    let see_all = matches!(ctx.role, Role::Admin | Role::Manager);
    // Members with can_approve still see only their own + approve-mailbox
    // queue — implemented as: fetch a wider page and filter in-app when
    // !see_all (mailbox grant checks are per-row).
    let fetch = if see_all { limit + 1 } else { (limit + 1) * 4 };
    let rows = repo::list_requests(
        &mut tx,
        ctx.principal_id,
        see_all,
        status,
        action_type,
        after,
        fetch,
    )
    .await?;
    let mut visible = Vec::new();
    for r in rows {
        let ma = target_mail_account(&mut tx, &r).await?;
        if can_view(&mut tx, ctx, &r, ma).await? {
            visible.push(r);
        }
        if visible.len() as i64 > limit {
            break;
        }
    }
    let has_more = visible.len() as i64 > limit;
    visible.truncate(limit as usize);
    let next_cursor = if has_more {
        visible.last().map(|r| {
            infrastructure::cursor::Cursor::new(
                vec![serde_json::json!(r.created_at), serde_json::json!(r.id)],
                filter_hash,
                ctx.principal_id,
            )
            .encode(hmac)
        })
    } else {
        None
    };
    let items = visible.iter().map(summary).collect();
    tx.commit().await?;
    Ok((
        items,
        Page {
            next_cursor,
            has_more,
        },
    ))
}

// ------------------------------------------------------------------- get

/// GET /approvals/{id}.
pub async fn get(db: &Db, ctx: &Ctx, id: Uuid) -> Result<ApprovalRequestDetail, AppError> {
    authz::require_scope(ctx, Scope::ApprovalsRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(req) = repo::get_request(&mut tx, id).await? else {
        return Err(AppError::not_found("approval request not found"));
    };
    let ma = target_mail_account(&mut tx, &req).await?;
    if !can_view(&mut tx, ctx, &req, ma).await? {
        // Hide existence from non-involved principals (§5.6 note).
        return Err(AppError::not_found("approval request not found"));
    }
    let decision = repo::get_decision(&mut tx, id).await?;
    let target = target_summary(&mut tx, ctx, &req, ma).await?;
    let (policy_results, can_decide) =
        policy_results_and_decision_hint(&mut tx, ctx, &req, &target, ma).await?;
    let snapshot = target.preview.clone();
    let recipients_count = snapshot.as_ref().and_then(|value| {
        value
            .get("recipients_count")
            .and_then(serde_json::Value::as_i64)
            .or_else(|| {
                value
                    .get("enrollments")
                    .and_then(|enrollments| enrollments.get("total"))
                    .and_then(serde_json::Value::as_i64)
            })
    });
    let request_summary = summary(&req);
    tx.commit().await?;
    Ok(ApprovalRequestDetail {
        self_request: req.requested_by == ctx.principal_id,
        can_decide,
        manifest_hash: request_summary.snapshot_hash.clone(),
        recipients_count,
        snapshot,
        policy_results,
        request: request_summary,
        decision: decision.map(|d| ApprovalDecisionView {
            id: d.id,
            decision: d.decision,
            decided_by: d.decided_by,
            decided_at: d.decided_at,
            reason: d.reason,
        }),
        target,
    })
}

/// B.2 "동결 스냅샷·policy·권한" — status + recomputed hash always; content
/// preview only with a mailbox read grant on the target's mailbox.
async fn target_summary(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &repo::ApprovalRequestRow,
    mail_account_id: Option<Uuid>,
) -> Result<ApprovalTargetSummary, AppError> {
    let target_id = req.target_id();
    let (status, mail_account_id) = match req.action_type.as_str() {
        d::action_type::CAMPAIGN_LAUNCH => {
            let st = repo::launch_state(tx, target_id).await?;
            (
                st.as_ref().map(|(s, _, _, _, _)| s.clone()),
                mail_account_id,
            )
        }
        d::action_type::MESSAGE_SUBMIT => {
            let st = repo::outbound_state(tx, target_id).await?;
            (st.as_ref().map(|(s, _, _, _)| s.clone()), mail_account_id)
        }
        d::action_type::BOOKING_CHANGE => (repo::booking_change_status(tx, target_id).await?, None),
        d::action_type::SUPPRESSION_RELEASE => {
            (repo::suppression_status(tx, target_id).await?, None)
        }
        _ => (None, None),
    };
    let current_hash = repo::target_current_hash(tx, &req.action_type, target_id)
        .await?
        .map(|h| hex_of(&h));

    let mut preview = None;
    if let Some(ma) = mail_account_id {
        let can_read = mailbox_has_permission(tx, ctx, ma, MailboxPerm::Read).await?;
        if can_read {
            preview = match req.action_type.as_str() {
                d::action_type::CAMPAIGN_LAUNCH => {
                    let counts =
                        infrastructure::repo::campaigns::enrollment_counts(tx, target_id).await?;
                    let launch = repo::launch_state(tx, target_id)
                        .await?
                        .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
                    let channel = infrastructure::repo::campaigns::get_channel(tx, launch.4)
                        .await?
                        .ok_or_else(|| AppError::internal("launch channel missing"))?;
                    let sender = infrastructure::repo::campaigns::get_sender_identity(
                        tx,
                        channel.sender_identity_id,
                    )
                    .await?
                    .map(|(address, _)| address);
                    const PREVIEW_LIMIT: i64 = 50;
                    let rows = infrastructure::repo::campaigns::list_launch_outbounds(
                        tx,
                        target_id,
                        None,
                        PREVIEW_LIMIT + 1,
                    )
                    .await?;
                    let has_more = rows.len() as i64 > PREVIEW_LIMIT;
                    let messages: Vec<_> = rows
                        .into_iter()
                        .take(PREVIEW_LIMIT as usize)
                        .map(|row| {
                            serde_json::json!({
                                "outbound_message_id": row.outbound.id,
                                "to_address_snapshot": row.outbound.to_address_snapshot,
                                "step_order": row.step_order,
                                "subject": row.outbound.subject,
                                "body_text": row.outbound.body_text,
                                "body_html": row.outbound.body_html,
                                "status": row.outbound.status,
                                "payload_hash": hex_of(&row.outbound.payload_hash),
                            })
                        })
                        .collect();
                    Some(serde_json::json!({
                        "campaign_launch_id": target_id,
                        "purpose": "marketing",
                        "sender_address": sender,
                        "manifest_hash": current_hash,
                        "recipients_count": counts.total,
                        "enrollments": {
                            "total": counts.total,
                            "pending": counts.pending,
                            "active": counts.active,
                            "stopped": counts.stopped + counts.suppressed,
                        },
                        "messages": messages,
                        "has_more": has_more,
                    }))
                }
                d::action_type::MESSAGE_SUBMIT => {
                    let row: Option<OutboundPreviewRow> = sqlx::query_as(
                        "SELECT to_address_snapshot, from_address_snapshot, subject, body_text,
                                body_html, payload_hash, purpose
                           FROM outbound_messages
                          WHERE workspace_id = $1 AND id = $2",
                    )
                    .bind(tx.workspace_id())
                    .bind(target_id)
                    .fetch_optional(tx.conn())
                    .await
                    .map_err(infrastructure::error::map_sqlx)?;
                    row.map(|row| {
                        serde_json::json!({
                            "purpose": row.purpose,
                            "sender_address": row.from_address_snapshot,
                            "recipients_count": 1,
                            "messages": [{
                                "to_address_snapshot": row.to_address_snapshot,
                                "subject": row.subject,
                                "body_text": row.body_text,
                                "body_html": row.body_html,
                                "payload_hash": hex_of(&row.payload_hash),
                            }],
                        })
                    })
                }
                _ => None,
            };
        }
    }
    Ok(ApprovalTargetSummary {
        status,
        current_hash,
        mail_account_id,
        preview,
    })
}

/// Current checks shown beside the frozen payload in approval detail. These
/// are review hints; `decide` repeats every security-sensitive check under
/// the request row lock before writing a decision.
async fn policy_results_and_decision_hint(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &repo::ApprovalRequestRow,
    target: &ApprovalTargetSummary,
    mail_account_id: Option<Uuid>,
) -> Result<(Vec<serde_json::Value>, bool), AppError> {
    let expected_hash = hex_of(&req.snapshot_hash);
    let snapshot_matches = target.current_hash.as_deref() == Some(expected_hash.as_str());
    let request_live = req.status == d::request_status::PENDING && req.expires_at > Utc::now();
    let self_request = req.requested_by == ctx.principal_id;
    let approver_active = repo::principal_kind_role_status(tx, ctx.principal_id)
        .await?
        .is_some_and(|(kind, role, status)| {
            kind == "human" && status == "active" && matches!(role.as_str(), "admin" | "manager")
        });
    let target_state_ready = matches!(
        (req.action_type.as_str(), target.status.as_deref()),
        (
            d::action_type::CAMPAIGN_LAUNCH,
            Some("frozen" | "awaiting_approval")
        ) | (d::action_type::MESSAGE_SUBMIT, Some("draft"))
            | (d::action_type::BOOKING_CHANGE, Some("proposed"))
            | (d::action_type::SUPPRESSION_RELEASE, Some("active"))
    );
    let mut results = vec![
        serde_json::json!({
            "code": "APPROVAL_REQUEST_ACTIVE",
            "outcome": if request_live { "pass" } else { "fail" },
        }),
        serde_json::json!({
            "code": "SELF_APPROVAL",
            "outcome": if self_request { "fail" } else { "pass" },
        }),
        serde_json::json!({
            "code": "APPROVER_ELIGIBLE",
            "outcome": if approver_active { "pass" } else { "fail" },
        }),
        serde_json::json!({
            "code": "SNAPSHOT_MATCH",
            "outcome": if snapshot_matches { "pass" } else { "fail" },
        }),
        serde_json::json!({
            "code": "TARGET_STATE",
            "outcome": if target_state_ready { "pass" } else { "fail" },
        }),
    ];

    let mut can_decide =
        request_live && !self_request && approver_active && snapshot_matches && target_state_ready;
    if let Some(mail_account_id) = mail_account_id {
        let can_read = mailbox_has_permission(tx, ctx, mail_account_id, MailboxPerm::Read).await?;
        let can_approve =
            mailbox_has_permission(tx, ctx, mail_account_id, MailboxPerm::Approve).await?;
        results.push(serde_json::json!({
            "code": "MAILBOX_READ",
            "outcome": if can_read { "pass" } else { "fail" },
        }));
        results.push(serde_json::json!({
            "code": "MAILBOX_APPROVE",
            "outcome": if can_approve { "pass" } else { "fail" },
        }));

        let channel_id = match req.action_type.as_str() {
            d::action_type::CAMPAIGN_LAUNCH => repo::launch_state(tx, req.target_id())
                .await?
                .map(|state| state.4),
            d::action_type::MESSAGE_SUBMIT => repo::outbound_state(tx, req.target_id())
                .await?
                .map(|state| state.3),
            _ => None,
        };
        let channel_state = match channel_id {
            Some(id) => repo::channel_policy_state(tx, id).await?,
            None => None,
        };
        let channel_enabled = channel_state.as_ref().is_some_and(|(enabled, _)| *enabled);
        let provider_allowed = channel_state
            .as_ref()
            .is_some_and(|(_, policy)| policy == "allowed");
        let emergency_stop = infrastructure::repo::campaigns::emergency_stop_active(tx).await?;
        results.push(serde_json::json!({
            "code": "DELIVERY_CHANNEL_ENABLED",
            "outcome": if channel_enabled { "pass" } else { "fail" },
        }));
        results.push(serde_json::json!({
            "code": "PROVIDER_POLICY",
            "outcome": if provider_allowed { "pass" } else { "fail" },
        }));
        results.push(serde_json::json!({
            "code": "WORKSPACE_EMERGENCY_STOP",
            "outcome": if emergency_stop { "fail" } else { "pass" },
        }));
        if req.action_type == d::action_type::CAMPAIGN_LAUNCH {
            results.push(serde_json::json!({
                "code": "RECIPIENT_SUPPRESSION_AND_BASIS",
                "outcome": "rechecked_at_activation_and_dispatch",
            }));
        }
        can_decide &=
            can_read && can_approve && channel_enabled && provider_allowed && !emergency_stop;
    } else {
        results.push(serde_json::json!({
            "code": "TARGET_POLICY",
            "outcome": "unavailable",
            "detail": "The owning workflow performs final policy checks before execution.",
        }));
    }
    Ok((results, can_decide))
}

// --------------------------------------------------------------- decide

/// POST /approvals/{id}/decisions (B.3.9) — runs inside the route's
/// `idempotency::run` tx; If-Match supplies `expected_version`.
///
/// Denial audits for self-approval / role / mailbox failures must be
/// recorded by the route in a *fresh* transaction (this tx rolls back with
/// the error) — the error code carries which audit to write.
pub async fn decide(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &CreateDecisionRequest,
) -> Result<DecisionResponse, AppError> {
    // Human-only, admin/manager only (§5.3 — agents can never approve).
    authz::require_human(ctx)?;
    authz::require_role(ctx, &[Role::Admin, Role::Manager])?;
    let decision = d::decision::parse(&req.decision).ok_or_else(|| {
        AppError::validation(
            "decision must be 'approved' or 'rejected'",
            vec![Violation {
                field: "decision".to_string(),
                code: "invalid_value".to_string(),
            }],
        )
    })?;

    // 1. Lock the request (A.6.3 — serializes concurrent decisions).
    let Some(row) = repo::get_request_for_update(tx, id).await? else {
        return Err(AppError::not_found("approval request not found"));
    };
    if row.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            format!(
                "version mismatch: expected {expected_version}, current {}",
                row.version
            ),
        ));
    }
    if row.status != d::request_status::PENDING {
        return Err(AppError::conflict(format!(
            "approval request is already {}",
            row.status
        )));
    }
    if row.expires_at <= Utc::now() {
        return Err(AppError::new(
            ErrorCode::ApprovalExpired,
            "approval request has expired",
        ));
    }
    if row.requested_by == ctx.principal_id {
        return Err(AppError::new(
            ErrorCode::SelfApprovalForbidden,
            "the requester cannot approve their own request",
        ));
    }
    let expected = decode_hex(&req.expected_snapshot_hash)
        .ok_or_else(|| AppError::validation("expected_snapshot_hash must be sha256 hex", vec![]))?;
    if expected != row.snapshot_hash {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "expected_snapshot_hash does not match the pinned snapshot",
        ));
    }

    // 2. Approver re-verification inside the tx (A.6.3): still an active
    //    human admin/manager — the DB trigger re-checks too.
    match repo::principal_kind_role_status(tx, ctx.principal_id).await? {
        Some((kind, role, status))
            if kind == "human"
                && status == "active"
                && matches!(role.as_str(), "admin" | "manager") => {}
        Some((_, _, status)) if status != "active" => {
            return Err(AppError::new(
                ErrorCode::PrincipalDisabled,
                "approver principal is not active",
            ));
        }
        _ => {
            return Err(AppError::forbidden(
                "approver must be an active human admin or manager",
            ));
        }
    }

    // 3. Target state + snapshot re-verification + mailbox can_approve +
    //    channel policy / emergency stop (A.6.3: "채널·발송 근거·현재
    //    principal 상태·권한 scope는 Application Service에서 추가 검사").
    let target_id = row.target_id();
    let mut mail_account = None;
    let mut channel_id = None;
    match row.action_type.as_str() {
        d::action_type::CAMPAIGN_LAUNCH => {
            let Some((status, manifest, _exp, ma, ch)) = repo::launch_state(tx, target_id).await?
            else {
                return Err(AppError::conflict("launch target no longer exists"));
            };
            mail_account = Some(ma);
            channel_id = Some(ch);
            if !matches!(
                status.as_str(),
                domain::campaigns::launch_status::FROZEN
                    | domain::campaigns::launch_status::AWAITING_APPROVAL
            ) {
                return Err(AppError::conflict(format!(
                    "launch is {status} — it can no longer be approved"
                )));
            }
            if manifest.unwrap_or_default() != row.snapshot_hash {
                return Err(AppError::new(
                    ErrorCode::SnapshotMismatch,
                    "launch manifest changed since the request was created",
                ));
            }
        }
        d::action_type::MESSAGE_SUBMIT => {
            let Some((status, payload, ma, ch)) = repo::outbound_state(tx, target_id).await? else {
                return Err(AppError::conflict("outbound message no longer exists"));
            };
            mail_account = Some(ma);
            channel_id = Some(ch);
            if status != "draft" {
                return Err(AppError::conflict(format!(
                    "outbound message is {status} — only draft rows can be approved"
                )));
            }
            if payload != row.snapshot_hash {
                return Err(AppError::new(
                    ErrorCode::SnapshotMismatch,
                    "outbound payload changed since the request was created",
                ));
            }
        }
        d::action_type::BOOKING_CHANGE => {
            match repo::booking_change_status(tx, target_id).await?.as_deref() {
                Some("proposed") => {}
                Some(s) => {
                    return Err(AppError::conflict(format!(
                        "booking change is {s} — it can no longer be approved"
                    )));
                }
                None => return Err(AppError::conflict("booking change no longer exists")),
            }
        }
        d::action_type::SUPPRESSION_RELEASE => {
            match repo::suppression_status(tx, target_id).await?.as_deref() {
                Some("active") => {}
                Some(s) => {
                    return Err(AppError::conflict(format!(
                        "suppression is {s} — release already settled"
                    )));
                }
                None => return Err(AppError::conflict("suppression no longer exists")),
            }
        }
        other => return Err(AppError::internal(format!("unknown action_type {other}"))),
    }
    if let Some(ma) = mail_account {
        authz::require_mailbox(tx, ctx, ma, MailboxPerm::Approve).await?;
    }
    // Channel policy re-verification — an approval never overrides provider
    // policy or an emergency stop (§8.4, B.3.8).
    if let Some(cid) = channel_id {
        if infrastructure::repo::campaigns::emergency_stop_active(tx).await? {
            return Err(AppError::new(
                ErrorCode::WorkspaceEmergencyStop,
                "workspace is in emergency stop",
            ));
        }
        let Some((enabled, policy)) = repo::channel_policy_state(tx, cid).await? else {
            return Err(AppError::conflict("delivery channel no longer exists"));
        };
        if !enabled {
            return Err(AppError::new(
                ErrorCode::ProviderPolicyUnverified,
                "delivery channel is disabled",
            ));
        }
        if policy != "allowed" {
            return Err(if policy == "blocked" {
                AppError::new(
                    ErrorCode::ProviderPolicyBlocked,
                    "delivery channel provider policy is blocked",
                )
            } else {
                AppError::new(
                    ErrorCode::ProviderPolicyUnverified,
                    "delivery channel provider policy is unverified",
                )
            });
        }
    }

    // 4. Decision row (append-only; trigger re-validates all invariants).
    let decision_id = repo::insert_decision(
        tx,
        id,
        ctx.principal_id,
        decision,
        &row.snapshot_hash,
        req.reason.as_deref(),
        row.version,
    )
    .await?;
    let new_status = if decision == d::decision::APPROVED {
        d::request_status::APPROVED
    } else {
        d::request_status::REJECTED
    };
    let n = repo::set_request_status(tx, id, new_status, row.version).await?;
    if n != 1 {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "approval request version changed during decision",
        ));
    }

    // 5. Side effects — target updates + job enqueues, same tx.
    let mut job_id = None;
    if decision == d::decision::APPROVED {
        match row.action_type.as_str() {
            d::action_type::CAMPAIGN_LAUNCH => {
                repo::set_launch_status(
                    tx,
                    target_id,
                    domain::campaigns::launch_status::APPROVED,
                    &[
                        domain::campaigns::launch_status::FROZEN,
                        domain::campaigns::launch_status::AWAITING_APPROVAL,
                    ],
                )
                .await?;
                job_id = Some(
                    infrastructure::jobs::enqueue(
                        tx,
                        "campaigns.activate_launch",
                        1,
                        serde_json::json!({
                            "campaign_launch_id": target_id,
                            "approval_request_id": id,
                        }),
                        Utc::now(),
                        Some(&format!("activate_launch:{target_id}")),
                    )
                    .await?,
                );
            }
            d::action_type::MESSAGE_SUBMIT => {
                repo::set_outbound_status(tx, target_id, "queued", &["draft"]).await?;
                job_id = Some(
                    infrastructure::jobs::enqueue(
                        tx,
                        "sending.dispatch_outbound",
                        1,
                        serde_json::json!({"outbound_message_id": target_id}),
                        Utc::now(),
                        Some(&format!("dispatch:{target_id}")),
                    )
                    .await?,
                );
            }
            d::action_type::BOOKING_CHANGE => {
                job_id = Some(
                    infrastructure::jobs::enqueue(
                        tx,
                        "booking.apply_change",
                        1,
                        serde_json::json!({"booking_change_id": target_id}),
                        Utc::now(),
                        Some(&format!("apply_change:{target_id}")),
                    )
                    .await?,
                );
            }
            d::action_type::SUPPRESSION_RELEASE => {
                job_id = Some(
                    infrastructure::jobs::enqueue(
                        tx,
                        "compliance.release_suppression",
                        1,
                        serde_json::json!({
                            "approval_request_id": id,
                            "suppression_id": target_id,
                        }),
                        Utc::now(),
                        Some(&format!("release_suppression:{id}")),
                    )
                    .await?,
                );
            }
            _ => {}
        }
    } else {
        // Rejection side effects (A.6.2: the frozen launch/unsent draft is
        // canceled; booking/suppression owners reconcile via the event).
        match row.action_type.as_str() {
            d::action_type::CAMPAIGN_LAUNCH => {
                repo::set_launch_status(
                    tx,
                    target_id,
                    domain::campaigns::launch_status::CANCELED,
                    &[
                        domain::campaigns::launch_status::FROZEN,
                        domain::campaigns::launch_status::AWAITING_APPROVAL,
                        domain::campaigns::launch_status::APPROVED,
                    ],
                )
                .await?;
                // Rejected launch enrollments stop; pending sends cancel.
                let enrollment_ids = infrastructure::repo::campaigns::stop_launch_enrollments(
                    tx,
                    target_id,
                    "approval_rejected",
                )
                .await?;
                infrastructure::repo::campaigns::cancel_pending_outbounds_for_enrollments(
                    tx,
                    &enrollment_ids,
                )
                .await?;
            }
            d::action_type::MESSAGE_SUBMIT => {
                repo::set_outbound_status(tx, target_id, "canceled", &["draft", "queued"]).await?;
            }
            _ => {}
        }
    }

    let mut entry = audited(
        ctx,
        "approval.decided",
        "approval_request",
        id,
        serde_json::json!({
            "decision": decision,
            "action_type": row.action_type,
            "target_id": target_id,
            "decision_id": decision_id,
        }),
    );
    entry.approved_by = Some(ctx.principal_id);
    audit::record(tx, entry).await?;
    outbox::emit(
        tx,
        "approval.decided",
        "approval_request",
        id,
        row.version + 1,
        serde_json::json!({
            "approval_request_id": id,
            "target_type": row.action_type,
            "target_id": target_id,
            "decision": decision,
        }),
    )
    .await?;

    Ok(DecisionResponse {
        approval_request_id: id,
        approval_status: new_status.to_string(),
        decision_id,
        execution_status: if decision == d::decision::APPROVED {
            "queued".to_string()
        } else {
            "none".to_string()
        },
        job_id,
        provider_accepted_count: 0,
        version: row.version + 1,
    })
}

// --------------------------------------------------------------- revoke

/// POST /approvals/{id}/revoke — requester or admin/manager, human only,
/// before the approved side effect executed (A.6.3).
pub async fn revoke(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    reason: Option<&str>,
) -> Result<RevokeResponse, AppError> {
    authz::require_human(ctx)?;
    let Some(row) = repo::get_request_for_update(tx, id).await? else {
        return Err(AppError::not_found("approval request not found"));
    };
    if row.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            format!(
                "version mismatch: expected {expected_version}, current {}",
                row.version
            ),
        ));
    }
    let allowed =
        row.requested_by == ctx.principal_id || matches!(ctx.role, Role::Admin | Role::Manager);
    if !allowed {
        return Err(AppError::forbidden(
            "only the requester or an admin/manager may revoke",
        ));
    }
    if !matches!(
        row.status.as_str(),
        d::request_status::PENDING | d::request_status::APPROVED
    ) {
        return Err(AppError::conflict(format!(
            "approval request is {} — only pending/approved can be revoked",
            row.status
        )));
    }

    let target_id = row.target_id();
    if row.status == d::request_status::APPROVED {
        // Late revoke: only while the side effect is still queued (§16.2 —
        // executed effects cannot be revoked).
        match row.action_type.as_str() {
            d::action_type::CAMPAIGN_LAUNCH => {
                repo::set_launch_status(
                    tx,
                    target_id,
                    domain::campaigns::launch_status::CANCELED,
                    &[domain::campaigns::launch_status::APPROVED],
                )
                .await
                .and_then(|n| {
                    if n == 1 {
                        Ok(())
                    } else {
                        Err(AppError::conflict(
                            "launch already activated — it can no longer be revoked",
                        ))
                    }
                })?;
                repo::cancel_queued_job(
                    tx,
                    "campaigns.activate_launch",
                    &format!("activate_launch:{target_id}"),
                )
                .await?;
                let ids = infrastructure::repo::campaigns::stop_launch_enrollments(
                    tx,
                    target_id,
                    "approval_revoked",
                )
                .await?;
                infrastructure::repo::campaigns::cancel_pending_outbounds_for_enrollments(tx, &ids)
                    .await?;
            }
            d::action_type::MESSAGE_SUBMIT => {
                repo::set_outbound_status(tx, target_id, "canceled", &["queued"])
                    .await
                    .and_then(|n| {
                        if n == 1 {
                            Ok(())
                        } else {
                            Err(AppError::conflict(
                                "outbound already submitted — it can no longer be revoked",
                            ))
                        }
                    })?;
                repo::cancel_queued_job(
                    tx,
                    "sending.dispatch_outbound",
                    &format!("dispatch:{target_id}"),
                )
                .await?;
            }
            d::action_type::BOOKING_CHANGE => {
                if repo::job_ran(
                    tx,
                    "booking.apply_change",
                    &format!("apply_change:{target_id}"),
                )
                .await?
                {
                    return Err(AppError::conflict(
                        "booking change already applied — it can no longer be revoked",
                    ));
                }
                repo::cancel_queued_job(
                    tx,
                    "booking.apply_change",
                    &format!("apply_change:{target_id}"),
                )
                .await?;
            }
            d::action_type::SUPPRESSION_RELEASE => {
                if repo::job_ran(
                    tx,
                    "compliance.release_suppression",
                    &format!("release_suppression:{id}"),
                )
                .await?
                {
                    return Err(AppError::conflict(
                        "suppression already released — it can no longer be revoked",
                    ));
                }
                repo::cancel_queued_job(
                    tx,
                    "compliance.release_suppression",
                    &format!("release_suppression:{id}"),
                )
                .await?;
            }
            _ => {}
        }
    } else if row.action_type == d::action_type::CAMPAIGN_LAUNCH {
        // The request is revoked while the launch stays awaiting_approval.
        // Launch statuses are forward-only after freeze; a later request can
        // pin the unchanged manifest without rolling the launch backwards.
    }

    let n = repo::set_request_status(tx, id, d::request_status::REVOKED, row.version).await?;
    if n != 1 {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "approval request version changed during revoke",
        ));
    }
    audit::record(
        tx,
        audited(
            ctx,
            "approval.revoked",
            "approval_request",
            id,
            serde_json::json!({
                "action_type": row.action_type,
                "target_id": target_id,
                "prior_status": row.status,
                "reason": reason,
            }),
        ),
    )
    .await?;
    outbox::emit(
        tx,
        "approval.revoked",
        "approval_request",
        id,
        row.version + 1,
        serde_json::json!({
            "approval_request_id": id,
            "action_type": row.action_type,
            "target_id": target_id,
            "prior_status": row.status,
        }),
    )
    .await?;
    Ok(RevokeResponse {
        approval_request_id: id,
        status: d::request_status::REVOKED.to_string(),
        version: row.version + 1,
    })
}
