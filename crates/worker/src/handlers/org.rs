//! org module job handlers (spec §6 — owned by the module worker).
//!
//! - `org.disable_principal` (payload `{principal_id, reassign_to,
//!   cancel_pending}`): atomically cancel the principal's queued/retry_wait
//!   jobs (never running/unknown — §5.5), reassign owned companies/contacts/
//!   open tasks/threads, recheck the last-admin guard, and finally deactivate
//!   the principal with its `principal.disabled` event. The API revokes
//!   sessions, credentials, and password login before it queues this job.
//! - `org.revoke_credential_jobs` (payload `{credential_id}`): cancel
//!   queued jobs carrying the revoked credential — running/unknown are
//!   untouched.
//!
//! Both are DB-only (no provider calls) → `external_effect = false`. Both
//! write an audit row (`job_id` populated) so the deferred work is
//! attributable; the runner's tx makes job completion + audit atomic.

use async_trait::async_trait;
use contracts::AppError;
use infrastructure::audit::{self, AuditEntry};
use infrastructure::db::Tx;
use infrastructure::error::map_sqlx;
use infrastructure::jobs::{JobCtx, JobHandler};
use infrastructure::repo::org as repo;
use serde::Deserialize;
use uuid::Uuid;

use crate::JobRegistry;

/// Register this module's JobHandlers on the worker registry.
pub fn register(registry: &mut JobRegistry) {
    registry.register(DisablePrincipal);
    registry.register(RevokeCredentialJobs);
}

/// `jobs.requested_by` for audit attribution (tagged at enqueue via
/// `repo::tag_job_requester`); falls back to the system principal.
async fn job_requester(tx: &mut Tx<'_>, job_id: Uuid) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar("SELECT requested_by FROM jobs WHERE workspace_id = $1 AND id = $2")
        .bind(tx.workspace_id())
        .bind(job_id)
        .fetch_optional(tx.conn())
        .await
        .map_err(map_sqlx)
        .map(|r| r.flatten())
}

/// Audit actor for worker-side rows: the designated system principal, or
/// (paranoid fallback for workspaces without one) the subject itself.
async fn worker_actor(tx: &mut Tx<'_>, fallback: Uuid) -> Result<Uuid, AppError> {
    Ok(repo::system_principal_id(tx).await?.unwrap_or(fallback))
}

// --------------------------------------------------------------------------
// org.disable_principal
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct DisablePrincipalPayload {
    principal_id: Uuid,
    #[serde(default)]
    reassign_to: Option<Uuid>,
    #[serde(default)]
    cancel_pending: Option<bool>,
}

struct DisablePrincipal;

#[async_trait]
impl JobHandler for DisablePrincipal {
    fn kind(&self) -> &'static str {
        application::org::admin::JOB_DISABLE_PRINCIPAL
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let p: DisablePrincipalPayload = serde_json::from_value(payload)
            .map_err(|_| AppError::internal("org.disable_principal: malformed payload"))?;

        // Match the request path's lock order. The active admin rows are
        // locked first so two cleanup jobs cannot both disable the final
        // administrators concurrently.
        let active_admins = repo::count_active_admins_locked(tx).await?;
        let Some(principal) = repo::lock_principal(tx, p.principal_id).await? else {
            return Err(AppError::not_found("principal not found"));
        };
        if principal.status == "disabled" {
            return Ok(());
        }
        if principal.kind == "system" {
            return Err(AppError::conflict("system principals cannot be disabled"));
        }
        if principal.kind == "human" && principal.role == "admin" && active_admins <= 1 {
            return Err(AppError::conflict(
                "the last active admin cannot be disabled",
            ));
        }

        if let Some(to) = p.reassign_to {
            let valid = repo::get_principal_full(tx, to).await?.is_some_and(|t| {
                t.kind == "human" && t.status == "active" && t.id != p.principal_id
            }) && repo::pending_disable_job(tx, to).await?.is_none();
            if !valid {
                return Err(AppError::conflict(
                    "reassignment target is no longer an active human principal",
                ));
            }
        } else if repo::has_reassignable_work(tx, p.principal_id).await? {
            return Err(AppError::conflict(
                "reassign_to is required while the principal owns open work",
            ));
        }

        let mut canceled = 0u64;
        if p.cancel_pending.unwrap_or(true) {
            canceled = repo::cancel_pending_jobs_for_principal(tx, p.principal_id).await?;
        }

        let mut reassigned = serde_json::json!(null);
        if let Some(to) = p.reassign_to {
            let (companies, contacts, tasks, threads) =
                repo::reassign_principal_ownership(tx, p.principal_id, to).await?;
            reassigned = serde_json::json!({
                "to": to,
                "companies": companies,
                "contacts": contacts,
                "tasks": tasks,
                "threads": threads,
            });
        }

        // This update, the handoff, pending-job cancellations, audit and
        // outbox event share the worker transaction. Any failure rolls back
        // the entire disable rather than leaving orphaned ownership.
        let principal = repo::set_principal_status(tx, p.principal_id, "disabled")
            .await?
            .ok_or_else(|| AppError::conflict("principal changed during disable cleanup"))?;
        infrastructure::outbox::emit(
            tx,
            application::org::admin::EVENT_PRINCIPAL_DISABLED,
            "principal",
            p.principal_id,
            principal.version,
            serde_json::json!({"principal_id": p.principal_id}),
        )
        .await?;

        let requester = job_requester(tx, ctx.job_id).await?;
        let actor = worker_actor(tx, requester.unwrap_or(p.principal_id)).await?;
        audit::record(
            tx,
            AuditEntry {
                actor_principal_id: Some(actor),
                requested_by: requester,
                job_id: Some(ctx.job_id),
                metadata: serde_json::json!({
                    "principal_id": p.principal_id,
                    "canceled_jobs": canceled,
                    "reassignment": reassigned,
                }),
                ..AuditEntry::new(
                    actor,
                    "principal.disable.cleanup",
                    "principal",
                    format!("job:{}", ctx.job_id),
                )
                .with_target(p.principal_id)
            },
        )
        .await?;
        Ok(())
    }
}

// --------------------------------------------------------------------------
// org.revoke_credential_jobs
// --------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RevokeCredentialJobsPayload {
    credential_id: Uuid,
}

struct RevokeCredentialJobs;

#[async_trait]
impl JobHandler for RevokeCredentialJobs {
    fn kind(&self) -> &'static str {
        application::org::admin::JOB_REVOKE_CREDENTIAL_JOBS
    }

    async fn handle(
        &self,
        ctx: &JobCtx,
        tx: &mut Tx<'_>,
        payload: serde_json::Value,
    ) -> Result<(), AppError> {
        let p: RevokeCredentialJobsPayload = serde_json::from_value(payload)
            .map_err(|_| AppError::internal("org.revoke_credential_jobs: malformed payload"))?;

        // Only 'queued'/'retry_wait' rows move — running/unknown/finished
        // jobs are never modified (task brief + §5.5).
        let canceled = repo::cancel_pending_jobs_for_credential(tx, p.credential_id).await?;

        let requester = job_requester(tx, ctx.job_id).await?;
        // Attribute to the system principal; the credential's owner is the
        // affected party, not the actor.
        let actor = worker_actor(tx, requester.unwrap_or(p.credential_id)).await?;
        audit::record(
            tx,
            AuditEntry {
                actor_principal_id: Some(actor),
                requested_by: requester,
                job_id: Some(ctx.job_id),
                metadata: serde_json::json!({
                    "credential_id": p.credential_id,
                    "canceled_jobs": canceled,
                }),
                ..AuditEntry::new(
                    actor,
                    "credential.revoke.jobs",
                    "service_credential",
                    format!("job:{}", ctx.job_id),
                )
                .with_target(p.credential_id)
            },
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! DB-backed handler tests mirroring `Runner::handle_one`: enqueue →
    //! claim (lease) → handle in a fresh ws tx → fenced complete. Requires
    //! DATABASE_URL pointing at a dedicated test DB.

    use super::*;
    use chrono::Utc;
    use infrastructure::db::{Db, ROLE_API, ROLE_WORKER};
    use infrastructure::jobs::{self, JobCtx};
    use infrastructure::repo::org as org_repo;
    use infrastructure::testing::seed_workspace;
    use uuid::Uuid;

    async fn test_dbs() -> Option<(Db, Db)> {
        let url = match std::env::var("DATABASE_URL") {
            Ok(u) if !u.is_empty() => u,
            _ => {
                eprintln!("DATABASE_URL not set — skipping worker test");
                return None;
            }
        };
        let owner = Db::connect(&url, None).await.unwrap();
        owner.migrate().await.unwrap();
        Some((
            Db::connect(&url, Some(ROLE_API)).await.unwrap(),
            Db::connect(&url, Some(ROLE_WORKER)).await.unwrap(),
        ))
    }

    async fn insert_pending_job(
        db: &Db,
        ws: Uuid,
        state: &str,
        requested_by: Option<Uuid>,
        cred: Option<Uuid>,
    ) -> Uuid {
        let mut tx = db.begin(ws).await.unwrap();
        // jobs_check: 'running' rows must carry lease fields. Fixture
        // queued/retry_wait rows get a future `available_at` so the test's
        // `jobs::claim` leases ONLY the handler job — claim would otherwise
        // move the fixtures to 'running' before the handler can cancel them.
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO jobs
                (workspace_id, kind, payload_version, payload, state, dedupe_key,
                 requested_by, source_credential_id, available_at,
                 lease_owner, lease_token, lease_expires_at)
             VALUES ($1, 'some.job', 1, '{}', $2, $3, $4, $5,
                     CASE WHEN $2 IN ('queued','retry_wait')
                          THEN now() + interval '1 hour' ELSE now() END,
                     CASE WHEN $2 = 'running' THEN 'w-test' END,
                     CASE WHEN $2 = 'running' THEN gen_random_uuid() END,
                     CASE WHEN $2 = 'running' THEN now() + interval '60 seconds' END)
             RETURNING id",
        )
        .bind(ws)
        .bind(state)
        .bind(format!("t3w-{state}-{}", Uuid::new_v4()))
        .bind(requested_by)
        .bind(cred)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    async fn state_of(db: &Db, ws: Uuid, job: Uuid) -> String {
        let mut tx = db.begin(ws).await.unwrap();
        let s: String =
            sqlx::query_scalar("SELECT state FROM jobs WHERE workspace_id = $1 AND id = $2")
                .bind(ws)
                .bind(job)
                .fetch_one(tx.conn())
                .await
                .unwrap();
        let _ = tx.rollback().await;
        s
    }

    async fn audit_rows_for_job(db: &Db, ws: Uuid, job_id: Uuid) -> i64 {
        let mut tx = db.begin(ws).await.unwrap();
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_logs WHERE workspace_id = $1 AND job_id = $2",
        )
        .bind(ws)
        .bind(job_id)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let _ = tx.rollback().await;
        n
    }

    async fn add_admin(db: &Db, ws: Uuid, name: &str) -> Uuid {
        let email = format!("{}-{}@test.dev", name.to_lowercase(), Uuid::new_v4());
        let user = infrastructure::repo::auth::create_user(db, &email, name)
            .await
            .unwrap();
        let mut tx = db.begin(ws).await.unwrap();
        let id = infrastructure::repo::auth::insert_principal(
            &mut tx,
            "human",
            Some(user.id),
            name,
            "admin",
            "active",
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    #[tokio::test]
    async fn disable_principal_job_cancels_pending_and_reassigns() {
        let Some((api_db, db)) = test_dbs().await else {
            return;
        };
        let world = seed_workspace(&api_db).await.unwrap();
        let ws = world.workspace_id;
        let subject = world.member_b.principal_id;

        // Work owned by the subject: a queued job (cancel), a running job
        // (untouched), and a company + open task (reassign to manager).
        let queued = insert_pending_job(&db, ws, "queued", Some(subject), None).await;
        let running = insert_pending_job(&db, ws, "running", Some(subject), None).await;
        let mut tx = db.begin(ws).await.unwrap();
        let company: Uuid = sqlx::query_scalar(
            "INSERT INTO companies (workspace_id, name, owner_principal_id)
             VALUES ($1, 'Acme', $2) RETURNING id",
        )
        .bind(ws)
        .bind(subject)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // The API request blocks authentication immediately but leaves the
        // principal active until this cleanup transaction commits.
        let mut access_tx = api_db.begin(ws).await.unwrap();
        sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
            .bind(world.member_b.user_id.unwrap())
            .execute(access_tx.conn())
            .await
            .unwrap();
        org_repo::revoke_sessions_for_principal(&mut access_tx, subject)
            .await
            .unwrap();
        let _ = org_repo::revoke_credentials_for_principal(&mut access_tx, subject)
            .await
            .unwrap();
        access_tx.commit().await.unwrap();

        let mut tx = db.begin(ws).await.unwrap();
        let job_id = jobs::enqueue(
            &mut tx,
            application::org::admin::JOB_DISABLE_PRINCIPAL,
            1,
            serde_json::json!({
                "principal_id": subject,
                "reassign_to": world.manager.principal_id,
                "cancel_pending": true,
            }),
            Utc::now(),
            Some(&format!("disable:{subject}")),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let mut before_tx = db.begin(ws).await.unwrap();
        let before = org_repo::get_principal_full(&mut before_tx, subject)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(before.status, "active", "cleanup must precede deactivation");
        let _ = before_tx.rollback().await;

        // Claim + run like the runner does.
        let mut claim_tx = db.begin(ws).await.unwrap();
        let claimed = jobs::claim(
            &mut claim_tx,
            "w-test",
            20,
            &[application::org::admin::JOB_DISABLE_PRINCIPAL],
        )
        .await
        .unwrap();
        claim_tx.commit().await.unwrap();
        let job = claimed.iter().find(|j| j.id == job_id).unwrap().clone();
        let ctx = JobCtx {
            job_id: job.id,
            workspace_id: ws,
            lease_token: job.lease_token.unwrap(),
            attempt_count: job.attempt_count,
            lease_owner: "w-test".into(),
        };
        let mut tx = db.begin(ws).await.unwrap();
        DisablePrincipal
            .handle(&ctx, &mut tx, job.payload.clone())
            .await
            .unwrap();
        assert!(
            jobs::complete(&mut tx, job.id, ctx.lease_token)
                .await
                .unwrap()
        );
        tx.commit().await.unwrap();

        assert_eq!(state_of(&db, ws, queued).await, "canceled");
        assert_eq!(state_of(&db, ws, running).await, "running");
        assert_eq!(state_of(&db, ws, job_id).await, "completed");
        assert_eq!(audit_rows_for_job(&db, ws, job_id).await, 1);

        let mut tx = db.begin(ws).await.unwrap();
        let principal = org_repo::get_principal_full(&mut tx, subject)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(principal.status, "disabled");
        let event_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM outbox_events
              WHERE workspace_id = $1 AND event_type = 'principal.disabled'
                AND aggregate_id = $2",
        )
        .bind(ws)
        .bind(subject)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let _ = tx.rollback().await;
        assert_eq!(event_count, 1, "event must commit with final deactivation");

        // Company moved to the manager.
        let mut tx = db.begin(ws).await.unwrap();
        let owner: Option<Uuid> = sqlx::query_scalar(
            "SELECT owner_principal_id FROM companies WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(company)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let _ = tx.rollback().await;
        assert_eq!(owner, Some(world.manager.principal_id));
    }

    #[tokio::test]
    async fn revoke_credential_jobs_cancels_only_pending() {
        let Some((api_db, db)) = test_dbs().await else {
            return;
        };
        let world = seed_workspace(&api_db).await.unwrap();
        let ws = world.workspace_id;

        let mut tx = api_db.begin(ws).await.unwrap();
        let (cred_id, _tok) = infrastructure::repo::auth::create_service_credential(
            &mut tx,
            world.agent.principal_id,
            "ci",
            &[],
            chrono::Duration::days(30),
        )
        .await
        .unwrap();
        org_repo::revoke_credential(&mut tx, cred_id).await.unwrap();
        let job_id = jobs::enqueue(
            &mut tx,
            application::org::admin::JOB_REVOKE_CREDENTIAL_JOBS,
            1,
            serde_json::json!({"credential_id": cred_id}),
            Utc::now(),
            Some(&format!("revoke-cred:{cred_id}")),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let queued = insert_pending_job(&db, ws, "queued", None, Some(cred_id)).await;
        let retry = insert_pending_job(&db, ws, "retry_wait", None, Some(cred_id)).await;
        let running = insert_pending_job(&db, ws, "running", None, Some(cred_id)).await;
        let unknown = insert_pending_job(&db, ws, "unknown", None, Some(cred_id)).await;

        let mut claim_tx = db.begin(ws).await.unwrap();
        let claimed = jobs::claim(
            &mut claim_tx,
            "w-test",
            20,
            &[application::org::admin::JOB_REVOKE_CREDENTIAL_JOBS],
        )
        .await
        .unwrap();
        claim_tx.commit().await.unwrap();
        let job = claimed.iter().find(|j| j.id == job_id).unwrap().clone();
        let ctx = JobCtx {
            job_id: job.id,
            workspace_id: ws,
            lease_token: job.lease_token.unwrap(),
            attempt_count: job.attempt_count,
            lease_owner: "w-test".into(),
        };
        let mut tx = db.begin(ws).await.unwrap();
        RevokeCredentialJobs
            .handle(&ctx, &mut tx, job.payload.clone())
            .await
            .unwrap();
        assert!(
            jobs::complete(&mut tx, job.id, ctx.lease_token)
                .await
                .unwrap()
        );
        tx.commit().await.unwrap();

        assert_eq!(state_of(&db, ws, queued).await, "canceled");
        assert_eq!(state_of(&db, ws, retry).await, "canceled");
        assert_eq!(state_of(&db, ws, running).await, "running");
        assert_eq!(state_of(&db, ws, unknown).await, "unknown");
        assert_eq!(audit_rows_for_job(&db, ws, job_id).await, 1);
    }

    #[tokio::test]
    async fn disable_job_rechecks_last_admin_before_cleanup_or_deactivation() {
        let Some((api_db, db)) = test_dbs().await else {
            return;
        };
        let world = seed_workspace(&api_db).await.unwrap();
        let ws = world.workspace_id;
        let second_admin = add_admin(&api_db, ws, "Second Admin").await;
        let subject = world.admin.principal_id;

        let mut setup = db.begin(ws).await.unwrap();
        let company: Uuid = sqlx::query_scalar(
            "INSERT INTO companies (workspace_id, name, owner_principal_id)
             VALUES ($1, 'Admin Owned', $2) RETURNING id",
        )
        .bind(ws)
        .bind(subject)
        .fetch_one(setup.conn())
        .await
        .unwrap();
        setup.commit().await.unwrap();

        // The request has already revoked access but has not deactivated the
        // principal. A concurrent admin change leaves this request as the
        // last active administrator, so cleanup must roll back completely.
        let mut request_tx = api_db.begin(ws).await.unwrap();
        sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
            .bind(world.admin.user_id.unwrap())
            .execute(request_tx.conn())
            .await
            .unwrap();
        org_repo::revoke_sessions_for_principal(&mut request_tx, subject)
            .await
            .unwrap();
        let _ = org_repo::revoke_credentials_for_principal(&mut request_tx, subject)
            .await
            .unwrap();
        request_tx.commit().await.unwrap();

        let mut enqueue_tx = db.begin(ws).await.unwrap();
        let job_id = jobs::enqueue(
            &mut enqueue_tx,
            application::org::admin::JOB_DISABLE_PRINCIPAL,
            1,
            serde_json::json!({
                "principal_id": subject,
                "reassign_to": world.manager.principal_id,
                "cancel_pending": true,
            }),
            Utc::now(),
            Some(&format!("disable:{subject}")),
        )
        .await
        .unwrap();
        enqueue_tx.commit().await.unwrap();

        let mut concurrent_tx = db.begin(ws).await.unwrap();
        org_repo::set_principal_status(&mut concurrent_tx, second_admin, "disabled")
            .await
            .unwrap();
        concurrent_tx.commit().await.unwrap();

        let mut claim_tx = db.begin(ws).await.unwrap();
        let claimed = jobs::claim(
            &mut claim_tx,
            "w-test",
            20,
            &[application::org::admin::JOB_DISABLE_PRINCIPAL],
        )
        .await
        .unwrap();
        claim_tx.commit().await.unwrap();
        let job = claimed.iter().find(|j| j.id == job_id).unwrap().clone();
        let ctx = JobCtx {
            job_id: job.id,
            workspace_id: ws,
            lease_token: job.lease_token.unwrap(),
            attempt_count: job.attempt_count,
            lease_owner: "w-test".into(),
        };
        let mut tx = db.begin(ws).await.unwrap();
        let err = DisablePrincipal
            .handle(&ctx, &mut tx, job.payload.clone())
            .await
            .unwrap_err();
        assert_eq!(err.code, contracts::ErrorCode::Conflict);
        let _ = tx.rollback().await;

        let mut tx = db.begin(ws).await.unwrap();
        let principal = org_repo::get_principal_full(&mut tx, subject)
            .await
            .unwrap()
            .unwrap();
        let owner: Uuid = sqlx::query_scalar(
            "SELECT owner_principal_id FROM companies WHERE workspace_id = $1 AND id = $2",
        )
        .bind(ws)
        .bind(company)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let event_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM outbox_events
              WHERE workspace_id = $1 AND event_type = 'principal.disabled'
                AND aggregate_id = $2",
        )
        .bind(ws)
        .bind(subject)
        .fetch_one(tx.conn())
        .await
        .unwrap();
        let _ = tx.rollback().await;
        assert_eq!(principal.status, "active");
        assert_eq!(owner, subject, "reassignment must roll back with the guard");
        assert_eq!(event_count, 0, "no event before a successful deactivation");
    }
}
