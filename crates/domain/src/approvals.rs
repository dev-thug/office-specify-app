//! approvals module domain types (spec §6, source §5.5, A.6.3, B.3.9).
//! String values equal the DDL CHECK values (§12.4).

/// `approval_requests.action_type` CHECK values — one concrete FK each.
pub mod action_type {
    pub const CAMPAIGN_LAUNCH: &str = "campaign_launch";
    pub const MESSAGE_SUBMIT: &str = "message_submit";
    pub const BOOKING_CHANGE: &str = "booking_change";
    pub const SUPPRESSION_RELEASE: &str = "suppression_release";
}

/// `approval_requests.status` CHECK values.
pub mod request_status {
    pub const PENDING: &str = "pending";
    pub const APPROVED: &str = "approved";
    pub const REJECTED: &str = "rejected";
    pub const EXPIRED: &str = "expired";
    pub const REVOKED: &str = "revoked";
}

/// `approval_decisions.decision` CHECK values.
pub mod decision {
    pub const APPROVED: &str = "approved";
    pub const REJECTED: &str = "rejected";

    pub fn parse(s: &str) -> Option<&'static str> {
        match s {
            APPROVED => Some(APPROVED),
            REJECTED => Some(REJECTED),
            _ => None,
        }
    }
}
