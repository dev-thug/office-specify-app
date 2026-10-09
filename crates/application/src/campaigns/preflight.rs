//! POST /campaigns/{id}/preflight (B.3.7) — informational only; it is never
//! an execution authorization. Read-only: nothing is written, so the tx is
//! a plain workspace read transaction.

use chrono::Utc;
use contracts::campaigns::{PreflightRecipient, PreflightRequest, PreflightResponse};
use contracts::{AppError, Ctx, ErrorCode};
use infrastructure::db::Tx;
use infrastructure::repo::campaigns as repo;
use infrastructure::repo::compliance_core;
use uuid::Uuid;

use super::campaign::require_compose_grant;
use crate::authz::{self, MailboxPerm};

/// Reason codes surfaced per recipient (B.5 stable names + two module-local
/// address codes documented on the DTO).
pub mod recipient_reason {
    pub const ADDRESS_NOT_FOUND: &str = "ADDRESS_NOT_FOUND";
    pub const ADDRESS_INVALID: &str = "ADDRESS_INVALID";
    pub const RECIPIENT_SUPPRESSED: &str = "RECIPIENT_SUPPRESSED";
    pub const SEND_BASIS_UNVERIFIED: &str = "SEND_BASIS_UNVERIFIED";
    pub const RECIPIENT_ACTIVE_ELSEWHERE: &str = "RECIPIENT_ACTIVE_ELSEWHERE";
    pub const MEETING_TYPE_BINDING_REQUIRED: &str = "MEETING_TYPE_BINDING_REQUIRED";
    pub const MEETING_TYPE_BINDING_INVALID: &str = "MEETING_TYPE_BINDING_INVALID";
}

async fn meeting_binding_block_reason(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    revision: &repo::RevisionRow,
) -> Result<Option<&'static str>, AppError> {
    let mut requires_booking_url = false;
    for step in repo::list_steps(tx, revision.id).await? {
        let template = repo::get_template_version(tx, step.template_version_id)
            .await?
            .ok_or_else(|| AppError::internal("campaign step template missing"))?;
        let specs = domain::templates::parse_variable_schema(&template.variable_schema).map_err(
            |error| {
                AppError::validation(
                    error.to_string(),
                    vec![contracts::Violation {
                        field: "variable_schema".into(),
                        code: "SCHEMA_INVALID".into(),
                    }],
                )
            },
        )?;
        if specs.iter().any(|spec| {
            spec.name == "meeting.booking_url"
                && (spec.required
                    || domain::templates::uses_variable(
                        "meeting.booking_url",
                        &template.subject,
                        &template.body_text,
                        template.body_html.as_deref(),
                    ))
        }) {
            requires_booking_url = true;
            break;
        }
    }
    if !requires_booking_url {
        return Ok(None);
    }
    let rules = domain::campaigns::ScheduleRules::from_json(&revision.schedule_rules)
        .map_err(|error| AppError::internal(format!("stored schedule_rules invalid: {error}")))?;
    let Some(meeting_type_id) = rules.meeting_type_id else {
        return Ok(Some(recipient_reason::MEETING_TYPE_BINDING_REQUIRED));
    };
    match crate::booking::validate_bound_link_in_tx(tx, ctx, meeting_type_id).await {
        Ok(()) => Ok(None),
        Err(error)
            if matches!(
                error.code,
                ErrorCode::NotFound
                    | ErrorCode::Forbidden
                    | ErrorCode::ScopeRequired
                    | ErrorCode::PrincipalDisabled
                    | ErrorCode::CalendarAvailabilityUnavailable
                    | ErrorCode::DependencyUnavailable
            ) =>
        {
            Ok(Some(recipient_reason::MEETING_TYPE_BINDING_INVALID))
        }
        Err(error) => Err(error),
    }
}

/// Recipient eligibility against the live tables — shared by preflight and
/// the freeze worker. Returns `None` when enrollable, else the reason code.
pub(crate) async fn recipient_block_reason(
    tx: &mut Tx<'_>,
    email_address_id: Uuid,
) -> Result<Option<String>, AppError> {
    let Some((_addr, verification)) = repo::get_email_address(tx, email_address_id).await? else {
        return Ok(Some(recipient_reason::ADDRESS_NOT_FOUND.to_string()));
    };
    if verification == "invalid" {
        return Ok(Some(recipient_reason::ADDRESS_INVALID.to_string()));
    }
    if repo::recipient_active_elsewhere(tx, email_address_id).await? {
        return Ok(Some(
            recipient_reason::RECIPIENT_ACTIVE_ELSEWHERE.to_string(),
        ));
    }
    if !compliance_core::active_suppressions(tx, email_address_id, "marketing")
        .await?
        .is_empty()
    {
        return Ok(Some(recipient_reason::RECIPIENT_SUPPRESSED.to_string()));
    }
    if repo::valid_marketing_basis(tx, email_address_id)
        .await?
        .is_none()
    {
        return Ok(Some(recipient_reason::SEND_BASIS_UNVERIFIED.to_string()));
    }
    Ok(None)
}

pub async fn preflight(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    campaign_id: Uuid,
    req: &PreflightRequest,
) -> Result<PreflightResponse, AppError> {
    let Some(campaign) = repo::get_campaign(tx, campaign_id).await? else {
        return Err(AppError::not_found("campaign not found"));
    };
    // Reading eligibility data is a read-level operation on the mailbox;
    // composing a launch is the compose-level one.
    authz::require_mailbox(tx, ctx, campaign.mail_account_id, MailboxPerm::Read).await?;

    let Some(revision) = repo::get_revision(tx, req.revision_id).await? else {
        return Err(AppError::not_found("revision not found"));
    };
    if revision.campaign_id != campaign_id {
        return Err(AppError::new(
            ErrorCode::ValidationFailed,
            "revision_id does not belong to this campaign",
        ));
    }
    // Compose/send-level mailbox access is required to *use* a revision for
    // launch decisions — preflight itself stays informational.
    require_compose_grant(tx, ctx, campaign.mail_account_id).await?;

    let channel = repo::get_channel(tx, revision.delivery_channel_id)
        .await?
        .ok_or_else(|| AppError::internal("revision delivery channel missing"))?;
    let emergency = repo::emergency_stop_active(tx).await?;
    let mut global = repo::channel_blocking_reasons(&channel, emergency);
    let meeting_binding_reason = meeting_binding_block_reason(tx, ctx, &revision).await?;
    if let Some(reason) = meeting_binding_reason {
        global.push(reason.to_string());
    }
    if revision.status == domain::campaigns::revision_status::FROZEN {
        // A frozen revision is immutable but launchable — not a blocker.
    }

    let mut recipients = Vec::with_capacity(req.email_address_ids.len());
    for id in &req.email_address_ids {
        let reason = recipient_block_reason(tx, *id).await?;
        let mut reason_codes = reason.clone().into_iter().collect::<Vec<_>>();
        if let Some(binding_reason) = meeting_binding_reason {
            reason_codes.push(binding_reason.to_string());
        }
        recipients.push(PreflightRecipient {
            email_address_id: *id,
            recipient_checks_passed: reason.is_none() && meeting_binding_reason.is_none(),
            reason_codes,
        });
    }
    // De-dup global list while preserving order.
    global.sort();
    global.dedup();
    Ok(PreflightResponse {
        checked_at: Utc::now(),
        is_execution_authorization: false,
        global_blocking_reasons: global,
        recipients,
    })
}
