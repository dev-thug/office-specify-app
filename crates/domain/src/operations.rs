//! operations module domain rules (spec §6) — job retryability and the
//! external-effect kind list used until the persisted column lands.

/// `POST /jobs/{id}/retry-requests`에 응할 수 있는 job 상태.
/// `dead`·`retry_wait`만 허용한다. `unknown`은 ADR-006에 따라 절대 자동·수동
/// 재시도되지 않는다 — 대조(reconcile)만 허용된다. queued/running은 아직
/// 실행 가능 상태이고 completed/canceled는 종결 상태라 대상이 아니다.
pub fn is_retryable_state(state: &str) -> bool {
    matches!(state, "dead" | "retry_wait")
}

/// `unknown` 상태 — 운영자 재시도 요청조차 거절되는지 판정 (B.5
/// PROVIDER_SUBMISSION_OUTCOME_UNKNOWN으로 응답).
pub fn is_unknown_state(state: &str) -> bool {
    state == "unknown"
}

/// Job payloads are internal queue messages. The operations API returns only
/// known identifier/version references and the fixed pause reason enum.
pub fn redact_job_payload(value: &serde_json::Value) -> serde_json::Value {
    let Some(object) = value.as_object() else {
        return serde_json::json!({});
    };
    let mut safe = serde_json::Map::new();
    for (key, value) in object {
        if key == "merge_request" {
            if let Some(nested) = value.as_object() {
                let mut refs = serde_json::Map::new();
                for (nested_key, nested_value) in nested {
                    if matches!(nested_key.as_str(), "source_id" | "target_id")
                        && is_uuid(nested_value)
                    {
                        refs.insert(nested_key.clone(), nested_value.clone());
                    }
                }
                if !refs.is_empty() {
                    safe.insert(key.clone(), serde_json::Value::Object(refs));
                }
            }
            continue;
        }
        let is_id = is_identifier_key(key) && is_uuid(value);
        let is_version = (key == "version" || key.ends_with("_version")) && value.is_i64();
        let is_cancel_pending = key == "cancel_pending" && value.is_boolean();
        let is_reason = key == "reason"
            && value.as_str().is_some_and(|v| {
                matches!(
                    v,
                    "human_reply"
                        | "auto_reply_review"
                        | "meeting_requested"
                        | "unsubscribe_request"
                )
            });
        if is_id || is_version || is_cancel_pending || is_reason {
            safe.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(safe)
}

/// Read-time audit allowlist. Recording helpers already forbid bodies and
/// tokens, but this also protects old or malformed rows written by another
/// module before the redaction contract was applied.
pub fn redact_audit_details(value: &serde_json::Value) -> serde_json::Value {
    let Some(object) = value.as_object() else {
        return serde_json::json!({});
    };
    let mut safe = serde_json::Map::new();
    for (key, value) in object {
        let is_label_key = matches!(
            key.as_str(),
            "outcome" | "kind" | "status" | "decision" | "error_code" | "tool_name" | "result_code"
        ) || key.ends_with("_state");
        let is_label = is_label_key && value.as_str().is_some_and(is_safe_label);
        let is_id = is_identifier_key(key) && is_uuid(value);
        let is_boolean = matches!(
            key.as_str(),
            "external_effect" | "send_workers_locked" | "emergency_stop"
        ) && value.is_boolean();
        let is_count = (key.ends_with("_count")
            || key.ends_with("_length")
            || key.ends_with("_attempts")
            || key.ends_with("_version"))
            && value.as_i64().is_some_and(|v| v >= 0);
        let is_hash = (key.ends_with("_hash") || key == "reason_hash")
            && value.as_str().is_some_and(is_sha256_hex);
        if is_label || is_id || is_boolean || is_count || is_hash {
            safe.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(safe)
}

fn is_identifier_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    !["credential", "secret", "token", "password"]
        .iter()
        .any(|sensitive| key.contains(sensitive))
        && (key == "id" || key.ends_with("_id") || key == "ids" || key.ends_with("_ids"))
}

fn is_uuid(value: &serde_json::Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
}

fn is_safe_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_states() {
        assert!(is_retryable_state("dead"));
        assert!(is_retryable_state("retry_wait"));
        for s in ["queued", "running", "completed", "canceled", "unknown"] {
            assert!(!is_retryable_state(s), "{s} must not be retryable");
        }
    }

    #[test]
    fn job_payload_redaction_keeps_only_known_refs_and_enum_reason() {
        let safe_id = "10000000-0000-4000-8000-000000000001";
        let value = serde_json::json!({
            "outbound_message_id": safe_id,
            "access_token": "secret",
            "body": "private message",
            "reason": "human_reply",
            "unexpected": "drop",
        });
        assert_eq!(
            redact_job_payload(&value),
            serde_json::json!({
                "outbound_message_id": safe_id,
                "reason": "human_reply",
            })
        );
    }

    #[test]
    fn audit_redaction_drops_sensitive_or_unapproved_values() {
        let id = "10000000-0000-4000-8000-000000000001";
        let details = serde_json::json!({
            "outcome": "ok",
            "job_id": id,
            "password": "secret",
            "body": "private message",
            "comment": "unreviewed data",
        });
        assert_eq!(
            redact_audit_details(&details),
            serde_json::json!({"outcome": "ok", "job_id": id})
        );
    }
}
