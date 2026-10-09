//! mailboxes module domain types (spec §6 — owned by the module worker).
//!
//! Pure helpers: grant action names (DDL CHECK dependency chain), Zoho
//! region allowlist (§14.1), and channel activation policy (§4.9/T33).

use contracts::Violation;

/// Grant actions — canonical names used by `grant_allows` and the
/// `allowed_actions` DTO list (B.3.1 `mail.*` wire strings).
pub mod grant_actions {
    pub const READ: &str = "read";
    pub const COMPOSE: &str = "compose";
    pub const SEND: &str = "send";
    pub const APPROVE: &str = "approve";
    pub const ASSIGN: &str = "assign";
    pub const ALL: &[&str] = &[READ, COMPOSE, SEND, APPROVE, ASSIGN];
}

/// `mail.*` wire strings for `allowed_actions` (B.3.1).
pub fn allowed_action_wire(action: &str) -> String {
    format!("mail.{action}")
}

/// Normalize an action name: accepts `read`, `can_read`, `mail.read`.
/// Returns the canonical column suffix or None for unknown actions.
pub fn normalize_grant_action(action: &str) -> Option<&'static str> {
    let a = action
        .trim()
        .trim_start_matches("can_")
        .trim_start_matches("mail.");
    match a {
        "read" => Some(grant_actions::READ),
        "compose" => Some(grant_actions::COMPOSE),
        "send" => Some(grant_actions::SEND),
        "approve" => Some(grant_actions::APPROVE),
        "assign" => Some(grant_actions::ASSIGN),
        _ => None,
    }
}

/// Validate grant flags against the DDL CHECK dependency chain
/// (§5.4): compose→read, send→compose, approve→send, assign→read.
/// `can_approve` additionally requires an Admin/Manager target — that check
/// stays in the use case (needs the principal row).
pub fn grant_flag_violations(
    can_read: bool,
    can_compose: bool,
    can_send: bool,
    can_approve: bool,
    can_assign: bool,
) -> Vec<Violation> {
    let mut v = Vec::new();
    if can_compose && !can_read {
        v.push(Violation {
            field: "can_compose".into(),
            code: "REQUIRES_CAN_READ".into(),
        });
    }
    if can_send && !can_compose {
        v.push(Violation {
            field: "can_send".into(),
            code: "REQUIRES_CAN_COMPOSE".into(),
        });
    }
    if can_approve && !can_send {
        v.push(Violation {
            field: "can_approve".into(),
            code: "REQUIRES_CAN_SEND".into(),
        });
    }
    if can_assign && !can_read {
        v.push(Violation {
            field: "can_assign".into(),
            code: "REQUIRES_CAN_READ".into(),
        });
    }
    v
}

/// Zoho datacenter region allowlist (§14.1 — hosts fixed server-side).
pub const ZOHO_REGIONS: &[&str] = &["com", "eu", "in", "com.au", "jp", "ca"];

pub fn zoho_region_allowed(region: &str) -> bool {
    ZOHO_REGIONS.contains(&region)
}

/// `https://accounts.zoho.<region>` — the OAuth accounts host.
pub fn zoho_accounts_base(region: &str) -> String {
    format!("https://accounts.zoho.{region}")
}

/// OAuth scopes requested per connection flow.
pub const ZOHO_MAIL_SCOPES: &[&str] = &["ZohoMail.messages.ALL", "ZohoMail.accounts.READ"];
pub const ZOHO_CALENDAR_SCOPES: &[&str] = &["ZohoCalendar.calendar.ALL"];

/// `delivery_channels.provider` values allowed for a purpose. Zoho Mail can
/// only ever drive `correspondence` (§4.9, DDL CHECK — marketing and
/// transactional must go through dedicated delivery providers).
pub fn provider_allowed_for_purpose(provider: &str, purpose: &str) -> bool {
    match provider {
        "zoho_mail" => purpose == "correspondence",
        "delivery_provider" | "transactional_provider" => {
            matches!(purpose, "marketing" | "transactional" | "correspondence")
        }
        _ => false,
    }
}

/// T33 / §4.9: a channel may never be enabled when the provider is
/// `zoho_mail` and the purpose is not `correspondence`.
pub fn activation_policy_blocked(provider: &str, purpose: &str) -> bool {
    provider == "zoho_mail" && purpose != "correspondence"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_chain_matches_ddl_checks() {
        assert!(
            grant_flag_violations(false, true, false, false, false)
                .iter()
                .any(|v| v.field == "can_compose")
        );
        assert!(
            grant_flag_violations(true, false, true, false, false)
                .iter()
                .any(|v| v.field == "can_send")
        );
        assert!(
            grant_flag_violations(true, true, false, true, false)
                .iter()
                .any(|v| v.field == "can_approve")
        );
        assert!(
            grant_flag_violations(false, false, false, false, true)
                .iter()
                .any(|v| v.field == "can_assign")
        );
        assert!(grant_flag_violations(true, true, true, true, true).is_empty());
    }

    #[test]
    fn t33_zoho_mail_marketing_is_never_enableable() {
        assert!(activation_policy_blocked("zoho_mail", "marketing"));
        assert!(activation_policy_blocked("zoho_mail", "transactional"));
        assert!(!activation_policy_blocked("zoho_mail", "correspondence"));
        assert!(!activation_policy_blocked("delivery_provider", "marketing"));
    }

    #[test]
    fn action_normalization() {
        assert_eq!(normalize_grant_action("read"), Some("read"));
        assert_eq!(normalize_grant_action("can_send"), Some("send"));
        assert_eq!(normalize_grant_action("mail.approve"), Some("approve"));
        assert_eq!(normalize_grant_action("bogus"), None);
    }
}
