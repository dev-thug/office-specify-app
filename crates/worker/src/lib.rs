//! Job runner (spec §4.7, A.6.7). The runner claims jobs per workspace under
//! `worker_runtime` RLS context, invokes the registered handler inside a
//! fresh workspace tx, and completes/fails fenced by the lease token.
//!
//! Per job:
//!   tx1: claim (commit — lease is durable before the handler runs)
//!   tx2: handler business writes + `complete` — atomic commit
//!   on error: rollback tx2, tx3: `fail_or_retry` (backoff/dead)

pub mod handlers;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use contracts::AppError;
use infrastructure::db::Db;
use infrastructure::jobs::{self, Job, JobCtx, JobHandler};
use uuid::Uuid;

pub struct JobRegistry {
    handlers: HashMap<&'static str, Arc<dyn JobHandler>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    pub fn register<H: JobHandler + 'static>(&mut self, handler: H) {
        self.handlers.insert(handler.kind(), Arc::new(handler));
    }

    pub fn get(&self, kind: &str) -> Option<Arc<dyn JobHandler>> {
        self.handlers.get(kind).cloned()
    }
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry with every module's `register` applied (spec §6 — module workers
/// fill `handlers/<m>.rs`, never touch this file).
pub fn registry() -> JobRegistry {
    let mut r = JobRegistry::new();
    handlers::agents::register(&mut r);
    handlers::approvals::register(&mut r);
    handlers::booking::register(&mut r);
    handlers::campaigns::register(&mut r);
    handlers::compliance::register(&mut r);
    handlers::crm::register(&mut r);
    handlers::imports::register(&mut r);
    handlers::inbox::register(&mut r);
    handlers::mailboxes::register(&mut r);
    handlers::operations::register(&mut r);
    handlers::org::register(&mut r);
    handlers::research::register(&mut r);
    handlers::sending::register(&mut r);
    handlers::templates::register(&mut r);
    r
}

pub struct Runner {
    db: Db,
    registry: JobRegistry,
    /// Unique lease owner per process (A.6.7).
    pub owner: String,
    pub poll_interval: Duration,
}

impl Runner {
    pub fn new(db: Db, registry: JobRegistry) -> Self {
        Self {
            db,
            registry,
            owner: format!("worker-{}", Uuid::new_v4()),
            poll_interval: Duration::from_millis(500),
        }
    }

    /// One poll iteration: reclaim expired leases, find workspaces with
    /// runnable jobs, claim and handle each.
    pub async fn run_once(&self) -> Result<usize, AppError> {
        jobs::reclaim_expired(&self.db).await?;
        let mut handled = 0;
        for ws in jobs::runnable_workspaces(&self.db).await? {
            // Claim in its own tx so the lease commits before work starts.
            let claimed: Vec<Job> = {
                let mut tx = self.db.begin(ws).await?;
                let jobs = jobs::claim(&mut tx, &self.owner, jobs::BATCH_SIZE).await?;
                tx.commit().await?;
                jobs
            };
            for job in claimed {
                self.handle_one(job).await;
                handled += 1;
            }
        }
        Ok(handled)
    }

    async fn handle_one(&self, job: Job) {
        let ctx = JobCtx {
            job_id: job.id,
            workspace_id: job.workspace_id,
            lease_token: job.lease_token.unwrap_or_default(),
            attempt_count: job.attempt_count,
            lease_owner: self.owner.clone(),
        };
        let Some(handler) = self.registry.get(&job.kind) else {
            tracing::warn!(job_id = %job.id, kind = %job.kind, "no handler registered");
            let mut tx = match self.db.begin(job.workspace_id).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "begin failed");
                    return;
                }
            };
            let _ = jobs::transition(
                &mut tx,
                job.id,
                ctx.lease_token,
                jobs::STATE_DEAD,
                None,
                Some("NO_HANDLER"),
            )
            .await;
            let _ = tx.commit().await;
            return;
        };

        let mut tx = match self.db.begin(job.workspace_id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "begin failed");
                return;
            }
        };
        match handler.handle(&ctx, &mut tx, job.payload.clone()).await {
            Ok(()) => {
                match jobs::complete(&mut tx, job.id, ctx.lease_token).await {
                    Ok(true) => {
                        if let Err(e) = tx.commit().await {
                            tracing::error!(job_id = %job.id, error = %e, "commit failed");
                        }
                    }
                    // Fencing lost — discard the business writes too.
                    _ => {
                        let _ = tx.rollback().await;
                        tracing::warn!(job_id = %job.id, "lease fencing lost; job abandoned");
                    }
                }
            }
            Err(e) => {
                let _ = tx.rollback().await;
                let code = e.code.as_str();
                let mut ftx = match self.db.begin(job.workspace_id).await {
                    Ok(t) => t,
                    Err(e2) => {
                        tracing::error!(error = %e2, "fail tx begin failed");
                        return;
                    }
                };
                let is_external = handler.external_effect();
                // §16.4: external-effect handlers are not auto-retried after
                // the submitting boundary — they mark their own business row
                // `unknown` and finish; a runner-level error before that
                // boundary may retry.
                if let Err(e2) = jobs::fail_or_retry(&mut ftx, &job, ctx.lease_token, code).await {
                    tracing::error!(job_id = %job.id, error = %e2, "fail_or_retry failed");
                }
                if is_external {
                    tracing::warn!(job_id = %job.id, code, "external-effect job failed");
                }
                let _ = ftx.commit().await;
                tracing::warn!(job_id = %job.id, kind = %job.kind, code, "job failed");
            }
        }
    }

    /// Poll loop — run until cancelled.
    pub async fn run(&self) -> Result<(), AppError> {
        loop {
            match self.run_once().await {
                Ok(0) => tokio::time::sleep(self.poll_interval).await,
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(error = %e, "poll failed");
                    tokio::time::sleep(self.poll_interval).await;
                }
            }
        }
    }
}
