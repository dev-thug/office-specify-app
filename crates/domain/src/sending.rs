//! sending module domain types (spec §6 — owned by the module worker).
//!
//! §8.3 single-message send state machine and the pure parts of the §8.4
//! execution-time policy chain. All predicates that depend on the database
//! live in `infrastructure::repo::sending`; this file holds the status enum,
//! the canonical payload-hash preimage, and the schedule/window math used
//! for quiet-hours checks.

use chrono::{DateTime, Datelike, Duration, NaiveTime, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// outbound_message status values (DDL CHECK constraint).
pub const OUTBOUND_STATUSES: &[&str] = &[
    "draft",
    "queued",
    "claimed",
    "submitting",
    "accepted",
    "failed",
    "unknown",
    "canceled",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutboundStatus {
    Draft,
    Queued,
    Claimed,
    Submitting,
    Accepted,
    Failed,
    Unknown,
    Canceled,
}

impl OutboundStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Submitting => "submitting",
            Self::Accepted => "accepted",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => Self::Draft,
            "queued" => Self::Queued,
            "claimed" => Self::Claimed,
            "submitting" => Self::Submitting,
            "accepted" => Self::Accepted,
            "failed" => Self::Failed,
            "unknown" => Self::Unknown,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }

    /// Statuses from which dispatch may still run.
    pub fn dispatchable(self) -> bool {
        matches!(self, Self::Queued | Self::Claimed)
    }
}

/// send_attempt status values (DDL CHECK constraint).
pub const ATTEMPT_STATUSES: &[&str] = &["started", "accepted", "definitive_failed", "unknown"];

/// reply_claim state values (DDL CHECK constraint).
pub const CLAIM_STATES: &[&str] = &["reserved", "submitted", "accepted", "released"];

/// delivery_events.event_type values (DDL CHECK constraint — provider-
/// confirmed events only; "deferred"/"policy_violation" are NOT storable).
pub const DELIVERY_EVENT_TYPES: &[&str] = &["accepted", "delivered", "bounced", "complained"];

/// Delivery status as reported to the UI (B.3.11): provider evidence only.
/// `accepted` means the provider confirmed acceptance — NOT delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryStatus {
    Unknown,
    Accepted,
    Delivered,
    Bounced,
    Complained,
    Deferred,
}

impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Accepted => "accepted",
            Self::Delivered => "delivered",
            Self::Bounced => "bounced",
            Self::Complained => "complained",
            Self::Deferred => "deferred",
        }
    }

    /// Fold delivery_event types into the UI status. Terminal evidence wins
    /// over softer states; an `accepted` event alone never yields
    /// "delivered" (T34).
    pub fn fold(events: impl IntoIterator<Item = String>) -> Self {
        let mut status = Self::Unknown;
        for e in events {
            status = match e.as_str() {
                "delivered" => Self::Delivered,
                "bounced" if status != Self::Delivered => Self::Bounced,
                "complained" if status != Self::Delivered => Self::Complained,
                "accepted" if status == Self::Unknown || status == Self::Deferred => Self::Accepted,
                "deferred" if status == Self::Unknown => Self::Deferred,
                _ => status,
            };
        }
        status
    }
}

/// Canonical payload-hash preimage (§8.3 immutable snapshot). The caller
/// hashes this with SHA-256. Order is fixed and fields are 0x1f-separated.
#[allow(clippy::too_many_arguments)]
pub fn payload_preimage(
    purpose: &str,
    mail_account_id: &str,
    delivery_channel_id: &str,
    sender_identity_id: &str,
    from_address: &str,
    to_email_address_id: &str,
    to_address: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
    thread_id: Option<&str>,
    reply_to_message_id: Option<&str>,
    communication_basis_id: Option<&str>,
    template_version_id: Option<&str>,
) -> Vec<u8> {
    [
        purpose,
        mail_account_id,
        delivery_channel_id,
        sender_identity_id,
        from_address,
        to_email_address_id,
        to_address,
        subject,
        body_text,
        body_html.unwrap_or(""),
        thread_id.unwrap_or(""),
        reply_to_message_id.unwrap_or(""),
        communication_basis_id.unwrap_or(""),
        template_version_id.unwrap_or(""),
    ]
    .join("\u{1f}")
    .into_bytes()
}

/// Add the required visible marketing unsubscribe footer to each rendered
/// body that does not already contain this message's generated link. Checking
/// each alternative independently keeps plain-text and HTML clients covered
/// when a template includes the system variable in only one representation.
///
/// The caller supplies a server-generated, approved-origin URL; it is escaped
/// for the HTML attribute before insertion. A template that renders the URL
/// itself is left intact and does not receive a duplicate footer.
pub fn append_unsubscribe_footer(
    body_text: &str,
    body_html: Option<&str>,
    url: &str,
) -> (String, Option<String>) {
    let body_text = if body_text.contains(url) {
        body_text.to_string()
    } else {
        append_text_footer(body_text, url)
    };
    let body_html = body_html.map(|body| {
        if body.contains(url) {
            body.to_string()
        } else {
            let escaped_url = escape_html_attribute(url);
            let prefix = body.trim_end();
            if prefix.is_empty() {
                format!("<p><a href=\"{escaped_url}\">Unsubscribe</a></p>")
            } else {
                format!("{prefix}\n<p><a href=\"{escaped_url}\">Unsubscribe</a></p>")
            }
        }
    });
    (body_text, body_html)
}

/// Recover opaque unsubscribe token candidates from current confirmation-page
/// links and legacy frozen API links. No URL or host is returned or trusted;
/// callers must verify each token's hash and outbound/recipient binding
/// before using it.
pub fn unsubscribe_token_candidates(body_text: &str, body_html: Option<&str>) -> Vec<String> {
    const MARKERS: [&str; 2] = ["/public/unsubscribe/", "/public/v1/unsubscribe/"];
    const TOKEN_LEN: usize = 43; // 32 random bytes, base64url without padding.
    let mut tokens = Vec::new();
    for body in [body_text, body_html.unwrap_or("")] {
        for marker in MARKERS {
            let mut offset = 0;
            while let Some(relative) = body[offset..].find(marker) {
                let token_start = offset + relative + marker.len();
                let token: String = body[token_start..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                    .collect();
                if token.len() == TOKEN_LEN && !tokens.contains(&token) {
                    tokens.push(token.clone());
                    if tokens.len() >= 32 {
                        return tokens;
                    }
                }
                offset = token_start.saturating_add(token.len().max(1));
                if offset >= body.len() {
                    break;
                }
            }
        }
    }
    tokens
}

fn append_text_footer(body: &str, url: &str) -> String {
    let prefix = body.trim_end();
    if prefix.is_empty() {
        format!("Unsubscribe: {url}")
    } else {
        format!("{prefix}\n\nUnsubscribe: {url}")
    }
}

fn escape_html_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// §8.4 schedule rules carried by campaign_revisions.schedule_rules:
/// `{timezone, weekdays:[1=Mon..7], window_start:"HH:MM", window_end:"HH:MM",
/// minimum_gap_seconds}`. All fields optional; missing fields are the
/// permissive default (any day, 00:00–24:00, no gap).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendWindow {
    pub timezone: Option<Tz>,
    /// ISO weekday numbers 1=Mon..7=Sun; empty = every day.
    pub weekdays: Vec<u8>,
    pub window_start: Option<NaiveTime>,
    pub window_end: Option<NaiveTime>,
    pub minimum_gap_seconds: i64,
}

impl SendWindow {
    pub fn parse(rules: &serde_json::Value) -> Result<Self, String> {
        let obj = rules
            .as_object()
            .ok_or_else(|| "schedule_rules must be an object".to_string())?;
        let timezone =
            match obj.get("timezone") {
                None | Some(serde_json::Value::Null) => None,
                Some(v) => {
                    let s = v
                        .as_str()
                        .ok_or_else(|| "schedule_rules.timezone must be a string".to_string())?;
                    Some(s.parse::<Tz>().map_err(|_| {
                        format!("schedule_rules.timezone '{s}' is not an IANA zone")
                    })?)
                }
            };
        let weekdays = match obj.get("weekdays") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(v) => {
                let arr = v
                    .as_array()
                    .ok_or_else(|| "schedule_rules.weekdays must be an array".to_string())?;
                let mut out = Vec::new();
                for d in arr {
                    let n = d.as_u64().ok_or_else(|| {
                        "schedule_rules.weekdays entries must be 1..7".to_string()
                    })?;
                    if !(1..=7).contains(&n) {
                        return Err("schedule_rules.weekdays entries must be 1..7".to_string());
                    }
                    out.push(n as u8);
                }
                out
            }
        };
        let parse_hm = |key: &str| -> Result<Option<NaiveTime>, String> {
            match obj.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                Some(v) => {
                    let s = v
                        .as_str()
                        .ok_or_else(|| format!("schedule_rules.{key} must be \"HH:MM\""))?;
                    NaiveTime::parse_from_str(s, "%H:%M")
                        .map(Some)
                        .map_err(|_| format!("schedule_rules.{key} must be \"HH:MM\""))
                }
            }
        };
        let minimum_gap_seconds = match obj.get("minimum_gap_seconds") {
            None | Some(serde_json::Value::Null) => 0,
            Some(v) => v
                .as_i64()
                .filter(|n| *n >= 0)
                .ok_or_else(|| "schedule_rules.minimum_gap_seconds must be >= 0".to_string())?,
        };
        Ok(Self {
            timezone,
            weekdays,
            window_start: parse_hm("window_start")?,
            window_end: parse_hm("window_end")?,
            minimum_gap_seconds,
        })
    }

    /// Is `at` inside the allowed window (timezone/schedule predicate)?
    pub fn within(&self, at: DateTime<Utc>) -> bool {
        let local = match self.timezone {
            Some(tz) => at.with_timezone(&tz).naive_local(),
            None => at.naive_utc(),
        };
        // chrono weekday(): Mon=1 .. Sun=7 via num_days_from_monday()+1
        let weekday = local.weekday().num_days_from_monday() as u8 + 1;
        if !self.weekdays.is_empty() && !self.weekdays.contains(&weekday) {
            return false;
        }
        let start = self
            .window_start
            .unwrap_or_else(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap());
        let end = self
            .window_end
            .unwrap_or_else(|| NaiveTime::from_hms_opt(23, 59, 59).unwrap());
        let t = local.time();
        t >= start && t <= end
    }

    /// Next instant at which sending is allowed given `last_send` (for the
    /// minimum-gap rule). Returns `None` when the rules are unbounded in a
    /// way that cannot produce a sane instant (fail-closed callers treat
    /// that as "cannot compute → block").
    pub fn next_allowed(
        &self,
        now: DateTime<Utc>,
        last_send: Option<DateTime<Utc>>,
    ) -> DateTime<Utc> {
        let mut candidate = now;
        if let (Some(last), gap) = (last_send, self.minimum_gap_seconds)
            && gap > 0
        {
            candidate = candidate.max(last + Duration::seconds(gap));
        }
        // Walk forward day-by-day until inside the window (bounded 14 days).
        for _ in 0..(14 * 24 * 4) {
            if self.within(candidate) {
                return candidate;
            }
            // Advance 15 minutes — simple and sufficient for our use.
            candidate += Duration::minutes(15);
        }
        candidate
    }
}

/// Local-time helper kept for tests.
#[allow(dead_code)]
pub fn local_hour(at: DateTime<Utc>, tz: Tz) -> u32 {
    at.with_timezone(&tz).hour()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn delivery_status_never_claims_delivery_without_evidence() {
        assert_eq!(DeliveryStatus::fold(vec![]), DeliveryStatus::Unknown);
        assert_eq!(
            DeliveryStatus::fold(vec!["accepted".to_string()]),
            DeliveryStatus::Accepted
        );
        assert_eq!(
            DeliveryStatus::fold(vec!["accepted".to_string(), "delivered".to_string()]),
            DeliveryStatus::Delivered
        );
        assert_eq!(
            DeliveryStatus::fold(vec!["delivered".to_string(), "bounced".to_string()]),
            DeliveryStatus::Delivered
        );
    }

    #[test]
    fn window_math() {
        // Asia/Seoul, weekday window 09:00–18:00 Mon–Fri.
        let rules = json!({
            "timezone": "Asia/Seoul",
            "weekdays": [1, 2, 3, 4, 5],
            "window_start": "09:00",
            "window_end": "18:00",
            "minimum_gap_seconds": 60
        });
        let w = SendWindow::parse(&rules).unwrap();
        // 2026-10-09 is a Friday. 10:00 KST = 01:00 UTC.
        let t = Utc.with_ymd_and_hms(2026, 10, 9, 1, 0, 0).unwrap();
        assert!(w.within(t));
        // 20:00 KST = 11:00 UTC — outside window.
        let late = Utc.with_ymd_and_hms(2026, 10, 9, 11, 0, 0).unwrap();
        assert!(!w.within(late));
        // Saturday 10:00 KST (Sat 2026-10-10 01:00 UTC) — wrong weekday.
        let sat = Utc.with_ymd_and_hms(2026, 10, 10, 1, 0, 0).unwrap();
        assert!(!w.within(sat));
        let next = w.next_allowed(late, None);
        assert!(w.within(next));
        // Gap rule pushes the candidate forward.
        let last = t;
        let n2 = w.next_allowed(t, Some(last));
        assert!(n2 >= last + Duration::seconds(60));
    }

    #[test]
    fn window_parse_errors() {
        assert!(SendWindow::parse(&json!({"timezone":"Mars/Olympus"})).is_err());
        assert!(SendWindow::parse(&json!({"weekdays":[0]})).is_err());
        assert!(SendWindow::parse(&json!({"window_start":"9am"})).is_err());
    }

    #[test]
    fn unsubscribe_footer_is_added_per_rendered_body_without_duplicates() {
        let url = "https://mail.example/public/unsubscribe/token_123";
        let text = "Hello";
        let html = Some("<p>Hello</p>");
        let (text, html) = append_unsubscribe_footer(text, html, url);
        assert!(text.contains(&format!("Unsubscribe: {url}")));
        assert!(
            html.as_deref()
                .unwrap()
                .contains(&format!("href=\"{url}\""))
        );

        let (text, html) = append_unsubscribe_footer(&text, html.as_deref(), url);
        assert_eq!(text.matches(url).count(), 1);
        assert_eq!(html.as_deref().unwrap().matches(url).count(), 1);
    }

    #[test]
    fn unsubscribe_token_reader_accepts_current_and_legacy_paths() {
        let current = "A".repeat(43);
        let legacy = "B".repeat(43);
        assert_eq!(
            unsubscribe_token_candidates(
                &format!("https://host.example/public/unsubscribe/{current}"),
                Some(&format!(
                    "<a href=\"https://old.example/public/v1/unsubscribe/{legacy}\">"
                )),
            ),
            vec![current, legacy]
        );
    }

    #[test]
    fn unsubscribe_token_reader_skips_non_token_segments_and_urls_are_not_returned() {
        let valid = "a_b-9".repeat(8) + "abc";
        assert_eq!(valid.len(), 43);
        assert_eq!(
            unsubscribe_token_candidates(
                &format!(
                    "https://attacker.example/public/unsubscribe/short, https://attacker.example/public/unsubscribe/{valid}?next=/bad"
                ),
                None,
            ),
            vec![valid]
        );
    }
}
