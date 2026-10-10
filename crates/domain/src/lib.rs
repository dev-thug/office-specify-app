//! Pure domain types: state machines, normalization, hashing, rendering,
//! slot math. No framework or HTTP dependencies (source §4.2).

pub mod email_address;

// Per-module domain types (spec §6). Module workers own these files.
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
