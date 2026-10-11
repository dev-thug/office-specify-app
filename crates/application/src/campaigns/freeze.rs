//! `campaigns.freeze_launch` — the A.6.2 freeze transaction. One durable job
//! turns a `building` launch into an immutable, approval-ready artifact:
//! locks revision + launch, fixes enrollments and per-step outbound
//! snapshots, computes the manifest hash, freezes revision and launch,
//! creates the approval request, audits and emits `campaign.frozen` — all
//! in the runner's single workspace tx.
//!
//! Re-entry is safe: the launch leaves `building` exactly once, so a
//! re-delivered job sees a non-building status and returns Ok.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use contracts::{AppError, AuthMethod, Ctx, ErrorCode, PrincipalKind, Scope, ScopeSet, Violation};
use domain::campaigns as d;
use infrastructure::audit;
use infrastructure::crypto;
use infrastructure::db::Tx;
use infrastructure::outbox;
use infrastructure::repo::approvals as approvals_repo;
use infrastructure::repo::campaigns as repo;
use infrastructure::repo::compliance_core;
use uuid::Uuid;

use super::campaign::{audited_system, hex_of};
use super::preflight::{recipient_block_reason, recipient_reason};

/// Per-recipient exclusion → enrollment status mapping.
fn excluded_status(reason: &str) -> &'static str {
    match reason {
        recipient_reason::RECIPIENT_SUPPRESSED => d::enrollment_status::SUPPRESSED,
        _ => d::enrollment_status::STOPPED,
    }
}

/// One rendered step ready to persist as a frozen outbound snapshot.
struct RenderedStep {
    step: repo::StepRow,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    unsubscribe_token: String,
}

fn render_error(error: domain::templates::TemplateError) -> AppError {
    match error {
        domain::templates::TemplateError::MissingRequired(name) => AppError::new(
            ErrorCode::TemplateVariableMissing,
            format!("required template variable '{name}' has no value"),
        )
        .with_violations(vec![Violation {
            field: format!("variables.{name}"),
            code: "TEMPLATE_VARIABLE_MISSING".into(),
        }]),
        other => AppError::validation(
            other.to_string(),
            vec![Violation {
                field: "template".into(),
                code: "TEMPLATE_RENDER".into(),
            }],
        ),
    }
}

fn templates_use_meeting_url(
    templates: &BTreeMap<Uuid, repo::TemplateVersionRow>,
) -> Result<bool, AppError> {
    for template in templates.values() {
        let specs = domain::templates::parse_variable_schema(&template.variable_schema)
            .map_err(render_error)?;
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
            return Ok(true);
        }
    }
    Ok(false)
}

async fn reconstruct_requester_ctx(
    tx: &mut Tx<'_>,
    launch_id: Uuid,
    job_id: Uuid,
    snapshot: &serde_json::Value,
) -> Result<Ctx, AppError> {
    let launch = repo::get_launch(tx, launch_id)
        .await?
        .ok_or_else(|| AppError::not_found("campaign launch not found"))?;
    let principal_id = snapshot
        .get("principal_id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| AppError::validation("requester context is missing principal_id", vec![]))?;
    let permission_version = snapshot
        .get("permission_version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| {
            AppError::validation("requester context is missing permission_version", vec![])
        })?;
    let request_id = snapshot
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| AppError::validation("requester context is missing request_id", vec![]))?
        .to_string();
    if principal_id != launch.requested_by {
        return Err(AppError::forbidden(
            "freeze requester does not match launch requester",
        ));
    }
    let credential_id = snapshot
        .get("credential_id")
        .and_then(serde_json::Value::as_str)
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| AppError::validation("invalid requester credential_id", vec![]))?;
    let (job_requester, job_credential) = repo::job_requester(tx, job_id)
        .await?
        .ok_or_else(|| AppError::internal("freeze job attribution missing"))?;
    if job_requester != Some(principal_id) || job_credential != credential_id {
        return Err(AppError::forbidden(
            "freeze job attribution does not match requester context",
        ));
    }
    let principal = infrastructure::repo::auth::get_principal(tx, principal_id)
        .await?
        .ok_or_else(|| AppError::new(ErrorCode::PrincipalDisabled, "requester is unavailable"))?;
    if principal.status != "active" {
        return Err(AppError::new(
            ErrorCode::PrincipalDisabled,
            "requester principal is disabled",
        ));
    }
    if principal.permission_version != permission_version {
        return Err(AppError::forbidden(
            "requester permissions changed before campaign freeze",
        ));
    }
    let principal_kind = principal.principal_kind();
    let role = principal.role();
    let scopes = if let Some(credential_id) = credential_id {
        let mut scopes = ScopeSet::new();
        for scope in [
            Scope::CampaignCompose,
            Scope::MailCompose,
            Scope::BookingRead,
            Scope::BookingPropose,
        ] {
            if infrastructure::repo::auth::credential_authorizes(
                tx,
                credential_id,
                Some(principal_id),
                &[scope.as_str()],
            )
            .await?
            {
                scopes.insert(scope);
            }
        }
        scopes
    } else if principal_kind == PrincipalKind::Human {
        role.implied_scopes()
    } else {
        return Err(AppError::forbidden(
            "campaign freeze requires the original authenticated requester",
        ));
    };
    let agent_run_id = snapshot
        .get("agent_run_id")
        .and_then(serde_json::Value::as_str)
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| AppError::validation("invalid requester agent_run_id", vec![]))?;
    let auth = match credential_id {
        Some(credential_id) => AuthMethod::ServiceCredential { credential_id },
        None => AuthMethod::Session {
            session_id: Uuid::nil(),
        },
    };
    let workspace_id = tx
        .workspace_id()
        .ok_or_else(|| AppError::internal("campaign freeze requires a workspace tx"))?;
    Ok(Ctx {
        request_id,
        workspace_id,
        principal_id,
        principal_kind,
        role,
        scopes,
        permission_version: principal.permission_version,
        auth,
        agent_run_id,
    })
}

/// Render every step of the revision for one recipient. Payload hashes are
/// calculated later, after outbound/thread/message identifiers and optional
/// engagement links have been frozen.
fn render_steps(
    steps: &[repo::StepRow],
    templates: &BTreeMap<Uuid, repo::TemplateVersionRow>,
    user_variables: &BTreeMap<String, String>,
    system_variables: &BTreeMap<String, String>,
    public_base_url: &str,
) -> Result<Vec<RenderedStep>, AppError> {
    let mut out = Vec::with_capacity(steps.len());
    for step in steps {
        let Some(tpl) = templates.get(&step.template_version_id) else {
            return Err(AppError::internal("step template version missing"));
        };
        let specs =
            domain::templates::parse_variable_schema(&tpl.variable_schema).map_err(render_error)?;
        let variables = serde_json::Map::from_iter(
            user_variables
                .iter()
                .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone()))),
        );
        let mut system_values = system_variables.clone();
        let unsubscribe_token = crypto::generate_token();
        system_values.insert(
            "compliance.unsubscribe_url".to_string(),
            format!(
                "{}/public/unsubscribe/{unsubscribe_token}",
                public_base_url.trim_end_matches('/')
            ),
        );
        let rendered = domain::templates::render(
            &specs,
            &tpl.subject,
            &tpl.body_text,
            tpl.body_html.as_deref(),
            &variables,
            &system_values,
        )
        .map_err(render_error)?;
        if domain::templates::contains_header_break(&rendered.subject) {
            return Err(AppError::validation(
                "rendered subject must not contain CR/LF",
                vec![Violation {
                    field: "subject".into(),
                    code: "HEADER_BREAK".into(),
                }],
            ));
        }
        let subject = rendered.subject;
        let unsubscribe_url = system_values
            .get("compliance.unsubscribe_url")
            .ok_or_else(|| AppError::internal("marketing unsubscribe URL missing"))?;
        let (body_text, body_html) = domain::sending::append_unsubscribe_footer(
            &rendered.body_text,
            rendered.body_html.as_deref(),
            unsubscribe_url,
        );
        out.push(RenderedStep {
            step: step.clone(),
            subject,
            body_text,
            body_html,
            unsubscribe_token,
        });
    }
    Ok(out)
}

/// The freeze job body. `actor` is the workspace system principal.
#[allow(clippy::too_many_arguments)]
pub async fn freeze_launch(
    tx: &mut Tx<'_>,
    actor: Uuid,
    job_id: Uuid,
    launch_id: Uuid,
    email_address_ids: &[Uuid],
    user_variables: &BTreeMap<String, String>,
    tracking_requested: bool,
    requester_snapshot: &serde_json::Value,
) -> Result<(), AppError> {
    // 1. Lock launch (A.6.2) — non-building means a previous run committed.
    let Some(launch) = repo::get_launch_for_update(tx, launch_id).await? else {
        return Err(AppError::not_found("campaign launch not found"));
    };
    if launch.status != d::launch_status::BUILDING {
        return Ok(());
    }
    // 2. Lock the revision row (guard order: revision → steps/enrollments).
    let Some(revision) = repo::get_revision_for_update(tx, launch.revision_id).await? else {
        return Err(AppError::internal("launch revision missing"));
    };
    let requester_ctx =
        reconstruct_requester_ctx(tx, launch_id, job_id, requester_snapshot).await?;
    let source_credential_id = match &requester_ctx.auth {
        AuthMethod::ServiceCredential { credential_id } => Some(*credential_id),
        _ => None,
    };
    super::campaign::require_compose_grant(tx, &requester_ctx, revision.mail_account_id).await?;
    let steps = repo::list_steps(tx, revision.id).await?;
    if steps.is_empty() {
        // Cannot happen via the API (steps required at creation); fail the
        // job so the launch stays `building` for manual inspection instead
        // of freezing an empty campaign.
        return Err(AppError::internal("revision has no steps"));
    }
    let channel = repo::get_channel(tx, revision.delivery_channel_id)
        .await?
        .ok_or_else(|| AppError::internal("revision channel missing"))?;
    if channel.provider != "delivery_provider" || channel.purpose != "marketing" {
        return Err(AppError::conflict(
            "campaign launches require an Office SMTP marketing channel",
        ));
    }
    let (from_address, display_name) = repo::get_sender_identity(tx, channel.sender_identity_id)
        .await?
        .ok_or_else(|| AppError::internal("channel sender identity missing"))?;
    let tz = d::parse_timezone(&revision.timezone)
        .ok_or_else(|| AppError::internal("stored timezone invalid"))?;
    let _ = tz; // schedule math happens at activation/schedule time.

    let template_ids: BTreeSet<Uuid> = steps.iter().map(|s| s.template_version_id).collect();
    let mut templates = BTreeMap::new();
    for id in template_ids {
        let tpl = repo::get_template_version(tx, id)
            .await?
            .ok_or_else(|| AppError::internal("step template version missing"))?;
        templates.insert(id, tpl);
    }
    let rules = d::ScheduleRules::from_json(&revision.schedule_rules)
        .map_err(|error| AppError::internal(format!("stored schedule_rules invalid: {error}")))?;
    let meeting_url_required = templates_use_meeting_url(&templates)?;
    if meeting_url_required {
        let Some(meeting_type_id) = rules.meeting_type_id else {
            return Err(AppError::new(
                ErrorCode::TemplateVariableMissing,
                "meeting.booking_url requires schedule_rules.meeting_type_id",
            )
            .with_violations(vec![Violation {
                field: "schedule_rules.meeting_type_id".into(),
                code: "MEETING_TYPE_BINDING_REQUIRED".into(),
            }]));
        };
        crate::booking::validate_bound_link_in_tx(tx, &requester_ctx, meeting_type_id).await?;
    }
    let public_base_url = infrastructure::repo::sending::public_base_url(tx).await?;

    // 3. Fix the recipient set — dedupe by email_address_id (T31: one
    // address on N contacts enrolls once), first occurrence wins.
    let mut seen = BTreeSet::new();
    let mut manifest_recipients = Vec::new();
    let mut enrolled_count = 0usize;
    for addr_id in email_address_ids {
        if !seen.insert(*addr_id) {
            continue;
        }
        // A.6.4 lock order: address row first, then suppression decisions.
        if !compliance_core::lock_email_address(tx, *addr_id).await? {
            manifest_recipients.push(d::ManifestRecipient {
                email_address_id: *addr_id,
                steps: vec![],
                excluded_reason: Some(recipient_reason::ADDRESS_NOT_FOUND.to_string()),
            });
            continue;
        }
        let reason = recipient_block_reason(tx, *addr_id).await?;
        if let Some(reason) = reason {
            // Record the excluded recipient on the frozen launch so the
            // reviewer sees the full target set and the exclusion reason.
            repo::insert_enrollment(
                tx,
                launch_id,
                revision.id,
                *addr_id,
                None,
                excluded_status(&reason),
                Some(&reason),
            )
            .await?;
            manifest_recipients.push(d::ManifestRecipient {
                email_address_id: *addr_id,
                steps: vec![],
                excluded_reason: Some(reason),
            });
            continue;
        }

        let (to_address, basis_id) = {
            let (addr, _) = repo::get_email_address(tx, *addr_id)
                .await?
                .ok_or_else(|| AppError::internal("address vanished"))?;
            let basis = repo::valid_marketing_basis(tx, *addr_id).await?;
            (addr, basis)
        };
        let contact_id = repo::first_contact_for_address(tx, *addr_id).await?;
        let mut system_vars = BTreeMap::from([
            ("recipient.email".to_string(), to_address.clone()),
            ("sender.address".to_string(), from_address.clone()),
            ("sender.display_name".to_string(), display_name.clone()),
        ]);
        if let Some(cid) = contact_id
            && let Some((cname, company)) = repo::contact_and_company_names(tx, cid).await?
        {
            system_vars.insert("contact.name".to_string(), cname);
            if let Some(co) = company {
                system_vars.insert("company.name".to_string(), co);
            }
        }
        if meeting_url_required {
            let meeting_type_id = rules.meeting_type_id.ok_or_else(|| {
                AppError::new(
                    ErrorCode::TemplateVariableMissing,
                    "meeting.booking_url requires schedule_rules.meeting_type_id",
                )
            })?;
            let url = crate::booking::issue_bound_link_in_tx(
                tx,
                &requester_ctx,
                meeting_type_id,
                *addr_id,
                &requester_ctx.request_id,
            )
            .await?;
            system_vars.insert("meeting.booking_url".into(), url);
        }
        let rendered = render_steps(
            &steps,
            &templates,
            user_variables,
            &system_vars,
            &public_base_url,
        )?;

        let enrollment_id = repo::insert_enrollment(
            tx,
            launch_id,
            revision.id,
            *addr_id,
            contact_id,
            d::enrollment_status::PENDING,
            None,
        )
        .await?;
        let mut step_hashes = Vec::with_capacity(rendered.len());
        // All steps for one recipient belong to one mailbox-local
        // conversation; each step receives its own stable RFC Message-ID.
        let thread_id = infrastructure::repo::inbox::create_thread(
            tx,
            revision.mail_account_id,
            None,
            contact_id,
        )
        .await?;
        let thread_id_string = thread_id.to_string();
        for snapshot in rendered {
            let outbound_id = Uuid::new_v4();
            let internet_message_id =
                super::super::sending::stable_internet_message_id(outbound_id, &from_address)?;
            let prepared = crate::engagement::prepare_frozen_outbound_in_tx(
                tx,
                outbound_id,
                *addr_id,
                &snapshot.body_text,
                snapshot.body_html.as_deref(),
                tracking_requested,
            )
            .await?;
            let payload_hash = crypto::sha256_bytes(&d::payload_bytes_with_context(
                &from_address,
                &to_address,
                &snapshot.subject,
                &prepared.body_text,
                prepared.body_html.as_deref(),
                Some(thread_id_string.as_str()),
                Some(internet_message_id.as_str()),
            ));
            let outbound_id = repo::insert_step_outbound_with_context(
                tx,
                outbound_id,
                revision.mail_account_id,
                revision.delivery_channel_id,
                channel.sender_identity_id,
                *addr_id,
                basis_id,
                launch_id,
                revision.id,
                enrollment_id,
                snapshot.step.id,
                Some(thread_id),
                launch.requested_by,
                source_credential_id,
                Some(&internet_message_id),
                &from_address,
                &to_address,
                &snapshot.subject,
                &prepared.body_text,
                prepared.body_html.as_deref(),
                &payload_hash,
            )
            .await?;
            crate::engagement::persist_frozen_outbound_in_tx(tx, outbound_id, *addr_id, &prepared)
                .await?;
            infrastructure::repo::sending::insert_unsubscribe_token_hash(
                tx,
                *addr_id,
                outbound_id,
                &snapshot.unsubscribe_token,
            )
            .await?;
            step_hashes.push((snapshot.step.step_order, payload_hash));
        }
        manifest_recipients.push(d::ManifestRecipient {
            email_address_id: *addr_id,
            steps: step_hashes,
            excluded_reason: None,
        });
        enrolled_count += 1;
    }

    // 4. Manifest over the full (enrolled + excluded) target set.
    let manifest = d::manifest_bytes(
        launch_id,
        revision.id,
        revision.delivery_channel_id,
        &revision.schedule_rules,
        launch.expires_at,
        &manifest_recipients,
    );
    let manifest_hash = crypto::sha256_bytes(&manifest);

    // 5. Freeze revision first (steps become immutable), then the launch
    //    (enrollments/outbound structure becomes immutable).
    repo::freeze_revision(tx, revision.id).await?;
    let n = repo::freeze_launch_row(tx, launch_id, &manifest_hash).await?;
    if n != 1 {
        return Err(AppError::conflict("launch was already frozen"));
    }

    // 6. The approval request pins this exact manifest (A.6.2).
    let approval_request_id = approvals_repo::insert_request(
        tx,
        launch.requested_by,
        approvals_repo::ApprovalTarget::CampaignLaunch(launch_id),
        &manifest_hash,
        launch.expires_at,
    )
    .await?;
    repo::transition_launch(tx, launch_id, d::launch_status::AWAITING_APPROVAL, None).await?;
    repo::sync_campaign_status(tx, revision.campaign_id).await?;

    audit::record(
        tx,
        audited_system(
            actor,
            Some(job_id),
            "campaign.frozen",
            "campaign_launch",
            launch_id,
            serde_json::json!({
                "campaign_id": revision.campaign_id,
                "revision_id": revision.id,
                "manifest_hash": hex_of(&manifest_hash),
                "recipients": email_address_ids.len(),
                "enrolled": enrolled_count,
                "approval_request_id": approval_request_id,
            }),
        ),
    )
    .await?;
    outbox::emit(
        tx,
        "campaign.frozen",
        "campaign_launch",
        launch_id,
        1,
        serde_json::json!({
            "campaign_launch_id": launch_id,
            "manifest_hash_hex": hex_of(&manifest_hash),
            "approval_request_id": approval_request_id,
        }),
    )
    .await?;
    let _ = Utc::now();
    Ok(())
}
