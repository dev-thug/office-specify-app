//! org module domain types (spec §6 — owned by the module worker).
//!
//! Pure validation helpers for principal role/status transitions and
//! agent/service-credential scope checks. No I/O.

use contracts::{Role, Scope, Violation};

/// Roles a human principal may hold (DDL CHECK).
pub const HUMAN_ROLES: &[&str] = &["admin", "manager", "member"];

/// Status values settable via `PATCH /principals/{id}` — `invited` is a
/// system-managed state (invitation accept only).
pub const SETTABLE_STATUSES: &[&str] = &["active", "disabled"];

pub fn human_role_valid(role: &str) -> bool {
    HUMAN_ROLES.contains(&role)
}

pub fn settable_status_valid(status: &str) -> bool {
    SETTABLE_STATUSES.contains(&status)
}

pub fn role_to_str(role: Role) -> &'static str {
    match role {
        Role::Admin => "admin",
        Role::Manager => "manager",
        Role::Member => "member",
        Role::Agent => "agent",
        Role::System => "system",
    }
}

/// Validate an agent/credential scope list: every entry must be a known
/// scope and must not be in `Scope::AGENT_FORBIDDEN` (§5.4 — agents never
/// approve, grant, send directly, or unsuppress). Duplicates are removed;
/// order is preserved.
pub fn validate_agent_scopes(scopes: &[String]) -> Result<Vec<String>, Vec<Violation>> {
    let mut out: Vec<String> = Vec::new();
    let mut violations = Vec::new();
    for s in scopes {
        match Scope::parse(s) {
            Some(scope) if Scope::AGENT_FORBIDDEN.contains(&scope) => violations.push(Violation {
                field: "scopes".into(),
                code: format!("AGENT_SCOPE_FORBIDDEN:{s}"),
            }),
            Some(_) => {
                if !out.contains(s) {
                    out.push(s.clone());
                }
            }
            None => violations.push(Violation {
                field: "scopes".into(),
                code: format!("UNKNOWN_SCOPE:{s}"),
            }),
        }
    }
    if violations.is_empty() {
        Ok(out)
    } else {
        Err(violations)
    }
}

/// Basic invitation e-mail sanity (not a deliverability check).
pub fn invitation_email_valid(email: &str) -> bool {
    let e = email.trim();
    !e.is_empty() && e.len() <= 254 && e.contains('@') && !e.contains(char::is_whitespace)
}

/// Role usable for a member invitation (humans only).
pub fn invitation_role_valid(role: Role) -> bool {
    matches!(role, Role::Admin | Role::Manager | Role::Member)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_scopes_reject_send_and_approve() {
        let bad = validate_agent_scopes(&["crm.read".into(), "mail.send".into()]);
        assert!(bad.is_err());
        let bad = validate_agent_scopes(&["mail.approve".into()]);
        assert!(bad.is_err());
        let ok = validate_agent_scopes(&["crm.read".into(), "mail.read".into(), "crm.read".into()]);
        assert_eq!(ok.unwrap(), vec!["crm.read", "mail.read"]);
        let unknown = validate_agent_scopes(&["nope".into()]);
        assert!(unknown.is_err());
    }
}
