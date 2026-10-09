//! Operational alert projections (source §17.2).

use chrono::Utc;
use contracts::operations::{
    AlertsResponse, BacklogAlert, DeadJobSample, DeadJobsAlert, ReauthMailboxAlert,
    ReauthMailboxSample, UnknownOutboundAlert,
};
use contracts::{AppError, Ctx, PrincipalKind, Role, Scope};
use infrastructure::db::Db;
use infrastructure::repo::operations as repo;

/// Live operational alerts. Samples contain metadata and internal IDs only;
/// they never include mail content, addresses, or OAuth secrets.
pub async fn list(db: &Db, ctx: &Ctx) -> Result<AlertsResponse, AppError> {
    if ctx.principal_kind != PrincipalKind::Human
        || !matches!(ctx.role, Role::Admin | Role::Manager)
        || !ctx.scopes.contains(Scope::OperationsRead)
    {
        return Err(AppError::forbidden(
            "operational alerts require an Admin or Manager",
        ));
    }
    let mut tx = db.begin(ctx.workspace_id).await?;
    let flags = repo::workspace_flags(&mut tx).await?;
    let (backlog_count, backlog_oldest) = repo::outbox_backlog(&mut tx).await?;
    let (unknown_messages, unknown_jobs, unknown_samples) = repo::unknown_outbound(&mut tx).await?;
    let (dead_count, dead_samples) = repo::dead_jobs(&mut tx).await?;
    let (reauth_count, reauth_samples) = repo::reauth_mailboxes(&mut tx).await?;
    tx.commit().await?;
    Ok(AlertsResponse {
        generated_at: Utc::now(),
        emergency_stop: flags.emergency_stop,
        send_workers_locked: flags.send_workers_locked,
        outbox_backlog: BacklogAlert {
            count: backlog_count,
            oldest_created_at: backlog_oldest,
        },
        unknown_outbound: UnknownOutboundAlert {
            outbound_messages: unknown_messages,
            jobs: unknown_jobs,
            sample_outbound_ids: unknown_samples,
        },
        dead_jobs: DeadJobsAlert {
            count: dead_count,
            samples: dead_samples
                .into_iter()
                .map(|row| DeadJobSample {
                    id: row.id,
                    kind: row.kind,
                    last_error_code: row.last_error_code,
                    finished_at: row.finished_at,
                })
                .collect(),
        },
        reauth_mailboxes: ReauthMailboxAlert {
            count: reauth_count,
            samples: reauth_samples
                .into_iter()
                .map(|row| ReauthMailboxSample {
                    id: row.id,
                    name: row.name,
                    last_synced_at: row.last_synced_at,
                })
                .collect(),
        },
    })
}
