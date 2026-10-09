//! sending module use cases (spec §6 — owned by the module worker).
//!
//! B.3.10/B.3.11: message_drafts → freeze (immutable outbound snapshot) →
//! preflight → approval-requests / submit → queued → dispatch job →
//! provider outcome via send_attempts. Reply claims implement A.6.5.

pub mod dispatch;
pub mod policy;

use chrono::{DateTime, Duration, Utc};
use contracts::sending::*;
use contracts::{AppError, Ctx, ErrorCode, PrincipalKind, Scope, Violation};
use uuid::Uuid;

use infrastructure::audit::{self, AuditEntry};
use infrastructure::crypto;
use infrastructure::db::{Db, Tx};
use infrastructure::idempotency as idem;
use infrastructure::repo::approvals::{self, ApprovalTarget};
use infrastructure::repo::compliance_core;
use infrastructure::repo::sending::{self as repo, DraftRow, NewOutbound};

use crate::authz;

const PURPOSES: &[&str] = &["correspondence", "marketing", "transactional"];
const DEFAULT_APPROVAL_TTL: Duration = Duration::days(7);

/// Local hex encoder (no `hex` dep in application).
fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    let mut out = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

fn hex_decode(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        let hi = (b[i * 2] as char).to_digit(16)?;
        let lo = (b[i * 2 + 1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

fn violation(field: &str, code: &str) -> Violation {
    Violation {
        field: field.into(),
        code: code.into(),
    }
}

// ===========================================================================
// message_drafts
// ===========================================================================

fn draft_response(r: DraftRow, outbound_message_id: Option<Uuid>) -> MessageDraftResponse {
    MessageDraftResponse {
        draft_id: r.id,
        outbound_message_id,
        mail_account_id: r.mail_account_id,
        owner_principal_id: r.owner_principal_id,
        visibility: r.visibility,
        thread_id: r.thread_id,
        reply_to_message_id: r.reply_to_message_id,
        expected_last_inbound_id: r.expected_last_inbound_id,
        to_email_address_id: r.to_email_address_id,
        template_version_id: r.template_version_id,
        purpose: r.purpose,
        subject: r.subject,
        body_text: r.body_text,
        body_html: r.body_html,
        variables: r.variables,
        status: r.status,
        version: r.version,
        created_at: r.created_at,
        updated_at: r.updated_at,
    }
}

/// Compose permission: mailbox `can_compose` grant (grants carry agents
/// too) + `mail.compose` scope for agent principals.
async fn require_compose(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Uuid,
) -> Result<(), AppError> {
    if ctx.principal_kind == PrincipalKind::Agent {
        authz::require_scope(ctx, Scope::MailCompose)?;
    }
    authz::require_mailbox(tx, ctx, mail_account_id, authz::MailboxPerm::Compose).await?;
    Ok(())
}

/// Read permission for draft/outbound content.
async fn require_read(tx: &mut Tx<'_>, ctx: &Ctx, mail_account_id: Uuid) -> Result<(), AppError> {
    authz::require_mailbox(tx, ctx, mail_account_id, authz::MailboxPerm::Read).await?;
    Ok(())
}

/// Draft visibility requires a current read grant even for the owner. Grant
/// revocation must remove access to the draft body as well as inbound mail.
async fn can_see_draft(tx: &mut Tx<'_>, ctx: &Ctx, d: &DraftRow) -> Result<bool, AppError> {
    if !ctx.scopes.contains(Scope::MailRead) {
        return Ok(false);
    }
    let Some(grant) = authz::mailbox_grant(tx, ctx, d.mail_account_id).await? else {
        return Ok(false);
    };
    if !grant.can_read {
        return Ok(false);
    }
    if d.owner_principal_id == ctx.principal_id {
        return Ok(true);
    }
    Ok(d.visibility == "mailbox")
}

/// Draft mutation requires a current compose grant, including for the owner.
async fn can_edit_draft(tx: &mut Tx<'_>, ctx: &Ctx, d: &DraftRow) -> Result<bool, AppError> {
    if !ctx.scopes.contains(Scope::MailCompose) {
        return Ok(false);
    }
    let Some(grant) = authz::mailbox_grant(tx, ctx, d.mail_account_id).await? else {
        return Ok(false);
    };
    if !grant.can_compose {
        return Ok(false);
    }
    if d.owner_principal_id == ctx.principal_id {
        return Ok(true);
    }
    Ok(d.visibility == "mailbox")
}

fn validate_content_fields(
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
) -> Result<(), AppError> {
    if subject.trim().is_empty() {
        return Err(AppError::validation(
            "subject must not be empty",
            vec![violation("subject", "REQUIRED")],
        ));
    }
    if domain::templates::contains_header_break(subject) {
        return Err(AppError::validation(
            "subject must not contain CR/LF (header injection)",
            vec![violation("subject", "HEADER_BREAK")],
        ));
    }
    if body_text.trim().is_empty() {
        return Err(AppError::validation(
            "body_text must not be empty",
            vec![violation("body_text", "REQUIRED")],
        ));
    }
    if let Some(h) = body_html
        && domain::templates::is_dangerous_html(h)
    {
        return Err(AppError::validation(
            "body_html contains disallowed constructs",
            vec![violation("body_html", "DANGEROUS_HTML")],
        ));
    }
    Ok(())
}

/// Reject caller-supplied system variables early (§8.1).
fn validate_variables(vars: &serde_json::Map<String, serde_json::Value>) -> Result<(), AppError> {
    for k in vars.keys() {
        if domain::templates::SYSTEM_VARIABLES.contains(&k.as_str()) {
            return Err(AppError::validation(
                format!("variable '{k}' is generated by the system"),
                vec![violation(&format!("variables.{k}"), "SYSTEM_VARIABLE")],
            ));
        }
    }
    Ok(())
}

fn template_uses_variable(source: &str, variable: &str) -> bool {
    let mut rest = source;
    while let Some(open) = rest.find("{{") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            return false;
        };
        if after[..close].trim() == variable {
            return true;
        }
        rest = &after[close + 2..];
    }
    false
}

pub async fn list_drafts(
    db: &Db,
    ctx: &Ctx,
    limit: i64,
    after: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<MessageDraftListItem>, AppError> {
    list_drafts_filtered(db, ctx, limit, None, after).await
}

pub async fn list_drafts_filtered(
    db: &Db,
    ctx: &Ctx,
    limit: i64,
    thread_id: Option<Uuid>,
    after: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<MessageDraftListItem>, AppError> {
    authz::require_scope(ctx, Scope::MailRead)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let rows = repo::list_drafts(&mut tx, ctx.principal_id, limit + 1, thread_id, after).await?;
    tx.commit().await?;
    Ok(rows
        .into_iter()
        .map(|r| MessageDraftListItem {
            draft_id: r.id,
            thread_id: r.thread_id,
            outbound_message_id: r.outbound_message_id,
            mail_account_id: r.mail_account_id,
            owner_principal_id: r.owner_principal_id,
            visibility: r.visibility,
            purpose: r.purpose,
            subject: r.subject,
            status: r.status,
            version: r.version,
            created_at: r.created_at,
            updated_at: r.updated_at,
        })
        .collect())
}

pub async fn create_draft(
    db: &Db,
    ctx: &Ctx,
    req: &CreateMessageDraftRequest,
) -> Result<MessageDraftResponse, AppError> {
    let tx = db.begin(ctx.workspace_id).await?;
    let (tx, response) = create_draft_in_tx(tx, ctx, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-owning variant used by the canonical idempotency runner.
pub async fn create_draft_in_tx<'db>(
    mut tx: Tx<'db>,
    ctx: &Ctx,
    req: &CreateMessageDraftRequest,
) -> Result<(Tx<'db>, MessageDraftResponse), AppError> {
    if !PURPOSES.contains(&req.purpose.as_str()) {
        return Err(AppError::validation(
            "purpose must be correspondence|marketing|transactional",
            vec![violation("purpose", "ENUM")],
        ));
    }
    validate_content_fields(&req.subject, &req.body_text, req.body_html.as_deref())?;
    let variables = req.variables.clone().unwrap_or_default();
    validate_variables(&variables)?;
    let visibility = req.visibility.as_deref().unwrap_or("private");
    if !matches!(visibility, "private" | "mailbox") {
        return Err(AppError::validation(
            "visibility must be private|mailbox",
            vec![violation("visibility", "ENUM")],
        ));
    }
    // Reply linkage — DDL CHECKs want thread_id whenever a reply field is set.
    if (req.reply_to_message_id.is_some() || req.expected_last_inbound_id.is_some())
        && req.thread_id.is_none()
    {
        return Err(AppError::validation(
            "thread_id is required with reply fields",
            vec![violation("thread_id", "REQUIRED")],
        ));
    }

    require_compose(&mut tx, ctx, req.mail_account_id).await?;

    // Referenced rows must exist inside this workspace+mailbox (friendly
    // 422s ahead of the composite FKs).
    if repo::email_address_row(&mut tx, req.to_email_address_id)
        .await?
        .is_none()
    {
        return Err(AppError::validation(
            "to_email_address_id not found",
            vec![violation("to_email_address_id", "NOT_FOUND")],
        ));
    }
    if let Some(tid) = req.thread_id {
        match repo::get_thread(&mut tx, tid).await? {
            Some((acct, _)) if acct == req.mail_account_id => {}
            Some(_) => {
                return Err(AppError::validation(
                    "thread_id belongs to a different mail account",
                    vec![violation("thread_id", "MISMATCH")],
                ));
            }
            None => {
                return Err(AppError::validation(
                    "thread_id not found",
                    vec![violation("thread_id", "NOT_FOUND")],
                ));
            }
        }
        for m in [req.reply_to_message_id, req.expected_last_inbound_id]
            .into_iter()
            .flatten()
        {
            if !repo::inbound_message_in_thread(&mut tx, req.mail_account_id, tid, m).await? {
                return Err(AppError::validation(
                    "reply linkage must reference an inbound message in the thread",
                    vec![violation("reply_to_message_id", "MISMATCH")],
                ));
            }
        }
    }
    if let Some(v) = req.template_version_id
        && infrastructure::repo::templates::get_version(&mut tx, v)
            .await?
            .is_none()
    {
        return Err(AppError::validation(
            "template_version_id not found",
            vec![violation("template_version_id", "NOT_FOUND")],
        ));
    }

    let row = repo::insert_draft(
        &mut tx,
        req.mail_account_id,
        ctx.principal_id,
        visibility,
        req.thread_id,
        req.reply_to_message_id,
        req.expected_last_inbound_id,
        req.to_email_address_id,
        req.template_version_id,
        &req.purpose,
        req.subject.trim(),
        &req.body_text,
        req.body_html.as_deref(),
        &serde_json::Value::Object(variables),
    )
    .await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "draft.created",
            "message_draft",
            ctx.request_id.clone(),
        )
        .with_target(row.id),
    )
    .await?;
    Ok((tx, draft_response(row, None)))
}

pub async fn get_draft(db: &Db, ctx: &Ctx, id: Uuid) -> Result<MessageDraftResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(d) = repo::get_draft(&mut tx, id).await? else {
        return Err(AppError::not_found("message draft not found"));
    };
    if !can_see_draft(&mut tx, ctx, &d).await? {
        // Private drafts of others are hidden as 404.
        return Err(AppError::not_found("message draft not found"));
    }
    let outbound_message_id = repo::find_frozen_outbound_for_draft(&mut tx, id).await?;
    tx.commit().await?;
    Ok(draft_response(d, outbound_message_id))
}

pub async fn patch_draft(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &PatchMessageDraftRequest,
) -> Result<MessageDraftResponse, AppError> {
    if let Some(subj) = &req.subject
        && (subj.trim().is_empty() || domain::templates::contains_header_break(subj))
    {
        return Err(AppError::validation(
            "subject must be non-empty and free of CR/LF",
            vec![violation("subject", "HEADER_BREAK")],
        ));
    }
    if let Some(b) = &req.body_text
        && b.trim().is_empty()
    {
        return Err(AppError::validation(
            "body_text must not be empty",
            vec![violation("body_text", "REQUIRED")],
        ));
    }
    if let Some(h) = &req.body_html
        && domain::templates::is_dangerous_html(h)
    {
        return Err(AppError::validation(
            "body_html contains disallowed constructs",
            vec![violation("body_html", "DANGEROUS_HTML")],
        ));
    }
    if let Some(v) = &req.variables {
        validate_variables(v)?;
    }
    if let Some(v) = &req.visibility
        && !matches!(v.as_str(), "private" | "mailbox")
    {
        return Err(AppError::validation(
            "visibility must be private|mailbox",
            vec![violation("visibility", "ENUM")],
        ));
    }

    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(d) = repo::get_draft_for_update(&mut tx, id).await? else {
        return Err(AppError::not_found("message draft not found"));
    };
    if !can_edit_draft(&mut tx, ctx, &d).await? {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no compose access to this draft",
        ));
    }
    if d.status != "draft" {
        return Err(AppError::conflict(format!(
            "draft is '{}' — frozen drafts are immutable",
            d.status
        )));
    }
    if d.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "draft version changed — reload and retry",
        ));
    }
    // Validate referential updates inside the tx.
    if let Some(addr) = req.to_email_address_id
        && repo::email_address_row(&mut tx, addr).await?.is_none()
    {
        return Err(AppError::validation(
            "to_email_address_id not found",
            vec![violation("to_email_address_id", "NOT_FOUND")],
        ));
    }
    if let Some(exp) = req.expected_last_inbound_id
        && let Some(tid) = d.thread_id
        && !repo::inbound_message_in_thread(&mut tx, d.mail_account_id, tid, exp).await?
    {
        return Err(AppError::validation(
            "expected_last_inbound_id must be an inbound message in the thread",
            vec![violation("expected_last_inbound_id", "MISMATCH")],
        ));
    }
    let Some(row) = repo::patch_draft(
        &mut tx,
        id,
        expected_version,
        req.subject.as_deref().map(str::trim),
        req.body_text.as_deref(),
        req.body_html.as_deref().map(Some), // Some(v) → set; null stays unchanged
        req.variables
            .as_ref()
            .map(|m| serde_json::Value::Object(m.clone()))
            .as_ref(),
        req.to_email_address_id,
        req.expected_last_inbound_id,
        req.visibility.as_deref(),
    )
    .await?
    else {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "draft version changed — reload and retry",
        ));
    };
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "draft.updated",
            "message_draft",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    tx.commit().await?;
    Ok(draft_response(row, None))
}

// ===========================================================================
// freeze → immutable outbound snapshot
// ===========================================================================

/// `POST /message-drafts/{id}/freeze` — renders the final content
/// (template + variables when present), mints the unsubscribe capability
/// for marketing, and inserts the immutable `outbound_messages` row.
/// `purpose` is copied from the delivery channel row — the composite-FK
/// safety fact (outbound.purpose = channel.purpose).
pub async fn freeze_draft(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &FreezeMessageDraftRequest,
) -> Result<FreezeMessageDraftResponse, AppError> {
    let tx = db.begin(ctx.workspace_id).await?;
    let (tx, response) = freeze_draft_in_tx(tx, ctx, id, expected_version, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-owning freeze variant used by the canonical idempotency
/// runner so the immutable outbound and replay response commit atomically.
pub async fn freeze_draft_in_tx<'db>(
    mut tx: Tx<'db>,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &FreezeMessageDraftRequest,
) -> Result<(Tx<'db>, FreezeMessageDraftResponse), AppError> {
    let Some(d) = repo::get_draft_for_update(&mut tx, id).await? else {
        return Err(AppError::not_found("message draft not found"));
    };
    if !can_edit_draft(&mut tx, ctx, &d).await? {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no compose access to this draft",
        ));
    }
    if d.status != "draft" {
        return Err(AppError::conflict(format!(
            "draft is '{}' — already frozen or discarded",
            d.status
        )));
    }
    if d.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "draft version changed — reload and retry",
        ));
    }

    // ---- channel + sender (composite-FK facts) --------------------------
    let Some(chan) = repo::channel_state(&mut tx, req.delivery_channel_id).await? else {
        return Err(AppError::validation(
            "delivery_channel_id not found",
            vec![violation("delivery_channel_id", "NOT_FOUND")],
        ));
    };
    if chan.mail_account_id != d.mail_account_id {
        return Err(AppError::validation(
            "delivery channel belongs to a different mail account",
            vec![violation("delivery_channel_id", "MISMATCH")],
        ));
    }
    // The outbound's purpose MUST equal the channel's purpose (fk_154).
    if chan.purpose != d.purpose {
        return Err(AppError::validation(
            format!(
                "draft purpose '{}' does not match channel purpose '{}'",
                d.purpose, chan.purpose
            ),
            vec![violation("delivery_channel_id", "PURPOSE_MISMATCH")],
        ));
    }
    if let Some(sid) = req.sender_identity_id
        && sid != chan.sender_identity_id
    {
        return Err(AppError::validation(
            "sender_identity_id is bound to the delivery channel",
            vec![violation("sender_identity_id", "MISMATCH")],
        ));
    }
    let sender_identity_id = chan.sender_identity_id;
    let Some((sender_status, sender_address)) =
        repo::sender_identity_status(&mut tx, sender_identity_id).await?
    else {
        return Err(AppError::validation(
            "channel sender identity not found",
            vec![violation("sender_identity_id", "NOT_FOUND")],
        ));
    };
    if sender_status != "verified" {
        return Err(AppError::validation(
            "sender identity is not verified",
            vec![violation("sender_identity_id", "SENDER_UNVERIFIED")],
        ));
    }

    // ---- reply-context check at freeze (T16) -----------------------------
    if d.thread_id.is_some() {
        let Some(tid) = d.thread_id else {
            unreachable!()
        };
        let Some((acct, last_inbound)) = repo::get_thread_for_update(&mut tx, tid).await? else {
            return Err(AppError::validation(
                "thread_id not found",
                vec![violation("thread_id", "NOT_FOUND")],
            ));
        };
        if acct != d.mail_account_id {
            return Err(AppError::validation(
                "thread belongs to a different mail account",
                vec![violation("thread_id", "MISMATCH")],
            ));
        }
        let expected = d.expected_last_inbound_id.or(d.reply_to_message_id);
        if let (Some(exp), Some(last)) = (expected, last_inbound)
            && exp != last
        {
            return Err(AppError::new(
                ErrorCode::ReplyContextChanged,
                "a newer inbound message arrived — review before sending",
            ));
        }
    }

    // ---- render + system variables ---------------------------------------
    let outbound_id = Uuid::new_v4();
    let mut unsubscribe_token: Option<String> = None;
    let mut unsubscribe_url: Option<String> = None;
    let mut system_vars = std::collections::BTreeMap::new();
    let (subject, mut body_text, mut body_html) = if let Some(vid) = d.template_version_id {
        let Some(v) = infrastructure::repo::templates::get_version(&mut tx, vid).await? else {
            return Err(AppError::validation(
                "template_version_id not found",
                vec![violation("template_version_id", "NOT_FOUND")],
            ));
        };
        let specs = domain::templates::parse_variable_schema(&v.variable_schema)
            .map_err(|e| AppError::internal(format!("stored template schema invalid: {e}")))?;

        let booking_var = specs.iter().find(|s| s.name == "meeting.booking_url");
        let booking_required = booking_var.is_some_and(|s| s.required);
        let booking_referenced = [
            v.subject.as_str(),
            v.body_text.as_str(),
            v.body_html.as_deref().unwrap_or(""),
        ]
        .iter()
        .any(|part| template_uses_variable(part, "meeting.booking_url"));
        let needs_booking_url = booking_var.is_some() && (booking_required || booking_referenced);
        if needs_booking_url {
            let meeting_type_id = req.meeting_type_id.ok_or_else(|| {
                AppError::validation(
                    "meeting_type_id is required when the template uses meeting.booking_url",
                    vec![violation("meeting_type_id", "REQUIRED")],
                )
                .with_status(422)
            })?;
            let booking_url = crate::booking::issue_bound_link_in_tx(
                &mut tx,
                ctx,
                meeting_type_id,
                d.to_email_address_id,
                &ctx.request_id,
            )
            .await?;
            system_vars.insert("meeting.booking_url".into(), booking_url);
        } else if req.meeting_type_id.is_some() {
            return Err(AppError::validation(
                "meeting_type_id is only valid for templates that require or use meeting.booking_url",
                vec![violation("meeting_type_id", "UNUSED")],
            )
            .with_status(422));
        }

        // Marketing always gets a capability, even when its template omits
        // the placeholder. Templates that declare the system variable also
        // receive the same generated confirmation-page URL for rendering.
        let unsubscribe_variable_declared =
            specs.iter().any(|s| s.name == "compliance.unsubscribe_url");
        if d.purpose == "marketing" || unsubscribe_variable_declared {
            let token = crypto::generate_token();
            let base = repo::public_base_url(&mut tx).await?;
            let url = format!("{}/public/unsubscribe/{token}", base.trim_end_matches('/'));
            if unsubscribe_variable_declared {
                system_vars.insert("compliance.unsubscribe_url".to_string(), url.clone());
            }
            unsubscribe_token = Some(token);
            unsubscribe_url = Some(url);
        }
        let variables = match &d.variables {
            serde_json::Value::Object(m) => m.clone(),
            _ => serde_json::Map::new(),
        };
        let r = domain::templates::render(
            &specs,
            &v.subject,
            &v.body_text,
            v.body_html.as_deref(),
            &variables,
            &system_vars,
        )
        .map_err(|e| match e {
            domain::templates::TemplateError::MissingRequired(name) => AppError::new(
                ErrorCode::TemplateVariableMissing,
                format!("required template variable '{name}' has no value"),
            )
            .with_violations(vec![violation(
                &format!("variables.{name}"),
                "TEMPLATE_VARIABLE_MISSING",
            )]),
            other => AppError::validation(
                other.to_string(),
                vec![violation("variables", "TEMPLATE_RENDER")],
            ),
        })?;
        (r.subject, r.body_text, r.body_html)
    } else {
        if req.meeting_type_id.is_some() {
            return Err(AppError::validation(
                "meeting_type_id requires a template using meeting.booking_url",
                vec![violation("meeting_type_id", "UNUSED")],
            )
            .with_status(422));
        }
        (d.subject.clone(), d.body_text.clone(), d.body_html.clone())
    };

    // Every marketing message must contain a usable confirmation-page link
    // in each rendered body representation. The domain helper leaves a
    // template-rendered placeholder intact and appends only missing footers.
    if d.purpose == "marketing" {
        if unsubscribe_url.is_none() {
            let token = crypto::generate_token();
            let base = repo::public_base_url(&mut tx).await?;
            unsubscribe_url = Some(format!(
                "{}/public/unsubscribe/{token}",
                base.trim_end_matches('/')
            ));
            unsubscribe_token = Some(token);
        }
        let url = unsubscribe_url
            .as_deref()
            .expect("marketing freeze minted an unsubscribe URL");
        (body_text, body_html) =
            domain::sending::append_unsubscribe_footer(&body_text, body_html.as_deref(), url);
    }

    // ---- freeze blockers (§8.1) ------------------------------------------
    if domain::templates::contains_header_break(&subject) {
        return Err(AppError::validation(
            "subject must not contain CR/LF (header injection)",
            vec![violation("subject", "HEADER_BREAK")],
        ));
    }
    if let Some(h) = &body_html
        && domain::templates::is_dangerous_html(h)
    {
        return Err(AppError::validation(
            "body_html contains disallowed constructs — freeze blocked",
            vec![violation("body_html", "DANGEROUS_HTML")],
        ));
    }

    // ---- basis attachment (FK requires purpose match) ---------------------
    let communication_basis_id = match d.purpose.as_str() {
        "correspondence" => None,
        purpose => repo::any_valid_basis(&mut tx, d.to_email_address_id, purpose).await?,
    };

    let Some((to_addr, _normalized_key)) =
        repo::email_address_row(&mut tx, d.to_email_address_id).await?
    else {
        return Err(AppError::validation(
            "to_email_address_id not found",
            vec![violation("to_email_address_id", "NOT_FOUND")],
        ));
    };

    let payload_hash = crypto::sha256_bytes(&domain::sending::payload_preimage(
        &chan.purpose,
        &d.mail_account_id.to_string(),
        &req.delivery_channel_id.to_string(),
        &sender_identity_id.to_string(),
        &sender_address,
        &d.to_email_address_id.to_string(),
        &to_addr,
        &subject,
        &body_text,
        body_html.as_deref(),
        d.thread_id.map(|t| t.to_string()).as_deref(),
        d.reply_to_message_id.map(|t| t.to_string()).as_deref(),
        communication_basis_id.map(|t| t.to_string()).as_deref(),
        d.template_version_id.map(|t| t.to_string()).as_deref(),
    ));

    let credential_id = match ctx.auth {
        contracts::AuthMethod::ServiceCredential { credential_id } => Some(credential_id),
        _ => None,
    };

    let new_outbound = NewOutbound {
        id: outbound_id,
        mail_account_id: d.mail_account_id,
        delivery_channel_id: req.delivery_channel_id,
        sender_identity_id,
        to_email_address_id: d.to_email_address_id,
        communication_basis_id,
        source_draft_id: Some(d.id),
        thread_id: d.thread_id,
        reply_to_message_id: d.reply_to_message_id,
        requested_by: ctx.principal_id,
        source_credential_id: credential_id,
        purpose: chan.purpose.clone(),
        from_address_snapshot: sender_address,
        to_address_snapshot: to_addr,
        subject,
        body_text,
        body_html,
        payload_hash,
    };
    let outbound = match repo::insert_outbound(&mut tx, &new_outbound).await {
        Ok(o) => o,
        Err(e) if e.code == ErrorCode::Conflict => {
            return Err(AppError::conflict("draft already has an outbound snapshot"));
        }
        Err(e) => return Err(e),
    };

    // Minted unsubscribe capability → store its hash now that the outbound
    // row exists (the FK target).
    if let Some(token) = unsubscribe_token {
        repo::insert_unsubscribe_token_hash(&mut tx, d.to_email_address_id, outbound_id, &token)
            .await?;
    }

    repo::set_draft_status(&mut tx, d.id, "frozen").await?;
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "draft.frozen",
            "outbound_message",
            ctx.request_id.clone(),
        )
        .with_target(outbound_id),
    )
    .await?;
    Ok((
        tx,
        FreezeMessageDraftResponse {
            outbound_message_id: outbound_id,
            status: outbound.status,
            payload_hash: hex_encode(payload_hash),
            version: outbound.version,
        },
    ))
}

// ===========================================================================
// outbound_messages
// ===========================================================================

/// `POST /outbound-messages/{id}/preflight` — evaluates the §8.4 chain
/// non-bindingly (B.3.10). `is_execution_authorization` is never true.
pub async fn preflight(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<OutboundPreflightResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(o) = repo::get_outbound(&mut tx, id).await? else {
        return Err(AppError::not_found("outbound message not found"));
    };
    require_read(&mut tx, ctx, o.mail_account_id).await?;
    // Read-side only — still lock the address so the report is consistent
    // with what dispatch will see.
    let _ = compliance_core::lock_email_address(&mut tx, o.to_email_address_id).await?;
    let expected = policy::expected_last_inbound(&mut tx, &o).await?;
    let checks = policy::evaluate(&mut tx, &o, expected).await?;
    tx.commit().await?;

    let global: Vec<String> = checks
        .iter()
        .filter(|c| !c.passed)
        .filter_map(|c| c.code)
        .filter(|c| policy::is_global(c))
        .map(str::to_string)
        .collect();
    Ok(OutboundPreflightResponse {
        checked_at: Utc::now(),
        is_execution_authorization: false,
        global_blocking_reasons: global,
        checks: checks
            .into_iter()
            .map(|c| OutboundPreflightCheck {
                name: c.name.to_string(),
                passed: c.passed,
                code: c.code.map(str::to_string),
            })
            .collect(),
    })
}

/// `POST /outbound-messages/{id}/approval-requests` — humans via compose
/// grant; agents via `mail.compose` scope (B.3.10: the agent path).
/// Snapshot hash binds the approval to the exact payload (A.6.3).
pub async fn create_approval_request(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    req: &CreateOutboundApprovalRequest,
) -> Result<OutboundApprovalRequestResponse, AppError> {
    let tx = db.begin(ctx.workspace_id).await?;
    let (tx, response) = create_approval_request_in_tx(tx, ctx, id, req).await?;
    tx.commit().await?;
    Ok(response)
}

/// Transaction-owning approval-request variant used by the canonical
/// idempotency runner.
pub async fn create_approval_request_in_tx<'db>(
    mut tx: Tx<'db>,
    ctx: &Ctx,
    id: Uuid,
    req: &CreateOutboundApprovalRequest,
) -> Result<(Tx<'db>, OutboundApprovalRequestResponse), AppError> {
    let Some(o) = repo::get_outbound_for_update(&mut tx, id).await? else {
        return Err(AppError::not_found("outbound message not found"));
    };
    require_compose(&mut tx, ctx, o.mail_account_id).await?;

    if o.status != "draft" {
        return Err(AppError::conflict(format!(
            "outbound is '{}' — approval requests target draft snapshots",
            o.status
        )));
    }
    let Some(expected) = hex_decode(&req.expected_payload_hash) else {
        return Err(AppError::validation(
            "expected_payload_hash must be 64-char hex",
            vec![violation("expected_payload_hash", "FORMAT")],
        ));
    };
    if expected[..] != o.payload_hash[..] {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "expected_payload_hash does not match the current snapshot",
        ));
    }
    // B.3.8-style guard: provider-policy global blocks reject operational
    // approval requests with 422.
    let _ = compliance_core::lock_email_address(&mut tx, o.to_email_address_id).await?;
    let expected_inbound = policy::expected_last_inbound(&mut tx, &o).await?;
    let checks = policy::evaluate(&mut tx, &o, expected_inbound).await?;
    let blocked: Vec<&str> = checks
        .iter()
        .filter(|c| !c.passed)
        .filter_map(|c| c.code)
        .filter(|c| matches!(*c, "PROVIDER_POLICY_UNVERIFIED" | "PROVIDER_POLICY_BLOCKED"))
        .collect();
    if !blocked.is_empty() {
        return Err(AppError::validation(
            "provider policy gate is not satisfied — operational approval cannot be requested",
            blocked
                .into_iter()
                .map(|c| violation("delivery_channel_id", c))
                .collect(),
        ));
    }

    let expires_at = req
        .expires_at
        .unwrap_or_else(|| Utc::now() + DEFAULT_APPROVAL_TTL);
    if expires_at <= Utc::now() {
        return Err(AppError::validation(
            "expires_at must be in the future",
            vec![violation("expires_at", "PAST")],
        ));
    }
    let request_id = match approvals::insert_request(
        &mut tx,
        ctx.principal_id,
        ApprovalTarget::OutboundMessage(id),
        &expected,
        expires_at,
    )
    .await
    {
        Ok(r) => r,
        Err(e) if e.code == ErrorCode::Conflict => {
            return Err(AppError::conflict(
                "an active approval request already exists for this outbound",
            ));
        }
        Err(e) => return Err(e),
    };
    Ok((
        tx,
        OutboundApprovalRequestResponse {
            approval_request_id: request_id,
            status: "pending".to_string(),
            expires_at,
            version: 1,
        },
    ))
}

/// `POST /outbound-messages/{id}/submit` — idempotent queue-for-dispatch.
/// Agents are forbidden (§5.4) with a recorded denial.
pub async fn submit(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    req: &SubmitOutboundRequest,
    idempotency_key: &str,
) -> Result<idem::Stored<SubmitOutboundResponse>, AppError> {
    // §5.4/T03: agents can never submit — deny + audit.
    if ctx.principal_kind == PrincipalKind::Agent {
        let mut tx = db.begin(ctx.workspace_id).await?;
        let entry = AuditEntry::new(
            ctx.principal_id,
            "outbound.submit",
            "outbound_message",
            ctx.request_id.clone(),
        )
        .with_target(id)
        .denied(&ErrorCode::AgentDirectSendForbidden);
        audit::record(&mut tx, entry).await?;
        tx.commit().await?;
        return Err(AppError::new(
            ErrorCode::AgentDirectSendForbidden,
            "agents cannot submit outbound messages",
        ));
    }

    let request_hash = crypto::sha256_bytes(
        serde_json::to_string(&serde_json::json!({
            "outbound_message_id": id,
            "expected_payload_hash": req.expected_payload_hash,
            "not_before": req.not_before,
        }))
        .unwrap_or_default()
        .as_bytes(),
    );
    let scope = idem::Scope {
        workspace_id: ctx.workspace_id,
        principal_or_capability: ctx.principal_id.to_string(),
        endpoint: format!("POST /api/v1/outbound-messages/{id}/submit"),
        key: idempotency_key.to_string(),
    };
    let ctx2 = ctx.clone();
    let expected_hash = req.expected_payload_hash.clone();
    let not_before = req.not_before;
    idem::run(db, &scope, &request_hash, |mut tx| async move {
        let stored = submit_tx(&mut tx, &ctx2, id, &expected_hash, not_before).await?;
        Ok((tx, stored))
    })
    .await
    .map(|o| o.into_inner())
}

async fn submit_tx(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    id: Uuid,
    expected_payload_hash: &str,
    not_before: Option<DateTime<Utc>>,
) -> Result<idem::Stored<SubmitOutboundResponse>, AppError> {
    let Some(o) = repo::get_outbound_for_update(tx, id).await? else {
        return Err(AppError::not_found("outbound message not found"));
    };
    // can_send on the mailbox is the §5.3 normal-reply permission.
    authz::require_mailbox(tx, ctx, o.mail_account_id, authz::MailboxPerm::Send).await?;

    if o.status != "draft" {
        return Err(AppError::conflict(format!(
            "outbound is '{}' — only draft snapshots can be submitted",
            o.status
        )));
    }
    let Some(expected) = hex_decode(expected_payload_hash) else {
        return Err(AppError::validation(
            "expected_payload_hash must be 64-char hex",
            vec![violation("expected_payload_hash", "FORMAT")],
        ));
    };
    if expected[..] != o.payload_hash[..] {
        return Err(AppError::new(
            ErrorCode::SnapshotMismatch,
            "expected_payload_hash does not match the current snapshot",
        ));
    }

    // Execution-state gate: emergency stop rejects new submissions (B.5).
    let Some((emergency_stop, workers_locked, _)) = repo::workspace_flags(tx).await? else {
        return Err(AppError::internal("workspace row missing"));
    };
    if emergency_stop {
        return Err(AppError::new(
            ErrorCode::WorkspaceEmergencyStop,
            "workspace emergency stop is active",
        ));
    }
    if workers_locked {
        return Err(AppError::dependency_unavailable(
            "send workers are locked pending restore reconciliation",
        ));
    }

    // A.6.4 lock order: address row first, then suppression reads.
    if !compliance_core::lock_email_address(tx, o.to_email_address_id).await? {
        return Err(AppError::new(
            ErrorCode::SendBasisUnverified,
            "recipient address missing",
        ));
    }
    let sup = compliance_core::active_suppressions(tx, o.to_email_address_id, &o.purpose).await?;
    if !sup.is_empty() {
        return Err(AppError::new(
            ErrorCode::RecipientSuppressed,
            "recipient is suppressed for this purpose",
        )
        .with_violations(vec![violation(
            "to_email_address_id",
            "WORKSPACE_MARKETING_SUPPRESSION",
        )]));
    }

    // Approval requirement: non-correspondence purposes, and every
    // agent-requested send, need a live approved request bound to this hash.
    let requester_is_agent = match repo::principal_state(tx, o.requested_by).await? {
        Some((_, k, _)) => k == "agent",
        None => false,
    };
    if o.purpose != "correspondence" || requester_is_agent {
        match repo::latest_outbound_approval(tx, o.id).await? {
            Some(a)
                if a.status == "approved"
                    && a.expires_at > Utc::now()
                    && a.snapshot_hash == o.payload_hash => {}
            Some(a) if a.expires_at <= Utc::now() || a.status == "expired" => {
                return Err(AppError::new(
                    ErrorCode::ApprovalExpired,
                    "the approval for this outbound has expired",
                ));
            }
            Some(a) if a.snapshot_hash != o.payload_hash => {
                return Err(AppError::new(
                    ErrorCode::SnapshotMismatch,
                    "the approval was issued for a different snapshot",
                ));
            }
            Some(a) if a.status == "pending" => {
                return Err(AppError::conflict("the approval request is still pending"));
            }
            Some(_) => {
                return Err(AppError::conflict(
                    "the approval request was rejected or revoked",
                ));
            }
            None => {
                return Err(AppError::conflict(
                    "this outbound requires an approved approval request first",
                ));
            }
        }
    }

    // A.6.5 reply claim — thread lock → context check → unique active claim.
    if let Some(reply_to) = o.reply_to_message_id {
        let Some(tid) = o.thread_id else {
            return Err(AppError::internal("reply outbound without thread_id"));
        };
        let Some((acct, last_inbound)) = repo::get_thread_for_update(tx, tid).await? else {
            return Err(AppError::conflict("thread vanished"));
        };
        if acct != o.mail_account_id {
            return Err(AppError::internal("thread/mailbox mismatch"));
        }
        let expected_inbound = policy::expected_last_inbound(tx, &o).await?;
        if let (Some(exp), Some(last)) = (expected_inbound, last_inbound)
            && exp != last
        {
            return Err(AppError::new(
                ErrorCode::ReplyContextChanged,
                "a newer inbound message arrived — review before sending",
            ));
        }
        if repo::active_claim_for_inbound(tx, o.mail_account_id, reply_to)
            .await?
            .is_some()
        {
            return Err(AppError::new(
                ErrorCode::ReplyAlreadySubmitted,
                "an active reply already exists for this inbound message",
            ));
        }
        if let Err(e) = repo::insert_reply_claim(tx, o.mail_account_id, tid, reply_to, o.id).await {
            if e.code == ErrorCode::Conflict {
                return Err(AppError::new(
                    ErrorCode::ReplyAlreadySubmitted,
                    "an active reply already exists for this inbound message",
                ));
            }
            return Err(e);
        }
    }

    repo::set_queued(tx, o.id, not_before).await?;
    let job_id = infrastructure::jobs::enqueue(
        tx,
        dispatch::DISPATCH_KIND,
        1,
        serde_json::json!({"outbound_message_id": o.id}),
        not_before.unwrap_or_else(Utc::now),
        Some(&format!("dispatch:{}", o.id)),
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "outbound.submitted",
            "outbound_message",
            ctx.request_id.clone(),
        )
        .with_target(o.id),
    )
    .await?;
    Ok(idem::Stored {
        status: 202,
        body: SubmitOutboundResponse {
            outbound_message_id: o.id,
            status: "queued".to_string(),
            job_id,
            provider_accepted: false,
        },
    })
}

/// `GET /outbound-messages/{id}` — B.3.11. `delivery_status` folds only
/// provider-confirmed delivery_events; an accepted outcome alone never
/// reads delivered (T34).
pub async fn get_outbound(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<OutboundMessageResponse, AppError> {
    outbound_response(db, ctx, id).await
}

async fn outbound_response(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
) -> Result<OutboundMessageResponse, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(o) = repo::get_outbound(&mut tx, id).await? else {
        return Err(AppError::not_found("outbound message not found"));
    };
    require_read(&mut tx, ctx, o.mail_account_id).await?;
    let event_types = repo::delivery_event_types(&mut tx, id).await?;
    let attempt = repo::latest_attempt(&mut tx, id).await?;
    tx.commit().await?;

    let delivery = domain::sending::DeliveryStatus::fold(event_types);
    let provider_accepted = match o.status.as_str() {
        "accepted" => Some(true),
        "failed" | "canceled" => Some(false),
        _ => None,
    };
    let next_action = match o.status.as_str() {
        "draft" => "submit_or_request_approval",
        "queued" | "claimed" => "wait_for_dispatch",
        "submitting" => "provider_call_in_flight",
        "accepted" => "none",
        "failed" => "inspect_failure_create_new_message",
        "unknown" => "reconcile_with_provider",
        _ => "none",
    };
    Ok(OutboundMessageResponse {
        outbound_message_id: o.id,
        status: o.status.clone(),
        payload_hash: hex_encode(&o.payload_hash),
        failure_code: o.failure_code.clone(),
        provider_accepted,
        delivery_status: delivery.as_str().to_string(),
        // ADR-006: never auto-resubmit; unknown reconciles by lookup only.
        automatic_resubmit_allowed: false,
        next_action: next_action.to_string(),
        last_attempt: attempt.map(|a| OutboundAttemptInfo {
            attempt_no: i64::from(a.attempt_no),
            status: a.status,
            error_code: a.error_code,
            started_at: a.started_at,
            finished_at: a.finished_at,
        }),
        purpose: o.purpose,
        from_address: o.from_address_snapshot,
        subject: o.subject,
        body_text: o.body_text,
        body_html: o.body_html,
        to_address: o.to_address_snapshot,
        not_before: o.not_before,
        accepted_at: o.accepted_at,
        version: o.version,
        created_at: o.created_at,
    })
}

/// `POST /outbound-messages/{id}/cancel` — only before the in-flight
/// boundary (draft/queued/claimed). Releases the reply claim and cancels
/// queued dispatch jobs; a `submitting` row is beyond recall (A.6.4).
pub async fn cancel(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &CancelOutboundRequest,
) -> Result<CancelOutboundResponse, AppError> {
    authz::require_scope(ctx, Scope::MailCompose)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let Some(o) = repo::get_outbound_for_update(&mut tx, id).await? else {
        return Err(AppError::not_found("outbound message not found"));
    };
    // 담당·정책: the requester, a compose grantee on the mailbox, or an
    // admin/manager may cancel.
    let grant = authz::mailbox_grant(&mut tx, ctx, o.mail_account_id).await?;
    let allowed = o.requested_by == ctx.principal_id
        || grant.is_some_and(|g| g.can_compose)
        || (ctx.principal_kind == PrincipalKind::Human
            && matches!(ctx.role, contracts::Role::Admin | contracts::Role::Manager));
    if !allowed {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no permission to cancel this outbound",
        ));
    }
    if o.version != expected_version {
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "outbound version changed — reload and retry",
        ));
    }
    let response_version = match o.status.as_str() {
        "canceled" => o.version,
        "draft" | "queued" | "claimed" => {
            repo::set_status(&mut tx, o.id, "canceled", req.reason.as_deref()).await?;
            repo::transition_claim(&mut tx, o.id, &["reserved", "submitted"], "released").await?;
            repo::cancel_queued_dispatch_jobs(&mut tx, o.id).await?;
            audit::record(
                &mut tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "outbound.canceled",
                    "outbound_message",
                    ctx.request_id.clone(),
                )
                .with_target(o.id),
            )
            .await?;
            o.version + 1
        }
        "submitting" => {
            return Err(AppError::conflict(
                "provider call in flight — the outcome row will record the result",
            ));
        }
        _ => {
            return Err(AppError::conflict(format!(
                "outbound is '{}' — already terminal",
                o.status
            )));
        }
    };
    tx.commit().await?;
    Ok(CancelOutboundResponse {
        outbound_message_id: id,
        status: "canceled".to_string(),
        version: response_version,
    })
}
