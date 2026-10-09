//! Per-module job handlers (spec §6 — module workers own these files and
//! only register their own kinds).

pub mod agents;
pub mod approvals;
pub mod booking;
pub mod campaigns;
pub mod compliance;
pub mod crm;
pub mod imports;
pub mod inbox;
pub mod mailboxes;
pub mod operations;
pub mod org;
pub mod research;
pub mod sending;
pub mod templates;

pub mod marketing;
