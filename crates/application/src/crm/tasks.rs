//! Task use cases (B.2 `/tasks`). Member principals see and modify only
//! their own tasks; Manager/Admin span the team (B.2 "담당·Manager").
//! Reassignment is a privileged field (§5.3).

use chrono::Utc;
use contracts::crm::{CreateTaskRequest, PatchTaskRequest, TaskResponse, TaskStatus};
use contracts::{AppError, Ctx, ErrorCode, Page, Role};
use domain::crm::activity_kind;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Db;
use infrastructure::idempotency::Stored;
use infrastructure::repo::crm::{self, TaskFilter, TaskInsert};
use uuid::Uuid;

use super::util;
use super::{require_crm_read, require_crm_write};
use crate::authz::{self, MailboxPerm};

// ------------------------------------------------------------------- query

#[derive(Debug, Default)]
pub struct TaskListQuery {
    pub status: Option<String>,
    /// Explicit owner filter — Manager/Admin only; Member is always scoped
    /// to self regardless of the requested filter (§5.3).
    pub owner_principal_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub limit: u32,
    pub cursor: Option<String>,
}

fn filters_json(q: &TaskListQuery, effective_owner: Option<Uuid>) -> serde_json::Value {
    serde_json::json!({
        "endpoint": "GET /api/v1/tasks",
        "status": q.status,
        "owner_principal_id": effective_owner,
        "company_id": q.company_id,
        "contact_id": q.contact_id,
        "limit": q.limit,
    })
}

/// `GET /tasks` — Member is silently scoped to own tasks (existence of
/// teammates' tasks is not an ACL problem, but scoping matches B.2 "담당").
pub async fn list(
    db: &Db,
    ctx: &Ctx,
    hmac_key: &[u8],
    q: &TaskListQuery,
) -> Result<(Vec<TaskResponse>, Page), AppError> {
    require_crm_read(ctx)?;
    if let Some(s) = &q.status
        && TaskStatus::parse(s).is_none()
    {
        return Err(util::bad_request(
            "invalid status filter",
            vec![util::violation("status", "ENUM")],
        ));
    }
    let effective_owner = match ctx.role {
        Role::Member => Some(ctx.principal_id),
        _ => q.owner_principal_id,
    };
    let filters = filters_json(q, effective_owner);
    let after = util::decode_cursor(q.cursor.as_deref(), hmac_key, ctx, &filters)?;
    let filter = TaskFilter {
        status: q.status.clone(),
        owner_principal_id: effective_owner,
        company_id: q.company_id,
        contact_id: q.contact_id,
    };
    let mut tx = db.begin(ctx.workspace_id).await?;
    let mut rows = crm::list_tasks(&mut tx, &filter, after, q.limit + 1).await?;
    tx.commit().await?;
    let page = util::page_for(&mut rows, q.limit, hmac_key, ctx, &filters, |r| {
        (r.created_at, r.id)
    });
    Ok((rows.iter().map(util::task_dto).collect(), page))
}

// ----------------------------------------------------------------- writers

/// `POST /tasks` — owner defaults to the requester; assigning someone else
/// is a reassignment (Manager/Admin). Thread linkage requires mailbox read.
pub async fn create(
    db: &Db,
    ctx: &Ctx,
    req: &CreateTaskRequest,
    idem_key: Option<String>,
) -> Result<Stored<TaskResponse>, AppError> {
    require_crm_write(ctx)?;
    if req.title.trim().is_empty() || req.title.len() > 500 {
        return Err(util::bad_request(
            "title must be 1-500 characters",
            vec![util::violation("title", "LENGTH")],
        ));
    }
    let owner = req.owner_principal_id.unwrap_or(ctx.principal_id);
    if owner != ctx.principal_id {
        authz::require_role(ctx, &[Role::Admin, Role::Manager])?;
    }
    let body = serde_json::to_value(req)
        .map_err(|e| AppError::internal(format!("request serialize: {e}")))?;
    let ctx_owned = ctx.clone();
    util::run_maybe_idempotent(
        db,
        ctx,
        "POST /api/v1/tasks",
        idem_key,
        &body,
        201,
        move |mut tx| async move {
            let r = create_in_tx(&mut tx, &ctx_owned, req, owner).await?;
            Ok((tx, r))
        },
    )
    .await
}

async fn create_in_tx(
    tx: &mut infrastructure::db::Tx<'_>,
    ctx: &Ctx,
    req: &CreateTaskRequest,
    owner: Uuid,
) -> Result<TaskResponse, AppError> {
    if let Some(id) = req.company_id
        && crm::get_company(tx, id).await?.is_none()
    {
        return Err(util::bad_request(
            "company_id does not reference a company",
            vec![util::violation("company_id", "NOT_FOUND")],
        ));
    }
    if let Some(id) = req.contact_id
        && crm::get_contact(tx, id).await?.is_none()
    {
        return Err(util::bad_request(
            "contact_id does not reference a contact",
            vec![util::violation("contact_id", "NOT_FOUND")],
        ));
    }
    if let Some(thread_id) = req.thread_id {
        let account = crm::thread_mail_account(tx, thread_id)
            .await?
            .ok_or_else(|| {
                util::bad_request(
                    "thread_id does not reference a thread",
                    vec![util::violation("thread_id", "NOT_FOUND")],
                )
            })?;
        // Linking a task to a thread reveals its existence → require read.
        authz::require_mailbox(tx, ctx, account, MailboxPerm::Read).await?;
    }
    if !crm::principal_is_active_human(tx, owner).await? {
        return Err(util::bad_request(
            "owner_principal_id must reference an active human principal",
            vec![util::violation("owner_principal_id", "INVALID_PRINCIPAL")],
        ));
    }
    let row = crm::insert_task(
        tx,
        &TaskInsert {
            title: req.title.trim().to_string(),
            owner_principal_id: owner,
            company_id: req.company_id,
            contact_id: req.contact_id,
            thread_id: req.thread_id,
            due_at: req.due_at,
        },
    )
    .await?;
    audit::record(
        tx,
        AuditEntry::new(
            ctx.principal_id,
            "task.create",
            "task",
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
            actor_principal_id: Some(ctx.principal_id),
            kind: activity_kind::TASK_CREATED,
            source_event_id: row.id,
            occurred_at: row.created_at,
            summary: serde_json::json!({"task_id": row.id}),
            ..Default::default()
        },
    )
    .await?;
    util::emit_change(tx, "task.created", "task", row.id, row.version).await?;
    Ok(util::task_dto(&row))
}

/// `PATCH /tasks/{id}` — owner or Manager/Admin (B.2), If-Match guarded.
/// Status transitions write the corresponding timeline rows.
pub async fn patch(
    db: &Db,
    ctx: &Ctx,
    id: Uuid,
    expected_version: i64,
    req: &PatchTaskRequest,
) -> Result<TaskResponse, AppError> {
    require_crm_write(ctx)?;
    if let Some(title) = &req.title
        && (title.trim().is_empty() || title.len() > 500)
    {
        return Err(util::bad_request(
            "title must be 1-500 characters",
            vec![util::violation("title", "LENGTH")],
        ));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let row = crm::get_task(&mut tx, id)
        .await?
        .ok_or_else(|| AppError::not_found("task not found"))?;
    let is_owner = row.owner_principal_id == ctx.principal_id;
    if !is_owner {
        authz::require_role(ctx, &[Role::Admin, Role::Manager])?;
    }
    let new_owner = match req.owner_principal_id {
        Some(o) if o != row.owner_principal_id => {
            // Reassignment is privileged even when the requester owns it.
            authz::require_role(ctx, &[Role::Admin, Role::Manager])?;
            if !crm::principal_is_active_human(&mut tx, o).await? {
                return Err(util::bad_request(
                    "owner_principal_id must reference an active human principal",
                    vec![util::violation("owner_principal_id", "INVALID_PRINCIPAL")],
                ));
            }
            o
        }
        _ => row.owner_principal_id,
    };
    let new_title = req
        .title
        .clone()
        .map(|t| t.trim().to_string())
        .unwrap_or_else(|| row.title.clone());
    let new_status = req
        .status
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|| row.status.clone());
    let new_due = match &req.due_at {
        Some(v) => *v,
        None => row.due_at,
    };
    let Some(updated) = crm::update_task_versioned(
        &mut tx,
        id,
        expected_version,
        &new_title,
        &new_status,
        new_due,
        new_owner,
    )
    .await?
    else {
        let _ = tx.rollback().await;
        return Err(AppError::new(
            ErrorCode::VersionMismatch,
            "task was modified by another request",
        ));
    };
    audit::record(
        &mut tx,
        AuditEntry::new(
            ctx.principal_id,
            "task.patch",
            "task",
            ctx.request_id.clone(),
        )
        .with_target(id),
    )
    .await?;
    if updated.status != row.status {
        let kind = match updated.status.as_str() {
            "done" => Some(activity_kind::TASK_COMPLETED),
            "canceled" => Some(activity_kind::TASK_CANCELED),
            _ => None,
        };
        if let Some(kind) = kind {
            util::write_activity(
                &mut tx,
                crm::ActivityInsert {
                    company_id: updated.company_id,
                    contact_id: updated.contact_id,
                    thread_id: updated.thread_id,
                    actor_principal_id: Some(ctx.principal_id),
                    kind,
                    source_event_id: updated.id,
                    occurred_at: Utc::now(),
                    summary: serde_json::json!({"task_id": id}),
                    ..Default::default()
                },
            )
            .await?;
        }
    }
    util::emit_change(&mut tx, "task.updated", "task", id, updated.version).await?;
    tx.commit().await?;
    Ok(util::task_dto(&updated))
}
