//! research module domain types (spec §6 — owned by the module worker).
//!
//! Pure helpers for the lead-proposal pipeline (source §13.4, B.3.12):
//! batch limits, required-field validation, and extraction of the canonical
//! contact/company shapes out of the free-form `proposed_data` object.
//! No I/O, no SQL — the accept path in `application::research` uses these.

use contracts::Violation;
use contracts::research::LeadProposalInput;

/// Max proposals per `POST /lead-proposals` request (§13.4).
pub const MAX_BATCH_PROPOSALS: usize = 100;
/// `companies.name` CHECK bound.
pub const MAX_NAME_LEN: usize = 300;
/// `lead_proposals.company_name` bound (same semantics as companies).
pub const MAX_COMPANY_NAME_LEN: usize = MAX_NAME_LEN;
/// Field length bound for contact names/titles imported from proposals.
pub const MAX_FIELD_LEN: usize = 300;

/// One contact candidate inside `proposed_data.contacts[]`.
///
/// The canonical shape the accept path understands is
/// `proposed_data.contacts: [{name, job_title?, department?, email?}]`.
/// Entries without a usable `name` are skipped — a nameless contact is not
/// actionable CRM data. Emails are stored verbatim; they are *candidates*
/// and never receive a valid communication basis on accept (§13.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedContact {
    pub name: String,
    pub job_title: Option<String>,
    pub department: Option<String>,
    /// Candidate email exactly as proposed — never fabricated from name
    /// patterns (§13.4 forbids generated addresses).
    pub email: Option<String>,
}

fn opt_str(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Extract contact candidates from `proposed_data["contacts"]`.
pub fn extract_contacts(proposed_data: &serde_json::Value) -> Vec<ProposedContact> {
    let Some(items) = proposed_data.get("contacts").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let name = opt_str(item, "name")?;
            Some(ProposedContact {
                name,
                job_title: opt_str(item, "job_title"),
                department: opt_str(item, "department"),
                email: opt_str(item, "email"),
            })
        })
        .collect()
}

/// Company-level fields a proposal may carry (`proposed_data.industry`,
/// `country_code`, `website_url`). Only string values are read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProposedCompanyFields {
    pub industry: Option<String>,
    pub country_code: Option<String>,
    pub website_url: Option<String>,
}

pub fn extract_company_fields(proposed_data: &serde_json::Value) -> ProposedCompanyFields {
    ProposedCompanyFields {
        industry: opt_str(proposed_data, "industry"),
        country_code: opt_str(proposed_data, "country_code"),
        website_url: opt_str(proposed_data, "website_url"),
    }
}

/// Normalize a company domain for dedupe comparison: trim, lowercase,
/// trailing dot stripped, IDN → ASCII (punycode). Returns `None` for empty
/// input or names that cannot be DNS names at all.
pub fn normalize_domain(raw: &str) -> Option<String> {
    let d = raw.trim().trim_end_matches('.').to_lowercase();
    if d.is_empty() {
        return None;
    }
    idna::domain_to_ascii(&d).ok().filter(|a| !a.is_empty())
}

/// §13.4 required-item validation for one proposal inside a batch.
/// `index` is the item position — violation fields point at
/// `proposals[i].<field>` so the client can fix the exact entry.
/// Deep URL policy (public http(s) only) is enforced by the infrastructure
/// layer — it needs a WHATWG URL parser — not here.
pub fn validate_proposal(item: &LeadProposalInput, index: usize) -> Vec<Violation> {
    let mut v = Vec::new();
    let prefix = format!("proposals[{index}]");
    if item.company_name.trim().is_empty() {
        v.push(Violation {
            field: format!("{prefix}.company_name"),
            code: "required".into(),
        });
    } else if item.company_name.chars().count() > MAX_COMPANY_NAME_LEN {
        v.push(Violation {
            field: format!("{prefix}.company_name"),
            code: "too_long".into(),
        });
    }
    // "회사명·도메인 또는 식별 근거": a proposal needs *some* identifier
    // beyond the name — a domain, an external key, or structured data.
    let has_identifier = item.domain.as_deref().is_some_and(|d| !d.trim().is_empty())
        || item
            .external_key
            .as_deref()
            .is_some_and(|k| !k.trim().is_empty())
        || item
            .proposed_data
            .as_object()
            .is_some_and(|o| !o.is_empty());
    if !has_identifier {
        v.push(Violation {
            field: format!("{prefix}.domain"),
            code: "identifier_required".into(),
        });
    }
    if !item.proposed_data.is_object() {
        v.push(Violation {
            field: format!("{prefix}.proposed_data"),
            code: "must_be_object".into(),
        });
    }
    if item.evidence.is_empty() {
        // At least one source is mandatory — unsubstantiated proposals are
        // storable data-wise only via their own row, never evidence-free.
        v.push(Violation {
            field: format!("{prefix}.evidence"),
            code: "required".into(),
        });
    }
    for (j, ev) in item.evidence.iter().enumerate() {
        if ev.field_path.trim().is_empty() {
            v.push(Violation {
                field: format!("{prefix}.evidence[{j}].field_path"),
                code: "required".into(),
            });
        }
        if ev.source_url.trim().is_empty() {
            v.push(Violation {
                field: format!("{prefix}.evidence[{j}].source_url"),
                code: "required".into(),
            });
        }
        if let Some(c) = ev.confidence
            && !(0.0..=1.0).contains(&c)
        {
            v.push(Violation {
                field: format!("{prefix}.evidence[{j}].confidence"),
                code: "out_of_range".into(),
            });
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::research::{EvidenceInput, EvidenceType, LeadProposalInput};

    fn item() -> LeadProposalInput {
        LeadProposalInput {
            external_key: Some("k1".into()),
            company_name: "Acme".into(),
            domain: Some("acme.example".into()),
            proposed_data: serde_json::json!({"industry": "mfg"}),
            evidence: vec![EvidenceInput {
                field_path: "industry".into(),
                source_url: "https://acme.example/about".into(),
                observed_at: chrono::Utc::now(),
                value: serde_json::json!("mfg"),
                evidence_type: EvidenceType::Observed,
                excerpt: None,
                confidence: Some(0.9),
            }],
        }
    }

    #[test]
    fn valid_item_passes() {
        assert!(validate_proposal(&item(), 0).is_empty());
    }

    #[test]
    fn missing_evidence_and_name_flagged() {
        let mut i = item();
        i.evidence.clear();
        i.company_name = "  ".into();
        let v = validate_proposal(&i, 2);
        assert!(v.iter().any(|x| x.field == "proposals[2].company_name"));
        assert!(v.iter().any(|x| x.field == "proposals[2].evidence"));
    }

    #[test]
    fn identifier_required() {
        let mut i = item();
        i.domain = None;
        i.external_key = None;
        i.proposed_data = serde_json::json!({});
        assert!(
            validate_proposal(&i, 0)
                .iter()
                .any(|x| x.code == "identifier_required")
        );
    }

    #[test]
    fn contacts_extraction_skips_nameless() {
        let data = serde_json::json!({"contacts": [
            {"name": "Kim", "email": "kim@x.dev", "job_title": "VP"},
            {"job_title": "no name"},
            {"name": "  "},
            {"name": "Lee", "department": "Sales"}
        ]});
        let cs = extract_contacts(&data);
        assert_eq!(cs.len(), 2);
        assert_eq!(cs[0].email.as_deref(), Some("kim@x.dev"));
        assert_eq!(cs[1].department.as_deref(), Some("Sales"));
    }
}
