//! Per-module MCP tool registries (spec §6, B.6 — module workers own these
//! files; `pub fn tools() -> Vec<ToolDef>` is pre-wired into the server).

pub mod agents;
pub mod approvals;
pub mod booking;
pub mod campaigns;
pub mod compliance;
pub mod crm;
pub mod discovery;
pub mod imports;
pub mod inbox;
pub mod mailboxes;
pub mod operations;
pub mod org;
pub mod research;
pub mod sales_campaigns;
pub mod sending;
pub mod templates;
