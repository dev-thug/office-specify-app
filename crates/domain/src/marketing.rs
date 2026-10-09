use serde::{Deserialize, Serialize};

/// Durable local preparation/submission states for a single Campaigns export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketingExportState {
    Preparing,
    Syncing,
    NeedsReview,
    Ready,
    Submitting,
    InProgress,
    Completed,
    Rejected,
    Unknown,
    Canceled,
}

impl MarketingExportState {
    pub const ALL: [Self; 10] = [
        Self::Preparing,
        Self::Syncing,
        Self::NeedsReview,
        Self::Ready,
        Self::Submitting,
        Self::InProgress,
        Self::Completed,
        Self::Rejected,
        Self::Unknown,
        Self::Canceled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Syncing => "syncing",
            Self::NeedsReview => "needs_review",
            Self::Ready => "ready",
            Self::Submitting => "submitting",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Rejected => "rejected",
            Self::Unknown => "unknown",
            Self::Canceled => "canceled",
        }
    }

    #[allow(clippy::should_implement_trait)] // Optional state parser, rather than a fallible conversion.
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "preparing" => Some(Self::Preparing),
            "syncing" => Some(Self::Syncing),
            "needs_review" => Some(Self::NeedsReview),
            "ready" => Some(Self::Ready),
            "submitting" => Some(Self::Submitting),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "rejected" => Some(Self::Rejected),
            "unknown" => Some(Self::Unknown),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }

    /// A safe status can move forward, pause for review, or terminate. An
    /// unknown provider result may only advance after a read-only lookup.
    pub const fn can_transition(self, to: Self) -> bool {
        if self as u8 == to as u8 {
            return true;
        }
        match self {
            Self::Preparing => matches!(
                to,
                Self::Syncing | Self::NeedsReview | Self::Rejected | Self::Canceled
            ),
            Self::Syncing => matches!(
                to,
                Self::Ready | Self::NeedsReview | Self::Unknown | Self::Rejected | Self::Canceled
            ),
            // Readback can resolve mutable provider state (for example,
            // double opt-in) while the immutable export hash remains pinned.
            Self::NeedsReview => matches!(to, Self::Syncing | Self::Canceled),
            Self::Ready => matches!(to, Self::Submitting | Self::NeedsReview | Self::Canceled),
            Self::Submitting => matches!(
                to,
                Self::InProgress
                    | Self::Completed
                    | Self::Unknown
                    | Self::Rejected
                    | Self::NeedsReview
            ),
            Self::InProgress => matches!(to, Self::Completed | Self::NeedsReview),
            Self::Unknown => matches!(
                to,
                Self::InProgress | Self::Completed | Self::Rejected | Self::NeedsReview
            ),
            Self::Completed | Self::Rejected | Self::Canceled => false,
        }
    }
}

/// Result of an idempotent journal claim. Unknown and rejected claims are
/// deliberately terminal; callers must never issue another provider write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationClaim {
    New,
    Completed { provider_id: Option<String> },
    Unknown,
    InProgress,
    Rejected { error_code: Option<String> },
}

/// Read-only provider evidence can reconcile an unknown mutation to a
/// terminal result. It never represents permission to issue another write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationResolution {
    Completed,
    Rejected,
}

#[cfg(test)]
mod tests {
    use super::MarketingExportState as State;

    #[test]
    fn export_states_parse_and_serialize_as_the_database_values() {
        for (state, text) in [
            (State::Preparing, "preparing"),
            (State::Syncing, "syncing"),
            (State::NeedsReview, "needs_review"),
            (State::Ready, "ready"),
            (State::Submitting, "submitting"),
            (State::InProgress, "in_progress"),
            (State::Completed, "completed"),
            (State::Rejected, "rejected"),
            (State::Unknown, "unknown"),
            (State::Canceled, "canceled"),
        ] {
            assert_eq!(state.as_str(), text);
            assert_eq!(State::from_str(text), Some(state));
        }
        assert_eq!(State::from_str("sent"), None);
    }

    #[test]
    fn export_state_transitions_allow_progress_and_lookup_reconciliation() {
        let forward = [
            (State::Preparing, State::Syncing),
            (State::Syncing, State::Ready),
            (State::Ready, State::Submitting),
            (State::Submitting, State::InProgress),
            (State::Submitting, State::Unknown),
            (State::Unknown, State::InProgress),
            (State::Unknown, State::Completed),
            (State::NeedsReview, State::Syncing),
            (State::InProgress, State::Completed),
        ];
        assert!(
            forward
                .into_iter()
                .all(|(from, to)| from.can_transition(to))
        );
    }

    #[test]
    fn export_states_cannot_skip_approval_or_reopen_terminal_results() {
        let blocked = [
            (State::Preparing, State::Completed),
            (State::Syncing, State::Submitting),
            (State::Ready, State::Completed),
            (State::Unknown, State::Submitting),
            (State::NeedsReview, State::Submitting),
            (State::Completed, State::Submitting),
            (State::Rejected, State::Ready),
            (State::Canceled, State::Syncing),
        ];
        assert!(
            blocked
                .into_iter()
                .all(|(from, to)| !from.can_transition(to))
        );
    }
}
