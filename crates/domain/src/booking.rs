//! Booking domain primitives: status values and DST-safe local time mapping.

use chrono::{DateTime, NaiveDateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookingStatus {
    Held,
    Confirming,
    Confirmed,
    Canceling,
    Canceled,
    Failed,
    Unknown,
    Expired,
}

impl BookingStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Confirming => "confirming",
            Self::Confirmed => "confirmed",
            Self::Canceling => "canceling",
            Self::Canceled => "canceled",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookingChangeKind {
    Create,
    Reschedule,
    Cancel,
}

impl BookingChangeKind {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "create" => Self::Create,
            "reschedule" => Self::Reschedule,
            "cancel" => Self::Cancel,
            _ => return None,
        })
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Reschedule => "reschedule",
            Self::Cancel => "cancel",
        }
    }
}

/// Resolve a local wall-clock time to every valid UTC instant.
///
/// A fall-back transition yields two candidates, both ordered by UTC. A
/// spring-forward gap yields none, so callers cannot accidentally book a
/// local time that never occurred.
pub fn local_instants(local: NaiveDateTime, timezone: Tz) -> Vec<DateTime<Utc>> {
    use chrono::TimeZone;
    match timezone.from_local_datetime(&local) {
        chrono::LocalResult::Single(value) => vec![value.with_timezone(&Utc)],
        chrono::LocalResult::Ambiguous(first, second) => {
            let mut values = [first.with_timezone(&Utc), second.with_timezone(&Utc)];
            values.sort();
            values.into()
        }
        chrono::LocalResult::None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    #[test]
    fn spring_gap_has_no_local_instant_and_fall_back_has_two() {
        let new_york: Tz = "America/New_York".parse().unwrap();
        let nonexistent = NaiveDateTime::new(
            NaiveDate::from_ymd_opt(2026, 3, 8).unwrap(),
            NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
        );
        assert!(local_instants(nonexistent, new_york).is_empty());

        let duplicated = NaiveDateTime::new(
            NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(),
            NaiveTime::from_hms_opt(1, 30, 0).unwrap(),
        );
        let options = local_instants(duplicated, new_york);
        assert_eq!(options.len(), 2);
        assert_eq!(options[1] - options[0], chrono::Duration::hours(1));
    }
}
