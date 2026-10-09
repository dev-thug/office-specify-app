//! Email address normalization (source §6.3). The normalized key is the
//! dedupe identity inside a workspace: trim + domain lowercase + IDNA, plus a
//! workspace-fixed local-part case policy. No `+tag` or dot stripping.
//! `normalization_version` records the rule set that produced the key.

use serde::{Deserialize, Serialize};

/// Current normalization rule version. Bump when rules change; address
/// updates are handled by linking a new `email_addresses` row, not mutation.
pub const NORMALIZATION_VERSION: i16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedEmail {
    pub original: String,
    pub normalized_key: String,
    pub normalization_version: i16,
}

/// Workspace-fixed local-part case policy (§6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalPartPolicy {
    /// Conservative default: compare case-insensitively.
    #[default]
    Lowercase,
    Preserve,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizeError {
    Empty,
    MissingAt,
    EmptyLocal,
    EmptyDomain,
    DomainInvalid,
    TooLong,
}

impl std::fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Empty => "email address is empty",
            Self::MissingAt => "email address is missing '@'",
            Self::EmptyLocal => "email local part is empty",
            Self::EmptyDomain => "email domain is empty",
            Self::DomainInvalid => "email domain is invalid",
            Self::TooLong => "email address exceeds 320 characters",
        };
        f.write_str(s)
    }
}

impl std::error::Error for NormalizeError {}

/// Normalize `raw` under the workspace's `policy`. Returns the original
/// (trimmed) address and the dedupe key.
pub fn normalize(raw: &str, policy: LocalPartPolicy) -> Result<NormalizedEmail, NormalizeError> {
    let original = raw.trim().to_string();
    if original.is_empty() {
        return Err(NormalizeError::Empty);
    }
    if original.len() > 320 {
        return Err(NormalizeError::TooLong);
    }
    let (local, domain) = original.rsplit_once('@').ok_or(NormalizeError::MissingAt)?;
    if local.is_empty() {
        return Err(NormalizeError::EmptyLocal);
    }
    let domain = domain.trim_end_matches('.');
    if domain.is_empty() {
        return Err(NormalizeError::EmptyDomain);
    }
    let domain_ascii = idna::domain_to_ascii(&domain.to_lowercase())
        .map_err(|_| NormalizeError::DomainInvalid)?;
    if domain_ascii.is_empty() {
        return Err(NormalizeError::DomainInvalid);
    }
    let local_key = match policy {
        LocalPartPolicy::Lowercase => local.to_lowercase(),
        LocalPartPolicy::Preserve => local.to_string(),
    };
    Ok(NormalizedEmail {
        original,
        normalized_key: format!("{local_key}@{domain_ascii}"),
        normalization_version: NORMALIZATION_VERSION,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_domain_case_and_idna() {
        let n = normalize("  User@Bücher.example ", LocalPartPolicy::Lowercase).unwrap();
        assert_eq!(n.original, "User@Bücher.example");
        assert_eq!(n.normalized_key, "user@xn--bcher-kva.example");
        assert_eq!(n.normalization_version, NORMALIZATION_VERSION);
    }

    #[test]
    fn preserves_local_case_when_policy_says_so() {
        let n = normalize("User@EXAMPLE.com", LocalPartPolicy::Preserve).unwrap();
        assert_eq!(n.normalized_key, "User@example.com");
    }

    #[test]
    fn keeps_plus_tags_and_dots() {
        let n = normalize("f.oo+tag@gmail.com", LocalPartPolicy::Lowercase).unwrap();
        assert_eq!(n.normalized_key, "f.oo+tag@gmail.com");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(normalize("", LocalPartPolicy::Lowercase), Err(NormalizeError::Empty));
        assert_eq!(normalize("noatsign", LocalPartPolicy::Lowercase), Err(NormalizeError::MissingAt));
        assert_eq!(normalize("@x.com", LocalPartPolicy::Lowercase), Err(NormalizeError::EmptyLocal));
        assert_eq!(normalize("a@", LocalPartPolicy::Lowercase), Err(NormalizeError::EmptyDomain));
    }
}
