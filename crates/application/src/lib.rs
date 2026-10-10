//! Application layer: use cases per module + authz/policy. `AppError` is the
//! `contracts` type re-exported (spec §4.3).

pub mod authz;

// Per-module use cases (spec §6). Module workers own these directories.
pub mod agents;
pub mod approvals;
pub mod booking;
pub mod campaigns;
pub mod compliance;
pub mod crm;
pub mod imports;
pub mod inbox;
pub mod mailboxes;
pub mod marketing;
pub mod operations;
pub mod org;
pub mod research;
pub mod sending;
pub mod smtp;
pub mod templates;

pub use contracts::AppError;
