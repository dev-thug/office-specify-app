//! Calendar connections (B.2, §10.1) — per-host connections, never implied
//! by mailbox access.

use chrono::{DateTime, Utc};
use contracts::AppError;
use contracts::Ctx;
use contracts::Role;
use contracts::mailboxes::{CalendarConnectionResponse, OAuthStartResponse};
use infrastructure::config::Config;
use infrastructure::db::Db;
use infrastructure::repo::mailboxes as repo;
use uuid::Uuid;

fn response(row: &repo::CalendarConnectionRow) -> CalendarConnectionResponse {
    CalendarConnectionResponse {
        id: row.id,
        host_principal_id: row.host_principal_id,
        connection_id: row.connection_id,
        provider: row.provider.clone(),
        provider_calendar_id: row.provider_calendar_id.clone(),
        status: row.status.clone(),
        include_in_busy: row.include_in_busy,
        version: row.version,
        created_at: row.created_at,
    }
}

pub struct CalendarPage {
    pub items: Vec<CalendarConnectionResponse>,
    pub has_more: bool,
    pub cursor_seed: Option<(DateTime<Utc>, Uuid)>,
}

/// `GET /calendar-connections` — Admin/Manager see all (operational
/// visibility); Member/Agent see only connections they host.
pub async fn list_calendar_connections(
    db: &Db,
    ctx: &Ctx,
    after: Option<(DateTime<Utc>, Uuid)>,
    limit: i64,
) -> Result<CalendarPage, AppError> {
    let mut tx = db.begin(ctx.workspace_id).await?;
    let all = matches!(ctx.role, Role::Admin | Role::Manager);
    let rows = repo::list_calendar_connections(
        &mut tx,
        (!all).then_some(ctx.principal_id),
        after,
        limit + 1,
    )
    .await?;
    tx.commit().await?;
    let has_more = rows.len() as i64 > limit;
    let rows: Vec<repo::CalendarConnectionRow> = rows.into_iter().take(limit as usize).collect();
    let cursor_seed = has_more
        .then(|| rows.last().map(|r| (r.created_at, r.id)))
        .flatten();
    Ok(CalendarPage {
        items: rows.iter().map(response).collect(),
        has_more,
        cursor_seed,
    })
}

/// `POST /calendar-connections/connection-requests` — delegates to the
/// shared OAuth start (default Zoho region `com`).
pub async fn request_calendar_connection(
    db: &Db,
    ctx: &Ctx,
    config: &Config,
    host_principal_id: Option<Uuid>,
) -> Result<OAuthStartResponse, AppError> {
    super::oauth::request_calendar_connection(db, ctx, config, host_principal_id, "com").await
}
