//! crm module use cases (spec §6 — owned by the module worker).
//!
//! Companies/contacts/notes/tasks REST use cases, the company-merge job
//! body (`merge`), and the outbox→activities projection (`projector`)
//! consumed by `worker`'s `crm.activity_projector`.
//!
//! Authorization model (§5.3, spec §4.6):
//! - reads: `crm.read` scope (human roles imply it; credentials must carry it),
//!   plus the §5.6 mailbox-grant filter on mail-derived rows (repo SQL).
//! - writes: humans act by role (`crm_can_edit` ownership rule; Member =
//!   own or unassigned records); bearer credentials additionally need
//!   `crm.write`. Agents never write company/contact fields directly — they
//!   go through proposals — but may add an internal note with `crm.write`.

pub mod activities;
pub mod companies;
pub mod contacts;
pub mod merge;
pub mod notes;
pub mod projector;
pub mod tasks;
pub(crate) mod util;

use contracts::{AppError, AuthMethod, Ctx, Scope};

use crate::authz;

/// CRM read: humans carry the implied scope; service credentials must have
/// been granted `crm.read` explicitly.
pub(crate) fn require_crm_read(ctx: &Ctx) -> Result<(), AppError> {
    authz::require_scope(ctx, Scope::CrmRead)
}

/// CRM write: session-authenticated humans are governed by role/ownership
/// (checked at the use-case level); bearer credentials need `crm.write`.
pub(crate) fn require_crm_write(ctx: &Ctx) -> Result<(), AppError> {
    if let AuthMethod::ServiceCredential { .. } = ctx.auth {
        authz::require_scope(ctx, Scope::CrmWrite)?;
    }
    Ok(())
}
