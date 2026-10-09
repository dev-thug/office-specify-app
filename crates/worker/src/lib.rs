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
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{FutureExt, StreamExt};

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

    /// The registered job kinds — the runner claims only these (F1).
    /// A worker running a subset of handlers never claims (or
    /// dead-letters) jobs of kinds it cannot run.
    pub fn kinds(&self) -> Vec<&'static str> {
        self.handlers.keys().copied().collect()
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
    event_type: String,
    failures: u32,
    next_at: Instant,
    /// Last failure timestamp — entries older than BACKOFF_PRUNE are
    /// dropped so the map cannot grow without bound (R17).
    last_failure: Instant,
}

pub struct Runner {
    db: Db,
    registry: JobRegistry,
    /// Unique lease owner per process (A.6.7).
    pub owner: String,
    pub poll_interval: Duration,
    /// First retry delay after a consumer failure; doubles up to 5 min.
    pub backoff_base: Duration,
    /// Job lease length in seconds (R13). Heartbeats run every lease/3.
    pub lease_seconds: i64,
    /// Max jobs handled concurrently per poll (R13; `WORKER_CONCURRENCY`).
    pub concurrency: usize,
    backoffs: Mutex<HashMap<(Uuid, &'static str), Backoff>>,
}

const OUTBOX_BATCH: i64 = 100;
const BACKOFF_CAP: Duration = Duration::from_secs(300);
/// Backoff entries idle longer than this are pruned (R17).
const BACKOFF_PRUNE: Duration = Duration::from_secs(3600);

impl Runner {
    pub fn new(db: Db, registry: JobRegistry) -> Self {
        Self {
            db,
            registry,
            owner: format!("worker-{}", Uuid::new_v4()),
            poll_interval: Duration::from_millis(500),
            backoff_base: Duration::from_secs(2),
            lease_seconds: 60,
            concurrency: 4,
            backoffs: Mutex::new(HashMap::new()),
        }
    }

    /// One poll iteration: reclaim expired leases, find workspaces with
    /// runnable jobs of this runner's registered kinds, claim and handle
    /// each — bounded-concurrency batches with a per-job heartbeat (R13).
    /// F1: kinds this runner cannot handle are never claimed.
    pub async fn run_once(&self) -> Result<usize, AppError> {
        jobs::reclaim_expired(&self.db).await?;
        let kinds = self.registry.kinds();
        let mut handled = 0;
        for ws in jobs::runnable_workspaces(&self.db, &kinds).await? {
            // Claim in its own tx so the lease commits before work starts.
            let claimed: Vec<Job> = {
                let mut tx = self.db.begin(ws).await?;
                let jobs = jobs::claim_lease(
                    &mut tx,
                    &self.owner,
                    jobs::BATCH_SIZE,
                    self.lease_seconds,
                    &kinds,
                )
                .await?;
                tx.commit().await?;
                jobs
            };
            handled += claimed.len();
            futures::stream::iter(claimed)
                .for_each_concurrent(self.concurrency, |job| self.handle_one(job))
                .await;
        }
        Ok(handled)
    }

    /// Heartbeat task for a running job (R13): extends the lease every
    /// lease/3, fenced by the lease token; exits when the fencing check
    /// fails or the handle aborts it on completion.
    fn spawn_heartbeat(&self, job: &Job) -> tokio::task::JoinHandle<()> {
        let db = self.db.clone();
        let ws = job.workspace_id;
        let job_id = job.id;
        let token = job.lease_token.unwrap_or_default();
        let every = Duration::from_millis(((self.lease_seconds.max(1) * 1000) / 3).max(50) as u64);
        let lease_seconds = self.lease_seconds;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                let ok = match db.begin(ws).await {
                    Ok(mut tx) => {
                        let r = jobs::heartbeat_lease(&mut tx, job_id, token, lease_seconds).await;
                        let _ = tx.commit().await;
                        matches!(r, Ok(true))
                    }
                    Err(_) => false,
                };
                if !ok {
                    return;
                }
            }
        })
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
            // F1: claims are kind-scoped, so this should be unreachable —
            // but if it ever happens the job goes back to `queued` with
            // the attempt refunded, never dead-lettered: work this runner
            // cannot handle is left for a runner that can.
            tracing::warn!(job_id = %job.id, kind = %job.kind, "claimed job has no registered handler — releasing");
            let mut tx = match self.db.begin(job.workspace_id).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!(error = %e, "begin failed");
                    return;
                }
            };
            let _ = jobs::release(&mut tx, job.id, ctx.lease_token).await;
            let _ = tx.commit().await;
            return;
        };

        // R13: a job outliving its lease is kept alive by heartbeats —
        // reclaim must never double-execute a slow handler.
        let heartbeat = self.spawn_heartbeat(&job);

        let mut tx = match self.db.begin(job.workspace_id).await {
            Ok(t) => t,
            Err(e) => {
                heartbeat.abort();
                tracing::error!(error = %e, "begin failed");
                return;
            }
        };
        // R14: a panicking handler becomes a failed attempt
        // (HANDLER_PANIC), never a crashed worker process.
        let outcome = AssertUnwindSafe(handler.handle(&ctx, &mut tx, job.payload.clone()))
            .catch_unwind()
            .await;
        heartbeat.abort();
        match outcome {
            Ok(Ok(())) => {
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
            Ok(Err(e)) => {
                let _ = tx.rollback().await;
                self.fail_job(
                    &job,
                    ctx.lease_token,
                    e.code.as_str(),
                    handler.external_effect(),
                )
                .await;
                tracing::warn!(job_id = %job.id, kind = %job.kind, code = e.code.as_str(), "job failed");
            }
            Err(_panic) => {
                let _ = tx.rollback().await;
                self.fail_job(
                    &job,
                    ctx.lease_token,
                    "HANDLER_PANIC",
                    handler.external_effect(),
                )
                .await;
                tracing::error!(job_id = %job.id, kind = %job.kind, "job handler panicked");
            }
        }
    }

    /// Record a failed attempt (retry with backoff or dead) in its own tx.
    async fn fail_job(&self, job: &Job, lease_token: Uuid, code: &str, is_external: bool) {
        // §16.4: external-effect handlers are not auto-retried after the
        // submitting boundary — they mark their own business row `unknown`
        // and finish; a runner-level error before that boundary may retry.
        let mut ftx = match self.db.begin(job.workspace_id).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(job_id = %job.id, error = %e, "fail tx begin failed");
                return;
            }
        };
        if let Err(e) = jobs::fail_or_retry(&mut ftx, job, lease_token, code).await {
            tracing::error!(job_id = %job.id, error = %e, "fail_or_retry failed");
        }
        if is_external {
            tracing::warn!(job_id = %job.id, code, "external-effect job failed");
        }
        let _ = ftx.commit().await;
    }

    // ------------------------- outbox dispatcher -------------------------

    fn backoff_ready(&self, event_id: Uuid, consumer: &'static str) -> bool {
        let m = self.backoffs.lock().unwrap();
        match m.get(&(event_id, consumer)) {
            Some(b) => Instant::now() >= b.next_at,
            None => true,
        }
    }

    fn record_backoff(&self, event: &OutboxRow, consumer: &'static str) {
        let mut m = self.backoffs.lock().unwrap();
        let e = m.entry((event.id, consumer)).or_insert_with(|| Backoff {
            event_type: event.event_type.clone(),
            failures: 0,
            next_at: Instant::now(),
            last_failure: Instant::now(),
        });
        e.failures += 1;
        e.last_failure = Instant::now();
        let delay = (self.backoff_base * 2u32.saturating_pow(e.failures.saturating_sub(1).min(9)))
            .min(BACKOFF_CAP);
        e.next_at = Instant::now() + delay;
    }

    fn clear_backoff(&self, event_id: Uuid, consumer: &'static str) {
        self.backoffs.lock().unwrap().remove(&(event_id, consumer));
    }

    /// Drop every backoff entry for an event (called once it publishes).
    fn clear_event_backoffs(&self, event_id: Uuid) {
        self.backoffs
            .lock()
            .unwrap()
            .retain(|(id, _), _| *id != event_id);
    }

    /// R17: events whose EVERY subscribed consumer is in active backoff.
    /// They are excluded from the claim so a run of poison events cannot
    /// starve deliverable ones. Also prunes entries idle >1h so the map
    /// stays bounded.
    fn fully_backed_off_ids(&self) -> Vec<Uuid> {
        let mut m = self.backoffs.lock().unwrap();
        let now = Instant::now();
        m.retain(|_, b| now.duration_since(b.last_failure) < BACKOFF_PRUNE);
        let mut active: HashMap<Uuid, (String, Vec<&'static str>)> = HashMap::new();
        for ((event_id, consumer), b) in m.iter() {
            if now < b.next_at {
                active
                    .entry(*event_id)
                    .or_insert_with(|| (b.event_type.clone(), Vec::new()))
                    .1
                    .push(*consumer);
            }
        }
        active
            .into_iter()
            .filter(|(_, (event_type, backing))| {
                let subscribed = self.registry.consumers_for(event_type);
                !subscribed.is_empty() && subscribed.iter().all(|c| backing.contains(&c.name()))
            })
            .map(|(id, _)| id)
            .collect()
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
        let excluded = self.fully_backed_off_ids();
        let events: Vec<OutboxRow> = {
            let mut tx = self.db.begin(workspace_id).await?;
            let events = outbox::claim_unpublished(&mut tx, OUTBOX_BATCH, &excluded).await?;
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
                        self.record_backoff(event, consumer.name());
                        all_processed = false;
                    }
                },
                Err(e) => {
                    let _ = tx.rollback().await;
                    // Never log payloads — event id, consumer, code only.
                    tracing::warn!(event_id = %event.id, consumer = consumer.name(), code = %e.code.as_str(), "consumer failed");
                    self.record_backoff(event, consumer.name());
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
                // R17: published events never need backoff state again.
                self.clear_event_backoffs(event.id);
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
