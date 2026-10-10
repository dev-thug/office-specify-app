//! campaigns module application layer (spec §8). Every state-changing use
//! case runs inside a workspace-scoped `Tx` with audit + outbox + job
//! enqueue committed atomically (§9.4). Freeze rules follow A.6.2: launches
//! build → freeze → approve → run; once frozen only execution-state columns
//! may change.

mod campaign;
mod flow;
mod freeze;
mod launch;
mod preflight;

pub use campaign::{create_campaign, create_revision, get_campaign, list_campaigns};
pub use flow::{
    activate_launch, mark_enrollment_failed, pause_address, pause_enrollments, schedule_next_step,
};
pub use freeze::freeze_launch;
pub use launch::{
    cancel_launch, create_launch, execute_launch, get_launch, get_launch_status, pause_launch,
    request_launch_approval,
};
pub use preflight::preflight;
