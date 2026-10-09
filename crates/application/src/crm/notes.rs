//! Note use cases (B.2 `POST /notes`). A note targets exactly one of
//! company/contact/thread (DDL CHECK). Thread notes carry the mailbox id and
//! require `can_read` on that mailbox — both to create and to list (§5.6:
//! even existence is hidden without a grant).

use contracts::crm::{CreateNoteRequest, NoteResponse};
use contracts::{AppError, Ctx, Page, PrincipalKind, Scope};
use domain::crm::activity_kind;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::idempotency::Stored;
use infrastructure::repo::crm::{self, NoteInsert};
use uuid::Uuid;

use super::util;
use super::{require_crm_read, require_crm_write};
use crate::authz::{self, MailboxPerm};

/// `POST /notes` — `created_by` is the authenticated principal (system-owned
/// field, never accepted from the body).
pub async fn create(
    db: &Db,
    ctx: &Ctx,
    req: &CreateNoteRequest,
    idem_key: Option<String>,
) -> Result<Stored<NoteResponse>, AppError> {
    require_crm_write(ctx)?;
    let targets = req.company_id.iter().count()
        + req.contact_id.iter().count()
        + req.thread_id.iter().count();
    if targets != 1 {
        return Err(util::bad_request(
            "exactly one of company_id, contact_id, thread_id is required",
            vec![
                util::violation("company_id", "ONE_TARGET_REQUIRED"),
                util::violation("contact_id", "ONE_TARGET_REQUIRED"),
                util::violation("thread_id", "ONE_TARGET_REQUIRED"),
            ],
        ));
    }
    if req.body.trim().is_empty() || req.body.len() > 200_000 {
        return Err(util::bad_request(
            "body must be 1-200000 characters",
            vec![util::violation("body", "LENGTH")],
        ));
    }
    let body = serde_json::to_value(req)
        .map_err(|e| AppError::internal(format!("request serialize: {e}")))?;
    let ctx_owned = ctx.clone();
    util::run_maybe_idempotent(
        db,
        ctx,
        "POST /api/v1/notes",
        idem_key,
        &body,
        201,
        move |mut tx| async move {
            let r = create_in_tx(&mut tx, &ctx_owned, req).await?;
            Ok((tx, r))
        },
    )
    .await
}

async fn create_in_tx(
    tx: &mut infrastructure::db::Tx<'_>,
    ctx: &Ctx,
    req: &CreateNoteRequest,
) -> Result<NoteResponse, AppError> {
    let mut mail_account_id = None;
    if let Some(company_id) = req.company_id {
        let company = crm::get_company(tx, company_id)
            .await?
            .ok_or_else(|| AppError::not_found("company not found"))?;
        require_note_target(ctx, company.owner_principal_id)?;
    }
    if let Some(contact_id) = req.contact_id {
        let contact = crm::get_contact(tx, contact_id)
            .await?
            .ok_or_else(|| AppError::not_found("contact not found"))?;
        require_note_target(ctx, contact.owner_principal_id)?;
    }
    if let Some(thread_id) = req.thread_id {
        let account = crm::thread_mail_account(tx, thread_id)
            .await?
            .ok_or_else(|| AppError::not_found("thread not found"))?;
        // Mailbox ACL: without can_read the thread's existence is hidden.
        authz::require_mailbox(tx, ctx, account, MailboxPerm::Read)
            .await
            .map_err(|_| AppError::not_found("thread not found"))?;
        mail_account_id = Some(account);
    }
    let row = crm::insert_note(
        tx,
        &NoteInsert {
            company_id: req.company_id,
            contact_id: req.contact_id,
            thread_id: req.thread_id,
            mail_account_id,
            created_by: ctx.principal_id,
            body: req.body.clone(),
        },
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "note.create",
            "note",
            ctx.request_id.clone(),
        )
        .with_target(row.id),
    )
    .await?;
    util::write_activity(
        tx,
        crm::ActivityInsert {
            company_id: row.company_id,
            contact_id: row.contact_id,
            thread_id: row.thread_id,
            mail_account_id: row.mail_account_id,
            actor_principal_id: Some(ctx.principal_id),
            kind: activity_kind::NOTE_CREATED,
            source_event_id: row.id,
            occurred_at: row.created_at,
            summary: serde_json::json!({"note_id": row.id}),
            ..Default::default()
        },
    )
    .await?;
    util::emit_change(tx, "note.created", "note", row.id, row.version).await?;
    Ok(util::note_dto(&row))
}

/// An agent with explicit `crm.write` may create a separate internal note;
/// this does not grant permission to patch company or contact fields. Humans
/// keep the normal CRM ownership rule, and thread notes still require the
/// source mailbox read grant below (§5.6/B.6.2).
fn require_note_target(ctx: &Ctx, owner_principal_id: Option<Uuid>) -> Result<(), AppError> {
    if ctx.principal_kind == PrincipalKind::Agent {
        authz::require_scope(ctx, Scope::CrmWrite)
    } else {
        authz::crm_can_edit(ctx, owner_principal_id)
    }
}

#[derive(Debug, Default)]
pub struct NoteListQuery {
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub thread_id: Option<Uuid>,
    pub limit: u32,
    pub cursor: Option<String>,
}

fn filters_json(q: &NoteListQuery) -> serde_json::Value {
    serde_json::json!({
        "endpoint": "GET /api/v1/notes",
        "company_id": q.company_id,
        "contact_id": q.contact_id,
        "thread_id": q.thread_id,
        "limit": q.limit,
    })
}

/// `GET /notes` — filter by target; mailbox-grant filtered in SQL (§5.6).
pub async fn list(
    db: &Db,
    ctx: &Ctx,
    hmac_key: &[u8],
    q: &NoteListQuery,
) -> Result<(Vec<NoteResponse>, Page), AppError> {
    require_crm_read(ctx)?;
    let filters = filters_json(q);
    let after = util::decode_cursor(q.cursor.as_deref(), hmac_key, ctx, &filters)?;
    let mut tx = db.begin(ctx.workspace_id).await?;
    let mut rows = crm::list_notes(
        &mut tx,
        q.company_id,
        q.contact_id,
        q.thread_id,
        ctx.principal_id,
        after,
        q.limit + 1,
    )
    .await?;
    tx.commit().await?;
    let page = util::page_for(&mut rows, q.limit, hmac_key, ctx, &filters, |r| {
        (r.created_at, r.id)
    });
    Ok((rows.iter().map(util::note_dto).collect(), page))
}
