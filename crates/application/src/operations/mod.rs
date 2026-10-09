//! operations module use cases (spec §6 — owned by the module worker).
//!
//! B.2 rows: jobs get / retry-requests, audit-logs, dashboard/summary plus
//! the ops alerts view and the T30 send-workers restore lock.

pub mod alerts;
pub mod audit_logs;
pub mod dashboard;
pub mod jobs;
pub mod send_workers;

// T30 — re-exported under the task-pinned name so dispatch-side modules can
// gate submissions as `operations::send_workers_locked(tx)` after merge.
pub use infrastructure::repo::operations::send_workers_locked;
