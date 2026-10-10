//! Campaign CRUD use cases (B.2): create (with initial revision + steps),
//! list (mailbox-ACL filtered), get, and additional-revision creation.

use chrono::{DateTime, Utc};
use contracts::campaigns::{
    CampaignDetail, CampaignStepInput, CampaignStepSummary, CampaignSummary, CreateCampaignRequest,
    CreateCampaignResponse, CreateRevisionRequest, CreateRevisionResponse, LaunchSummary,
    RevisionSummary, ScheduleRulesInput,
};
use contracts::{AppError, Ctx, ErrorCode, Page, Scope, Violation};
use domain::campaigns as d;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::cursor::Cursor;
use infrastructure::db::{Db, Tx};
use infrastructure::outbox;
use infrastructure::repo::campaigns as repo;
use uuid::Uuid;

use crate::authz::{self, MailboxGrant, MailboxPerm};

// ------------------------------------------------------------- helpers

/// Audit entry with metadata (allowlisted values only — never bodies).
pub(crate) fn audited(
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

/// Same for worker-side writes (actor = system principal, job correlation).
pub(crate) fn audited_system(
    actor: Uuid,
    job_id: Option<Uuid>,
    action: &str,
    target_type: &str,
    target_id: Uuid,
    metadata: serde_json::Value,
) -> AuditEntry {
    let mut e = AuditEntry::new(actor, action, target_type, "").with_target(target_id);
    e.job_id = job_id;
    e.metadata = metadata;
    e
}

/// B.2 grants `compose|send` on the mailbox for campaign management —
/// either permission authorizes (mailbox ACL stays separate from scopes).
pub(crate) async fn require_mailbox_compose_or_send(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<MailboxGrant, AppError> {
    let grant = authz::mailbox_grant(tx, ctx, mail_account_id)
        .await?
        .ok_or_else(|| {
            AppError::new(ErrorCode::MailboxAccessDenied, "no access to this mailbox")
        })?;
    if grant.can_compose || grant.can_send {
        Ok(grant)
    } else {
        Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no access to this mailbox",
        ))
    }
}

/// §5.3/B.2 "compose grant": session-authenticated humans (any role,
/// including members) are authorized by the mailbox can_compose/can_send
/// grant alone — admins get nothing implicit (§5.3 note). Credential calls
/// (agents / service credentials) additionally need the `campaign.compose`
/// scope (§5.2 scope conjunct applies to the call).
pub(crate) async fn require_compose_grant(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<MailboxGrant, AppError> {
    let credential_call = ctx.principal_kind == contracts::PrincipalKind::Agent
        || matches!(ctx.auth, contracts::AuthMethod::ServiceCredential { .. });
    if credential_call {
        authz::require_scope(ctx, Scope::CampaignCompose)?;
    }
    require_mailbox_compose_or_send(tx, ctx, mail_account_id).await
}

/// Reads are grant-governed for everyone (§5.4 — the mail_account_grants
/// row is the explicit authorization, for agents too); credential calls
/// still need the scope.
pub(crate) fn require_read_scope_for_credentials(ctx: &Ctx) -> Result<(), AppError> {
    let credential_call = ctx.principal_kind == contracts::PrincipalKind::Agent
        || matches!(ctx.auth, contracts::AuthMethod::ServiceCredential { .. });
    if credential_call {
        authz::require_scope(ctx, Scope::CampaignCompose)?;
    }
    Ok(())
}

/// Convert the request DTO rules into the validated domain rules.
pub(crate) fn parse_rules(input: &ScheduleRulesInput) -> Result<d::ScheduleRules, AppError> {
    let v = serde_json::to_value(input).unwrap_or_default();
    d::ScheduleRules::from_json(&v).map_err(|e| {
        AppError::validation(
            "invalid schedule_rules",
            vec![Violation {
                field: "schedule_rules".to_string(),
                code: format!("invalid_rules: {e}"),
            }],
        )
    })
}

pub(crate) fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn decode_hex(s: &str) -> Option<Vec<u8>> {
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

pub(crate) fn version_check(expected: i64, actual: i64) -> Result<(), AppError> {
    if expected == actual {
        Ok(())
    } else {
        Err(AppError::new(
            ErrorCode::VersionMismatch,
            format!("version mismatch: expected {expected}, current {actual}"),
        ))
    }
}

/// Keyset (created_at DESC, id DESC) cursor decode bound to the filter
/// projection + principal.
pub(crate) fn decode_page_cursor(
    cursor: &str,
    hmac: &[u8],
    principal: Uuid,
    filter: &serde_json::Value,
) -> Result<Option<(DateTime<Utc>, Uuid)>, AppError> {
    let filter_hash = Cursor::hash_filter(filter);
    let cur = Cursor::decode(cursor, hmac, principal, &filter_hash)?;
    let (Some(created), Some(id)) = (cur.sort_values.first(), cur.sort_values.get(1)) else {
        return Err(AppError::validation("invalid pagination cursor", vec![]));
    };
    let created: DateTime<Utc> = serde_json::from_value(created.clone())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    let id: Uuid = serde_json::from_value(id.clone())
        .map_err(|_| AppError::validation("invalid pagination cursor", vec![]))?;
    Ok(Some((created, id)))
}

pub(crate) fn encode_page_cursor(
    created_at: DateTime<Utc>,
    id: Uuid,
    hmac: &[u8],
    principal: Uuid,
    filter: &serde_json::Value,
) -> String {
    Cursor::new(
        vec![serde_json::json!(created_at), serde_json::json!(id)],
        Cursor::hash_filter(filter),
        principal,
    )
    .encode(hmac)
}

/// Campaign authoring now supports a single immediate message. Keep the
/// legacy table shape intact so older revisions remain readable, but reject
/// any new follow-up step or configured delay at this API boundary.
fn validate_steps(steps: &[CampaignStepInput], violations: &mut Vec<Violation>) {
    if steps.len() != 1 {
        violations.push(Violation {
            field: "steps".to_string(),
            code: "must_contain_exactly_one_immediate_step".to_string(),
        });
    }
    let mut orders: Vec<i32> = steps.iter().map(|s| s.step_order).collect();
    orders.sort_unstable();
    if !orders.iter().enumerate().all(|(i, o)| *o == i as i32 + 1) {
        violations.push(Violation {
            field: "steps[].step_order".to_string(),
            code: "must_be_unique_contiguous_from_1".to_string(),
        });
    }
    for s in steps {
        if s.delay_hours != 0 {
            violations.push(Violation {
                field: "steps[].delay_hours".to_string(),
                code: "must_be_zero_for_immediate_send".to_string(),
            });
        }
    }
}

/// Insert revision + steps inside `tx`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn insert_revision_with_steps(
    tx: &mut Tx<'_>,
    campaign_id: Uuid,
    mail_account_id: Uuid,
    revision_no: i32,
    delivery_channel_id: Uuid,
    timezone: &str,
    schedule_rules: &serde_json::Value,
    stop_on_reply: bool,
    stop_on_booking: bool,
    steps: &[CampaignStepInput],
    created_by: Uuid,
) -> Result<Uuid, AppError> {
    let revision_id = repo::insert_revision(
        tx,
        campaign_id,
        mail_account_id,
        delivery_channel_id,
        revision_no,
        timezone,
        schedule_rules,
        stop_on_reply,
        stop_on_booking,
        created_by,
    )
    .await?;
    for s in steps {
        repo::insert_step(
            tx,
            revision_id,
            s.template_version_id,
            s.step_order,
            s.delay_hours,
        )
        .await?;
    }
    Ok(revision_id)
}

/// Shared validation for a (new or initial) revision: timezone, rules,
/// channel purpose + mailbox ownership, template version existence.
/// Returns the validated channel row.
pub(crate) async fn validate_revision_input(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
    delivery_channel_id: Uuid,
    timezone: &str,
    rules_input: &ScheduleRulesInput,
    steps: &[CampaignStepInput],
) -> Result<(repo::ChannelRow, serde_json::Value), AppError> {
    let mut violations = Vec::new();

    let channel = repo::get_channel(tx, delivery_channel_id).await?;
    match &channel {
        None => violations.push(Violation {
            field: "delivery_channel_id".to_string(),
            code: "not_found".to_string(),
        }),
        Some(c) => {
            // Composite-FK requirement: campaign_revisions.purpose is fixed
            // to 'marketing' and references delivery_channels(workspace_id,
            // id, purpose) — a non-marketing channel can never satisfy it.
            if c.purpose != "marketing" {
                violations.push(Violation {
                    field: "delivery_channel_id".to_string(),
                    code: "requires_purpose_marketing".to_string(),
                });
            }
            if c.provider != "delivery_provider" {
                violations.push(Violation {
                    field: "delivery_channel_id".to_string(),
                    code: "requires_office_smtp_provider".to_string(),
                });
            }
            if c.mail_account_id != mail_account_id {
                violations.push(Violation {
                    field: "delivery_channel_id".to_string(),
                    code: "belongs_to_different_mail_account".to_string(),
                });
            }
        }
    }

    if d::parse_timezone(timezone).is_none() {
        violations.push(Violation {
            field: "timezone".to_string(),
            code: "invalid_iana_timezone".to_string(),
        });
    }
    let rules = match parse_rules(rules_input) {
        Ok(r) => Some(r.to_json()),
        Err(e) => {
            violations.extend(e.violations);
            None
        }
    };

    if let Some(meeting_type_id) = rules_input.meeting_type_id {
        crate::booking::validate_bound_link_in_tx(tx, ctx, meeting_type_id).await?;
    }

    validate_steps(steps, &mut violations);
    if violations.is_empty() {
        for s in steps {
            if repo::get_template_version(tx, s.template_version_id)
                .await?
                .is_none()
            {
                violations.push(Violation {
                    field: "steps[].template_version_id".to_string(),
                    code: "not_found".to_string(),
                });
            }
        }
    }
    if !violations.is_empty() {
        return Err(AppError::validation(
            "invalid campaign revision",
            violations,
        ));
    }
    Ok((
        channel.expect("channel present when no violations"),
        rules.unwrap_or_else(|| serde_json::json!({})),
    ))
}

// ------------------------------------------------------------- use cases

/// POST /campaigns (B.3.6) — runs inside the route's `idempotency::run`
/// business tx (creates + enqueues nothing external, but the endpoint is
/// POST-with-creation so the key is required).
pub async fn create_campaign(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    req: &CreateCampaignRequest,
) -> Result<CreateCampaignResponse, AppError> {
    require_compose_grant(tx, ctx, req.mail_account_id).await?;

    if req.name.trim().is_empty() || req.name.len() > 300 {
        return Err(AppError::validation(
            "invalid campaign name",
            vec![Violation {
                field: "name".to_string(),
                code: "required_max_300".to_string(),
            }],
        ));
    }
    let (channel, rules_json) = validate_revision_input(
        tx,
        ctx,
        req.mail_account_id,
        req.delivery_channel_id,
        &req.timezone,
        &req.schedule_rules,
        &req.steps,
    )
    .await?;

    let campaign_id = repo::insert_campaign(
        tx,
        req.mail_account_id,
        req.name.trim(),
        req.objective.as_deref(),
        ctx.principal_id,
    )
    .await?;
    let revision_id = insert_revision_with_steps(
        tx,
        campaign_id,
        req.mail_account_id,
        1,
        req.delivery_channel_id,
        &req.timezone,
        &rules_json,
        req.stop_on_reply,
        req.stop_on_booking,
        &req.steps,
        ctx.principal_id,
    )
    .await?;

    audit::record(
        tx,
        audited(
            ctx,
            "campaign.created",
            "campaign",
            campaign_id,
            serde_json::json!({
                "name": req.name.trim(),
                "revision_id": revision_id,
                "delivery_channel_id": req.delivery_channel_id,
            }),
        ),
    )
    .await?;
    outbox::emit(
        tx,
        "campaign.created",
        "campaign",
        campaign_id,
        1,
        serde_json::json!({
            "campaign_id": campaign_id,
            "revision_id": revision_id,
        }),
    )
    .await?;

    let emergency = repo::emergency_stop_active(tx).await?;
    let blocking = repo::channel_blocking_reasons(&channel, emergency);
    Ok(CreateCampaignResponse {
        campaign_id,
        revision_id,
        status: d::campaign_status::DRAFT.to_string(),
        revision_no: 1,
        version: 1,
        channel_ready: blocking.is_empty(),
        blocking_reasons: blocking,
    })
}

/// GET /campaigns — keyset page filtered by `can_read` mailbox grants.
pub async fn list_campaigns(
    db: &Db,
    ctx: &Ctx,
    cursor: Option<&str>,
    limit: i64,
    hmac: &[u8],
) -> Result<(Vec<CampaignSummary>, Page), AppError> {
    require_read_scope_for_credentials(ctx)?;
    let filter = serde_json::json!({"route": "GET /campaigns"});
    let after = match cursor {
        Some(c) => decode_page_cursor(c, hmac, ctx.principal_id, &filter)?,
        None => None,
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_campaigns(&mut tx, ctx.principal_id, after, limit + 1).await?;
    let has_more = rows.len() as i64 > limit;
    let page_rows: Vec<repo::CampaignRow> = rows.into_iter().take(limit as usize).collect();
    let next_cursor = if has_more {
        page_rows
            .last()
            .map(|r| encode_page_cursor(r.created_at, r.id, hmac, ctx.principal_id, &filter))
    } else {
        None
    };
    let items = page_rows
        .into_iter()
        .map(|r| CampaignSummary {
            id: r.id,
            name: r.name,
            objective: r.objective,
            mail_account_id: r.mail_account_id,
            owner_principal_id: r.owner_principal_id,
            status: r.status,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect();
    tx.commit().await?;
    Ok((
        items,
        Page {
            next_cursor,
            has_more,
        },
    ))
}

/// `can_*` hints for the detail response (UI hint only — B.1.2).
pub(crate) async fn allowed_actions(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    campaign: &repo::CampaignRow,
) -> Result<Vec<String>, AppError> {
    let grant = authz::mailbox_grant(tx, ctx, campaign.mail_account_id)
        .await?
        .unwrap_or_default();
    let mut out = vec!["read".to_string()];
    if (grant.can_compose || grant.can_send) && ctx.scopes.contains(Scope::CampaignCompose) {
        out.push("create_revision".into());
        out.push("preflight".into());
        out.push("create_launch".into());
    }
    let can_pause = ctx.scopes.contains(Scope::CampaignPause)
        || matches!(ctx.role, contracts::Role::Admin | contracts::Role::Manager)
        || campaign.owner_principal_id == ctx.principal_id;
    if can_pause {
        out.push("pause_launch".into());
        out.push("cancel_launch".into());
    }
    Ok(out)
}

/// GET /campaigns/{id} — requires `can_read` on the campaign's mailbox.
pub async fn get_campaign(
    db: &Db,
    ctx: &Ctx,
    campaign_id: Uuid,
) -> Result<CampaignDetail, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(c) = repo::get_campaign(&mut tx, campaign_id).await? else {
        return Err(AppError::not_found("campaign not found"));
    };
    authz::require_mailbox(&mut tx, ctx, c.mail_account_id, MailboxPerm::Read).await?;

    let revision_rows = repo::list_revisions(&mut tx, campaign_id).await?;
    let mut revisions = Vec::with_capacity(revision_rows.len());
    for row in revision_rows {
        let steps = repo::list_steps(&mut tx, row.revision.id)
            .await?
            .into_iter()
            .map(|step| CampaignStepSummary {
                id: step.id,
                step_order: step.step_order,
                template_version_id: step.template_version_id,
                delay_hours: step.delay_hours,
            })
            .collect();
        revisions.push(RevisionSummary {
            id: row.revision.id,
            campaign_id: row.revision.campaign_id,
            revision_no: row.revision.revision_no,
            status: row.revision.status,
            delivery_channel_id: row.revision.delivery_channel_id,
            mail_account_id: row.revision.mail_account_id,
            timezone: row.revision.timezone,
            schedule_rules: row.revision.schedule_rules,
            stop_on_reply: row.revision.stop_on_reply,
            stop_on_booking: row.revision.stop_on_booking,
            step_count: row.step_count,
            steps,
            frozen_at: row.revision.frozen_at,
            created_at: row.revision.created_at,
        });
    }
    let launches = repo::list_launches_for_campaign(&mut tx, campaign_id).await?;
    let allowed = allowed_actions(&mut tx, ctx, &c).await?;
    tx.commit().await?;

    Ok(CampaignDetail {
        campaign: CampaignSummary {
            id: c.id,
            name: c.name,
            objective: c.objective,
            mail_account_id: c.mail_account_id,
            owner_principal_id: c.owner_principal_id,
            status: c.status,
            version: c.version,
            created_at: c.created_at,
            updated_at: c.updated_at,
        },
        revisions,
        launches: launches
            .into_iter()
            .map(|l| LaunchSummary {
                id: l.id,
                revision_id: l.revision_id,
                delivery_channel_id: l.delivery_channel_id,
                requested_by: l.requested_by,
                status: l.status,
                manifest_hash: l.manifest_hash.as_deref().map(hex_of),
                frozen_at: l.frozen_at,
                expires_at: l.expires_at,
                version: l.version,
                created_at: l.created_at,
            })
            .collect(),
        allowed_actions: allowed,
    })
}

/// POST /campaigns/{id}/revisions — a later edit creates a NEW revision
/// (A.6.2: prior revisions stay immutable). If-Match gates the campaign row.
pub async fn create_revision(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    campaign_id: Uuid,
    expected_version: i64,
    req: &CreateRevisionRequest,
) -> Result<CreateRevisionResponse, AppError> {
    let Some(c) = repo::get_campaign(tx, campaign_id).await? else {
        return Err(AppError::not_found("campaign not found"));
    };
    require_compose_grant(tx, ctx, c.mail_account_id).await?;
    version_check(expected_version, c.version)?;

    let (channel, rules_json) = validate_revision_input(
        tx,
        ctx,
        c.mail_account_id,
        req.delivery_channel_id,
        &req.timezone,
        &req.schedule_rules,
        &req.steps,
    )
    .await?;

    let revision_no = repo::next_revision_no(tx, campaign_id).await?;
    let revision_id = insert_revision_with_steps(
        tx,
        campaign_id,
        c.mail_account_id,
        revision_no,
        req.delivery_channel_id,
        &req.timezone,
        &rules_json,
        req.stop_on_reply,
        req.stop_on_booking,
        &req.steps,
        ctx.principal_id,
    )
    .await?;

    audit::record(
        tx,
        audited(
            ctx,
            "campaign.revision_created",
            "campaign",
            campaign_id,
            serde_json::json!({
                "revision_id": revision_id,
                "revision_no": revision_no,
            }),
        ),
    )
    .await?;
    outbox::emit(
        tx,
        "campaign.revision.created",
        "campaign_revision",
        revision_id,
        i64::from(revision_no),
        serde_json::json!({
            "campaign_id": campaign_id,
            "revision_id": revision_id,
            "revision_no": revision_no,
        }),
    )
    .await?;

    let emergency = repo::emergency_stop_active(tx).await?;
    let blocking = repo::channel_blocking_reasons(&channel, emergency);
    Ok(CreateRevisionResponse {
        revision_id,
        campaign_id,
        revision_no,
        status: d::revision_status::DRAFT.to_string(),
        channel_ready: blocking.is_empty(),
        blocking_reasons: blocking,
    })
}
