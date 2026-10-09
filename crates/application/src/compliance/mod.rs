//! compliance module use cases (spec §6 — owned by the module worker).
//!
//! - `bases` — `/communication-bases` (+reviews, +revoke).
//! - `suppressions` — `/suppressions` (+release-requests).
//! - `public_unsubscribe` — `/public/v1/unsubscribe/{token}` GET/POST and
//!   the RFC 8058 one-click endpoint (currently gated off).
//! - `release` — the `compliance.release_suppression` job body.

pub mod bases;
pub mod public_unsubscribe;
pub mod release;
pub mod suppressions;

use contracts::{AppError, Role, Violation};

/// Read access to compliance data — every human role may inspect the
/// suppression/basis state that governs their own sends (§5.3 shared CRM
/// read; agents carry no compliance scope).
pub(crate) const READ_ROLES: &[Role] = &[Role::Admin, Role::Manager, Role::Member];

/// Decisions about communication bases and suppression release —
/// compliance management (B.2 "준수 관리"/"인간 검토").
pub(crate) const REVIEW_ROLES: &[Role] = &[Role::Admin, Role::Manager];

/// Adding a suppression or registering a basis is fail-safe: any active
/// principal (incl. agents — §5.3 "차단 제안/제한적 추가") may record one;
/// status stays `pending`/the actor is audited either way. The §5.3
/// qualifiers — Member "업무 범위", Agent "제한적 추가" — are enforced in
/// the use case (`require_address_in_scope` + `agent_create_quota`), not
/// by this role list.
pub(crate) const WRITE_ROLES: &[Role] = &[Role::Admin, Role::Manager, Role::Member, Role::Agent];

pub(crate) fn field_violation(field: &str, code: &str, detail: &str) -> AppError {
    AppError::validation(
        detail.to_string(),
        vec![Violation {
            field: field.into(),
            code: code.into(),
        }],
    )
    .with_status(422)
}
