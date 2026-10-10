//! campaigns module domain types (spec §6): status enums equal to DDL CHECK
//! values (§12.4), `schedule_rules` shape + IANA-timezone send-time math,
//! the freeze manifest canonical form, and the §8.1 allowlist renderer used
//! when freezing per-step outbound snapshots.
//!
//! NOTE(templates-merge): §8.1's canonical renderer lives in the templates
//! module (Task 6). Until merge, this module ships the same contract — only
//! declared variables of the form `{{name}}` are substituted, every
//! `{{…}}` that is not a declared variable rejects, and missing required
//! variables reject. Swap the call site when `domain::templates` lands.

use chrono::{DateTime, Datelike, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use uuid::Uuid;

// ----------------------------------------------------------- status enums

/// `campaigns.status` CHECK values.
pub mod campaign_status {
    pub const DRAFT: &str = "draft";
    pub const AWAITING_APPROVAL: &str = "awaiting_approval";
    pub const ACTIVE: &str = "active";
    pub const PAUSED: &str = "paused";
    pub const COMPLETED: &str = "completed";
    pub const CANCELED: &str = "canceled";
}

/// `campaign_revisions.status` CHECK values.
pub mod revision_status {
    pub const DRAFT: &str = "draft";
    pub const FROZEN: &str = "frozen";
}

/// `campaign_launches.status` CHECK values.
pub mod launch_status {
    pub const BUILDING: &str = "building";
    pub const FROZEN: &str = "frozen";
    pub const AWAITING_APPROVAL: &str = "awaiting_approval";
    pub const APPROVED: &str = "approved";
    pub const RUNNING: &str = "running";
    pub const PAUSED: &str = "paused";
    pub const COMPLETED: &str = "completed";
    pub const CANCELED: &str = "canceled";

    /// Status may only move forward once the launch has frozen — the A.6.2
    /// DB freeze guards reject structural edits, and this order is the
    /// application-level transition legality (risk-reduction: pause/cancel
    /// is always legal from a non-terminal state).
    pub fn is_terminal(s: &str) -> bool {
        matches!(s, COMPLETED | CANCELED)
    }

    /// Legal forward transitions. `paused` → `running` exists as a reserved
    /// resume path; `paused`/`canceled` are reachable from any non-terminal
    /// state (risk reduction, A.6.2/§8.2).
    pub fn can_transition(from: &str, to: &str) -> bool {
        if from == to {
            return true;
        }
        if is_terminal(from) {
            return false;
        }
        match to {
            CANCELED | PAUSED => true,
            _ => {
                let order = |s: &str| match s {
                    BUILDING => 0,
                    FROZEN | AWAITING_APPROVAL => 1,
                    APPROVED => 2,
                    RUNNING | PAUSED => 3,
                    COMPLETED => 4,
                    _ => -1,
                };
                order(to) > order(from)
            }
        }
    }
}

/// `campaign_enrollments.status` CHECK values.
pub mod enrollment_status {
    pub const PENDING: &str = "pending";
    pub const ACTIVE: &str = "active";
    pub const COMPLETED: &str = "completed";
    pub const STOPPED: &str = "stopped";
    pub const SUPPRESSED: &str = "suppressed";
}

/// `campaigns.pause_address` / `compliance.pause_enrollments` stop reasons.
/// `pause_address` payload reasons per integration-contracts.
pub mod pause_reason {
    pub const HUMAN_REPLY: &str = "human_reply";
    pub const AUTO_REPLY_REVIEW: &str = "auto_reply_review";
    pub const MEETING_REQUESTED: &str = "meeting_requested";
    pub const UNSUBSCRIBE_REQUEST: &str = "unsubscribe_request";

    /// Whether the reason maps to the `stop_on_reply` / `stop_on_booking`
    /// revision flags (`None` = unconditional stop per §8.4 defaults).
    pub fn governed_flag(reason: &str) -> Option<&'static str> {
        match reason {
            HUMAN_REPLY => Some("stop_on_reply"),
            MEETING_REQUESTED => Some("stop_on_booking"),
            // auto_reply is always a review-required stop (§8.4); an
            // unsubscribe must stop regardless of campaign flags.
            _ => None,
        }
    }
}

// ---------------------------------------------------------- schedule rules

/// Validated `campaign_revisions.schedule_rules` (B.3.6 shape).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRules {
    /// ISO weekdays (1=Mon…7=Sun). Empty = every day.
    #[serde(default)]
    pub weekdays: Vec<u8>,
    pub window_start: Option<NaiveTime>,
    pub window_end: Option<NaiveTime>,
    pub internal_daily_cap: Option<u32>,
    pub minimum_gap_seconds: Option<u32>,
    pub launch_not_before: Option<DateTime<Utc>>,
    pub launch_expires_at: Option<DateTime<Utc>>,
    /// Optional system binding used to generate `meeting.booking_url` during
    /// a campaign launch freeze. Included in the manifest through JSONB.
    pub meeting_type_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RulesError {
    NotAnObject,
    UnknownField(String),
    Weekday(u8),
    Clock(String),
    WindowOrder,
    CapZero,
    MeetingTypeId(String),
}

impl std::fmt::Display for RulesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "schedule_rules must be an object"),
            Self::UnknownField(k) => write!(f, "schedule_rules.{k} is not a known field"),
            Self::Weekday(d) => write!(f, "weekday {d} is out of range (1=Mon … 7=Sun)"),
            Self::Clock(v) => write!(f, "invalid HH:MM value {v:?}"),
            Self::WindowOrder => write!(f, "window_start must be before window_end"),
            Self::CapZero => write!(f, "internal_daily_cap must be > 0"),
            Self::MeetingTypeId(id) => {
                write!(f, "meeting_type_id must be a UUID, got {id:?}")
            }
        }
    }
}

fn parse_clock(v: &serde_json::Value) -> Result<NaiveTime, RulesError> {
    let s = v.as_str().unwrap_or_default();
    for fmt in ["%H:%M", "%H:%M:%S", "%H:%M:%S%.f"] {
        if let Ok(t) = NaiveTime::parse_from_str(s, fmt) {
            return Ok(t);
        }
    }
    Err(RulesError::Clock(s.to_string()))
}

impl ScheduleRules {
    /// Parse the JSONB `schedule_rules` column into the validated shape.
    /// Unknown keys reject — the rules are part of the approved manifest.
    pub fn from_json(v: &serde_json::Value) -> Result<Self, RulesError> {
        let obj = v.as_object().ok_or(RulesError::NotAnObject)?;
        let mut r = ScheduleRules::default();
        for (k, val) in obj {
            if val.is_null() {
                continue;
            }
            match k.as_str() {
                "weekdays" => {
                    for d in val.as_array().cloned().unwrap_or_default() {
                        let d = d.as_u64().unwrap_or(0) as u8;
                        if !(1..=7).contains(&d) {
                            return Err(RulesError::Weekday(d));
                        }
                        r.weekdays.push(d);
                    }
                }
                "window_start" => r.window_start = Some(parse_clock(val)?),
                "window_end" => r.window_end = Some(parse_clock(val)?),
                "internal_daily_cap" => {
                    let n = val.as_u64().unwrap_or(0) as u32;
                    if n == 0 {
                        return Err(RulesError::CapZero);
                    }
                    r.internal_daily_cap = Some(n);
                }
                "minimum_gap_seconds" => {
                    r.minimum_gap_seconds = Some(val.as_u64().unwrap_or(0) as u32)
                }
                "launch_not_before" | "launch_expires_at" => {
                    let t = DateTime::parse_from_rfc3339(val.as_str().unwrap_or_default())
                        .map_err(|_| RulesError::Clock(val.to_string()))?
                        .with_timezone(&Utc);
                    if k == "launch_not_before" {
                        r.launch_not_before = Some(t);
                    } else {
                        r.launch_expires_at = Some(t);
                    }
                }
                "meeting_type_id" => {
                    let raw = val.as_str().unwrap_or_default();
                    r.meeting_type_id = Some(
                        Uuid::parse_str(raw).map_err(|_| RulesError::MeetingTypeId(raw.into()))?,
                    );
                }
                other => return Err(RulesError::UnknownField(other.to_string())),
            }
        }
        if let (Some(s), Some(e)) = (r.window_start, r.window_end)
            && s >= e
        {
            return Err(RulesError::WindowOrder);
        }
        r.weekdays.sort_unstable();
        r.weekdays.dedup();
        Ok(r)
    }

    /// Canonical JSONB form stored on the revision: only set fields, clock
    /// values normalized to "HH:MM" so `from_json(to_json(r))` round-trips
    /// and the manifest hash is stable.
    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::Map::new();
        if !self.weekdays.is_empty() {
            m.insert("weekdays".into(), serde_json::json!(self.weekdays));
        }
        let fmt = |t: NaiveTime| t.format("%H:%M").to_string();
        if let Some(t) = self.window_start {
            m.insert("window_start".into(), fmt(t).into());
        }
        if let Some(t) = self.window_end {
            m.insert("window_end".into(), fmt(t).into());
        }
        if let Some(n) = self.internal_daily_cap {
            m.insert("internal_daily_cap".into(), n.into());
        }
        if let Some(n) = self.minimum_gap_seconds {
            m.insert("minimum_gap_seconds".into(), n.into());
        }
        if let Some(t) = self.launch_not_before {
            m.insert("launch_not_before".into(), t.to_rfc3339().into());
        }
        if let Some(t) = self.launch_expires_at {
            m.insert("launch_expires_at".into(), t.to_rfc3339().into());
        }
        if let Some(id) = self.meeting_type_id {
            m.insert("meeting_type_id".into(), id.to_string().into());
        }
        serde_json::Value::Object(m)
    }

    fn allows(&self, weekday_iso: u8) -> bool {
        self.weekdays.is_empty() || self.weekdays.contains(&weekday_iso)
    }

    /// The daily send window in local time (defaults to all day).
    pub fn window(&self) -> (NaiveTime, NaiveTime) {
        (
            self.window_start
                .unwrap_or_else(|| NaiveTime::from_hms_opt(0, 0, 0).unwrap()),
            self.window_end
                .unwrap_or_else(|| NaiveTime::from_hms_opt(23, 59, 59).unwrap()),
        )
    }
}

/// Parse an IANA timezone (source B.1.1 `Timezone`).
pub fn parse_timezone(tz: &str) -> Option<Tz> {
    Tz::from_str(tz).ok()
}

/// Clamp `earliest` (UTC) forward into the next allowed send instant under
/// `rules` in timezone `tz`: honoring `launch_not_before`, weekday filter and
/// the daily window. Iterates at most 370 days; `None` when nothing fits.
pub fn next_send_time(
    earliest: DateTime<Utc>,
    rules: &ScheduleRules,
    tz: Tz,
) -> Option<DateTime<Utc>> {
    let mut cand = earliest;
    if let Some(nb) = rules.launch_not_before
        && cand < nb
    {
        cand = nb;
    }
    let (win_start, win_end) = rules.window();
    for _ in 0..370 {
        let local = cand.with_timezone(&tz);
        let iso = local.weekday().number_from_monday() as u8;
        if !rules.allows(iso) {
            cand = (local.date_naive() + chrono::Days::new(1))
                .and_time(win_start)
                .and_local_timezone(tz)
                .earliest()?
                .with_timezone(&Utc);
            continue;
        }
        let t = local.time();
        if t < win_start {
            cand = local
                .date_naive()
                .and_time(win_start)
                .and_local_timezone(tz)
                .earliest()?
                .with_timezone(&Utc);
            continue;
        }
        if t > win_end {
            cand = (local.date_naive() + chrono::Days::new(1))
                .and_time(win_start)
                .and_local_timezone(tz)
                .earliest()?
                .with_timezone(&Utc);
            continue;
        }
        return Some(cand);
    }
    None
}

/// Position `index` (0-based) inside a bulk activation: `internal_daily_cap`
/// spreads the batch across days and `minimum_gap_seconds` spaces sends
/// inside a day; the result is then window-clamped (§7.5 step 4).
pub fn staggered_send_time(
    base_earliest: DateTime<Utc>,
    index: u64,
    rules: &ScheduleRules,
    tz: Tz,
) -> Option<DateTime<Utc>> {
    let cap = rules.internal_daily_cap.unwrap_or(u32::MAX).max(1) as u64;
    let gap = rules.minimum_gap_seconds.unwrap_or(0) as i64;
    let day_offset = index / cap;
    let intra_day = (index % cap) as i64 * gap;
    let mut cand = base_earliest + chrono::Duration::days(day_offset as i64);
    cand += chrono::Duration::seconds(intra_day);
    next_send_time(cand, rules, tz)
}

// --------------------------------------------------------------- manifest

/// One manifest recipient line-set: the address plus every frozen step
/// payload hash in `step_order` (only included for enrollable recipients).
#[derive(Debug, Clone)]
pub struct ManifestRecipient {
    pub email_address_id: Uuid,
    /// (step_order, payload_hash bytes) sorted by step_order.
    pub steps: Vec<(i32, [u8; 32])>,
    /// Exclusion reason when the address could not be enrolled (the frozen
    /// launch records why; e.g. "suppressed", "basis_unverified").
    pub excluded_reason: Option<String>,
}

/// Canonical byte form of a launch manifest (§8.2): sorted recipients,
/// per-step payload hashes, channel, rules and the expiry. Any recipient,
/// content or rule change changes the hash — it is what approval pins.
pub fn manifest_bytes(
    launch_id: Uuid,
    revision_id: Uuid,
    delivery_channel_id: Uuid,
    schedule_rules: &serde_json::Value,
    expires_at: DateTime<Utc>,
    recipients: &[ManifestRecipient],
) -> Vec<u8> {
    let mut sorted: Vec<&ManifestRecipient> = recipients.iter().collect();
    sorted.sort_by_key(|r| r.email_address_id);
    let mut out = String::new();
    out.push_str("manifest/v1\n");
    out.push_str(&format!("launch:{launch_id}\n"));
    out.push_str(&format!("revision:{revision_id}\n"));
    out.push_str(&format!("channel:{delivery_channel_id}\n"));
    // Canonical JSON: serde_json preserves BTreeMap order for objects built
    // via `to_string` only when the value came from a map — normalize
    // through `serde_json::to_string` on the raw JSONB (keys are stored
    // sorted by Postgres jsonb already).
    out.push_str(&format!("rules:{}\n", canonical_json(schedule_rules)));
    out.push_str(&format!("expires:{}\n", expires_at.to_rfc3339()));
    for r in sorted {
        match &r.excluded_reason {
            Some(reason) => {
                out.push_str(&format!("excluded:{}:{reason}\n", r.email_address_id));
            }
            None => {
                out.push_str(&format!("recipient:{}\n", r.email_address_id));
                for (order, hash) in &r.steps {
                    out.push_str(&format!("  step:{order}:{}\n", hex_encode(hash)));
                }
            }
        }
    }
    out.into_bytes()
}

fn hex_encode(b: &[u8; 32]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn canonical_json(v: &serde_json::Value) -> String {
    // serde_json::Map is BTreeMap-ordered (no preserve_order feature), so a
    // rebuild with normalized children is canonical.
    fn norm(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(m) => {
                serde_json::Value::Object(m.iter().map(|(k, v)| (k.clone(), norm(v))).collect())
            }
            serde_json::Value::Array(a) => serde_json::Value::Array(a.iter().map(norm).collect()),
            other => other.clone(),
        }
    }
    norm(v).to_string()
}

/// The per-message payload hash input: the frozen outbound snapshot fields.
pub fn payload_bytes(
    from: &str,
    to: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
) -> Vec<u8> {
    payload_bytes_with_context(from, to, subject, body_text, body_html, None, None)
}

/// Campaign payload hash input with immutable correspondence identifiers.
/// `None` for both identifiers preserves the exact v1 input used by legacy
/// frozen campaign outbounds.
pub fn payload_bytes_with_context(
    from: &str,
    to: &str,
    subject: &str,
    body_text: &str,
    body_html: Option<&str>,
    thread_id: Option<&str>,
    internet_message_id: Option<&str>,
) -> Vec<u8> {
    let legacy = format!(
        "payload/v1\nfrom:{from}\nto:{to}\nsubject:{subject}\ntext:{body_text}\nhtml:{}",
        body_html.unwrap_or("")
    );
    if thread_id.is_none() && internet_message_id.is_none() {
        return legacy.into_bytes();
    }
    format!(
        "{legacy}\nthread_id:{}\ninternet_message_id:{}",
        thread_id.unwrap_or(""),
        internet_message_id.unwrap_or("")
    )
    .into_bytes()
}

// -------------------------------------------------------------- renderer

/// §8.1 allowlist renderer: substitutes `{{name}}` / `{{ name }}` where
/// `name` is a *declared* variable (schema key), HTML-escaping values on the
/// HTML variant (`render_html`). Every remaining `{{` after substitution —
/// and every declared-but-missing value — is an error: unresolved variables
/// are forbidden at freeze (§7.5).
#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    MissingVariable(String),
    UndeclaredVariable(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingVariable(v) => write!(f, "missing template variable {v}"),
            Self::UndeclaredVariable(v) => write!(f, "undeclared template variable {v}"),
        }
    }
}

fn var_name_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
}

fn substitute<F: Fn(&str) -> String>(
    text: &str,
    declared: &BTreeSet<String>,
    values: &BTreeMap<String, String>,
    escape: F,
) -> Result<String, RenderError> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            return Err(RenderError::UndeclaredVariable("{{".to_string()));
        };
        let name = after[..close].trim();
        if !var_name_ok(name) || !declared.contains(name) {
            return Err(RenderError::UndeclaredVariable(name.to_string()));
        }
        let Some(v) = values.get(name) else {
            return Err(RenderError::MissingVariable(name.to_string()));
        };
        out.push_str(&escape(v));
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Plain-text render — values substituted verbatim.
pub fn render_text(
    text: &str,
    declared: &BTreeSet<String>,
    values: &BTreeMap<String, String>,
) -> Result<String, RenderError> {
    substitute(text, declared, values, |v| v.to_string())
}

/// HTML render — values HTML-escaped (§8.1 HTML escaping).
pub fn render_html(
    text: &str,
    declared: &BTreeSet<String>,
    values: &BTreeMap<String, String>,
) -> Result<String, RenderError> {
    substitute(text, declared, values, |v| {
        v.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    })
}

/// Variable names declared by `template_versions.variable_schema`. Accepts
/// the natural shape `{ "var.name": { "required": bool, "default": "…" } }`.
/// Returns (declared names, defaults).
pub fn declared_variables(
    schema: &serde_json::Value,
) -> (BTreeSet<String>, BTreeMap<String, String>) {
    let mut declared = BTreeSet::new();
    let mut defaults = BTreeMap::new();
    if let Some(obj) = schema.as_object() {
        for (k, v) in obj {
            if var_name_ok(k) {
                declared.insert(k.clone());
                if let Some(d) = v.get("default").and_then(|d| d.as_str()) {
                    defaults.insert(k.clone(), d.to_string());
                }
            }
        }
    }
    (declared, defaults)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn next_send_respects_window_and_weekday() {
        let rules = ScheduleRules {
            weekdays: vec![1, 2, 3, 4, 5],
            window_start: NaiveTime::from_hms_opt(9, 0, 0),
            window_end: NaiveTime::from_hms_opt(17, 0, 0),
            ..Default::default()
        };
        // 2026-10-10 is a Saturday — next slot is Monday 09:00 Seoul.
        let sat = Utc.with_ymd_and_hms(2026, 10, 10, 3, 0, 0).unwrap();
        let t = next_send_time(sat, &rules, chrono_tz::Asia::Seoul).unwrap();
        let local = t.with_timezone(&chrono_tz::Asia::Seoul);
        assert_eq!(local.weekday(), chrono::Weekday::Mon);
        assert_eq!(local.time(), NaiveTime::from_hms_opt(9, 0, 0).unwrap());
    }

    #[test]
    fn next_send_same_day_inside_window() {
        let rules = ScheduleRules {
            window_start: NaiveTime::from_hms_opt(9, 0, 0),
            window_end: NaiveTime::from_hms_opt(17, 0, 0),
            ..Default::default()
        };
        let inside = Utc.with_ymd_and_hms(2026, 10, 12, 5, 0, 0).unwrap(); // 14:00 Seoul
        let t = next_send_time(inside, &rules, chrono_tz::Asia::Seoul).unwrap();
        assert_eq!(t, inside);
    }

    #[test]
    fn schedule_rules_keep_optional_meeting_binding_in_manifest_json() {
        let meeting_type_id = Uuid::new_v4();
        let rules = ScheduleRules::from_json(&serde_json::json!({
            "meeting_type_id": meeting_type_id,
            "weekdays": [1, 2, 3, 4, 5],
            "window_start": "09:00",
            "window_end": "17:00"
        }))
        .unwrap();
        assert_eq!(rules.meeting_type_id, Some(meeting_type_id));
        assert_eq!(
            rules.to_json()["meeting_type_id"],
            meeting_type_id.to_string()
        );
        assert!(
            ScheduleRules::from_json(&serde_json::json!({
                "meeting_type_id": "not-a-uuid"
            }))
            .is_err()
        );
    }

    #[test]
    fn stagger_spreads_over_cap_and_gap() {
        let rules = ScheduleRules {
            internal_daily_cap: Some(2),
            minimum_gap_seconds: Some(60),
            ..Default::default()
        };
        let base = Utc.with_ymd_and_hms(2026, 10, 12, 0, 0, 0).unwrap();
        let t0 = staggered_send_time(base, 0, &rules, chrono_tz::UTC).unwrap();
        let t1 = staggered_send_time(base, 1, &rules, chrono_tz::UTC).unwrap();
        let t2 = staggered_send_time(base, 2, &rules, chrono_tz::UTC).unwrap();
        assert_eq!((t1 - t0).num_seconds(), 60);
        assert_eq!((t2 - t0).num_days(), 1);
    }

    #[test]
    fn render_rejects_undeclared_and_missing() {
        let declared: BTreeSet<String> = ["company.name".to_string()].into_iter().collect();
        let mut values = BTreeMap::new();
        values.insert("company.name".to_string(), "Acme".to_string());
        assert_eq!(
            render_text("Hi {{ company.name }}!", &declared, &values).unwrap(),
            "Hi Acme!"
        );
        assert!(matches!(
            render_text("{{evil}}", &declared, &values),
            Err(RenderError::UndeclaredVariable(_))
        ));
        assert!(matches!(
            render_text("{{company.name}} {{other}}", &declared, &values),
            Err(RenderError::UndeclaredVariable(_))
        ));
        // Declared but no value.
        let empty = BTreeMap::new();
        assert!(matches!(
            render_text("{{company.name}}", &declared, &empty),
            Err(RenderError::MissingVariable(_))
        ));
    }

    #[test]
    fn render_html_escapes() {
        let declared: BTreeSet<String> = ["v".to_string()].into_iter().collect();
        let mut values = BTreeMap::new();
        values.insert("v".to_string(), "<b>&\"".to_string());
        assert_eq!(
            render_html("{{v}}", &declared, &values).unwrap(),
            "&lt;b&gt;&amp;&quot;"
        );
    }

    #[test]
    fn manifest_is_order_independent_and_sensitive() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let rules = serde_json::json!({"weekdays":[1,2]});
        let exp = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap();
        let r1 = vec![
            ManifestRecipient {
                email_address_id: a,
                steps: vec![(1, [1u8; 32])],
                excluded_reason: None,
            },
            ManifestRecipient {
                email_address_id: b,
                steps: vec![],
                excluded_reason: Some("suppressed".into()),
            },
        ];
        let mut r2 = r1.clone();
        r2.reverse();
        let m1 = manifest_bytes(a, b, a, &rules, exp, &r1);
        let m2 = manifest_bytes(a, b, a, &rules, exp, &r2);
        assert_eq!(m1, m2, "recipient order must not matter");
        let r3 = vec![ManifestRecipient {
            email_address_id: a,
            steps: vec![(1, [2u8; 32])],
            excluded_reason: None,
        }];
        let m3 = manifest_bytes(a, b, a, &rules, exp, &r3);
        assert_ne!(m1, m3, "content change must change the manifest");
    }

    #[test]
    fn campaign_payload_context_is_hashed_without_changing_legacy_none_context() {
        let legacy = payload_bytes(
            "from@example.test",
            "to@example.test",
            "subject",
            "body",
            None,
        );
        let expected_legacy =
            b"payload/v1\nfrom:from@example.test\nto:to@example.test\nsubject:subject\ntext:body\nhtml:";
        assert_eq!(legacy, expected_legacy);

        let contextual = payload_bytes_with_context(
            "from@example.test",
            "to@example.test",
            "subject",
            "body",
            None,
            Some("thread-1"),
            Some("<outbound-1@example.test>"),
        );
        assert_ne!(contextual, legacy);
        assert_ne!(
            contextual,
            payload_bytes_with_context(
                "from@example.test",
                "to@example.test",
                "subject",
                "body",
                None,
                Some("thread-2"),
                Some("<outbound-1@example.test>"),
            )
        );
        assert_eq!(
            payload_bytes_with_context(
                "from@example.test",
                "to@example.test",
                "subject",
                "body",
                None,
                None,
                None,
            ),
            legacy
        );
    }
}
