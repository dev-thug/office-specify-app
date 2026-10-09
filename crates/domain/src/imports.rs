//! imports module domain types (spec §6 — owned by the module worker).
//!
//! Pure logic for the CSV import pipeline (source §7.3 UI11, §15.1, B.3.13):
//! mapping validation, per-row normalization into `import_rows
//! .normalized_data`, the per-row error shape, and the OWASP formula-
//! injection guard applied to every cell written on export.

use std::collections::BTreeMap;

use contracts::Violation;
use serde::{Deserialize, Serialize};

/// B.1.1 global input limits: CSV 20MB / 100k rows.
pub const MAX_IMPORT_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_IMPORT_ROWS: usize = 100_000;
/// Export row cap — a fixed snapshot needs a bound (B.1.5).
pub const MAX_EXPORT_ROWS: usize = 100_000;

/// Canonical fields a CSV column may map to (B.3.13 `mapping`).
/// `contact.email_label` annotates the address link (`contact_emails.label`).
pub const CANONICAL_FIELDS: &[&str] = &[
    "company.name",
    "company.domain",
    "company.industry",
    "company.country_code",
    "company.website_url",
    "contact.name",
    "contact.job_title",
    "contact.department",
    "contact.email",
    "contact.email_label",
];

/// Fields selectable on `POST /exports` (superset of import fields + CRM
/// metadata). Mail-derived fields are deliberately absent — the export
/// allowlist can never expose mailbox content (§5.6).
pub const EXPORTABLE_FIELDS: &[&str] = &[
    "company.name",
    "company.domain",
    "company.industry",
    "company.country_code",
    "company.website_url",
    "company.lifecycle_stage",
    "contact.name",
    "contact.job_title",
    "contact.department",
    "contact.status",
    "contact.email",
    "contact.email_label",
];

/// A single row-level problem, stored in `import_rows.errors` (JSONB array).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowError {
    /// Canonical field the error belongs to, when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Stable machine code: `required`, `invalid_email`, `invalid_domain`,
    /// `invalid_country_code`, `too_long`, `duplicate_company_domain`,
    /// `duplicate_email`, `unparseable`.
    pub code: String,
    pub message: String,
}

impl RowError {
    pub fn new(field: Option<&str>, code: &str, message: impl Into<String>) -> Self {
        Self {
            field: field.map(str::to_string),
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// `mapping.columns` value → canonical field allowlist; each canonical
/// field may be mapped at most once; at least one of `company.name`,
/// `contact.name`, `contact.email` must be present for a row to be
/// meaningful.
pub fn validate_mapping(columns: &BTreeMap<String, String>) -> Vec<Violation> {
    let mut v = Vec::new();
    if columns.is_empty() {
        v.push(Violation {
            field: "mapping.columns".into(),
            code: "required".into(),
        });
        return v;
    }
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (csv_col, canonical) in columns {
        if !CANONICAL_FIELDS.contains(&canonical.as_str()) {
            v.push(Violation {
                field: format!("mapping.columns.{csv_col}"),
                code: "unknown_field".into(),
            });
        } else if let Some(prev) = seen.insert(canonical.as_str(), csv_col.as_str()) {
            let _ = prev;
            v.push(Violation {
                field: format!("mapping.columns.{csv_col}"),
                code: "field_mapped_twice".into(),
            });
        }
    }
    let has_anchor = ["company.name", "contact.name", "contact.email"]
        .iter()
        .any(|f| columns.values().any(|c| c == f));
    if !has_anchor {
        v.push(Violation {
            field: "mapping.columns".into(),
            code: "anchor_required".into(),
        });
    }
    v
}

/// Build `normalized_data` for one CSV record: raw trimmed values grouped
/// under `company`/`contact`. Email normalization (dedupe key) is added by
/// `validate_row` — it needs the workspace local-part policy.
pub fn apply_mapping(
    headers: &[String],
    record: &[String],
    columns: &BTreeMap<String, String>,
) -> serde_json::Value {
    let mut company = serde_json::Map::new();
    let mut contact = serde_json::Map::new();
    for (i, h) in headers.iter().enumerate() {
        let Some(canon) = columns.get(h) else {
            continue;
        };
        let cell = record.get(i).map(|s| s.trim()).unwrap_or("");
        if cell.is_empty() {
            continue;
        }
        let (group, key) = canon.split_once('.').unwrap_or(("", canon.as_str()));
        let target = match group {
            "company" => &mut company,
            _ => &mut contact,
        };
        target.insert(key.to_string(), serde_json::Value::String(cell.to_string()));
    }
    serde_json::json!({"company": company, "contact": contact})
}

/// Validate a normalized row. Returns row errors; on success the caller
/// treats it as `valid` or `duplicate` (dup check is a DB concern).
///
/// `email_normalized` is embedded into `contact.email_normalized` when the
/// candidate parses — callers index on it.
pub fn validate_row(
    normalized: &mut serde_json::Value,
    local_policy: crate::email_address::LocalPartPolicy,
) -> Vec<RowError> {
    let mut errors = Vec::new();
    let company = normalized
        .get("company")
        .and_then(|c| c.as_object())
        .cloned()
        .unwrap_or_default();
    let contact = normalized
        .get("contact")
        .and_then(|c| c.as_object())
        .cloned()
        .unwrap_or_default();

    let get = |m: &serde_json::Map<String, serde_json::Value>, k: &str| {
        m.get(k).and_then(|v| v.as_str()).map(str::to_string)
    };

    // Row must carry at least one meaningful value.
    let meaningful = get(&company, "name").is_some()
        || get(&company, "domain").is_some()
        || get(&contact, "name").is_some()
        || get(&contact, "email").is_some();
    if !meaningful {
        errors.push(RowError::new(None, "empty_row", "row has no mapped values"));
        return errors;
    }

    if let Some(name) = get(&company, "name")
        && name.chars().count() > 300
    {
        errors.push(RowError::new(
            Some("company.name"),
            "too_long",
            "company name exceeds 300 characters",
        ));
    }
    if let Some(domain) = get(&company, "domain")
        && crate::research::normalize_domain(&domain).is_none()
    {
        errors.push(RowError::new(
            Some("company.domain"),
            "invalid_domain",
            "company domain is empty",
        ));
    }
    if let Some(cc) = get(&company, "country_code") {
        let ok = cc.len() == 2 && cc.bytes().all(|b| b.is_ascii_uppercase());
        if !ok {
            errors.push(RowError::new(
                Some("company.country_code"),
                "invalid_country_code",
                "country code must be two uppercase letters (ISO 3166-1 alpha-2)",
            ));
        }
    }
    if let Some(name) = get(&contact, "name")
        && name.chars().count() > 300
    {
        errors.push(RowError::new(
            Some("contact.name"),
            "too_long",
            "contact name exceeds 300 characters",
        ));
    }
    if let Some(email) = get(&contact, "email") {
        match crate::email_address::normalize(&email, local_policy) {
            Ok(n) => {
                if let Some(c) = normalized
                    .get_mut("contact")
                    .and_then(|c| c.as_object_mut())
                {
                    c.insert(
                        "email_normalized".to_string(),
                        serde_json::Value::String(n.normalized_key.clone()),
                    );
                }
            }
            Err(e) => errors.push(RowError::new(
                Some("contact.email"),
                "invalid_email",
                e.to_string(),
            )),
        }
    }
    errors
}

// ------------------------------------------------------- CSV export safety

/// OWASP CSV/formula injection guard (§15.1): cells starting with
/// `=`, `+`, `-`, `@`, a tab or a carriage return are prefixed with a
/// single quote so spreadsheet apps render them as text.
pub fn sanitize_csv_cell(cell: &str) -> String {
    match cell.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{cell}"),
        _ => cell.to_string(),
    }
}

fn escape_csv_field(cell: &str) -> String {
    if cell.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", cell.replace('"', "\"\""))
    } else {
        cell.to_string()
    }
}

/// Render a CSV document; every cell passes `sanitize_csv_cell` then RFC
/// 4180 quoting. Header cells are canonical field names (safe), but are
/// sanitized anyway for defense in depth.
pub fn render_csv(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    let line = |cells: &[String]| {
        cells
            .iter()
            .map(|c| escape_csv_field(&sanitize_csv_cell(c)))
            .collect::<Vec<_>>()
            .join(",")
    };
    out.push_str(&line(headers));
    out.push('\n');
    for row in rows {
        out.push_str(&line(row));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_prefixes_dangerous_cells() {
        assert_eq!(sanitize_csv_cell("=1+1"), "'=1+1");
        assert_eq!(sanitize_csv_cell("+cmd"), "'+cmd");
        assert_eq!(sanitize_csv_cell("-2"), "'-2");
        assert_eq!(sanitize_csv_cell("@x"), "'@x");
        assert_eq!(sanitize_csv_cell("\t=x"), "'\t=x");
        assert_eq!(sanitize_csv_cell("\r=x"), "'\r=x");
        assert_eq!(sanitize_csv_cell("plain"), "plain");
        assert_eq!(sanitize_csv_cell(""), "");
    }

    #[test]
    fn csv_render_escapes_and_sanitizes() {
        let out = render_csv(
            &["a".into(), "b".into()],
            &[vec!["=x".into(), "he said \"hi\", ok".into()]],
        );
        assert_eq!(out, "a,b\n'=x,\"he said \"\"hi\"\", ok\"\n");
    }

    #[test]
    fn mapping_validation() {
        let mut cols = BTreeMap::new();
        cols.insert("Company".to_string(), "company.name".to_string());
        assert!(
            validate_mapping(&cols)
                .iter()
                .all(|v| v.code != "unknown_field")
        );
        cols.insert("Weird".to_string(), "nope".to_string());
        cols.insert("Company2".to_string(), "company.name".to_string());
        let v = validate_mapping(&cols);
        assert!(v.iter().any(|x| x.code == "unknown_field"));
        assert!(v.iter().any(|x| x.code == "field_mapped_twice"));
    }

    #[test]
    fn row_validation_email_and_anchor() {
        let mut n = serde_json::json!({"company": {}, "contact": {"email": "A@B.COM"}});
        let errs = validate_row(&mut n, crate::email_address::LocalPartPolicy::Lowercase);
        assert!(errs.is_empty());
        assert_eq!(
            n["contact"]["email_normalized"].as_str().unwrap(),
            "a@b.com"
        );

        let mut bad = serde_json::json!({"company": {}, "contact": {"email": "nope"}});
        let errs = validate_row(&mut bad, crate::email_address::LocalPartPolicy::Lowercase);
        assert!(errs.iter().any(|e| e.code == "invalid_email"));
    }
}
