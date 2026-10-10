//! Thread read/mutation use cases (B.2 rows 3605–3610). Every path applies
//! the mailbox grant (§5.4): no `can_read` → the thread does not exist for
//! this caller (404-hiding); a grant without the required flag → 403
//! `MAILBOX_ACCESS_DENIED`.

use chrono::{DateTime, Utc};
use contracts::inbox::{
    AssignmentRequest, MessageDraftDto, PatchThreadRequest, ReadStateResponse, ThreadDetail,
    ThreadMutationResponse, ThreadStatus, ThreadSummary,
};
use contracts::{AppError, Ctx, ErrorCode, Scope, Violation};
use uuid::Uuid;

use crate::authz::{self, MailboxPerm};
use infrastructure::db::Tx;
use infrastructure::repo::inbox as repo;
use infrastructure::{audit, audit::AuditEntry};

/// Load a thread and enforce a mailbox permission on its account.
/// Missing thread or missing grant → 404 (existence hidden); grant without
/// the flag → 403 MAILBOX_ACCESS_DENIED.
async fn thread_with_perm(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    perm: MailboxPerm,
) -> Result<(repo::ThreadRow, authz::MailboxGrant), AppError> {
    let Some(thread) = repo::get_thread(tx, thread_id).await? else {
        return Err(AppError::not_found("thread not found"));
    };
    let Some(grant) = authz::mailbox_grant(tx, ctx, thread.mail_account_id).await? else {
        return Err(
            AppError::new(ErrorCode::MailboxAccessDenied, "no access to this mailbox")
                .with_status(404),
        );
    };
    let ok = match perm {
        MailboxPerm::Read => grant.can_read,
        MailboxPerm::Compose => grant.can_compose,
        MailboxPerm::Send => grant.can_send,
        MailboxPerm::Approve => grant.can_approve,
        MailboxPerm::Assign => grant.can_assign,
    };
    if !ok {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no access to this mailbox",
        ));
    }
    Ok((thread, grant))
}

/// Authorize reply derivation, which reads an inbound subject and recipient.
/// Reply creation therefore needs both mail.compose and mail.read scopes and
/// the matching current mailbox grants. The returned thread is safe to use
/// for deriving reply headers and recipients.
pub async fn authorize_reply(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
) -> Result<repo::ThreadRow, AppError> {
    authz::require_scope(ctx, Scope::MailCompose)?;
    authz::require_scope(ctx, Scope::MailRead)?;
    let (thread, grant) = thread_with_perm(tx, ctx, thread_id, MailboxPerm::Compose).await?;
    if !grant.can_read {
        return Err(AppError::new(
            ErrorCode::MailboxAccessDenied,
            "no read access to this mailbox",
        ));
    }
    Ok(thread)
}

fn summary(row: repo::ThreadListRow) -> ThreadSummary {
    ThreadSummary {
        id: row.id,
        mail_account_id: row.mail_account_id,
        contact_id: row.contact_id,
        assigned_principal_id: row.assigned_principal_id,
        status: ThreadStatus::parse(&row.status).unwrap_or(ThreadStatus::Open),
        subject: row.subject,
        snippet: row.snippet,
        message_count: row.message_count,
        unread: row.unread,
        last_message_at: row.last_message_at,
        version: row.version,
        updated_at: row.updated_at,
        created_at: row.created_at,
    }
}

/// `GET /email-threads` — keyset page of `ThreadSummary`. The caller maps
/// the last row's `(updated_at, id)` into the signed cursor.
pub async fn list_threads(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    filter: &repo::ThreadFilter,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<repo::ThreadListRow>, AppError> {
    authz::require_scope(ctx, Scope::MailRead)?;
    repo::list_threads(tx, ctx.principal_id, filter, after, limit).await
}

/// Search message subject and plain-text content for MCP inbox discovery.
/// The repository query only returns messages from mailboxes with a current
/// `can_read` grant; this application boundary also requires `mail.read`.
pub async fn search_messages(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    mail_account_id: Option<Uuid>,
    query: &str,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<Vec<repo::MessageSearchRow>, AppError> {
    authz::require_scope(ctx, Scope::MailRead)?;
    let normalized = query.trim();
    if normalized.is_empty() || normalized.chars().count() > 200 {
        return Err(AppError::validation(
            "query must contain 1 to 200 characters",
            vec![Violation {
                field: "query".into(),
                code: "out_of_range".into(),
            }],
        ));
    }
    repo::search_messages(
        tx,
        ctx.principal_id,
        mail_account_id,
        normalized,
        after,
        limit,
    )
    .await
}

pub fn to_summary(row: repo::ThreadListRow) -> ThreadSummary {
    summary(row)
}

/// `GET /email-threads/{id}` — messages + participants + attachments with
/// HTML sanitized at DTO build time (T26).
pub async fn get_thread(tx: &mut Tx<'_>, ctx: &Ctx, id: Uuid) -> Result<ThreadDetail, AppError> {
    authz::require_scope(ctx, Scope::MailRead)?;
    let (thread, _) = thread_with_perm(tx, ctx, id, MailboxPerm::Read).await?;
    let messages = repo::thread_messages(tx, id).await?;
    let ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();
    let participants = repo::participants_for(tx, &ids).await?;
    let attachments = repo::attachments_for(tx, &ids).await?;
    let my_last_read_at = repo::get_read_state(tx, id, ctx.principal_id).await?;

    let messages = messages
        .into_iter()
        .map(|m| {
            let participants = participants
                .iter()
                .filter(|p| p.email_message_id == m.id)
                .map(|p| contracts::inbox::ParticipantDto {
                    role: p.role.clone(),
                    address: p.address.clone(),
                    display_name: p.display_name.clone(),
                })
                .collect();
            let attachments = attachments
                .iter()
                .filter(|a| a.email_message_id == m.id)
                .map(|a| contracts::inbox::AttachmentDto {
                    id: a.id,
                    filename: a.filename.clone(),
                    media_type: a.media_type.clone(),
                    byte_size: a.byte_size,
                    is_inline: a.is_inline,
                    downloadable: a.scan_status != "rejected",
                })
                .collect();
            contracts::inbox::EmailMessageDto {
                id: m.id,
                direction: m.direction,
                provider_message_id: m.provider_message_id,
                subject: m.subject,
                body_text: m.body_text,
                body_html: m.body_html.map(|h| repo::sanitize_html(&h)),
                message_at: m.message_at,
                classification: contracts::inbox::Classification::parse(&m.classification)
                    .unwrap_or(contracts::inbox::Classification::Unknown),
                classification_reviewed: m.classification_reviewed_by.is_some(),
                participants,
                attachments,
            }
        })
        .collect();

    Ok(ThreadDetail {
        id: thread.id,
        mail_account_id: thread.mail_account_id,
        contact_id: thread.contact_id,
        assigned_principal_id: thread.assigned_principal_id,
        status: ThreadStatus::parse(&thread.status).unwrap_or(ThreadStatus::Open),
        version: thread.version,
        last_inbound_message_id: thread.last_inbound_message_id,
        my_last_read_at,
        updated_at: thread.updated_at,
        created_at: thread.created_at,
        messages,
    })
}

fn mutation_response(t: repo::ThreadRow) -> ThreadMutationResponse {
    ThreadMutationResponse {
        id: t.id,
        status: ThreadStatus::parse(&t.status).unwrap_or(ThreadStatus::Open),
        assigned_principal_id: t.assigned_principal_id,
        version: t.version,
        updated_at: t.updated_at,
    }
}

/// Shared optimistic-lock tail: `update` returned `None` → re-check access
/// (404 when the thread is gone/hidden), then 412 VERSION_MISMATCH.
async fn version_tail<T>(tx: &mut Tx<'_>, ctx: &Ctx, thread_id: Uuid) -> Result<T, AppError> {
    thread_with_perm(tx, ctx, thread_id, MailboxPerm::Read).await?;
    Err(AppError::new(
        ErrorCode::VersionMismatch,
        "thread version does not match If-Match",
    ))
}

/// `PUT /email-threads/{id}/assignment` — `can_assign` + If-Match.
/// `null` unassigns; the target principal must be active in the workspace.
pub async fn set_assignment(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    if_match: i64,
    req: &AssignmentRequest,
) -> Result<ThreadMutationResponse, AppError> {
    authz::require_scope(ctx, Scope::MailAssign)?;
    let (thread, _) = thread_with_perm(tx, ctx, thread_id, MailboxPerm::Assign).await?;
    if let Some(pid) = req.assigned_principal_id
        && !repo::principal_active(tx, pid).await?
    {
        return Err(AppError::validation(
            "assigned principal does not exist or is not active",
            vec![Violation {
                field: "assigned_principal_id".into(),
                code: "not_found".into(),
            }],
        ));
    }
    match repo::set_assignment(tx, thread_id, if_match, req.assigned_principal_id).await? {
        Some(t) => {
            audit::record(
                tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "thread.assigned",
                    "email_thread",
                    ctx.request_id.clone(),
                )
                .with_target(thread_id),
            )
            .await?;
            Ok(mutation_response(t))
        }
        None => version_tail(tx, ctx, thread.id).await,
    }
}

/// Recheck assignment scope and mailbox capability before an idempotency
/// replay returns a stored mutation response.
pub async fn authorize_assignment(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
) -> Result<(), AppError> {
    authz::require_scope(ctx, Scope::MailAssign)?;
    thread_with_perm(tx, ctx, thread_id, MailboxPerm::Assign).await?;
    Ok(())
}

/// Recheck local inbox-management scope and read capability before a cached
/// status/read-state response is replayed.
pub async fn authorize_management(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
) -> Result<(), AppError> {
    authz::require_scope(ctx, Scope::MailManage)?;
    thread_with_perm(tx, ctx, thread_id, MailboxPerm::Read).await?;
    Ok(())
}

/// `PUT /email-threads/{id}/read-state` — caller's own mark only, monotonic
/// (B.1.4). No If-Match (explicitly exempt).
pub async fn set_read_state(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    last_read_at: Option<DateTime<Utc>>,
) -> Result<ReadStateResponse, AppError> {
    authz::require_scope(ctx, Scope::MailRead)?;
    thread_with_perm(tx, ctx, thread_id, MailboxPerm::Read).await?;
    let at = last_read_at.unwrap_or_else(Utc::now);
    let stored = repo::upsert_read_state(tx, thread_id, ctx.principal_id, at).await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "thread.read_state",
            "email_thread",
            ctx.request_id.clone(),
        )
        .with_target(thread_id),
    )
    .await?;
    Ok(ReadStateResponse {
        thread_id,
        principal_id: ctx.principal_id,
        last_read_at: stored,
    })
}

/// MCP-local read/unread control. Unlike the human API's monotonic read
/// position, marking unread clears only the authenticated caller's read row.
pub async fn set_managed_read_state(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    read: bool,
) -> Result<Option<DateTime<Utc>>, AppError> {
    authorize_management(tx, ctx, thread_id).await?;
    let last_read_at = if read {
        Some(repo::upsert_read_state(tx, thread_id, ctx.principal_id, Utc::now()).await?)
    } else {
        repo::clear_read_state(tx, thread_id, ctx.principal_id).await?;
        None
    };
    let mut entry = AuditEntry::new(
        ctx.principal_id,
        "thread.read_state",
        "email_thread",
        ctx.request_id.clone(),
    )
    .with_target(thread_id);
    entry.metadata = serde_json::json!({ "read": read });
    audit::record(tx, entry).await?;
    Ok(last_read_at)
}

/// `PATCH /email-threads/{id}` — status only, If-Match.
pub async fn patch_status(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    if_match: i64,
    req: &PatchThreadRequest,
) -> Result<ThreadMutationResponse, AppError> {
    patch_status_with_scope(tx, ctx, thread_id, if_match, req, Scope::MailRead).await
}

/// Status changes initiated by MCP require the dedicated `mail.manage` scope.
pub async fn manage_status(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    if_match: i64,
    req: &PatchThreadRequest,
) -> Result<ThreadMutationResponse, AppError> {
    patch_status_with_scope(tx, ctx, thread_id, if_match, req, Scope::MailManage).await
}

async fn patch_status_with_scope(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    if_match: i64,
    req: &PatchThreadRequest,
    required_scope: Scope,
) -> Result<ThreadMutationResponse, AppError> {
    authz::require_scope(ctx, required_scope)?;
    let (thread, _) = thread_with_perm(tx, ctx, thread_id, MailboxPerm::Read).await?;
    match repo::set_status(tx, thread_id, if_match, req.status.as_str()).await? {
        Some(t) => {
            audit::record(
                tx,
                AuditEntry::new(
                    ctx.principal_id,
                    "thread.status_changed",
                    "email_thread",
                    ctx.request_id.clone(),
                )
                .with_target(thread_id),
            )
            .await?;
            Ok(mutation_response(t))
        }
        None => version_tail(tx, ctx, thread.id).await,
    }
}

/// Resolve the reply recipient: the `from` address of the latest inbound
/// (never an arbitrary Reply-To — B.3.10), falling back to the linked
/// contact's primary address.
async fn reply_recipient(
    tx: &mut Tx<'_>,
    thread: &repo::ThreadRow,
) -> Result<Option<Uuid>, AppError> {
    if let Some(last) = thread.last_inbound_message_id {
        if let Some(id) = repo::from_address_id(tx, last).await? {
            return Ok(Some(id));
        }
        if let Some(addr) = repo::from_address(tx, last).await?
            && let Ok(n) = domain::email_address::normalize(
                &addr,
                domain::email_address::LocalPartPolicy::Lowercase,
            )
        {
            return Ok(Some(
                repo::upsert_email_address(tx, &n.normalized_key, &n.original).await?,
            ));
        }
    }
    if let Some(contact) = thread.contact_id {
        return repo::contact_primary_address(tx, contact).await;
    }
    Ok(None)
}

/// `POST /email-threads/{id}/reply-drafts` — creates a `message_drafts`
/// row. `expected_last_inbound_id` mismatch → 409 REPLY_CONTEXT_CHANGED
/// (T16 draft side). Idempotent via the caller's `idempotency::run` wrapper.
pub async fn create_reply_draft(
    tx: &mut Tx<'_>,
    ctx: &Ctx,
    thread_id: Uuid,
    req: &contracts::inbox::ReplyDraftRequest,
) -> Result<MessageDraftDto, AppError> {
    let thread = authorize_reply(tx, ctx, thread_id).await?;

    if req.body_text.trim().is_empty() {
        return Err(AppError::validation(
            "body_text is required",
            vec![Violation {
                field: "body_text".into(),
                code: "required".into(),
            }],
        )
        .with_status(422));
    }
    if req
        .body_html
        .as_deref()
        .is_some_and(domain::templates::is_dangerous_html)
    {
        return Err(AppError::validation(
            "body_html contains disallowed constructs",
            vec![Violation {
                field: "body_html".into(),
                code: "DANGEROUS_HTML".into(),
            }],
        ));
    }
    let visibility = req.visibility.as_deref().unwrap_or("private");
    if !matches!(visibility, "private" | "mailbox") {
        return Err(AppError::validation(
            "visibility must be private or mailbox",
            vec![Violation {
                field: "visibility".into(),
                code: "invalid".into(),
            }],
        ));
    }

    // T16: a new inbound since the client last looked must force re-review.
    if let Some(expected) = req.expected_last_inbound_id
        && thread.last_inbound_message_id != Some(expected)
    {
        return Err(AppError::new(
            ErrorCode::ReplyContextChanged,
            "new inbound mail arrived; review the latest message before replying",
        ));
    }

    let Some(to_address_id) = reply_recipient(tx, &thread).await? else {
        return Err(AppError::validation(
            "cannot determine reply recipient for this thread",
            vec![Violation {
                field: "thread_id".into(),
                code: "no_recipient".into(),
            }],
        ));
    };

    let source_credential_id = match &ctx.auth {
        contracts::AuthMethod::ServiceCredential { credential_id } => Some(*credential_id),
        _ => None,
    };

    // Subject: `Re:` the latest inbound subject (or the last message's).
    let subject = match thread.last_inbound_message_id {
        Some(id) => repo::get_message(tx, id)
            .await?
            .and_then(|m| m.subject)
            .map(|s| {
                if s.to_lowercase().starts_with("re:") {
                    s
                } else {
                    format!("Re: {s}")
                }
            })
            .unwrap_or_else(|| "Re: (no subject)".to_string()),
        None => "Re: (no subject)".to_string(),
    };

    let draft = repo::insert_reply_draft(
        tx,
        thread.mail_account_id,
        ctx.principal_id,
        source_credential_id,
        visibility,
        thread.id,
        thread.last_inbound_message_id,
        req.expected_last_inbound_id,
        to_address_id,
        &subject,
        &req.body_text,
        req.body_html.as_deref(),
    )
    .await?;

    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "message_draft.created",
            "message_draft",
            ctx.request_id.clone(),
        )
        .with_target(draft.id),
    )
    .await?;

    Ok(MessageDraftDto {
        id: draft.id,
        mail_account_id: draft.mail_account_id,
        owner_principal_id: draft.owner_principal_id,
        visibility: draft.visibility,
        thread_id: draft.thread_id,
        reply_to_message_id: draft.reply_to_message_id,
        expected_last_inbound_id: draft.expected_last_inbound_id,
        to_email_address_id: draft.to_email_address_id,
        purpose: draft.purpose,
        subject: draft.subject,
        body_text: draft.body_text,
        body_html: draft.body_html,
        status: draft.status,
        version: draft.version,
        created_at: draft.created_at,
        updated_at: draft.updated_at,
    })
}
