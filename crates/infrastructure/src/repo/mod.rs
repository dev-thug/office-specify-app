//! Repositories — the only modules allowed to speak SQL (spec §6).
//! `auth` and `compliance_core` are foundation-owned; each `<module>.rs` is
//! owned by its module worker (empty placeholders registered now).

pub mod auth;
pub mod compliance_core;

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
pub mod templates;

pub mod zoho_templates;
