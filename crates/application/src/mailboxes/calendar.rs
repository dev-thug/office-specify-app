//! Calendar connections (B.2, §10.1) — per-host connections, never implied
//! by mailbox access.

use chrono::{DateTime, Utc};
use contracts::AppError;
use contracts::Ctx;
use contracts::mailboxes::{CalendarConnectionResponse, OAuthStartResponse};
use contracts::{AuthMethod, ErrorCode, PrincipalKind, Role, Scope};
use infrastructure::audit::{self, AuditEntry};
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
    let all = (matches!(ctx.role, Role::Admin | Role::Manager) || crate::authz::agent_admin(ctx));
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

/// Create or reactivate the caller's one Office-owned local calendar.
/// Returns true only when a new connection row was created.
pub async fn connect_local_calendar_in_tx(
    tx: &mut infrastructure::db::Tx<'_>,
    ctx: &Ctx,
) -> Result<(CalendarConnectionResponse, bool), AppError> {
    crate::authz::require_operator(ctx)?;
    crate::authz::require_scope(ctx, Scope::BookingPropose)?;
    if !matches!(&ctx.auth, AuthMethod::Session { .. })
        && ctx.principal_kind != PrincipalKind::Agent
    {
        return Err(AppError::forbidden(
            "local calendar connection requires a browser session",
        ));
    }
    let active_human: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM principals
              WHERE workspace_id = $1 AND id = $2
                AND kind IN ('human', 'agent') AND status = 'active'
         )",
    )
    .bind(ctx.workspace_id)
    .bind(ctx.principal_id)
    .fetch_one(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;
    if !active_human {
        return Err(AppError::new(
            ErrorCode::PrincipalDisabled,
            "local calendar host is inactive",
        ));
    }

    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "office-calendar-connect:{}:{}",
            ctx.workspace_id, ctx.principal_id
        ))
        .execute(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;

    let existing = sqlx::query_as::<_, repo::CalendarConnectionRow>(
        "SELECT c.id, c.workspace_id, c.provider, c.host_principal_id,
                c.connection_id, c.provider_calendar_id, c.status,
                c.include_in_busy, c.version, c.created_at
           FROM calendar_connections c
           JOIN integration_connections i
             ON i.workspace_id = c.workspace_id AND i.id = c.connection_id
          WHERE c.workspace_id = $1
            AND c.host_principal_id = $2
            AND c.provider = 'office_calendar'
            AND i.provider = 'office_calendar'
          ORDER BY c.created_at, c.id
          LIMIT 1
          FOR UPDATE OF c, i",
    )
    .bind(ctx.workspace_id)
    .bind(ctx.principal_id)
    .fetch_optional(tx.conn())
    .await
    .map_err(infrastructure::error::map_sqlx)?;

    let (row, created) = if let Some(existing) = existing {
        sqlx::query(
            "UPDATE integration_connections
                SET status = 'active', last_error_code = NULL,
                    last_checked_at = now(), version = version + 1
              WHERE workspace_id = $1 AND id = $2 AND provider = 'office_calendar'
                AND (status <> 'active' OR last_error_code IS NOT NULL)",
        )
        .bind(ctx.workspace_id)
        .bind(existing.connection_id)
        .execute(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        sqlx::query(
            "UPDATE calendar_connections
                SET status = 'active', version = version + 1
              WHERE workspace_id = $1 AND id = $2 AND provider = 'office_calendar'
                AND status <> 'active'",
        )
        .bind(ctx.workspace_id)
        .bind(existing.id)
        .execute(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        let row = sqlx::query_as::<_, repo::CalendarConnectionRow>(
            "SELECT id, workspace_id, provider, host_principal_id, connection_id,
                    provider_calendar_id, status, include_in_busy, version, created_at
               FROM calendar_connections
              WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ctx.workspace_id)
        .bind(existing.id)
        .fetch_one(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        (row, false)
    } else {
        let connection_id = Uuid::new_v4();
        let calendar_id = Uuid::new_v4();
        let provider_calendar_id = format!("office:{}:{}", ctx.workspace_id, calendar_id);
        sqlx::query(
            "INSERT INTO integration_connections
                (id, workspace_id, provider, region, secret_ref, status, connected_by)
             VALUES ($1, $2, 'office_calendar', 'local', '', 'active', $3)",
        )
        .bind(connection_id)
        .bind(ctx.workspace_id)
        .bind(ctx.principal_id)
        .execute(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        let row = sqlx::query_as::<_, repo::CalendarConnectionRow>(
            "INSERT INTO calendar_connections
                (id, workspace_id, provider, host_principal_id, connection_id,
                 provider_calendar_id, status)
             VALUES ($1, $2, 'office_calendar', $3, $4, $5, 'active')
             RETURNING id, workspace_id, provider, host_principal_id, connection_id,
                       provider_calendar_id, status, include_in_busy, version, created_at",
        )
        .bind(calendar_id)
        .bind(ctx.workspace_id)
        .bind(ctx.principal_id)
        .bind(connection_id)
        .bind(&provider_calendar_id)
        .fetch_one(tx.conn())
        .await
        .map_err(infrastructure::error::map_sqlx)?;
        (row, true)
    };

    if created {
        audit::record(
            tx,
            AuditEntry::new(
                ctx.principal_id,
                "calendar_connection.local_created",
                "calendar_connection",
                ctx.request_id.clone(),
            )
            .with_target(row.id),
        )
        .await?;
    }
    Ok((response(&row), created))
}
