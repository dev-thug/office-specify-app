//! Shared contracts: error codes, problem details, envelopes, request context.
//! Every module uses these types and nothing else for cross-cutting concerns
//! (spec §4, source B.1).

mod envelope;
mod error;
mod shared;

pub use envelope::{Envelope, Meta, Page};
pub use error::{AppError, ErrorCode, Problem, Violation};
pub use shared::{AuthMethod, Ctx, PrincipalKind, Role, Scope, ScopeSet};

// Per-module DTOs (spec §6). Module workers own these files.
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
pub mod smtp;
pub mod templates;
