//! crm module domain types (spec §6): email/doman normalization glue,
//! merge field-choice validation, activity kinds. Pure — no DB/HTTP.

use crate::email_address::LocalPartPolicy;
use serde_json::Value;

/// `activities.kind` values written by this module. The projector uses the
/// outbox event type names verbatim; crm's own notes/tasks/companies write
/// the `*.created`-style kinds. `UNIQUE (workspace_id, source_event_id,
/// kind)` is the dedupe anchor, so kinds must be stable.
pub mod activity_kind {
    pub const LEAD_ACCEPTED: &str = "lead.accepted";
    pub const APPROVAL_DECIDED: &str = "approval.decided";
    pub const OUTBOUND_ACCEPTED: &str = "outbound.accepted";
    pub const MESSAGE_RECEIVED: &str = "message.received";
    pub const SUPPRESSION_CREATED: &str = "suppression.created";
    pub const BOOKING_CONFIRMED: &str = "booking.confirmed";
    pub const COMPANY_CREATED: &str = "company.created";
    pub const COMPANY_ARCHIVED: &str = "company.archived";
    pub const COMPANY_MERGED: &str = "company.merged";
    pub const CONTACT_CREATED: &str = "contact.created";
    pub const CONTACT_EMAIL_ADDED: &str = "contact.email_added";
    pub const NOTE_CREATED: &str = "note.created";
    pub const TASK_CREATED: &str = "task.created";
    pub const TASK_COMPLETED: &str = "task.completed";
    pub const TASK_CANCELED: &str = "task.canceled";
}

/// Fields a merge request may pick from the source company
/// (integration-contracts `crm.merge_companies` payload).
pub const MERGEABLE_FIELDS: &[&str] = &[
    "name",
    "domain",
    "website_url",
    "industry",
    "country_code",
    "lifecycle_stage",
    "description",
    "custom_fields",
    "owner_principal_id",
];

/// One `field_choices` value: which side wins for the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergePick {
    Source,
    Target,
}

impl MergePick {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "source" => Some(Self::Source),
            "target" => Some(Self::Target),
            _ => None,
        }
    }
}

/// Validate a `field_choices` map: keys ⊆ MERGEABLE_FIELDS, values ∈
/// {source, target}. Returns the offending entries for a 400 violation list.
pub fn invalid_field_choices(choices: &std::collections::BTreeMap<String, String>) -> Vec<String> {
    let mut bad = Vec::new();
    for (field, pick) in choices {
        if !MERGEABLE_FIELDS.contains(&field.as_str()) || MergePick::parse(pick).is_none() {
            bad.push(format!("field_choices.{field}"));
        }
    }
    bad
}

/// Workspace-fixed local-part case policy (§6.3). `workspaces.settings`
/// key `email_local_part_policy`: `"preserve"` keeps local-part case;
/// anything else (incl. absent) is the conservative lowercase default.
pub fn local_part_policy(settings: Option<&Value>) -> LocalPartPolicy {
    match settings
        .and_then(|s| s.get("email_local_part_policy"))
        .and_then(Value::as_str)
    {
        Some("preserve") => LocalPartPolicy::Preserve,
        _ => LocalPartPolicy::Lowercase,
    }
}

/// Conservative company-domain normalization for dedupe/compare (§6.3 does
/// not mandate provider guessing): trim, lowercase, strip a URL scheme and
/// path/port/trailing dot. Returns None when nothing usable remains.
pub fn normalize_domain(raw: &str) -> Option<String> {
    let mut d = raw.trim().to_lowercase();
    if let Some(rest) = d.split_once("://") {
        d = rest.1.to_string();
    }
    // Keep only the host part.
    if let Some((host, _)) = d.split_once('/') {
        d = host.to_string();
    }
    if let Some((host, _)) = d.split_once(':') {
        d = host.to_string();
    }
    while d.ends_with('.') {
        d.pop();
    }
    if let Some(stripped) = d.strip_prefix("www.") {
        d = stripped.to_string();
    }
    let valid = !d.is_empty()
        && d.len() <= 253
        && d.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'));
    valid.then_some(d)
}

/// Escape `ILIKE` wildcards in a user search string (used with `ESCAPE '\'`).
pub fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn domain_normalization() {
        assert_eq!(
            normalize_domain("  HTTPS://WWW.Example.COM/path?q=1 ").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_domain("example.com:443").as_deref(),
            Some("example.com")
        );
        assert_eq!(normalize_domain("localhost").as_deref(), Some("localhost"));
        assert_eq!(normalize_domain("  "), None);
        assert_eq!(normalize_domain("bad domain!.com"), None);
    }

    #[test]
    fn field_choices_validation() {
        let mut ok = std::collections::BTreeMap::new();
        ok.insert("name".to_string(), "source".to_string());
        assert!(invalid_field_choices(&ok).is_empty());
        ok.insert("version".to_string(), "source".to_string());
        ok.insert("name".to_string(), "sideways".to_string());
        let bad = invalid_field_choices(&ok);
        assert_eq!(bad.len(), 2);
    }

    #[test]
    fn local_part_policy_from_settings() {
        assert_eq!(local_part_policy(None), LocalPartPolicy::Lowercase);
        assert_eq!(
            local_part_policy(Some(&json!({"email_local_part_policy": "preserve"}))),
            LocalPartPolicy::Preserve
        );
        assert_eq!(
            local_part_policy(Some(&json!({"email_local_part_policy": "lower"}))),
            LocalPartPolicy::Lowercase
        );
    }
}
