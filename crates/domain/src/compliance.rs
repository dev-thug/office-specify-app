//! compliance module domain types (spec §6 — owned by the module worker).
//!
//! String-valued enums mirror the DDL CHECK constraints verbatim
//! (§12.4: API enums must equal DDL values).

use serde::{Deserialize, Serialize};

/// `communication_bases.purpose` / outbound `purpose` (B.1.1 `Purpose`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Purpose {
    #[serde(rename = "marketing")]
    Marketing,
    #[serde(rename = "correspondence")]
    Correspondence,
    #[serde(rename = "transactional")]
    Transactional,
}

impl Purpose {
    pub const VALUES: &'static [&'static str] = &["marketing", "correspondence", "transactional"];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Marketing => "marketing",
            Self::Correspondence => "correspondence",
            Self::Transactional => "transactional",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "marketing" => Self::Marketing,
            "correspondence" => Self::Correspondence,
            "transactional" => Self::Transactional,
            _ => return None,
        })
    }
}

/// `suppressions.purpose_scope` — the suppression scope (§11.1: marketing
/// is the default scope; `all` blocks every purpose).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PurposeScope {
    #[serde(rename = "marketing")]
    Marketing,
    #[serde(rename = "all")]
    All,
}

impl PurposeScope {
    pub const VALUES: &'static [&'static str] = &["marketing", "all"];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Marketing => "marketing",
            Self::All => "all",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "marketing" => Self::Marketing,
            "all" => Self::All,
            _ => return None,
        })
    }

    /// §11.1: `all` blocks every purpose; `marketing` blocks marketing only.
    pub fn blocks_purpose(&self, purpose: &str) -> bool {
        matches!(self, Self::All) || purpose == self.as_str()
    }
}

/// `suppressions.reason` (DDL CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SuppressionReason {
    #[serde(rename = "unsubscribe")]
    Unsubscribe,
    #[serde(rename = "complaint")]
    Complaint,
    #[serde(rename = "hard_bounce")]
    HardBounce,
    #[serde(rename = "manual")]
    Manual,
}

impl SuppressionReason {
    pub const VALUES: &'static [&'static str] =
        &["unsubscribe", "complaint", "hard_bounce", "manual"];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsubscribe => "unsubscribe",
            Self::Complaint => "complaint",
            Self::HardBounce => "hard_bounce",
            Self::Manual => "manual",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unsubscribe" => Self::Unsubscribe,
            "complaint" => Self::Complaint,
            "hard_bounce" => Self::HardBounce,
            "manual" => Self::Manual,
            _ => return None,
        })
    }
}

/// `communication_bases.basis_type` (DDL CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisType {
    #[serde(rename = "explicit_consent")]
    ExplicitConsent,
    #[serde(rename = "requested_reply")]
    RequestedReply,
    #[serde(rename = "requested_transaction")]
    RequestedTransaction,
    #[serde(rename = "reviewed_exception")]
    ReviewedException,
}

impl BasisType {
    pub const VALUES: &'static [&'static str] = &[
        "explicit_consent",
        "requested_reply",
        "requested_transaction",
        "reviewed_exception",
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitConsent => "explicit_consent",
            Self::RequestedReply => "requested_reply",
            Self::RequestedTransaction => "requested_transaction",
            Self::ReviewedException => "reviewed_exception",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "explicit_consent" => Self::ExplicitConsent,
            "requested_reply" => Self::RequestedReply,
            "requested_transaction" => Self::RequestedTransaction,
            "reviewed_exception" => Self::ReviewedException,
            _ => return None,
        })
    }
}

/// `communication_bases.status` (DDL CHECK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasisStatus {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "valid")]
    Valid,
    #[serde(rename = "rejected")]
    Rejected,
    #[serde(rename = "revoked")]
    Revoked,
    #[serde(rename = "expired")]
    Expired,
}

impl BasisStatus {
    pub const VALUES: &'static [&'static str] =
        &["pending", "valid", "rejected", "revoked", "expired"];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Valid => "valid",
            Self::Rejected => "rejected",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "valid" => Self::Valid,
            "rejected" => Self::Rejected,
            "revoked" => Self::Revoked,
            "expired" => Self::Expired,
            _ => return None,
        })
    }
}

/// Review decisions accepted by `POST /communication-bases/{id}/reviews`
/// (B.2: "valid/rejected 판단"). Expiry is time-derived, never a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewDecision {
    #[serde(rename = "valid")]
    Valid,
    #[serde(rename = "rejected")]
    Rejected,
}

impl ReviewDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Rejected => "rejected",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "valid" => Self::Valid,
            "rejected" => Self::Rejected,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_blocks_only_matching_purpose() {
        assert!(PurposeScope::All.blocks_purpose("correspondence"));
        assert!(PurposeScope::All.blocks_purpose("marketing"));
        assert!(PurposeScope::Marketing.blocks_purpose("marketing"));
        assert!(!PurposeScope::Marketing.blocks_purpose("correspondence"));
        assert!(!PurposeScope::Marketing.blocks_purpose("transactional"));
    }

    #[test]
    fn parses_only_ddl_values() {
        for s in SuppressionReason::VALUES {
            assert!(SuppressionReason::parse(s).is_some());
        }
        for s in BasisStatus::VALUES {
            assert!(BasisStatus::parse(s).is_some());
        }
        for s in BasisType::VALUES {
            assert!(BasisType::parse(s).is_some());
        }
        for s in Purpose::VALUES {
            assert!(Purpose::parse(s).is_some());
        }
        assert!(Purpose::parse("promo").is_none());
        assert!(BasisStatus::parse("active").is_none());
    }
}
