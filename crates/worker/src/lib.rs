//! Job runner (spec §4.7, A.6.7). The runner claims jobs per workspace under
//! `worker_runtime` RLS context, invokes the registered handler inside a
//! fresh workspace tx, and completes/fails fenced by the lease token.
//!
//! Per job:
//!   tx1: claim (commit — lease is durable before the handler runs)
//!   tx2: handler business writes + `complete` — atomic commit
//!   on error: rollback tx2, tx3: `fail_or_retry` (backoff/dead)

pub mod consumers;
pub mod handlers;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use consumers::{EventConsumer, EventCtx};
use contracts::AppError;
use infrastructure::db::Db;
use infrastructure::jobs::{self, Job, JobCtx, JobHandler};
use infrastructure::outbox::{self, OutboxRow};
use uuid::Uuid;

pub struct JobRegistry {
    handlers: HashMap<&'static str, Arc<dyn JobHandler>>,
    consumers: Vec<Arc<dyn EventConsumer>>,
}

impl JobRegistry {
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            consumers: Vec::new(),
        }
    }

    pub fn register<H: JobHandler + 'static>(&mut self, handler: H) {
        self.handlers.insert(handler.kind(), Arc::new(handler));
    }

    /// Register an outbox event consumer (spec §4.7 dispatcher). Modules
    /// call this from the same `handlers::<m>::register` as job handlers.
    pub fn register_consumer<C: EventConsumer + 'static>(&mut self, consumer: C) {
        self.consumers.push(Arc::new(consumer));
    }

    /// All consumers subscribed to `event_type`, in registration order.
    pub fn consumers_for(&self, event_type: &str) -> Vec<Arc<dyn EventConsumer>> {
        self.consumers
            .iter()
            .filter(|c| c.event_types().contains(&event_type))
            .cloned()
            .collect()
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

/// In-memory per-(event, consumer) retry backoff — outbox_events has no
/// attempt columns, so failures are throttled in the dispatcher only.
struct Backoff {
    failures: u32,
    next_at: Instant,
}

pub struct Runner {
    db: Db,
    registry: JobRegistry,
    /// Unique lease owner per process (A.6.7).
    pub owner: String,
    pub poll_interval: Duration,
    /// First retry delay after a consumer failure; doubles up to 5 min.
    pub backoff_base: Duration,
    backoffs: Mutex<HashMap<(Uuid, &'static str), Backoff>>,
}

const OUTBOX_BATCH: i64 = 100;
const BACKOFF_CAP: Duration = Duration::from_secs(300);

impl Runner {
    pub fn new(db: Db, registry: JobRegistry) -> Self {
        Self {
            db,
            registry,
            owner: format!("worker-{}", Uuid::new_v4()),
            poll_interval: Duration::from_millis(500),
            backoff_base: Duration::from_secs(2),
            backoffs: Mutex::new(HashMap::new()),
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

    // ------------------------- outbox dispatcher -------------------------

    fn backoff_ready(&self, event_id: Uuid, consumer: &'static str) -> bool {
        let m = self.backoffs.lock().unwrap();
        match m.get(&(event_id, consumer)) {
            Some(b) => Instant::now() >= b.next_at,
            None => true,
        }
    }

    fn record_backoff(&self, event_id: Uuid, consumer: &'static str) {
        let mut m = self.backoffs.lock().unwrap();
        let e = m.entry((event_id, consumer)).or_insert(Backoff {
            failures: 0,
            next_at: Instant::now(),
        });
        e.failures += 1;
        let delay = (self.backoff_base * 2u32.saturating_pow(e.failures.saturating_sub(1).min(9)))
            .min(BACKOFF_CAP);
        e.next_at = Instant::now() + delay;
    }

    fn clear_backoff(&self, event_id: Uuid, consumer: &'static str) {
        self.backoffs.lock().unwrap().remove(&(event_id, consumer));
    }

    /// One outbox tick: scan workspaces with unpublished events, deliver each
    /// event to every subscribed consumer in its own tx (`processed_events`
    /// insert + `handle` + commit), then `mark_published` once every
    /// subscriber has a committed processed row.
    ///
    /// Concurrent dispatchers are safe: `processed_once` blocks on the
    /// in-doubt unique row and resolves as a conflict once the winner
    /// commits, so a consumer's `handle` runs at most once.
    pub async fn dispatch_outbox_once(&self) -> Result<usize, AppError> {
        let mut handled = 0;
        for ws in outbox::pending_workspaces(&self.db).await? {
            handled += self.dispatch_workspace(ws).await?;
        }
        Ok(handled)
    }

    /// Deliver unpublished events of a single workspace (≤100, oldest
    /// first). Also the unit tests use — scans don't bleed across parallel
    /// workspaces this way.
    pub async fn dispatch_workspace(&self, workspace_id: Uuid) -> Result<usize, AppError> {
        let events: Vec<OutboxRow> = {
            let mut tx = self.db.begin(workspace_id).await?;
            let events = outbox::claim_unpublished(&mut tx, OUTBOX_BATCH).await?;
            tx.commit().await?;
            events
        };
        let mut handled = 0;
        for event in events {
            handled += self.dispatch_event(&event).await;
        }
        Ok(handled)
    }

    async fn dispatch_event(&self, event: &OutboxRow) -> usize {
        let consumers = self.registry.consumers_for(&event.event_type);
        if consumers.is_empty() {
            self.publish(event).await;
            return 0;
        }
        let mut ran = 0;
        let mut all_processed = true;
        for consumer in consumers {
            if !self.backoff_ready(event.id, consumer.name()) {
                all_processed = false;
                continue;
            }
            let mut tx = match self.db.begin(event.workspace_id).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(event_id = %event.id, consumer = consumer.name(), error = %e, "consumer tx begin failed");
                    all_processed = false;
                    continue;
                }
            };
            match outbox::processed_once(&mut tx, consumer.name(), event.id).await {
                Ok(true) => {}
                // Already processed (committed) — nothing to do.
                Ok(false) => {
                    let _ = tx.rollback().await;
                    continue;
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    tracing::error!(event_id = %event.id, consumer = consumer.name(), error = %e, "processed_once failed");
                    all_processed = false;
                    continue;
                }
            }
            let ctx = EventCtx {
                event_id: event.id,
                workspace_id: event.workspace_id,
                consumer: consumer.name(),
                dispatcher: self.owner.clone(),
            };
            match consumer.handle(&ctx, &mut tx, event).await {
                Ok(()) => match tx.commit().await {
                    Ok(()) => {
                        self.clear_backoff(event.id, consumer.name());
                        ran += 1;
                    }
                    Err(e) => {
                        tracing::warn!(event_id = %event.id, consumer = consumer.name(), code = %e.code.as_str(), "consumer commit failed");
                        self.record_backoff(event.id, consumer.name());
                        all_processed = false;
                    }
                },
                Err(e) => {
                    let _ = tx.rollback().await;
                    // Never log payloads — event id, consumer, code only.
                    tracing::warn!(event_id = %event.id, consumer = consumer.name(), code = %e.code.as_str(), "consumer failed");
                    self.record_backoff(event.id, consumer.name());
                    all_processed = false;
                }
            }
        }
        if all_processed {
            self.publish(event).await;
        }
        ran
    }

    async fn publish(&self, event: &OutboxRow) {
        match self.db.begin(event.workspace_id).await {
            Ok(mut tx) => {
                if let Err(e) = outbox::mark_published(&mut tx, event.id).await {
                    tracing::error!(event_id = %event.id, error = %e, "mark_published failed");
                }
                let _ = tx.commit().await;
            }
            Err(e) => tracing::error!(event_id = %event.id, error = %e, "publish tx begin failed"),
        }
    }

    /// Poll loop — run until cancelled.
    pub async fn run(&self) -> Result<(), AppError> {
        loop {
            let mut busy = false;
            match self.run_once().await {
                Ok(0) => {}
                Ok(_) => busy = true,
                Err(e) => tracing::error!(error = %e, "job poll failed"),
            }
            match self.dispatch_outbox_once().await {
                Ok(0) => {}
                Ok(_) => busy = true,
                Err(e) => tracing::error!(error = %e, "outbox poll failed"),
            }
            if !busy {
                tokio::time::sleep(self.poll_interval).await;
            }
        }
    }
}
