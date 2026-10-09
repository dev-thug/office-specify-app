//! agents module domain types (spec §6) — `agent_runs` 상태기계.
//! Status 문자열은 DDL CHECK(migration 0001)와 동일 값이다.

use serde::{Deserialize, Serialize};

/// `agent_runs.status` CHECK 값과 동일한 열거형 (§12.4). API enum은 이
/// 타입에서 파생하며 wire 문자열은 `as_str`과 동일해야 한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    Queued,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Canceled,
}

impl AgentRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "awaiting_approval" => Self::AwaitingApproval,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "canceled" => Self::Canceled,
            _ => return None,
        })
    }

    /// 종결 상태 — 취소·변경 대상이 아니다.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }

    /// §16.3 "에이전트 취소 → 새 도구 실행 중단" — 아직 진행 가능한 run만.
    pub fn can_cancel(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::AwaitingApproval)
    }
}

/// Defense-in-depth for MCP tool action metadata: retain stable IDs and
/// small status/version labels, never arbitrary text, addresses, bodies or
/// credentials. The API also applies this projection when reading old rows.
pub fn redact_action_arguments(value: &serde_json::Value) -> serde_json::Value {
    let Some(object) = value.as_object() else {
        return serde_json::json!({});
    };
    let mut safe = serde_json::Map::new();
    for (key, value) in object {
        let normalized = key.to_ascii_lowercase();
        let sensitive_key = ["credential", "secret", "token", "password"]
            .iter()
            .any(|needle| normalized.contains(needle));
        let is_id = !sensitive_key
            && (normalized == "id" || normalized.ends_with("_id"))
            && value
                .as_str()
                .is_some_and(|raw| uuid::Uuid::parse_str(raw).is_ok());
        let is_version = matches!(normalized.as_str(), "version" | "expected_version")
            && value.as_i64().is_some_and(|n| n >= 0);
        let is_label = matches!(
            normalized.as_str(),
            "status" | "decision" | "result_code" | "reason_code"
        ) && value.as_str().is_some_and(is_safe_action_label);
        if is_id || is_version || is_label {
            safe.insert(key.clone(), value.clone());
        }
    }
    serde_json::Value::Object(safe)
}

fn is_safe_action_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip_matches_ddl() {
        for s in [
            "queued",
            "running",
            "awaiting_approval",
            "completed",
            "failed",
            "canceled",
        ] {
            let st = AgentRunStatus::parse(s).expect(s);
            assert_eq!(st.as_str(), s);
        }
        assert!(AgentRunStatus::parse("bogus").is_none());
    }

    #[test]
    fn cancel_rules() {
        assert!(AgentRunStatus::Queued.can_cancel());
        assert!(AgentRunStatus::Running.can_cancel());
        assert!(AgentRunStatus::AwaitingApproval.can_cancel());
        assert!(AgentRunStatus::Canceled.is_terminal());
        assert!(!AgentRunStatus::Completed.can_cancel());
    }

    #[test]
    fn action_argument_redaction_keeps_only_ids_and_labels() {
        let id = "10000000-0000-4000-8000-000000000001";
        assert_eq!(
            redact_action_arguments(&serde_json::json!({
                "company_id": id,
                "email": "person@example.com",
                "body": "private text",
                "status": "pending",
                "access_token": "secret",
            })),
            serde_json::json!({"company_id": id, "status": "pending"})
        );
    }
}
